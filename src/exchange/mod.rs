pub mod hyperliquid;
pub mod lighter;
pub mod risex;
pub mod zero_one;

use async_trait::async_trait;
use tokio::sync::{mpsc::Sender, watch};

use crate::domain::BboTick;

#[async_trait]
pub trait ExchangeAdapter: Send + Sync {
    async fn run(&self, tx: Sender<BboTick>, shutdown: watch::Receiver<bool>)
    -> anyhow::Result<()>;
}
