use std::{convert::Infallible, path::PathBuf, sync::atomic::Ordering, time::Duration};

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::watch;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

use crate::{
    clickhouse::{ClickHouse, HistoryResponse},
    store::{LiveBookStore, LiveNotice},
    wal::DurableEventLog,
};

#[derive(Clone)]
pub struct ApiState {
    pub store: LiveBookStore,
    pub clickhouse: ClickHouse,
    pub wal: DurableEventLog,
    pub shutdown: watch::Receiver<bool>,
}

#[derive(Debug, Deserialize)]
struct PairQuery {
    leg_a: String,
    leg_b: String,
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    leg_a: String,
    leg_b: String,
    range_ms: i64,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

pub fn router(state: ApiState, web_dir: PathBuf) -> Router {
    let index = web_dir.join("index.html");
    let static_files = ServeDir::new(web_dir).not_found_service(ServeFile::new(index));
    Router::new()
        .route("/v1/markets", get(markets))
        .route("/v1/spreads", get(history))
        .route("/v1/live/spread", get(live_spread))
        .route("/v1/health", get(health))
        .route("/metrics", get(metrics))
        .fallback_service(static_files)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn markets(State(state): State<ApiState>) -> Json<serde_json::Value> {
    Json(json!({
        "serverTimeMs": chrono::Utc::now().timestamp_millis(),
        "markets": state.store.markets().await
    }))
}

async fn history(
    State(state): State<ApiState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryResponse>, ApiError> {
    let (leg_a, leg_b, conversion) = state
        .store
        .catalog_pair(&query.leg_a, &query.leg_b)
        .await
        .map_err(|error| bad_request("INVALID_PAIR", error))?;
    let (from_ms, to_ms) = history_bounds(chrono::Utc::now().timestamp_millis(), query.range_ms)
        .map_err(|error| bad_request("INVALID_RANGE", error))?;
    state
        .clickhouse
        .history_spread(
            &query.leg_a,
            &query.leg_b,
            from_ms,
            to_ms,
            &leg_a.venue_instance_id,
            &leg_b.venue_instance_id,
            conversion,
        )
        .await
        .map(Json)
        .map_err(|error| ApiError {
            status: StatusCode::BAD_GATEWAY,
            code: "HISTORY_QUERY_FAILED",
            message: error.to_string(),
        })
}

fn history_bounds(server_now_ms: i64, range_ms: i64) -> anyhow::Result<(i64, i64)> {
    const MAX_HISTORY_RANGE_MS: i64 = 31 * 24 * 60 * 60 * 1_000;
    if !(1..=MAX_HISTORY_RANGE_MS).contains(&range_ms) {
        anyhow::bail!("rangeMs must be between 1ms and 31 days");
    }
    let from_ms = server_now_ms
        .checked_sub(range_ms)
        .context("history range underflow")?;
    Ok((from_ms, server_now_ms))
}

async fn live_spread(
    State(state): State<ApiState>,
    Query(query): Query<PairQuery>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let initial = state
        .store
        .spread(&query.leg_a, &query.leg_b)
        .await
        .map_err(|error| bad_request("INVALID_PAIR", error))?;
    let initial_json = serde_json::to_string(&initial).map_err(internal)?;
    let mut receiver = state.store.subscribe();
    let mut shutdown = state.shutdown.clone();
    let store = state.store.clone();
    let leg_a = query.leg_a;
    let leg_b = query.leg_b;

    let stream = async_stream::stream! {
        let mut freshness = tokio::time::interval(Duration::from_secs(1));
        freshness.tick().await;
        yield Ok(Event::default().event("snapshot").data(initial_json));
        loop {
            tokio::select! {
                notice = receiver.recv() => match notice {
                    Ok(notice) if relevant(&notice, &leg_a, &leg_b) => {
                        match store.spread(&leg_a, &leg_b).await {
                            Ok(spread) => {
                                if let Ok(data) = serde_json::to_string(&spread) {
                                    yield Ok(Event::default().event("spread").data(data));
                                }
                            }
                            Err(error) => {
                                let data = json!({
                                    "state": "unavailable",
                                    "reason": error.to_string(),
                                    "point": null
                                }).to_string();
                                yield Ok(Event::default().event("spread").data(data));
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        let data = json!({"skipped": skipped}).to_string();
                        yield Ok(Event::default().event("resync").data(data));
                        if let Ok(spread) = store.spread(&leg_a, &leg_b).await
                            && let Ok(data) = serde_json::to_string(&spread)
                        {
                            yield Ok(Event::default().event("spread").data(data));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                _ = freshness.tick() => {
                    match store.spread(&leg_a, &leg_b).await {
                        Ok(spread) => {
                            if let Ok(data) = serde_json::to_string(&spread) {
                                yield Ok(Event::default().event("spread").data(data));
                            }
                        }
                        Err(error) => {
                            let data = json!({
                                "state": "unavailable",
                                "reason": error.to_string(),
                                "point": null
                            }).to_string();
                            yield Ok(Event::default().event("spread").data(data));
                        }
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

async fn health(State(state): State<ApiState>) -> Response {
    let clickhouse = state.clickhouse.ping().await;
    let wal_pending = state.wal.pending_count();
    let clickhouse_version = clickhouse.as_ref().ok().cloned();
    let clickhouse_error = clickhouse.as_ref().err().map(ToString::to_string);
    let wal_pending_value = wal_pending.as_ref().ok().copied();
    let wal_error = wal_pending.as_ref().err().map(ToString::to_string);
    let stats = state.store.stats();
    let healthy = clickhouse.is_ok() && wal_pending.is_ok();
    (
        if healthy {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({
            "status": if healthy { "ok" } else { "degraded" },
            "clickhouseVersion": clickhouse_version,
            "clickhouseError": clickhouse_error,
            "walPending": wal_pending_value,
            "walError": wal_error,
            "receivedTicks": stats.received_ticks.load(Ordering::Relaxed),
            "acceptedTicks": stats.accepted_ticks.load(Ordering::Relaxed),
            "rejectedTicks": stats.rejected_ticks.load(Ordering::Relaxed),
            "venueResets": stats.venue_resets.load(Ordering::Relaxed)
        })),
    )
        .into_response()
}

async fn metrics(State(state): State<ApiState>) -> Response {
    let stats = state.store.stats();
    let wal_pending = state.wal.pending_count().unwrap_or_default();
    let body = format!(
        concat!(
            "# TYPE spread_received_ticks_total counter\n",
            "spread_received_ticks_total {}\n",
            "# TYPE spread_accepted_ticks_total counter\n",
            "spread_accepted_ticks_total {}\n",
            "# TYPE spread_rejected_ticks_total counter\n",
            "spread_rejected_ticks_total {}\n",
            "# TYPE spread_venue_resets_total counter\n",
            "spread_venue_resets_total {}\n",
            "# TYPE spread_wal_pending gauge\n",
            "spread_wal_pending {}\n"
        ),
        stats.received_ticks.load(Ordering::Relaxed),
        stats.accepted_ticks.load(Ordering::Relaxed),
        stats.rejected_ticks.load(Ordering::Relaxed),
        stats.venue_resets.load(Ordering::Relaxed),
        wal_pending
    );
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        body,
    )
        .into_response()
}

fn relevant(notice: &LiveNotice, leg_a: &str, leg_b: &str) -> bool {
    match notice {
        LiveNotice::Catalog { instrument_key } | LiveNotice::Tick { instrument_key } => {
            instrument_key == leg_a || instrument_key == leg_b
        }
        LiveNotice::VenueReset { .. } => true,
    }
}

fn bad_request(code: &'static str, error: anyhow::Error) -> ApiError {
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code,
        message: error.to_string(),
    }
}

fn internal(error: serde_json::Error) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "SERIALIZATION_ERROR",
        message: error.to_string(),
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": {
                    "code": self.code,
                    "message": self.message
                }
            })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::history_bounds;

    #[test]
    fn anchors_history_range_to_server_time() {
        assert_eq!(history_bounds(10_000, 1_000).unwrap(), (9_000, 10_000));
    }

    #[test]
    fn rejects_invalid_history_ranges() {
        assert!(history_bounds(10_000, 0).is_err());
        assert!(history_bounds(10_000, 31 * 24 * 60 * 60 * 1_000 + 1).is_err());
    }
}
