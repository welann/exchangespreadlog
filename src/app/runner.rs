use std::sync::Arc;

use anyhow::{Context, anyhow};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::{
    config::{Config, VenueConfig},
    exchange::{
        ExchangeAdapter, hyperliquid::HyperliquidAdapter, lighter::LighterAdapter,
        risex::RisexAdapter, zero_one::ZeroOneAdapter,
    },
    pipeline::fanout,
    state::new_shared_state,
    storage::{BboSink, jsonl::JsonlSink, noop::NoopSink},
    tui,
};

pub struct AppRunner {
    config: Config,
}

impl AppRunner {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let sink = self.build_sink();
        let state = new_shared_state();
        let (tx, rx) = mpsc::channel(self.config.pipeline.channel_capacity);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let writer = tokio::spawn(fanout::run_pipeline(rx, sink, state.clone()));
        let mut handles = Vec::new();
        let mut tui_handle = None;
        let (tui_done_tx, mut tui_done_rx) = watch::channel(false);

        if self.config.tui.enabled {
            let tui_state = state.clone();
            let tui_shutdown_tx = shutdown_tx.clone();
            let tui_shutdown_rx = shutdown_rx.clone();
            let refresh_ms = self.config.tui.refresh_ms;
            tui_handle = Some(tokio::task::spawn_blocking(move || {
                let result = tui::run(tui_state, tui_shutdown_tx, tui_shutdown_rx, refresh_ms);
                let _ = tui_done_tx.send(true);
                result
            }));
        }

        for venue in self.config.venues.iter().filter(|venue| venue.enabled) {
            let adapter = build_adapter(venue)?;
            let adapter_tx = tx.clone();
            let adapter_shutdown = shutdown_rx.clone();
            handles.push(tokio::spawn(async move {
                adapter.run(adapter_tx, adapter_shutdown).await
            }));
        }

        if handles.is_empty() {
            warn!("no venues enabled; waiting for Ctrl-C");
        }

        info!("collector started; press Ctrl-C to stop");
        tokio::select! {
            ctrl = tokio::signal::ctrl_c() => {
                ctrl.context("listen for Ctrl-C")?;
            }
            _ = tui_done_rx.changed(), if tui_handle.is_some() => {
                info!("TUI requested shutdown");
            }
        }
        info!("shutdown requested");

        let _ = shutdown_tx.send(true);
        drop(tx);

        for handle in handles {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => warn!(error = %err, "adapter exited with error"),
                Err(err) => warn!(error = %err, "adapter task join error"),
            }
        }

        if let Some(handle) = tui_handle {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => warn!(error = %err, "TUI exited with error"),
                Err(err) => warn!(error = %err, "TUI task join error"),
            }
        }

        writer.await??;
        info!("collector stopped");
        Ok(())
    }

    fn build_sink(&self) -> Arc<dyn BboSink> {
        if self.config.storage.jsonl_enabled {
            Arc::new(JsonlSink::new(&self.config.storage.jsonl_dir))
        } else {
            Arc::new(NoopSink)
        }
    }
}

fn build_adapter(config: &VenueConfig) -> anyhow::Result<Box<dyn ExchangeAdapter>> {
    match config.name.as_str() {
        "hyperliquid" => Ok(Box::new(HyperliquidAdapter::from_config(config))),
        "lighter" => Ok(Box::new(LighterAdapter::from_config(config))),
        "rise" | "risex" => Ok(Box::new(RisexAdapter::from_config(config))),
        "01" | "zero_one" | "zeroone" => Ok(Box::new(ZeroOneAdapter::from_config(config))),
        other => Err(anyhow!("unsupported venue: {other}")),
    }
}
