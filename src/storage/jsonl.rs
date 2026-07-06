use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
};

use crate::{
    domain::{BboTick, InstrumentCatalog},
    storage::BboSink,
};

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

    fn tick_path_for(&self, tick: &BboTick) -> PathBuf {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        self.base_dir
            .join("bbo")
            .join(day)
            .join(format!("{}.jsonl", tick.instrument.venue_instance_id))
    }

    fn catalog_path_for(&self, catalog: &InstrumentCatalog) -> PathBuf {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        self.base_dir
            .join("catalog")
            .join(day)
            .join(format!("{}.jsonl", catalog.venue_instance_id))
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }
}

#[async_trait]
impl BboSink for JsonlSink {
    async fn write_catalog(&self, catalog: &InstrumentCatalog) -> anyhow::Result<()> {
        write_json_line(self.catalog_path_for(catalog), catalog).await
    }

    async fn write_tick(&self, tick: &BboTick) -> anyhow::Result<()> {
        write_json_line(self.tick_path_for(tick), tick).await
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

async fn write_json_line<T: serde::Serialize>(path: PathBuf, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    let line = serde_json::to_string(value)?;
    file.write_all(line.as_bytes()).await?;
    file.write_all(b"\n").await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use tempfile::tempdir;

    use crate::{
        domain::{BboTick, BestLevel, Fixed, InstrumentCatalog, ProductType, SourceKind},
        storage::{BboSink, jsonl::JsonlSink},
    };

    fn catalog() -> InstrumentCatalog {
        InstrumentCatalog::new(
            "hyperliquid",
            "BTC",
            "BTC",
            Some("BTC".to_string()),
            ProductType::Perp,
            "BTC",
            "USDC",
            "USDC",
            "USDC",
            None,
            None,
            None,
            "active",
            None,
        )
    }

    #[tokio::test]
    async fn writes_jsonl_tick() {
        let dir = tempdir().unwrap();
        let sink = JsonlSink::new(dir.path());
        let catalog = catalog();
        let tick = BboTick::new(
            catalog.instrument_ref(),
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

        sink.write_catalog(&catalog).await.unwrap();
        sink.write_tick(&tick).await.unwrap();
        let entries = std::fs::read_dir(
            dir.path()
                .join("bbo")
                .join(chrono::Utc::now().format("%Y-%m-%d").to_string()),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        assert_eq!(entries.len(), 1);

        let raw = std::fs::read_to_string(entries[0].path()).unwrap();
        let persisted: crate::domain::BboTick = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(persisted.instrument.venue_instance_id, "hyperliquid");
        assert_eq!(persisted.instrument.instrument_id, "BTC");
        assert_eq!(persisted.bid.unwrap().price.to_string(), "10.1");
    }
}
