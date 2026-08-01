use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

use anyhow::Context;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::catalog::CatalogInstrument;

#[derive(Clone)]
pub struct CatalogControl {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogInstrumentView {
    pub venue: String,
    pub instrument_id: String,
    pub symbol: String,
    pub normalized_symbol: String,
    pub product_type: String,
    pub quote_asset: String,
    pub status: String,
    pub eligible: bool,
    pub eligibility_reason: Option<String>,
    pub present: bool,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
    pub raw_json: String,
    pub asset_group_id: Option<i64>,
    pub asset_group_symbol: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetGroup {
    pub id: i64,
    pub symbol: String,
    pub members: Vec<InstrumentKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentKey {
    pub venue: String,
    pub instrument_id: String,
}

impl CatalogControl {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("create catalog database directory {}", parent.display())
            })?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("open catalog database {}", path.display()))?;
        connection.execute_batch(
            r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS venue_instruments
(
    venue TEXT NOT NULL,
    instrument_id TEXT NOT NULL,
    symbol TEXT NOT NULL,
    normalized_symbol TEXT NOT NULL,
    product_type TEXT NOT NULL,
    quote_asset TEXT NOT NULL,
    status TEXT NOT NULL,
    eligible INTEGER NOT NULL,
    eligibility_reason TEXT,
    present INTEGER NOT NULL,
    first_seen_ms INTEGER NOT NULL,
    last_seen_ms INTEGER NOT NULL,
    raw_json TEXT NOT NULL,
    PRIMARY KEY (venue, instrument_id)
);

CREATE TABLE IF NOT EXISTS asset_groups
(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol TEXT NOT NULL UNIQUE COLLATE NOCASE,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS instrument_mappings
(
    venue TEXT NOT NULL,
    instrument_id TEXT NOT NULL,
    asset_group_id INTEGER NOT NULL REFERENCES asset_groups(id) ON DELETE CASCADE,
    PRIMARY KEY (venue, instrument_id),
    UNIQUE (asset_group_id, venue),
    FOREIGN KEY (venue, instrument_id)
        REFERENCES venue_instruments(venue, instrument_id) ON DELETE CASCADE
);
"#,
        )?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn sync_venue(&self, venue: &str, instruments: &[CatalogInstrument]) -> anyhow::Result<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE venue_instruments SET present = 0 WHERE venue = ?1",
            params![venue],
        )?;
        {
            let mut statement = transaction.prepare_cached(
                r#"
INSERT INTO venue_instruments
(
    venue, instrument_id, symbol, normalized_symbol, product_type,
    quote_asset, status, eligible, eligibility_reason, present,
    first_seen_ms, last_seen_ms, raw_json
)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?10, ?11)
ON CONFLICT(venue, instrument_id) DO UPDATE SET
    symbol = excluded.symbol,
    normalized_symbol = excluded.normalized_symbol,
    product_type = excluded.product_type,
    quote_asset = excluded.quote_asset,
    status = excluded.status,
    eligible = excluded.eligible,
    eligibility_reason = excluded.eligibility_reason,
    present = 1,
    last_seen_ms = excluded.last_seen_ms,
    raw_json = excluded.raw_json
"#,
            )?;
            for instrument in instruments {
                statement.execute(params![
                    instrument.venue,
                    instrument.instrument_id,
                    instrument.symbol,
                    instrument.normalized_symbol,
                    instrument.product_type.as_str(),
                    instrument.quote_asset,
                    instrument.status,
                    instrument.eligible,
                    instrument.eligibility_reason,
                    now_ms,
                    serde_json::to_string(&instrument.raw_json)?,
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn instruments(&self) -> anyhow::Result<Vec<CatalogInstrumentView>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            r#"
SELECT
    i.venue, i.instrument_id, i.symbol, i.normalized_symbol,
    i.product_type, i.quote_asset, i.status, i.eligible,
    i.eligibility_reason, i.present, i.first_seen_ms, i.last_seen_ms,
    i.raw_json, g.id, g.symbol
FROM venue_instruments AS i
LEFT JOIN instrument_mappings AS m
    ON m.venue = i.venue AND m.instrument_id = i.instrument_id
LEFT JOIN asset_groups AS g ON g.id = m.asset_group_id
ORDER BY i.venue, i.symbol, i.instrument_id
"#,
        )?;
        let rows = statement.query_map([], |row| {
            Ok(CatalogInstrumentView {
                venue: row.get(0)?,
                instrument_id: row.get(1)?,
                symbol: row.get(2)?,
                normalized_symbol: row.get(3)?,
                product_type: row.get(4)?,
                quote_asset: row.get(5)?,
                status: row.get(6)?,
                eligible: row.get(7)?,
                eligibility_reason: row.get(8)?,
                present: row.get(9)?,
                first_seen_ms: row.get(10)?,
                last_seen_ms: row.get(11)?,
                raw_json: row.get(12)?,
                asset_group_id: row.get(13)?,
                asset_group_symbol: row.get(14)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn groups(&self) -> anyhow::Result<Vec<AssetGroup>> {
        let connection = self.lock()?;
        load_groups(&connection)
    }

    pub fn create_group(
        &self,
        symbol: &str,
        members: &[InstrumentKey],
    ) -> anyhow::Result<AssetGroup> {
        let symbol = normalized_group_symbol(symbol)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO asset_groups(symbol, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?2)",
            params![symbol, now_ms],
        )?;
        let id = transaction.last_insert_rowid();
        replace_members(&transaction, id, members)?;
        transaction.commit()?;
        Ok(AssetGroup {
            id,
            symbol,
            members: members.to_vec(),
        })
    }

    pub fn update_group(
        &self,
        id: i64,
        symbol: &str,
        members: &[InstrumentKey],
    ) -> anyhow::Result<AssetGroup> {
        let symbol = normalized_group_symbol(symbol)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE asset_groups SET symbol = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![symbol, now_ms, id],
        )?;
        replace_members(&transaction, id, members)?;
        transaction.commit()?;
        Ok(AssetGroup {
            id,
            symbol,
            members: members.to_vec(),
        })
    }

    pub fn delete_group(&self, id: i64) -> anyhow::Result<()> {
        self.lock()?
            .execute("DELETE FROM asset_groups WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn mapping_overrides(&self) -> anyhow::Result<HashMap<(String, String), String>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            r#"
SELECT m.venue, m.instrument_id, g.symbol
FROM instrument_mappings AS m
JOIN asset_groups AS g ON g.id = m.asset_group_id
"#,
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    fn lock(&self) -> anyhow::Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("catalog database lock is poisoned"))
    }
}

fn load_groups(connection: &Connection) -> anyhow::Result<Vec<AssetGroup>> {
    let mut groups = connection
        .prepare("SELECT id, symbol FROM asset_groups ORDER BY symbol")?
        .query_map([], |row| {
            Ok(AssetGroup {
                id: row.get(0)?,
                symbol: row.get(1)?,
                members: Vec::new(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut statement = connection.prepare(
        "SELECT venue, instrument_id FROM instrument_mappings WHERE asset_group_id = ?1 ORDER BY venue",
    )?;
    for group in &mut groups {
        group.members = statement
            .query_map(params![group.id], |row| {
                Ok(InstrumentKey {
                    venue: row.get(0)?,
                    instrument_id: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
    }
    Ok(groups)
}

fn replace_members(
    transaction: &rusqlite::Transaction<'_>,
    group_id: i64,
    members: &[InstrumentKey],
) -> anyhow::Result<()> {
    transaction.execute(
        "DELETE FROM instrument_mappings WHERE asset_group_id = ?1",
        params![group_id],
    )?;
    let mut statement = transaction.prepare_cached(
        r#"
INSERT INTO instrument_mappings(venue, instrument_id, asset_group_id)
VALUES (?1, ?2, ?3)
ON CONFLICT(venue, instrument_id) DO UPDATE SET asset_group_id = excluded.asset_group_id
"#,
    )?;
    for member in members {
        statement.execute(params![member.venue, member.instrument_id, group_id])?;
    }
    Ok(())
}

fn normalized_group_symbol(symbol: &str) -> anyhow::Result<String> {
    let symbol = symbol.trim().to_ascii_uppercase();
    anyhow::ensure!(!symbol.is_empty(), "asset group symbol cannot be empty");
    Ok(symbol)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use crate::{catalog::CatalogInstrument, domain::ProductType};

    use super::{CatalogControl, InstrumentKey};

    #[test]
    fn persists_manual_mappings_across_reopen() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite3");
        let control = CatalogControl::open(&path).unwrap();
        control
            .sync_venue(
                "hyperliquid",
                &[CatalogInstrument {
                    venue: "hyperliquid".to_string(),
                    instrument_id: "xyz:SKHX".to_string(),
                    symbol: "xyz:SKHX".to_string(),
                    normalized_symbol: "SKHX".to_string(),
                    product_type: ProductType::Perp,
                    quote_asset: "USDC".to_string(),
                    status: "active".to_string(),
                    eligible: true,
                    eligibility_reason: None,
                    raw_json: json!({"name": "xyz:SKHX"}),
                }],
            )
            .unwrap();
        control
            .create_group(
                "skhynix",
                &[InstrumentKey {
                    venue: "hyperliquid".to_string(),
                    instrument_id: "xyz:SKHX".to_string(),
                }],
            )
            .unwrap();
        drop(control);

        let reopened = CatalogControl::open(path).unwrap();
        assert_eq!(
            reopened
                .mapping_overrides()
                .unwrap()
                .get(&("hyperliquid".to_string(), "xyz:SKHX".to_string())),
            Some(&"SKHYNIX".to_string())
        );
    }
}
