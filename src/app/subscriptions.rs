use std::time::Duration;
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveTime, TimeZone, Utc};
use tokio::{
    process::Command,
    sync::{mpsc, watch},
    time,
};
use tracing::{info, warn};

use crate::config::{Config, SubscriptionRefreshConfig, VenueConfig};

fn next_refresh_delay(now: DateTime<Utc>, daily_at_utc: &str) -> Result<Duration> {
    let time = NaiveTime::parse_from_str(daily_at_utc, "%H:%M")?;
    let today = Utc.from_utc_datetime(&now.date_naive().and_time(time));
    let target = if today > now {
        today
    } else {
        today + chrono::Duration::days(1)
    };
    Ok((target - now).to_std()?)
}

pub fn validate_subscription_refresh(settings: &SubscriptionRefreshConfig) -> Result<()> {
    next_refresh_delay(Utc::now(), &settings.daily_at_utc).with_context(|| {
        format!(
            "invalid subscription_refresh.daily_at_utc `{}`",
            settings.daily_at_utc
        )
    })?;
    if settings.market_limit == 0 {
        bail!("subscription_refresh.market_limit must be positive");
    }
    if settings.generator_script.trim().is_empty() {
        bail!("subscription_refresh.generator_script must not be empty");
    }
    Ok(())
}

pub async fn run_subscription_refresh(
    settings: SubscriptionRefreshConfig,
    config_path: PathBuf,
    config_tx: mpsc::Sender<Config>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    validate_subscription_refresh(&settings)?;

    while !*shutdown.borrow() {
        let delay = next_refresh_delay(Utc::now(), &settings.daily_at_utc)?;
        info!(
            refresh_at_utc = %settings.daily_at_utc,
            next_refresh_in = ?delay,
            "subscription refresh scheduled"
        );

        tokio::select! {
            _ = time::sleep(delay) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
                continue;
            }
        }

        let generation = tokio::select! {
            result = generate_config(&settings, &config_path) => result,
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
                continue;
            }
        };
        match generation {
            Ok(()) => {}
            Err(error) => {
                warn!(%error, "subscription config generation failed; keeping current plan");
                continue;
            }
        }

        let config = match Config::load(&config_path) {
            Ok(config) => config,
            Err(error) => {
                warn!(%error, path = %config_path.display(), "generated subscription config is invalid; keeping current plan");
                continue;
            }
        };

        tokio::select! {
            result = config_tx.send(config) => {
                if result.is_err() {
                    return Ok(());
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }

    Ok(())
}

async fn generate_config(
    settings: &SubscriptionRefreshConfig,
    config_path: &PathBuf,
) -> Result<()> {
    let output = Command::new("uv")
        .arg("run")
        .arg("python")
        .arg(&settings.generator_script)
        .arg("--limit")
        .arg(settings.market_limit.to_string())
        .arg("--refresh-at-utc")
        .arg(&settings.daily_at_utc)
        .arg("--generator-script")
        .arg(&settings.generator_script)
        .arg("--output")
        .arg(config_path)
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("run subscription generator `{}`", settings.generator_script))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "subscription generator exited with {}: {}",
            output.status,
            stderr.trim()
        );
    }

    info!(path = %config_path.display(), "subscription config regenerated");
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SubscriptionPlan {
    venues: BTreeMap<String, VenueConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionDiff {
    pub removed: Vec<String>,
    pub upserted: Vec<VenueConfig>,
}

impl SubscriptionPlan {
    pub fn from_venues(venues: &[VenueConfig]) -> Result<Self> {
        let mut enabled = BTreeMap::new();
        for venue in venues.iter().filter(|venue| venue.enabled) {
            let mut venue = venue.clone();
            let mut instruments = venue
                .instruments
                .drain(..)
                .map(|instrument| Ok((serde_json::to_string(&instrument)?, instrument)))
                .collect::<Result<Vec<_>>>()?;
            instruments.sort_by(|lhs, rhs| lhs.0.cmp(&rhs.0));
            venue.instruments = instruments
                .into_iter()
                .map(|(_, instrument)| instrument)
                .collect();

            if enabled
                .insert(venue.venue_instance_id.clone(), venue.clone())
                .is_some()
            {
                bail!(
                    "duplicate venue_instance_id `{}` in enabled venues",
                    venue.venue_instance_id
                );
            }
        }
        Ok(Self { venues: enabled })
    }

    pub fn diff(&self, next: &Self) -> SubscriptionDiff {
        let removed = self
            .venues
            .keys()
            .filter(|venue_id| !next.venues.contains_key(*venue_id))
            .cloned()
            .collect();
        let upserted = next
            .venues
            .iter()
            .filter(|(venue_id, venue)| self.venues.get(*venue_id) != Some(*venue))
            .map(|(_, venue)| venue.clone())
            .collect();

        SubscriptionDiff { removed, upserted }
    }

    pub fn validate_replacement(&self, next: &Self) -> Result<()> {
        if !self.venues.is_empty() && next.venues.is_empty() {
            bail!(
                "refusing to replace a non-empty subscription plan with an empty subscription plan"
            );
        }
        Ok(())
    }

    pub fn venues(&self) -> impl Iterator<Item = &VenueConfig> {
        self.venues.values()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use crate::config::Config;

    use super::SubscriptionPlan;

    #[test]
    fn diff_reports_only_removed_and_changed_venues() {
        let config = Config::default();
        let current = SubscriptionPlan::from_venues(&config.venues[..2]).unwrap();

        let mut next_venues = config.venues[..2].to_vec();
        next_venues[0].enabled = false;
        next_venues[1].instruments.pop();
        let next = SubscriptionPlan::from_venues(&next_venues).unwrap();

        let diff = current.diff(&next);

        assert_eq!(diff.removed, vec!["hyperliquid"]);
        assert_eq!(diff.upserted.len(), 1);
        assert_eq!(diff.upserted[0].venue_instance_id, "lighter");
    }

    #[test]
    fn instrument_order_does_not_change_the_subscription_plan() {
        let config = Config::default();
        let current = SubscriptionPlan::from_venues(&config.venues[..1]).unwrap();
        let mut reordered = config.venues[..1].to_vec();
        reordered[0].instruments.reverse();
        let next = SubscriptionPlan::from_venues(&reordered).unwrap();

        let diff = current.diff(&next);

        assert!(diff.removed.is_empty());
        assert!(diff.upserted.is_empty());
    }

    #[test]
    fn duplicate_enabled_venue_ids_are_rejected() {
        let config = Config::default();
        let duplicate = vec![config.venues[0].clone(), config.venues[0].clone()];

        let error = SubscriptionPlan::from_venues(&duplicate).unwrap_err();

        assert!(error.to_string().contains("duplicate venue_instance_id"));
    }

    #[test]
    fn non_empty_plan_rejects_an_empty_refresh_candidate() {
        let config = Config::default();
        let current = SubscriptionPlan::from_venues(&config.venues).unwrap();
        let empty = SubscriptionPlan::from_venues(&[]).unwrap();

        let error = current.validate_replacement(&empty).unwrap_err();

        assert!(error.to_string().contains("empty subscription plan"));
    }

    #[test]
    fn daily_refresh_waits_until_the_next_configured_utc_time() {
        let before = Utc.with_ymd_and_hms(2026, 7, 22, 0, 4, 30).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 7, 22, 0, 5, 1).unwrap();

        assert_eq!(
            super::next_refresh_delay(before, "00:05")
                .unwrap()
                .as_secs(),
            30
        );
        assert_eq!(
            super::next_refresh_delay(after, "00:05").unwrap().as_secs(),
            86_399
        );
        assert!(super::next_refresh_delay(before, "25:00").is_err());
    }
}
