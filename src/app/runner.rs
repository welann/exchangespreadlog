use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, anyhow};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time,
};
use tracing::{info, warn};

use crate::{
    app::subscriptions::{
        SubscriptionPlan, run_subscription_refresh, validate_subscription_refresh,
    },
    config::{Config, StorageMode, VenueConfig},
    domain::MarketEvent,
    exchange::{
        ExchangeAdapter, ethereal::EtherealAdapter, hyperliquid::HyperliquidAdapter,
        lighter::LighterAdapter, ondo::adapter::OndoAdapter, perpl::adapter::PerplAdapter,
        risex::RisexAdapter, zero_one::ZeroOneAdapter,
    },
    pipeline::fanout,
    state::new_shared_state,
    storage::{
        BboSink, clickhouse::ClickHouseSink, jsonl::JsonlSink, multi::MultiSink, noop::NoopSink,
    },
    tui,
};

pub struct AppRunner {
    config: Config,
    config_path: Option<PathBuf>,
}

impl AppRunner {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            config_path: None,
        }
    }

    pub fn with_config_path(mut self, config_path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(config_path.into());
        self
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let refresh_config_path = if self.config.subscription_refresh.enabled {
            validate_subscription_refresh(&self.config.subscription_refresh)?;
            Some(
                self.config_path
                    .clone()
                    .context("subscription refresh requires AppRunner::with_config_path")?,
            )
        } else {
            None
        };

        let sink = self.build_sink().await?;
        let state = new_shared_state(self.config.quote_rate_book());
        let (tx, rx) = mpsc::channel::<MarketEvent>(self.config.pipeline.channel_capacity);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let stale_after_ms = self.config.pipeline.stale_after_ms;

        let writer = tokio::spawn(fanout::run_pipeline(
            rx,
            sink,
            state.clone(),
            stale_after_ms,
        ));
        let mut adapter_manager = AdapterManager::start(&self.config.venues, tx.clone())?;
        let mut tui_handle = None;
        let (tui_done_tx, mut tui_done_rx) = watch::channel(false);
        let (refresh_tx, mut refresh_rx) = mpsc::channel(1);

        let refresh_handle = if self.config.subscription_refresh.enabled {
            let config_path = refresh_config_path.expect("validated refresh config path");
            let settings = self.config.subscription_refresh.clone();
            let refresh_shutdown = shutdown_rx.clone();
            Some(tokio::spawn(async move {
                run_subscription_refresh(settings, config_path, refresh_tx, refresh_shutdown).await
            }))
        } else {
            None
        };

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

        if adapter_manager.is_empty() {
            warn!("no venues enabled; waiting for Ctrl-C");
        }

        info!("collector started; press Ctrl-C to stop");
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::pin!(ctrl_c);
        let mut refresh_active = refresh_handle.is_some();
        loop {
            tokio::select! {
                ctrl = &mut ctrl_c => {
                    ctrl.context("listen for Ctrl-C")?;
                    break;
                }
                _ = tui_done_rx.changed(), if tui_handle.is_some() => {
                    info!("TUI requested shutdown");
                    break;
                }
                refreshed = refresh_rx.recv(), if refresh_active => {
                    match refreshed {
                        Some(config) => {
                            if let Err(error) = adapter_manager.apply(&config.venues).await {
                                warn!(%error, "subscription refresh rejected; keeping current plan");
                            }
                        }
                        None => {
                            refresh_active = false;
                            warn!("subscription refresh task stopped; current subscriptions remain active");
                        }
                    }
                }
            }
        }
        info!("shutdown requested");

        let _ = shutdown_tx.send(true);
        adapter_manager.stop_all().await;
        drop(adapter_manager);
        drop(tx);

        if let Some(handle) = refresh_handle {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => warn!(error = %err, "subscription refresh task exited with error"),
                Err(err) => warn!(error = %err, "subscription refresh task join error"),
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

    async fn build_sink(&self) -> anyhow::Result<Arc<dyn BboSink>> {
        let mut sinks: Vec<Arc<dyn BboSink>> = Vec::new();

        match self.config.storage.effective_mode() {
            StorageMode::None => {}
            StorageMode::Jsonl => {
                sinks.push(Arc::new(JsonlSink::new(&self.config.storage.jsonl_dir)));
            }
            StorageMode::Clickhouse => {
                sinks.push(Arc::new(
                    ClickHouseSink::new(&self.config.storage.clickhouse).await?,
                ));
            }
            StorageMode::Both => {
                sinks.push(Arc::new(JsonlSink::new(&self.config.storage.jsonl_dir)));
                sinks.push(Arc::new(
                    ClickHouseSink::new(&self.config.storage.clickhouse).await?,
                ));
            }
        }

        let sink: Arc<dyn BboSink> = match sinks.len() {
            0 => Arc::new(NoopSink),
            1 => sinks.pop().expect("sink exists"),
            _ => Arc::new(MultiSink::new(sinks)),
        };

        Ok(sink)
    }
}

struct AdapterManager {
    tx: mpsc::Sender<MarketEvent>,
    plan: SubscriptionPlan,
    tasks: HashMap<String, AdapterTask>,
}

struct AdapterTask {
    shutdown: watch::Sender<bool>,
    join: JoinHandle<anyhow::Result<()>>,
}

impl AdapterManager {
    fn start(venues: &[VenueConfig], tx: mpsc::Sender<MarketEvent>) -> anyhow::Result<Self> {
        let plan = SubscriptionPlan::from_venues(venues)?;
        let prepared = prepare_adapters(plan.venues())?;
        let mut manager = Self {
            tx,
            plan,
            tasks: HashMap::new(),
        };
        for (venue_id, adapter) in prepared {
            manager.spawn(venue_id, adapter);
        }
        Ok(manager)
    }

    fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    async fn apply(&mut self, venues: &[VenueConfig]) -> anyhow::Result<()> {
        let next = SubscriptionPlan::from_venues(venues)?;
        self.plan.validate_replacement(&next)?;
        let diff = self.plan.diff(&next);
        if diff.removed.is_empty() && diff.upserted.is_empty() {
            info!("subscription refresh found no changes");
            return Ok(());
        }

        // Build every replacement first so one invalid venue cannot partially update the plan.
        let prepared = prepare_adapters(diff.upserted.iter())?;
        let mut reset_ids = diff.removed.clone();
        reset_ids.extend(
            diff.upserted
                .iter()
                .map(|venue| venue.venue_instance_id.clone()),
        );
        reset_ids.sort();
        reset_ids.dedup();

        for venue_id in &reset_ids {
            self.stop(venue_id).await;
        }
        for venue_id in &reset_ids {
            self.tx
                .send(MarketEvent::VenueReset {
                    venue_instance_id: venue_id.clone(),
                })
                .await
                .context("reset venue state before applying subscription refresh")?;
        }
        for (venue_id, adapter) in prepared {
            self.spawn(venue_id, adapter);
        }

        info!(
            removed = ?diff.removed,
            restarted = ?diff.upserted.iter().map(|venue| &venue.venue_instance_id).collect::<Vec<_>>(),
            "subscription plan updated"
        );
        self.plan = next;
        Ok(())
    }

    fn spawn(&mut self, venue_id: String, adapter: Box<dyn ExchangeAdapter>) {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let adapter_tx = self.tx.clone();
        let join = tokio::spawn(async move { adapter.run(adapter_tx, shutdown_rx).await });
        self.tasks.insert(
            venue_id,
            AdapterTask {
                shutdown: shutdown_tx,
                join,
            },
        );
    }

    async fn stop(&mut self, venue_id: &str) {
        let Some(task) = self.tasks.remove(venue_id) else {
            return;
        };
        let _ = task.shutdown.send(true);
        let mut join = task.join;
        match time::timeout(Duration::from_secs(10), &mut join).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => warn!(venue = venue_id, %error, "adapter exited while stopping"),
            Ok(Err(error)) => warn!(venue = venue_id, %error, "adapter task join error"),
            Err(_) => {
                warn!(
                    venue = venue_id,
                    "adapter did not stop within 10s; aborting task"
                );
                join.abort();
                let _ = join.await;
            }
        }
    }

    async fn stop_all(&mut self) {
        let venue_ids = self.tasks.keys().cloned().collect::<Vec<_>>();
        for venue_id in venue_ids {
            self.stop(&venue_id).await;
        }
    }
}

fn prepare_adapters<'a>(
    venues: impl Iterator<Item = &'a VenueConfig>,
) -> anyhow::Result<Vec<(String, Box<dyn ExchangeAdapter>)>> {
    venues
        .map(|venue| Ok((venue.venue_instance_id.clone(), build_adapter(venue)?)))
        .collect()
}

fn build_adapter(config: &VenueConfig) -> anyhow::Result<Box<dyn ExchangeAdapter>> {
    match config.adapter.as_str() {
        "hyperliquid" => Ok(Box::new(HyperliquidAdapter::from_config(config))),
        "lighter" => Ok(Box::new(LighterAdapter::from_config(config))),
        "rise" | "risex" => Ok(Box::new(RisexAdapter::from_config(config))),
        "01" | "zero_one" | "zeroone" => Ok(Box::new(ZeroOneAdapter::from_config(config))),
        "ethereal" => Ok(Box::new(EtherealAdapter::from_config(config))),
        "perpl" => Ok(Box::new(PerplAdapter::from_config(config))),
        "ondo" => Ok(Box::new(OndoAdapter::from_config(config))),
        other => Err(anyhow!("unsupported venue: {other}")),
    }
}
