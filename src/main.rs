use std::{collections::HashMap, future::IntoFuture, sync::Arc, time::Duration};

use anyhow::{Context, anyhow, bail};
use clap::Parser;
use exchange_spread_v2::{
    api::{ApiState, router},
    catalog::{discover_venue_configs_with_fallback, load_catalog_cache, save_catalog_cache},
    clickhouse::ClickHouse,
    config::{RuntimeConfig, VenueConfig},
    domain::MarketEvent,
    exchange::{
        ExchangeAdapter, hyperliquid::HyperliquidAdapter, lighter::LighterAdapter,
        ondo::adapter::OndoAdapter, perpl::adapter::PerplAdapter, risex::RisexAdapter,
        zero_one::ZeroOneAdapter,
    },
    pipeline,
    store::LiveBookStore,
    wal::DurableEventLog,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time,
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Correct BBO collection and low-latency spread monitoring"
)]
struct Args {
    #[arg(long)]
    migrate_only: bool,
    #[arg(long)]
    catalog_only: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install rustls crypto provider");

    let args = Args::parse();
    let config = RuntimeConfig::from_env()?;
    let clickhouse = ClickHouse::new(config.clickhouse.clone())?;
    if args.migrate_only {
        let version = clickhouse.ping().await.context("connect to ClickHouse")?;
        info!(%version, url = %config.clickhouse.url, "connected to ClickHouse");
        clickhouse.ensure_schema().await?;
        return Ok(());
    }

    let http_client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let cached_venues = match load_catalog_cache(&config.catalog_cache_path).await {
        Ok(venues) => {
            info!(
                path = %config.catalog_cache_path.display(),
                venues = venues.len(),
                "loaded last-known-good catalog"
            );
            venues
        }
        Err(error) => {
            warn!(
                path = %config.catalog_cache_path.display(),
                %error,
                "catalog cache unavailable; live discovery must provide a usable catalog"
            );
            Vec::new()
        }
    };
    let venues =
        discover_venue_configs_with_fallback(&http_client, &cached_venues, &config.disabled_venues)
            .await?;
    if let Err(error) = save_catalog_cache(&config.catalog_cache_path, &venues).await {
        warn!(%error, "failed to persist last-known-good catalog");
    }
    if args.catalog_only {
        println!("{}", serde_json::to_string_pretty(&venues)?);
        return Ok(());
    }

    let wal = DurableEventLog::open(&config.wal_path)?;
    let store = LiveBookStore::new(config.query_max_book_age_ms, config.quote_rates.clone());
    let (event_tx, event_rx) = mpsc::channel::<MarketEvent>(16_384);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let mut pipeline_handle = tokio::spawn(pipeline::run(
        event_rx,
        wal.clone(),
        store.clone(),
        config.stale_after_ms,
    ));
    let projector_handle = {
        let clickhouse = clickhouse.clone();
        let wal = wal.clone();
        let shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            let result = clickhouse.run_projector(wal, shutdown).await;
            if let Err(error) = &result {
                warn!(error = %format!("{error:#}"), "ClickHouse projector stopped");
            }
            result
        })
    };
    let mut adapter_manager = AdapterManager::start(&venues, event_tx.clone())?;

    let app = router(
        ApiState {
            store,
            clickhouse,
            wal,
            max_book_age_ms: config.query_max_book_age_ms,
            shutdown: shutdown_rx.clone(),
        },
        config.web_dir.clone(),
    );
    let listener = tokio::net::TcpListener::bind(config.http_addr)
        .await
        .with_context(|| format!("bind HTTP server {}", config.http_addr))?;
    info!(
        address = %config.http_addr,
        web_dir = %config.web_dir.display(),
        "v2 server started"
    );

    let mut server_shutdown = shutdown_rx.clone();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            while !*server_shutdown.borrow() {
                if server_shutdown.changed().await.is_err() {
                    break;
                }
            }
        })
        .into_future();
    tokio::pin!(server);

    let mut shutdown_signal = Box::pin(shutdown_signal());
    let refresh_start = time::Instant::now() + config.catalog_refresh_interval;
    let mut catalog_refresh = time::interval_at(refresh_start, config.catalog_refresh_interval);
    catalog_refresh.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut adapter_health = time::interval(Duration::from_secs(1));
    adapter_health.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut pipeline_finished = false;
    let mut server_finished = false;

    let run_result: anyhow::Result<()> = loop {
        tokio::select! {
            _ = &mut shutdown_signal => {
                info!("shutdown requested");
                break Ok(());
            }
            result = &mut server => {
                server_finished = true;
                break result.context("run HTTP server");
            }
            result = &mut pipeline_handle => {
                pipeline_finished = true;
                break task_result("pipeline", result);
            }
            _ = catalog_refresh.tick() => {
                let current = adapter_manager.configs();
                match discover_venue_configs_with_fallback(
                    &http_client,
                    &current,
                    &config.disabled_venues,
                ).await {
                    Ok(next) => {
                        if let Err(error) = adapter_manager.apply(&next).await {
                            break Err(error.context("apply refreshed catalog"));
                        }
                        if let Err(error) = save_catalog_cache(&config.catalog_cache_path, &next).await {
                            warn!(%error, "failed to persist refreshed catalog");
                        }
                    }
                    Err(error) => {
                        warn!(%error, "catalog refresh rejected; current subscriptions remain active");
                    }
                }
            }
            _ = adapter_health.tick() => {
                if let Err(error) = adapter_manager.restart_finished().await {
                    break Err(error.context("restart failed adapter"));
                }
            }
        }
    };

    let _ = shutdown_tx.send(true);
    adapter_manager.stop_all().await;
    drop(adapter_manager);
    drop(event_tx);
    let mut final_result = run_result;
    if !server_finished && let Err(error) = server.await {
        let error = anyhow!(error).context("gracefully stop HTTP server");
        if final_result.is_ok() {
            final_result = Err(error);
        } else {
            warn!(error = %format!("{error:#}"), "HTTP server shutdown failed");
        }
    }
    if !pipeline_finished {
        report_task("pipeline", pipeline_handle).await;
    }
    report_task("ClickHouse projector", projector_handle).await;
    final_result
}

struct AdapterTask {
    config: VenueConfig,
    shutdown: watch::Sender<bool>,
    join: JoinHandle<anyhow::Result<()>>,
}

struct AdapterManager {
    sender: mpsc::Sender<MarketEvent>,
    tasks: HashMap<String, AdapterTask>,
}

impl AdapterManager {
    fn start(venues: &[VenueConfig], sender: mpsc::Sender<MarketEvent>) -> anyhow::Result<Self> {
        let mut manager = Self {
            sender,
            tasks: HashMap::new(),
        };
        for venue in venues {
            manager.spawn(venue.clone())?;
        }
        Ok(manager)
    }

    fn configs(&self) -> Vec<VenueConfig> {
        self.tasks
            .values()
            .map(|task| task.config.clone())
            .collect()
    }

    async fn apply(&mut self, venues: &[VenueConfig]) -> anyhow::Result<()> {
        let next = venues
            .iter()
            .cloned()
            .map(|venue| (venue.venue_instance_id.clone(), venue))
            .collect::<HashMap<_, _>>();
        let changed_or_removed = self
            .tasks
            .iter()
            .filter_map(|(venue, task)| {
                (next.get(venue) != Some(&task.config)).then_some(venue.clone())
            })
            .collect::<Vec<_>>();
        for venue in changed_or_removed {
            self.stop(&venue, true).await;
        }
        for (venue_id, config) in next {
            if !self.tasks.contains_key(&venue_id) {
                self.spawn(config)?;
            }
        }
        Ok(())
    }

    async fn restart_finished(&mut self) -> anyhow::Result<()> {
        let finished = self
            .tasks
            .iter()
            .filter_map(|(venue, task)| task.join.is_finished().then_some(venue.clone()))
            .collect::<Vec<_>>();
        for venue in finished {
            let task = self.tasks.remove(&venue).expect("finished task exists");
            match task.join.await {
                Ok(Ok(())) => warn!(venue, "adapter stopped unexpectedly; restarting"),
                Ok(Err(error)) => {
                    warn!(venue, error = %format!("{error:#}"), "adapter task escaped its reconnect supervisor; restarting")
                }
                Err(error) => warn!(venue, %error, "adapter task panicked; restarting"),
            }
            self.sender
                .send(MarketEvent::VenueReset {
                    venue_instance_id: venue,
                })
                .await
                .context("reset unexpectedly stopped adapter")?;
            self.spawn(task.config)?;
        }
        Ok(())
    }

    fn spawn(&mut self, config: VenueConfig) -> anyhow::Result<()> {
        let adapter = build_adapter(&config)?;
        let venue = config.venue_instance_id.clone();
        let sender = self.sender.clone();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let join = tokio::spawn(async move { adapter.run(sender, shutdown_rx).await });
        self.tasks.insert(
            venue,
            AdapterTask {
                config,
                shutdown: shutdown_tx,
                join,
            },
        );
        Ok(())
    }

    async fn stop(&mut self, venue: &str, reset: bool) {
        let Some(task) = self.tasks.remove(venue) else {
            return;
        };
        let _ = task.shutdown.send(true);
        report_task("adapter", task.join).await;
        if reset {
            let _ = self
                .sender
                .send(MarketEvent::VenueReset {
                    venue_instance_id: venue.to_string(),
                })
                .await;
        }
    }

    async fn stop_all(&mut self) {
        let venues = self.tasks.keys().cloned().collect::<Vec<_>>();
        for venue in venues {
            self.stop(&venue, false).await;
        }
    }
}

fn build_adapter(config: &VenueConfig) -> anyhow::Result<Arc<dyn ExchangeAdapter>> {
    match config.adapter.as_str() {
        "hyperliquid" => Ok(Arc::new(HyperliquidAdapter::from_config(config))),
        "lighter" => Ok(Arc::new(LighterAdapter::from_config(config))),
        "rise" | "risex" => Ok(Arc::new(RisexAdapter::from_config(config))),
        "01" | "zero_one" => Ok(Arc::new(ZeroOneAdapter::from_config(config))),
        // Ethereal is intentionally disabled; its source is preserved but not compiled.
        // "ethereal" => Ok(Arc::new(EtherealAdapter::from_config(config))),
        "perpl" => Ok(Arc::new(PerplAdapter::from_config(config))),
        "ondo" => Ok(Arc::new(OndoAdapter::from_config(config))),
        other => Err(anyhow!("unsupported venue adapter `{other}`")),
    }
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(%error, "failed to install Ctrl-C handler");
    }
}

fn task_result(
    name: &str,
    result: Result<anyhow::Result<()>, tokio::task::JoinError>,
) -> anyhow::Result<()> {
    match result {
        Ok(Ok(())) => bail!("{name} stopped unexpectedly"),
        Ok(Err(error)) => Err(error).with_context(|| format!("{name} stopped")),
        Err(error) => Err(error).with_context(|| format!("{name} task join failed")),
    }
}

async fn report_task(name: &str, handle: JoinHandle<anyhow::Result<()>>) {
    match handle.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            warn!(task = name, error = %format!("{error:#}"), "task stopped with error")
        }
        Err(error) => warn!(task = name, %error, "task join failed"),
    }
}
