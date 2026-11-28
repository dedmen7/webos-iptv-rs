use crate::live_routes::{
    CHANNEL_LIMIT, FAILURE_COOLDOWN_SECS, Format, SAMPLE_LIFETIME_SECS, TimingSource, recent,
};
use serde::{Deserialize, Serialize};
use std::hash::{DefaultHasher, Hash, Hasher};

pub(crate) struct FeedSample<'a> {
    pub(crate) startup_ms: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) epg_id: &'a str,
    pub(crate) format: Option<Format>,
}

#[derive(Deserialize, Serialize)]
struct StreamSample {
    format: Option<Format>,
    startup_ms: u32,
    #[serde(default)]
    timing_source: TimingSource,
    width: u32,
    height: u32,
    at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failed_at: Option<i64>,
}

#[derive(Deserialize, Serialize)]
struct Feed {
    source: String,
    id: String,
    epg: u64,
    streams: Vec<StreamSample>,
    failed_at: Option<i64>,
}

impl Feed {
    fn last_activity(&self) -> Option<i64> {
        self.streams
            .iter()
            .map(|sample| sample.at)
            .max()
            .max(self.failed_at)
    }

    fn recent_streams(&self, now: i64) -> impl Iterator<Item = &StreamSample> {
        self.streams
            .iter()
            .filter(move |sample| recent(sample.at, now, SAMPLE_LIFETIME_SECS))
    }
}

#[derive(Default, Deserialize, Serialize)]
pub(crate) struct Feeds(Vec<Feed>);

fn epg_hash(epg_id: &str) -> Option<u64> {
    let id = epg_id.trim();
    if id.is_empty() {
        return None;
    }
    let mut hash = DefaultHasher::new();
    id.to_ascii_lowercase().hash(&mut hash);
    Some(hash.finish())
}

fn faster_feed_cutoff(startup_ms: u32) -> u32 {
    startup_ms
        .saturating_sub(800)
        .min(startup_ms - startup_ms.div_ceil(4))
}

impl Feeds {
    pub(crate) fn has_samples_for_source(&self, source: &str) -> bool {
        self.0
            .iter()
            .any(|feed| feed.source == source && !feed.streams.is_empty())
    }

    pub(crate) fn has_preview_candidate(
        &self,
        source: &str,
        first_items: &[crate::catalog::Item],
        now: i64,
    ) -> bool {
        first_items.iter().any(|logical| {
            logical.kind == crate::catalog::Kind::Live
                && self
                    .best_feed(source, &logical.id, &logical.epg_id, now, |id| {
                        !first_items.iter().any(|visible| {
                            visible.id == id
                                && visible
                                    .epg_id
                                    .trim()
                                    .eq_ignore_ascii_case(logical.epg_id.trim())
                        })
                    })
                    .is_some()
        })
    }

    pub(crate) fn preview_alternatives<'a>(
        &'a self,
        source: &str,
        logical_id: &str,
        epg_id: &str,
        now: i64,
    ) -> impl Iterator<Item = &'a str> + use<'a> {
        let guide = epg_hash(epg_id);
        let original = self.0.iter().find(|feed| {
            feed.id == logical_id
                && feed.source == source
                && Some(feed.epg) == guide
                && feed.recent_streams(now).next().is_some()
        });
        // A slower or cooling feed can become the winner while this cached preview is still current.
        original.into_iter().flat_map(move |original| {
            self.0
                .iter()
                .filter(move |feed| {
                    feed.id != original.id
                        && feed.source == original.source
                        && feed.epg == original.epg
                        && feed.recent_streams(now).any(|sample| {
                            original.recent_streams(now).any(|baseline| {
                                sample.width >= baseline.width
                                    && sample.height >= baseline.height
                                    && sample.startup_ms <= faster_feed_cutoff(baseline.startup_ms)
                            })
                        })
                })
                .map(|feed| feed.id.as_str())
        })
    }

    pub(crate) fn record(
        &mut self,
        source: &str,
        physical_id: &str,
        now: i64,
        sample: FeedSample<'_>,
    ) -> bool {
        self.record_sample(source, physical_id, now, sample, TimingSource::Cold)
    }

    pub(crate) fn record_estimated(
        &mut self,
        source: &str,
        physical_id: &str,
        now: i64,
        sample: FeedSample<'_>,
    ) -> bool {
        self.record_sample(source, physical_id, now, sample, TimingSource::Preload)
    }

    fn record_sample(
        &mut self,
        source: &str,
        physical_id: &str,
        now: i64,
        sample: FeedSample<'_>,
        timing_source: TimingSource,
    ) -> bool {
        let Some(epg) = epg_hash(sample.epg_id) else {
            return false;
        };
        if source.is_empty()
            || physical_id.is_empty()
            || sample.width == 0
            || sample.height == 0
            || sample.startup_ms == 0
        {
            return false;
        }
        let index = self
            .0
            .iter()
            .position(|feed| feed.source == source && feed.id == physical_id);
        let feed = if let Some(index) = index {
            &mut self.0[index]
        } else {
            if self.0.len() >= CHANNEL_LIMIT
                && let Some((index, _)) = self
                    .0
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, feed)| feed.last_activity())
            {
                self.0.swap_remove(index);
            }
            self.0.push(Feed {
                source: source.to_owned(),
                id: physical_id.to_owned(),
                epg,
                streams: Vec::new(),
                failed_at: None,
            });
            self.0.last_mut().expect("just inserted feed")
        };
        if epg != feed.epg {
            feed.streams.clear();
            feed.epg = epg;
        }
        feed.failed_at = None;
        let mut measured = StreamSample {
            format: sample.format,
            startup_ms: sample.startup_ms,
            timing_source,
            width: sample.width,
            height: sample.height,
            at: now,
            failed_at: None,
        };
        if let Some(prior) = feed
            .streams
            .iter_mut()
            .find(|stream| stream.format == sample.format)
        {
            if prior.width == sample.width
                && prior.height == sample.height
                && recent(prior.at, now, SAMPLE_LIFETIME_SECS)
            {
                let Some(updated) =
                    timing_source.merge(sample.startup_ms, prior.timing_source, prior.startup_ms)
                else {
                    prior.failed_at = None;
                    return true;
                };
                measured.startup_ms = updated;
            }
            *prior = measured;
        } else {
            feed.streams.push(measured);
        }
        true
    }

    pub(crate) fn failure(
        &mut self,
        source: &str,
        physical_id: &str,
        now: i64,
        format: Option<Format>,
    ) -> bool {
        let Some(feed) = self
            .0
            .iter_mut()
            .find(|feed| feed.source == source && feed.id == physical_id)
        else {
            return false;
        };
        feed.failed_at = Some(now);
        if let Some(stream) = feed
            .streams
            .iter_mut()
            .find(|stream| stream.format == format)
        {
            stream.failed_at = Some(now);
        }
        true
    }

    pub(crate) fn best_feed(
        &self,
        source: &str,
        logical_id: &str,
        logical_epg_id: &str,
        now: i64,
        mut eligible: impl FnMut(&str) -> bool,
    ) -> Option<(&str, Option<Format>, u32)> {
        let logical = self
            .0
            .iter()
            .find(|feed| feed.source == source && feed.id == logical_id)?;
        if logical.epg != epg_hash(logical_epg_id)? {
            return None;
        }
        let mut baseline_ms = u32::MAX;
        let mut width = 0;
        let mut height = 0;
        for sample in logical.recent_streams(now) {
            baseline_ms = baseline_ms.min(sample.startup_ms);
            width = width.max(sample.width);
            height = height.max(sample.height);
        }
        if width == 0 || height == 0 {
            return None;
        }
        // A substitution must improve even the fastest known original and preserve its resolution.
        let cutoff = faster_feed_cutoff(baseline_ms);
        self.0
            .iter()
            .filter(|feed| {
                feed.source == source
                    && feed.epg == logical.epg
                    && feed.id != logical_id
                    && !feed
                        .failed_at
                        .is_some_and(|at| recent(at, now, FAILURE_COOLDOWN_SECS))
            })
            .filter_map(|feed| {
                let best = feed
                    .recent_streams(now)
                    .filter(|sample| {
                        sample.width >= width
                            && sample.height >= height
                            && sample.startup_ms <= cutoff
                            && !sample
                                .failed_at
                                .is_some_and(|at| recent(at, now, FAILURE_COOLDOWN_SECS))
                    })
                    .min_by_key(|sample| sample.startup_ms)?;
                eligible(&feed.id).then_some((feed.id.as_str(), best))
            })
            .min_by_key(|(_, sample)| sample.startup_ms)
            .map(|(id, sample)| (id, sample.format, sample.startup_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample<'a>(epg_id: &'a str, format: Format, ms: u32, height: u32) -> FeedSample<'a> {
        FeedSample {
            startup_ms: ms,
            width: height * 16 / 9,
            height,
            epg_id,
            format: Some(format),
        }
    }

    #[test]
    fn chooses_only_equivalent_higher_or_equal_quality_and_its_measured_format() {
        let mut feeds = Feeds::default();
        let now = 100;
        for (source, id, epg, ms, height) in [
            ("a", "home", "test", 6400, 1080),
            ("a", "hd", "Test", 900, 1080),
            ("a", "uhd", "test", 1100, 2160),
            ("a", "sd", "test", 200, 576),
            ("a", "wrong-name", "test", 100, 2160),
            ("a", "wrong-epg", "different", 100, 2160),
            ("b", "foreign", "test", 100, 2160),
        ] {
            feeds.record(source, id, now, sample(epg, Format::Hls, ms, height));
        }
        feeds.record("a", "hd", now, sample("test", Format::Ts, 150, 576));
        let restored: Feeds =
            serde_json::from_str(&serde_json::to_string(&feeds).unwrap()).unwrap();
        assert_eq!(
            restored.best_feed("a", "home", "test", now, |id| id != "wrong-name"),
            Some(("hd", Some(Format::Hls), 900))
        );
        assert_eq!(
            restored.best_feed("a", "home", "test", now, |id| id == "uhd"),
            Some(("uhd", Some(Format::Hls), 1100))
        );
        assert_eq!(
            restored.best_feed("a", "home", "test", now, |_| false),
            None
        );
        assert_eq!(
            restored.best_feed("a", "home", "replacement", now, |_| true),
            None
        );
    }

    #[test]
    fn requires_fresh_measured_baseline_and_skips_failed_feeds() {
        let mut feeds = Feeds::default();
        let now = 300;
        feeds.record("a", "alt", now, sample("test", Format::Hls, 900, 1080));
        assert_eq!(feeds.best_feed("a", "home", "test", now, |_| true), None);
        feeds.record("a", "home", now, sample("test", Format::Ts, 6500, 1080));
        feeds.failure("a", "alt", now + 1, Some(Format::Hls));
        assert_eq!(
            feeds.best_feed("a", "home", "test", now + 2, |_| true),
            None
        );
        assert_eq!(
            feeds.best_feed("a", "home", "test", now + 1 + FAILURE_COOLDOWN_SECS, |_| {
                true
            }),
            Some(("alt", Some(Format::Hls), 900))
        );
        assert_eq!(
            feeds.best_feed("a", "home", "test", now + SAMPLE_LIFETIME_SECS, |_| true),
            None
        );
    }

    #[test]
    fn preload_estimates_enable_substitutions_and_yield_to_cold_tunes() {
        let mut feeds = Feeds::default();
        feeds.record_estimated("a", "home", 10, sample("test", Format::Ts, 5000, 1080));
        feeds.record_estimated("a", "alt", 20, sample("test", Format::Hls, 2000, 1080));
        assert_eq!(
            feeds
                .best_feed("a", "home", "test", 20, |_| true)
                .unwrap()
                .2,
            2000
        );
        let mut restored: Feeds =
            serde_json::from_str(&serde_json::to_string(&feeds).unwrap()).unwrap();
        restored.record("a", "alt", 30, sample("test", Format::Hls, 900, 1080));
        restored.record_estimated("a", "alt", 40, sample("test", Format::Hls, 3300, 1080));
        assert_eq!(
            restored
                .best_feed("a", "home", "test", 40, |_| true)
                .unwrap()
                .2,
            900
        );
        let expired = 30 + SAMPLE_LIFETIME_SECS;
        restored.record("a", "home", expired, sample("test", Format::Ts, 5000, 1080));
        assert!(
            restored
                .best_feed("a", "home", "test", expired, |_| true)
                .is_none()
        );
        restored.record_estimated("a", "alt", expired, sample("test", Format::Hls, 1200, 1080));
        assert_eq!(
            restored
                .best_feed("a", "home", "test", expired, |_| true)
                .unwrap()
                .2,
            1200
        );
        restored.record("a", "alt", expired, sample("test", Format::Hls, 900, 1080));
        restored.record_estimated(
            "a",
            "alt",
            expired + 1,
            sample("test", Format::Hls, 1200, 576),
        );
        assert!(
            restored
                .best_feed("a", "home", "test", expired + 1, |_| true)
                .is_none()
        );
    }

    #[test]
    fn a_successful_fallback_does_not_rehabilitate_the_failed_transport() {
        let mut feeds = Feeds::default();
        feeds.record("a", "home", 10, sample("test", Format::Ts, 7000, 1080));
        feeds.record("a", "alt", 11, sample("test", Format::Ts, 650, 1080));
        feeds.failure("a", "alt", 12, Some(Format::Ts));
        feeds.record("a", "alt", 13, sample("test", Format::Hls, 7500, 1080));
        assert_eq!(feeds.best_feed("a", "home", "test", 14, |_| true), None);
        feeds.record("a", "alt", 15, sample("test", Format::Ts, 900, 1080));
        assert_eq!(
            feeds
                .best_feed("a", "home", "test", 16, |_| true)
                .map(|x| x.1),
            Some(Some(Format::Ts))
        );
    }

    #[test]
    fn rejects_small_gains_changed_epg_and_unmeasured_resolution() {
        let mut feeds = Feeds::default();
        let now = 50;
        assert!(!feeds.record("a", "low", now, sample("test", Format::Ts, 5, 0)));
        feeds.record("a", "home", now, sample("test", Format::Ts, 4000, 1080));
        feeds.record("a", "close", now, sample("test", Format::Hls, 3100, 1080));
        assert_eq!(feeds.best_feed("a", "home", "test", now, |_| true), None);
        feeds.record("a", "home", now, sample("test", Format::Hls, 1300, 1080));
        feeds.record("a", "small", now, sample("test", Format::Hls, 700, 1080));
        assert_eq!(feeds.best_feed("a", "home", "test", now, |_| true), None);
        feeds.record(
            "a",
            "small",
            now,
            sample("new station", Format::Hls, 100, 1080),
        );
        assert_eq!(feeds.best_feed("a", "home", "test", now, |_| true), None);
    }
}
