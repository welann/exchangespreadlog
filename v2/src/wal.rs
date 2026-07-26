use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Context;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{domain::MarketEvent, ingest::time::unix_time_ns};

const PROJECTOR_NAME: &str = "clickhouse";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalEvent {
    #[serde(with = "crate::domain::integer::i128_string")]
    pub recorded_ts_ns: i128,
    pub event: MarketEvent,
}

#[derive(Debug, Clone)]
pub struct WalRecord {
    pub seq: i64,
    pub event_id: String,
    pub value: WalEvent,
}

#[derive(Clone)]
pub struct DurableEventLog {
    connection: Arc<Mutex<Connection>>,
}

impl DurableEventLog {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create WAL directory {}", parent.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("open SQLite WAL {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
CREATE TABLE IF NOT EXISTS event_log
(
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    payload TEXT NOT NULL,
    created_at_ns TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS projector_checkpoint
(
    name TEXT PRIMARY KEY,
    last_seq INTEGER NOT NULL
);
INSERT OR IGNORE INTO projector_checkpoint(name, last_seq)
VALUES ('clickhouse', 0);
"#,
        )?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn append(&self, event: MarketEvent) -> anyhow::Result<i64> {
        let (recorded_ts_ns, payload, event_id) = encode_event(&event)?;
        let connection = self.lock()?;
        connection.execute(
            "INSERT OR IGNORE INTO event_log(event_id, payload, created_at_ns) VALUES (?1, ?2, ?3)",
            params![event_id, payload, recorded_ts_ns.to_string()],
        )?;
        connection
            .query_row(
                "SELECT seq FROM event_log WHERE event_id = ?1",
                params![event_id],
                |row| row.get(0),
            )
            .context("resolve appended WAL sequence")
    }

    pub fn append_batch(&self, events: &[MarketEvent]) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT OR IGNORE INTO event_log(event_id, payload, created_at_ns) VALUES (?1, ?2, ?3)",
            )?;
            for event in events {
                let (recorded_ts_ns, payload, event_id) = encode_event(event)?;
                statement.execute(params![event_id, payload, recorded_ts_ns.to_string()])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn read_projector_batch(&self, limit: usize) -> anyhow::Result<Vec<WalRecord>> {
        let connection = self.lock()?;
        let checkpoint: i64 = connection.query_row(
            "SELECT last_seq FROM projector_checkpoint WHERE name = ?1",
            params![PROJECTOR_NAME],
            |row| row.get(0),
        )?;
        let mut statement = connection.prepare(
            "SELECT seq, event_id, payload FROM event_log WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = statement.query_map(params![checkpoint, limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (seq, event_id, payload) = row?;
            let value = serde_json::from_str(&payload)
                .with_context(|| format!("decode WAL event at sequence {seq}"))?;
            Ok(WalRecord {
                seq,
                event_id,
                value,
            })
        })
        .collect()
    }

    pub fn acknowledge_projector(&self, last_seq: i64) -> anyhow::Result<()> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE projector_checkpoint SET last_seq = MAX(last_seq, ?1) WHERE name = ?2",
            params![last_seq, PROJECTOR_NAME],
        )?;
        transaction.execute("DELETE FROM event_log WHERE seq <= ?1", params![last_seq])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn pending_count(&self) -> anyhow::Result<u64> {
        let connection = self.lock()?;
        let value: i64 = connection.query_row(
            r#"
SELECT count()
FROM event_log
WHERE seq > (
    SELECT last_seq FROM projector_checkpoint WHERE name = ?1
)
"#,
            params![PROJECTOR_NAME],
            |row| row.get(0),
        )?;
        u64::try_from(value).context("WAL pending count is negative")
    }

    pub fn latest_sequence(&self) -> anyhow::Result<Option<i64>> {
        let connection = self.lock()?;
        connection
            .query_row("SELECT max(seq) FROM event_log", [], |row| {
                row.get::<_, Option<i64>>(0)
            })
            .map_err(Into::into)
    }

    fn lock(&self) -> anyhow::Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("SQLite WAL lock is poisoned"))
    }
}

fn encode_event(event: &MarketEvent) -> anyhow::Result<(i128, String, String)> {
    let recorded_ts_ns = match event {
        MarketEvent::Tick { tick } => tick.recv_ts_ns,
        _ => unix_time_ns(),
    };

    #[derive(Serialize)]
    struct WalEventRef<'a> {
        #[serde(with = "crate::domain::integer::i128_string")]
        recorded_ts_ns: i128,
        event: &'a MarketEvent,
    }

    let value = WalEventRef {
        recorded_ts_ns,
        event,
    };
    let payload = serde_json::to_string(&value).context("serialize WAL event")?;
    let event_id = hash_event(&payload);
    Ok((recorded_ts_ns, payload, event_id))
}

fn hash_event(payload: &str) -> String {
    let digest = Sha256::digest(payload.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::{
        domain::{BboTick, MarketEvent},
        wal::{DurableEventLog, WalEvent},
    };

    #[test]
    fn appends_reads_and_acknowledges_events() {
        let directory = tempdir().unwrap();
        let log = DurableEventLog::open(directory.path().join("wal.sqlite3")).unwrap();
        log.append(MarketEvent::VenueReset {
            venue_instance_id: "lighter".to_string(),
        })
        .unwrap();
        let rows = log.read_projector_batch(10).unwrap();
        assert_eq!(rows.len(), 1);
        log.acknowledge_projector(rows[0].seq).unwrap();
        assert_eq!(log.pending_count().unwrap(), 0);
    }

    #[test]
    fn appends_a_batch_in_one_transaction() {
        let directory = tempdir().unwrap();
        let log = DurableEventLog::open(directory.path().join("wal.sqlite3")).unwrap();
        log.append_batch(&[
            MarketEvent::VenueReset {
                venue_instance_id: "perpl".to_string(),
            },
            MarketEvent::VenueReset {
                venue_instance_id: "lighter".to_string(),
            },
        ])
        .unwrap();

        let rows = log.read_projector_batch(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].seq + 1, rows[1].seq);
    }

    #[test]
    fn decodes_numeric_i128_fields_written_before_string_encoding() {
        let payload = r#"{
            "recorded_ts_ns":1785040548687075000,
            "event":{
                "type":"tick",
                "tick":{
                    "instrument":{
                        "catalog_id":"perpl:1:test",
                        "venue_instance_id":"perpl",
                        "instrument_id":"1"
                    },
                    "recv_ts_ns":1785040548687075000,
                    "exchange_ts_ms":1785040548000,
                    "sequence":90392603,
                    "bid":{"price":"64507.4","size":"0.07750","order_count":1},
                    "ask":{"price":"64507.9","size":"0.00250","order_count":1},
                    "spread":"0.5",
                    "mid":"64507.65",
                    "source":"l2_book",
                    "quality":{"gap":false,"stale":false,"inconsistent":false}
                }
            }
        }"#;
        let decoded: WalEvent = serde_json::from_str(payload).unwrap();
        let MarketEvent::Tick {
            tick:
                BboTick {
                    recv_ts_ns,
                    sequence,
                    ..
                },
        } = decoded.event
        else {
            panic!("expected tick event");
        };
        assert_eq!(recv_ts_ns, 1_785_040_548_687_075_000);
        assert_eq!(sequence, Some(90_392_603));
    }
}
