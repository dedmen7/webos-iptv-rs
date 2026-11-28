use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub(crate) const CHANNEL_LIMIT: usize = 128;
pub(crate) const SAMPLE_LIFETIME_SECS: i64 = 7 * 24 * 60 * 60;
pub(crate) const FAILURE_COOLDOWN_SECS: i64 = 6 * 60 * 60;
pub(crate) const FEED_TRIAL_THRESHOLD_MS: u32 = 2000;
const INTERRUPTED_TRIAL_COOLDOWN_SECS: i64 = 10 * 60;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Format {
    Ts,
    Hls,
}

impl Format {
    pub(crate) fn other(self) -> Self {
        match self {
            Self::Ts => Self::Hls,
            Self::Hls => Self::Ts,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Choice {
    pub(crate) format: Format,
    pub(crate) proven: bool,
    pub(crate) trial_limit_ms: Option<u32>,
}

impl Choice {
    pub(crate) fn trial_against_feed(&mut self, feed_ms: u32) -> bool {
        let Some(limit) = &mut self.trial_limit_ms else {
            return false;
        };
        if feed_ms < FEED_TRIAL_THRESHOLD_MS {
            return false;
        }
        *limit = (*limit).min(feed_ms.saturating_add(650));
        true
    }
}

#[derive(Default, Deserialize, Serialize)]
struct Evidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    success_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trial_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timing: Option<Timing>,
}

#[derive(Deserialize, Serialize)]
struct Timing {
    measured_at: i64,
    startup_ms: u32,
    #[serde(default)]
    source: TimingSource,
}

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TimingSource {
    #[default]
    Cold,
    Preload,
}

impl TimingSource {
    pub(crate) fn merge(self, ms: u32, previous: Self, previous_ms: u32) -> Option<u32> {
        match (self, previous) {
            (Self::Preload, Self::Cold) => None,
            (Self::Cold, Self::Preload) => Some(ms),
            _ => Some(previous_ms.midpoint(ms)),
        }
    }
}

impl Evidence {
    fn score(&self, now: i64) -> Option<u32> {
        let timing = self.timing.as_ref()?;
        recent(timing.measured_at, now, SAMPLE_LIFETIME_SECS).then_some(timing.startup_ms)
    }

    fn proven(&self, now: i64) -> bool {
        self.failure_at.is_none()
            && self
                .success_at
                .is_some_and(|at| recent(at, now, SAMPLE_LIFETIME_SECS))
    }

    fn blocked(&self, now: i64) -> bool {
        self.failure_at
            .is_some_and(|at| recent(at, now, FAILURE_COOLDOWN_SECS))
            || self
                .trial_at
                .is_some_and(|at| recent(at, now, INTERRUPTED_TRIAL_COOLDOWN_SECS))
    }

    fn last_activity(&self) -> Option<i64> {
        self.success_at.max(self.failure_at).max(self.trial_at)
    }
}

pub(crate) fn recent(at: i64, now: i64, lifetime: i64) -> bool {
    (0..lifetime).contains(&now.saturating_sub(at))
}

#[derive(Default, Deserialize, Serialize)]
struct Channel {
    ts: Evidence,
    hls: Evidence,
}

impl Channel {
    fn route(&self, format: Format) -> &Evidence {
        match format {
            Format::Ts => &self.ts,
            Format::Hls => &self.hls,
        }
    }

    fn route_mut(&mut self, format: Format) -> &mut Evidence {
        match format {
            Format::Ts => &mut self.ts,
            Format::Hls => &mut self.hls,
        }
    }

    fn last_activity(&self) -> Option<i64> {
        self.ts.last_activity().max(self.hls.last_activity())
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(from = "SavedRoutes")]
pub(crate) struct Routes(HashMap<String, Channel>);

#[derive(Deserialize)]
#[serde(untagged)]
enum SavedRoutes {
    Timings(HashMap<String, Channel>),
    Legacy(HashMap<String, i64>),
}

impl From<SavedRoutes> for Routes {
    fn from(saved: SavedRoutes) -> Self {
        let mut channels = match saved {
            SavedRoutes::Timings(channels) => channels,
            SavedRoutes::Legacy(alternates) => {
                let mut channels = HashMap::<String, Channel>::new();
                for (key, at) in alternates {
                    let Some((channel, advertised)) = key.rsplit_once(':') else {
                        continue;
                    };
                    let alternate = match advertised {
                        "ts" => Format::Hls,
                        "m3u8" => Format::Ts,
                        _ => continue,
                    };
                    channels
                        .entry(channel.to_owned())
                        .or_default()
                        .route_mut(alternate)
                        .success_at = Some(at);
                }
                channels
            }
        };
        if channels.len() > CHANNEL_LIMIT {
            let mut ordered: Vec<_> = channels.into_iter().collect();
            ordered.sort_unstable_by_key(|(_, channel)| std::cmp::Reverse(channel.last_activity()));
            ordered.truncate(CHANNEL_LIMIT);
            channels = ordered.into_iter().collect();
        }
        Self(channels)
    }
}

impl Routes {
    pub(crate) fn select(&self, key: &str, advertised: Format, now: i64) -> Choice {
        self.choose(key, advertised, now, true)
    }

    pub(crate) fn preferred(&self, key: &str, advertised: Format, now: i64) -> Choice {
        self.choose(key, advertised, now, false)
    }

    fn choose(&self, key: &str, advertised: Format, now: i64, explore: bool) -> Choice {
        let Some(channel) = self.0.get(key) else {
            return Choice {
                format: advertised,
                proven: false,
                trial_limit_ms: None,
            };
        };
        let alternate = advertised.other();
        let first = channel.route(advertised);
        let second = channel.route(alternate);
        let mut trial_limit_ms = None;
        let format = match (first.blocked(now), second.blocked(now)) {
            (true, false) => alternate,
            (false, true) => advertised,
            (true, true) if second.failure_at < first.failure_at => alternate,
            (true, true) => advertised,
            (false, false) => match (first.score(now), second.score(now)) {
                (Some(first_ms), Some(second_ms)) if second_ms < first_ms => alternate,
                (Some(_), Some(_)) => advertised,
                (None, Some(_)) if !explore => alternate,
                (Some(_), None) if !explore => advertised,
                (None, Some(known_ms)) | (Some(known_ms), None) => {
                    // Give the unmeasured format one tune, with time to beat the working stream.
                    trial_limit_ms = Some(known_ms.saturating_add(650).clamp(1500, 12_000));
                    if first.score(now).is_none() {
                        advertised
                    } else {
                        alternate
                    }
                }
                (None, None) if !first.proven(now) && second.proven(now) => alternate,
                (None, None) => advertised,
            },
        };
        Choice {
            format,
            proven: channel.route(format).proven(now),
            trial_limit_ms,
        }
    }

    fn evidence(&mut self, key: &str, format: Format) -> &mut Evidence {
        if self.0.len() >= CHANNEL_LIMIT
            && !self.0.contains_key(key)
            && let Some(oldest) = self
                .0
                .iter()
                .min_by_key(|(_, channel)| channel.last_activity())
                .map(|(key, _)| key.clone())
        {
            self.0.remove(&oldest);
        }
        self.0.entry(key.to_owned()).or_default().route_mut(format)
    }

    pub(crate) fn begin_trial(&mut self, key: &str, format: Format, now: i64) {
        self.evidence(key, format).trial_at = Some(now);
    }

    pub(crate) fn success(&mut self, key: &str, format: Format, now: i64, startup_ms: Option<u32>) {
        self.record_success(key, format, now, startup_ms, TimingSource::Cold);
    }

    pub(crate) fn success_estimated(
        &mut self,
        key: &str,
        format: Format,
        now: i64,
        estimate_ms: u32,
    ) {
        self.record_success(key, format, now, Some(estimate_ms), TimingSource::Preload);
    }

    fn record_success(
        &mut self,
        key: &str,
        format: Format,
        now: i64,
        startup_ms: Option<u32>,
        source: TimingSource,
    ) {
        let evidence = self.evidence(key, format);
        evidence.success_at = Some(now);
        evidence.failure_at = None;
        if let Some(mut ms) = startup_ms {
            evidence.trial_at = None;
            if let Some(prior) = &evidence.timing
                && recent(prior.measured_at, now, SAMPLE_LIFETIME_SECS)
            {
                let Some(updated) = source.merge(ms, prior.source, prior.startup_ms) else {
                    return;
                };
                ms = updated;
            }
            evidence.timing = Some(Timing {
                measured_at: now,
                startup_ms: ms,
                source,
            });
        }
    }

    pub(crate) fn failure(&mut self, key: &str, format: Format, now: i64) {
        let evidence = self.evidence(key, format);
        evidence.failure_at = Some(now);
        evidence.trial_at = None;
        evidence.timing = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{CHANNEL_LIMIT, FAILURE_COOLDOWN_SECS, Format, Routes, SAMPLE_LIFETIME_SECS};

    #[test]
    fn compares_clean_tunes_and_restores_winners_per_channel_and_provider() {
        let mut routes = Routes::default();
        let channel = "source-a:Live:1";
        assert_eq!(routes.select(channel, Format::Ts, 1).format, Format::Ts);
        routes.success(channel, Format::Ts, 1, Some(6500));
        assert_eq!(routes.preferred(channel, Format::Ts, 2).format, Format::Ts);
        let trial = routes.select(channel, Format::Ts, 2);
        assert_eq!(trial.format, Format::Hls);
        assert!(trial.trial_limit_ms.unwrap() > 6500);
        routes.success(channel, Format::Hls, 2, Some(700));
        routes.success("source-a:Live:2", Format::Ts, 2, Some(900));
        routes.success("source-a:Live:2", Format::Hls, 3, Some(3900));
        let restored: Routes =
            serde_json::from_str(&serde_json::to_string(&routes).unwrap()).unwrap();
        let winner = restored.select(channel, Format::Ts, 4);
        assert_eq!(winner.format, Format::Hls);
        assert!(winner.proven);
        assert_eq!(winner.trial_limit_ms, None);
        assert_eq!(
            restored.select("source-a:Live:2", Format::Hls, 4).format,
            Format::Ts
        );
        assert_eq!(
            restored.select("source-b:Live:1", Format::Ts, 4).format,
            Format::Ts
        );
    }

    #[test]
    fn only_a_slow_measured_feed_yields_to_a_bounded_unknown_original() {
        let mut routes = Routes::default();
        let key = "source:Live:1";
        routes.success(key, Format::Ts, 1, Some(7400));
        assert!(!routes.select(key, Format::Ts, 2).trial_against_feed(800));
        let mut pending = routes.select(key, Format::Ts, 2);
        assert!(pending.trial_against_feed(2200));
        assert_eq!(pending.format, Format::Hls);
        assert_eq!(pending.trial_limit_ms, Some(2850));
        routes.failure(key, pending.format, 5);
        assert!(!routes.select(key, Format::Ts, 6).trial_against_feed(2200));
        routes.success(key, Format::Hls, 7, Some(900));
        let mut known = routes.select(key, Format::Ts, 8);
        assert!(!known.trial_against_feed(2200));
        assert_eq!(known.format, Format::Hls);
    }

    #[test]
    fn warm_and_abandoned_trials_do_not_replace_cold_measurements_or_repeat_immediately() {
        let mut routes = Routes::default();
        let key = "source:Live:1";
        routes.success(key, Format::Ts, 1, Some(6500));
        routes.begin_trial(key, Format::Hls, 2);
        routes.success(key, Format::Hls, 2, None);
        assert_eq!(routes.select(key, Format::Ts, 3).format, Format::Ts);
        assert_eq!(routes.select(key, Format::Ts, 603).format, Format::Hls);
        routes.begin_trial(key, Format::Hls, 604);
        assert_eq!(routes.select(key, Format::Ts, 605).format, Format::Ts);
        routes.success(key, Format::Hls, 606, Some(8000));
        routes.success(key, Format::Hls, 607, None);
        assert_eq!(routes.select(key, Format::Hls, 608).format, Format::Ts);
    }

    #[test]
    fn preloaded_tunes_learn_both_formats_until_a_cold_measurement_is_available() {
        let key = "source:Live:1";
        let mut routes = Routes::default();
        routes.success_estimated(key, Format::Ts, 1, 3600);
        let trial = routes.select(key, Format::Ts, 2);
        assert_eq!(trial.format, Format::Hls);
        routes.begin_trial(key, trial.format, 2);
        routes.success_estimated(key, trial.format, 3, 1700);
        assert_eq!(routes.select(key, Format::Ts, 4).format, Format::Hls);
        let mut restored: Routes =
            serde_json::from_str(&serde_json::to_string(&routes).unwrap()).unwrap();
        restored.success(key, Format::Ts, 5, Some(900));
        assert_eq!(restored.select(key, Format::Ts, 6).format, Format::Ts);
    }

    #[test]
    fn preload_estimates_leave_a_legacy_cold_score_and_its_expiry_intact() {
        let key = "source:Live:1";
        let legacy = r#"{"source:Live:1":{"ts":{"success_at":1,"timing":{"measured_at":1,"startup_ms":900}},"hls":{}}}"#;
        let mut routes: Routes = serde_json::from_str(legacy).unwrap();
        routes.success_estimated(key, Format::Ts, 10, 5000);
        routes.success(key, Format::Hls, 15, Some(1200));
        assert_eq!(routes.preferred(key, Format::Ts, 16).format, Format::Ts);
        let expired = 1 + SAMPLE_LIFETIME_SECS;
        assert_eq!(
            routes.preferred(key, Format::Ts, expired).format,
            Format::Hls
        );
        routes.success_estimated(key, Format::Ts, expired, 500);
        assert_eq!(
            routes.preferred(key, Format::Ts, expired).format,
            Format::Ts
        );
    }

    #[test]
    fn failing_winner_falls_back_and_can_be_tested_again_after_cooldown() {
        let mut routes = Routes::default();
        let key = "source:Live:1";
        routes.success(key, Format::Hls, 1, Some(800));
        routes.success(key, Format::Ts, 1, Some(5000));
        routes.failure(key, Format::Hls, 2);
        assert_eq!(routes.select(key, Format::Hls, 3).format, Format::Ts);
        let retry = routes.select(key, Format::Ts, 2 + FAILURE_COOLDOWN_SECS);
        assert_eq!(retry.format, Format::Hls);
        assert!(!retry.proven);
        routes.success(key, Format::Hls, 3 + FAILURE_COOLDOWN_SECS, Some(6900));
        assert_eq!(
            routes
                .select(key, Format::Hls, 4 + FAILURE_COOLDOWN_SECS)
                .format,
            Format::Ts
        );
    }

    #[test]
    fn legacy_alternates_preserve_format_across_changes_to_advertised_route_and_expire() {
        let old = r#"{"source:Live:1:m3u8":300,"source:Live:2:ts":310}"#;
        let routes: Routes = serde_json::from_str(old).unwrap();
        assert_eq!(
            routes.select("source:Live:1", Format::Hls, 400).format,
            Format::Ts
        );
        assert_eq!(
            routes.select("source:Live:2", Format::Ts, 400).format,
            Format::Hls
        );
        assert_eq!(
            routes.select("source:Live:2", Format::Hls, 400).format,
            Format::Hls
        );
        assert_eq!(
            routes
                .select("source:Live:1", Format::Hls, 300 + SAMPLE_LIFETIME_SECS)
                .format,
            Format::Hls
        );
    }

    #[test]
    fn benchmarking_evicted_or_expired_channels_starts_with_the_provider_variant() {
        let mut routes = Routes::default();
        for i in 0..=CHANNEL_LIMIT {
            routes.success(&i.to_string(), Format::Ts, i as i64, Some(800));
        }
        assert_eq!(routes.0.len(), CHANNEL_LIMIT);
        assert_eq!(routes.select("0", Format::Ts, 200).trial_limit_ms, None);
        assert_eq!(routes.select("1", Format::Ts, 200).format, Format::Hls);
        let fresh = routes.select("1", Format::Ts, 1 + SAMPLE_LIFETIME_SECS);
        assert_eq!(fresh.format, Format::Ts);
        assert!(!fresh.proven);
    }
}
