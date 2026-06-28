use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
};

use crate::{domain::BboTick, storage::BboSink};

#[derive(Debug, Clone)]
pub struct JsonlSink {
    base_dir: PathBuf,
}

impl JsonlSink {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    fn path_for(&self, tick: &BboTick) -> PathBuf {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        self.base_dir
            .join(day)
            .join(format!("{}.jsonl", tick.venue.as_str()))
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }
}

#[async_trait]
impl BboSink for JsonlSink {
    async fn write(&self, tick: &BboTick) -> anyhow::Result<()> {
        let path = self.path_for(tick);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        let line = serde_json::to_string(tick)?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use tempfile::tempdir;

    use crate::{
        domain::{BboTick, BestLevel, Fixed, MarketRef, SourceKind, Venue},
        storage::{BboSink, jsonl::JsonlSink},
    };

    #[tokio::test]
    async fn writes_jsonl_tick() {
        let dir = tempdir().unwrap();
        let sink = JsonlSink::new(dir.path());
        let tick = BboTick::new(
            Venue::Hyperliquid,
            MarketRef::new("BTC", Some("BTC".to_string())),
            123,
            Some(1000),
            None,
            Some(BestLevel::new(
                Fixed::from_str("10.1").unwrap(),
                Fixed::from_str("2").unwrap(),
                Some(1),
            )),
            None,
            SourceKind::Bbo,
        );

        sink.write(&tick).await.unwrap();
        let entries = std::fs::read_dir(
            dir.path()
                .join(chrono::Utc::now().format("%Y-%m-%d").to_string()),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        assert_eq!(entries.len(), 1);

        let raw = std::fs::read_to_string(entries[0].path()).unwrap();
        let persisted: crate::domain::BboTick = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(persisted.venue, Venue::Hyperliquid);
        assert_eq!(persisted.market.label(), "BTC");
        assert_eq!(persisted.bid.unwrap().price.to_string(), "10.1");
    }
}
