use super::i18n::{message, tr};
use super::{
    App, CONFIG_KEY, LIBRARY_KEY, Playback, Shared, View, find_item, focus, io, live, on, sources,
    ui,
};
use crate::{
    catalog::{Item, Kind, Language, http_url, language, subtitle_title},
    live_feeds::FeedSample,
    live_routes::{Choice, FEED_TRIAL_THRESHOLD_MS, Format},
    series::SeriesProgress,
    subtitles::{self, Download, Subtitle},
    sync::Timing,
};
use js_sys::{Array, Reflect};
use serde::{Deserialize, Serialize};
use std::hash::{DefaultHasher, Hash, Hasher};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    Blob, BlobPropertyBag, HtmlElement, HtmlInputElement, HtmlMediaElement, HtmlTrackElement,
    TextTrack, TextTrackKind, TextTrackMode,
};

const LIVE_PRELOAD_DELAY_MS: i32 = 450;
const LIVE_PRELOAD_LIMIT_MS: i32 = 12_000;
const STREAM_START_TIMEOUT_MS: i32 = 18_000;
const LIVE_NO_METADATA_TIMEOUT_MS: i32 = 6_000;
const KNOWN_LIVE_ROUTE_TIMEOUT_MS: i32 = 9_000;
const LIVE_STALL_SECONDS: i64 = 8;
const LIVE_RECOVERY_RESET_SECONDS: i64 = 30;
pub(super) const LIVE_ROUTES_KEY: &str = "iptv-rs.live-routes";
pub(super) const LIVE_FEEDS_KEY: &str = "iptv-rs.live-feeds";
const STREAM_FAILED: &str = "Deze stream kon niet worden afgespeeld.";
const STREAM_RECONNECTING: &str = "Verbinding herstellen…";
const RADIO_LOADING: &str = "Radiozender laden…";
const SYNC_UNAVAILABLE: &str = "Automatische controle niet beschikbaar voor deze stream.";
const LIVE_LOADING_MESSAGES: &[&str] = &[
    "Even de antenne rechtzetten…",
    "Orbis zoekt beeld…",
    "De zender komt eraan…",
];

#[derive(Default)]
pub(super) struct StreamHealth {
    observation: Option<StreamProgress>,
    retried: bool,
    user_paused: bool,
}

struct StreamProgress {
    position: f64,
    advanced_at: i64,
    stable_since: i64,
}

impl StreamHealth {
    fn stalled(&mut self, position: f64, now: i64) -> bool {
        let progress = self.observation.get_or_insert(StreamProgress {
            position,
            advanced_at: now,
            stable_since: now,
        });
        let stopped_for = now.saturating_sub(progress.advanced_at);
        if (position - progress.position).abs() >= 0.05 {
            if stopped_for > 3 {
                progress.stable_since = now;
            }
            progress.position = position;
            progress.advanced_at = now;
            if now.saturating_sub(progress.stable_since) >= LIVE_RECOVERY_RESET_SECONDS {
                self.retried = false;
            }
            return false;
        }
        stopped_for >= LIVE_STALL_SECONDS
    }
}

pub(super) struct LiveRoute {
    key: String,
    choice: Option<Choice>,
    source: String,
    physical_id: String,
    epg_id: String,
    url: url::Url,
    startup: Option<StartupClock>,
    awaiting_picture: bool,
    fallback: Option<Box<StreamUrls>>,
    previous_urls: Vec<url::Url>,
    donor_limit_ms: Option<u32>,
}

fn media_clock_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or_else(js_sys::Date::now, |clock| clock.now())
}

impl LiveRoute {
    fn trial_limit_ms(&self) -> Option<u32> {
        self.donor_limit_ms
            .or_else(|| self.choice.as_ref()?.trial_limit_ms)
    }

    fn current(&self, video: &web_sys::HtmlVideoElement) -> bool {
        video.current_src() == self.url.as_str() && video.src() == self.url.as_str()
    }
}

#[derive(Clone, Copy)]
enum StartupClock {
    Cold { started_at: f64 },
    Focused { started_at: f64, idle_ms: f64 },
}

impl StartupClock {
    fn cold() -> Self {
        Self::Cold {
            started_at: media_clock_ms(),
        }
    }

    fn elapsed_ms(self) -> Option<u32> {
        let now = media_clock_ms();
        let elapsed = match self {
            Self::Cold { started_at } => now - started_at,
            Self::Focused {
                started_at,
                idle_ms,
            } => now - started_at - idle_ms,
        };
        (0.0..=f64::from(STREAM_START_TIMEOUT_MS))
            .contains(&elapsed)
            .then(|| elapsed.round().max(1.0) as u32)
    }
}

struct PreloadedStream {
    url: url::Url,
    load_started_at: Option<f64>,
    decoded_at: Option<f64>,
}

impl PreloadedStream {
    fn startup(&self, video: &web_sys::HtmlVideoElement, selected_at: f64) -> Option<StartupClock> {
        let current = video.current_src();
        if (!current.is_empty() && current != self.url.as_str()) || video.seeking() {
            return None;
        }
        let (started_at, idle_ms) = match (self.load_started_at, self.decoded_at) {
            (Some(start), Some(decoded)) => (start, selected_at - decoded),
            (_, _) if has_picture(video) => return None,
            (Some(start), None) if video.network_state() == HtmlMediaElement::NETWORK_LOADING => {
                (start, 0.)
            }
            (None, None) if video.ready_state() == HtmlMediaElement::HAVE_NOTHING => {
                (selected_at, 0.)
            }
            _ => return None,
        };
        Some(StartupClock::Focused {
            started_at,
            idle_ms,
        })
    }
}

fn has_picture(video: &web_sys::HtmlVideoElement) -> bool {
    video.ready_state() >= HtmlMediaElement::HAVE_CURRENT_DATA
        && video.video_width() > 0
        && video.video_height() > 0
}

struct StreamUrls {
    primary: url::Url,
    alternate: Option<url::Url>,
    live_route: Option<LiveRoute>,
}

pub(super) struct LivePreload {
    key: String,
    revision: u64,
    sequence: u64,
    stream: Option<PreloadedStream>,
}

#[derive(Clone, Copy, PartialEq)]
enum StreamSwitch {
    Error,
    Deadline,
    NoMetadata,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SavedSubtitle {
    Off,
    Embedded(EmbeddedTrack),
    External {
        subtitle: Subtitle,
        timing: SubtitleTiming,
    },
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct EmbeddedTrack {
    id: String,
    label: String,
    language: Language,
    cc: bool,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
pub(super) struct SubtitleTiming {
    #[serde(flatten)]
    clock: Timing,
    auto: bool,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum SubtitleSize {
    Original,
    #[default]
    Standard,
    Large,
}

impl SubtitleSize {
    fn value(self) -> &'static str {
        match self {
            Self::Original => "100",
            Self::Standard => "120",
            Self::Large => "150",
        }
    }
}

pub fn wire(app: &Shared) -> Result<(), JsValue> {
    render_subtitle_size(&app.borrow());
    let video = app.borrow().video();
    for event in ["loadstart", "loadeddata", "canplay", "resize"] {
        let shared = app.clone();
        on(&video, event, move |_| {
            observe_preload(&shared, event == "loadstart")
        })?;
    }
    let shared = app.clone();
    on(&video, "loadedmetadata", move |_| {
        let mut state = shared.borrow_mut();
        let video = state.video();
        if let Some(play) = &mut state.playback {
            play.awaiting_cross_origin = false;
            let duration = video.duration();
            if play.resume_at > 0. && duration.is_finite() && play.resume_at < duration {
                video.set_current_time(play.resume_at);
            }
        }
        restore_tracks(&state);
        update_timeline(&state);
    })?;
    let shared = app.clone();
    on(&video, "timeupdate", move |_| {
        save_position(&shared, false, false);
        update_timeline(&shared.borrow());
    })?;
    let shared = app.clone();
    on(&video, "durationchange", move |_| {
        update_timeline(&shared.borrow())
    })?;
    let shared = app.clone();
    let seek: HtmlInputElement = app.borrow().element("player-seek").unchecked_into();
    on(&seek, "input", move |event| {
        if let Some(input) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
        {
            seek_to(&shared.borrow(), input.value_as_number());
        }
    })?;
    let shared = app.clone();
    on(&video, "pause", move |_| {
        save_position(&shared, true, false)
    })?;
    for event in ["playing", "pause", "waiting", "ended", "error"] {
        let shared = app.clone();
        on(&video, event, move |_| {
            update_radio_motion(&shared.borrow())
        })?;
    }
    let shared = app.clone();
    let document = app.borrow().document.clone();
    for event in ["focusin", "focusout"] {
        let shared = app.clone();
        on(&document, event, move |_| queue_live_preload(&shared))?;
    }
    on(&document, "visibilitychange", move |_| {
        update_radio_motion(&shared.borrow());
        queue_live_preload(&shared);
        super::load_visible_live_programmes(&shared);
        if let Some(health) = shared
            .borrow_mut()
            .playback
            .as_mut()
            .and_then(|play| play.stream_health.as_mut())
        {
            health.observation = None;
        }
        if shared.borrow().document.hidden() {
            {
                let mut state = shared.borrow_mut();
                if state
                    .live_preload
                    .take()
                    .is_some_and(|preload| preload.stream.is_some())
                {
                    release_preloaded_media(&state);
                }
            }
            if let Some(route) = shared
                .borrow_mut()
                .playback
                .as_mut()
                .and_then(|play| play.live_route.as_mut())
            {
                route.startup = None;
            }
            save_position(&shared, true, false);
        } else {
            focus_live_video(&shared.borrow());
        }
    })?;
    let shared = app.clone();
    let window = web_sys::window().ok_or(tr("Browservenster ontbreekt"))?;
    on(&window, "pagehide", move |_| {
        save_position(&shared, true, false)
    })?;
    let shared = app.clone();
    on(&video, "ended", move |_| {
        let live_version = {
            let state = shared.borrow();
            let Some(play) = &state.playback else { return };
            if play.stream_failed
                || play
                    .stream_health
                    .as_ref()
                    .is_some_and(|health| health.user_paused)
            {
                return;
            }
            play.stream_health
                .as_ref()
                .filter(|_| play.started)
                .map(|_| play.version)
        };
        if let Some(version) = live_version {
            reconnect_stream(&shared, version);
            return;
        }
        save_position(&shared, true, true);
        shared.borrow().player_note(tr("Einde van deze titel"));
    })?;
    let shared = app.clone();
    on(&video, "error", move |_| {
        let mut state = shared.borrow_mut();
        let video = state.video();
        let Some(play) = &mut state.playback else {
            return;
        };
        if play.stream_failed
            || play.live_route.as_ref().is_some_and(|route| {
                let current = video.current_src();
                video.error().is_none()
                    || video.src() != route.url.as_str()
                    || (!current.is_empty() && current != route.url.as_str())
            })
            || play
                .stream_health
                .as_ref()
                .is_some_and(|health| health.user_paused)
        {
            return;
        }
        if play.started && play.stream_health.is_some() {
            let version = play.version;
            drop(state);
            reconnect_stream(&shared, version);
        } else if play.awaiting_cross_origin {
            play.awaiting_cross_origin = false;
            let url = video.src();
            video.set_cross_origin(None);
            video.set_src(&url);
            drop(state);
            request_play(&shared);
        } else {
            let version = play.version;
            drop(state);
            if !try_alternate_stream(&shared, version, StreamSwitch::Error) {
                fail_stream(&shared, version);
            }
        }
    })?;
    let shared = app.clone();
    let tracks = video
        .text_tracks()
        .ok_or(tr("Ondertitelsporen ontbreken"))?;
    on(&tracks, "addtrack", move |_| {
        restore_tracks(&shared.borrow())
    })?;
    let shared = app.clone();
    on(&video, "playing", move |_| media_playing(&shared))?;
    for event in ["resize", "timeupdate"] {
        let shared = app.clone();
        on(&video, event, move |_| {
            if shared
                .borrow()
                .playback
                .as_ref()
                .and_then(|play| play.live_route.as_ref())
                .is_some_and(|route| route.awaiting_picture)
            {
                media_playing(&shared);
            }
        })?;
    }
    let shared = app.clone();
    on(&video, "click", move |_| {
        let playback = shared
            .borrow()
            .playback
            .as_ref()
            .map(|play| (play.item.kind, play.panel));
        let Some((kind, panel)) = playback else {
            return;
        };
        if panel {
            show_panel(&shared, false);
            return;
        }
        let live = kind == Kind::Live;
        if live && dismiss_controls(&shared) {
            return;
        }
        if !show_controls(&shared) && !live {
            toggle(&shared);
        }
    })?;
    let shared = app.clone();
    let player = app.borrow().element("player");
    on(&player, "mousemove", move |_| {
        let live = shared
            .borrow()
            .playback
            .as_ref()
            .is_some_and(|play| play.item.kind == Kind::Live);
        if !live {
            show_controls(&shared);
        }
    })?;
    let playback_monitor = app.clone();
    let shifted = app.clone();
    let app = app.clone();
    let hide_controls = Closure::<dyn FnMut()>::new(move || {
        check_stream_health(&playback_monitor);
        let state = app.borrow();
        if state.playback.as_ref().is_some_and(|play| {
            !play.panel
                && play.item.kind != Kind::Radio
                && play.controls_until < io::now()
                && (play.item.kind == Kind::Live
                    || (!state.video().paused() && (!play.archive || play.started)))
        }) && !state.live_guide.is_open()
            && !controls_hidden(&state)
        {
            hide_controls(&state);
        }
    });
    web_sys::window()
        .ok_or(tr("Browservenster ontbreekt"))?
        .set_interval_with_callback_and_timeout_and_arguments_0(
            hide_controls.as_ref().unchecked_ref(),
            1000,
        )?;
    hide_controls.forget();
    on(&video, "seeking", move |_| {
        let mut state = shifted.borrow_mut();
        let time = state.video().current_time();
        if let Some(timing) = state.playback.as_mut().and_then(|play| play.sync.as_mut()) {
            timing.skip(time);
        }
    })?;
    Ok(())
}

fn update_radio_motion(state: &App) {
    let player = state.element("player");
    let video = state.video();
    if player.has_attribute("data-radio")
        && !state.document.hidden()
        && state.playback.is_some()
        && !video.paused()
        && !video.ended()
        && video.error().is_none()
        && video.ready_state() >= HtmlMediaElement::HAVE_FUTURE_DATA
    {
        let _ = player.set_attribute("data-radio-playing", "");
    } else {
        let _ = player.remove_attribute("data-radio-playing");
    }
}

fn focused_live_key(state: &App) -> Option<String> {
    if state.view != View::Live || state.playback.is_some() || state.document.hidden() {
        return None;
    }
    let tile = state.document.active_element()?;
    tile.matches("#search-results .tile[data-cmd='item']")
        .ok()?
        .then(|| tile.get_attribute("data-value"))
        .flatten()
}

pub(super) fn queue_live_preload(app: &Shared) {
    let app = app.clone();
    spawn_local(async move {
        let sequence = {
            let mut state = app.borrow_mut();
            let key = focused_live_key(&state);
            if state.live_preload.as_ref().is_some_and(|preload| {
                key.as_ref() == Some(&preload.key) && preload.revision == state.revision
            }) {
                return;
            }
            if state
                .live_preload
                .take()
                .is_some_and(|preload| preload.stream.is_some())
            {
                release_preloaded_media(&state);
            }
            let Some(key) = key else { return };
            let sequence = state.next_sequence();
            state.live_preload = Some(LivePreload {
                key,
                revision: state.revision,
                sequence,
                stream: None,
            });
            sequence
        };
        io::delay(LIVE_PRELOAD_DELAY_MS).await;
        if !preload_focused_stream(&app, sequence) {
            return;
        }
        io::delay(LIVE_PRELOAD_LIMIT_MS).await;
        let mut state = app.borrow_mut();
        if let Some(preload) = &mut state.live_preload
            && preload.sequence == sequence
            && preload.stream.take().is_some()
        {
            release_preloaded_media(&state);
        }
    });
}

fn preload_focused_stream(app: &Shared, sequence: u64) -> bool {
    let mut state = app.borrow_mut();
    let Some(preload) = state.live_preload.as_ref().filter(|preload| {
        preload.sequence == sequence
            && preload.revision == state.revision
            && focused_live_key(&state).as_ref() == Some(&preload.key)
    }) else {
        return false;
    };
    let Some(item) = find_item(&state, &preload.key).filter(|item| item.kind == Kind::Live) else {
        return false;
    };
    let Some(streams) = stream_urls(&state, &item) else {
        return false;
    };
    let video = state.video();
    video.set_cross_origin(None);
    video.set_preload("auto");
    video.set_src(streams.primary.as_str());
    if let Some(preload) = &mut state.live_preload {
        preload.stream = Some(PreloadedStream {
            url: streams.primary,
            load_started_at: None,
            decoded_at: None,
        });
    }
    true
}

fn observe_preload(app: &Shared, load_start: bool) {
    let mut state = app.borrow_mut();
    if state.live_preload.is_none() || state.playback.is_some() || state.document.hidden() {
        return;
    }
    let video = state.video();
    if video.seeking() || !video.paused() || video.error().is_some() {
        return;
    }
    let revision = state.revision;
    let focused = focused_live_key(&state);
    if let Some(preload) = &mut state.live_preload
        && preload.revision == revision
        && focused.as_ref() == Some(&preload.key)
        && let Some(stream) = &mut preload.stream
        && video.src() == stream.url.as_str()
    {
        let current = video.current_src();
        if load_start && (current.is_empty() || current == stream.url.as_str()) {
            stream.load_started_at.get_or_insert_with(media_clock_ms);
        } else if !load_start
            && stream.decoded_at.is_none()
            && current == stream.url.as_str()
            && has_picture(&video)
        {
            stream.decoded_at = Some(media_clock_ms());
        }
    }
}

fn media_playing(shared: &Shared) {
    let save_episode = {
        let mut state = shared.borrow_mut();
        let video = state.video();
        let visible = !state.document.hidden();
        let App {
            playback,
            live_routes,
            live_feeds,
            live_preview,
            ..
        } = &mut *state;
        let Some(play) = playback else { return };
        if play.stream_failed
            || play
                .live_route
                .as_ref()
                .is_some_and(|route| !route.current(&video))
            || (play.stream_health.as_ref().is_some_and(|health| {
                health.user_paused
                    || video.paused()
                    || video.ready_state() < HtmlMediaElement::HAVE_CURRENT_DATA
            }))
        {
            return;
        }
        if let Some(route) = &mut play.live_route {
            route.awaiting_picture = play.item.kind == Kind::Live
                && (video.video_width() == 0 || video.video_height() == 0);
            if route.awaiting_picture {
                return;
            }
        }
        let first_play = !play.started;
        if first_play
            && (!visible || video.seeking())
            && let Some(route) = &mut play.live_route
        {
            route.startup = None;
        }
        play.started = true;
        play.completed = false;
        if first_play
            && visible
            && !video.seeking()
            && let Some(route) = &mut play.live_route
        {
            let startup = route.startup.take();
            let startup_ms = startup.and_then(StartupClock::elapsed_ms);
            let estimated = matches!(startup, Some(StartupClock::Focused { .. }));
            let now = io::now();
            if let Some(choice) = &route.choice {
                if estimated && let Some(ms) = startup_ms {
                    live_routes.success_estimated(&route.key, choice.format, now, ms);
                } else {
                    live_routes.success(&route.key, choice.format, now, startup_ms);
                }
                io::save_private(LIVE_ROUTES_KEY, live_routes);
            }
            if play.item.kind == Kind::Live
                && let Some(startup_ms) = startup_ms
            {
                let sample = FeedSample {
                    format: route.choice.as_ref().map(|choice| choice.format),
                    epg_id: &route.epg_id,
                    startup_ms,
                    width: video.video_width(),
                    height: video.video_height(),
                };
                let changed = if estimated {
                    live_feeds.record_estimated(&route.source, &route.physical_id, now, sample)
                } else {
                    live_feeds.record(&route.source, &route.physical_id, now, sample)
                };
                if changed {
                    io::save_private(LIVE_FEEDS_KEY, live_feeds);
                    live_preview.feed_changed(shared, &route.epg_id);
                }
            }
        }
        play.item.kind == Kind::Episode
    };
    if save_episode {
        save_position(shared, true, false);
    }
    let mut state = shared.borrow_mut();
    state.hide_live_loading();
    live::record_playing(&mut state);
    focus_live_video(&state);
    if state.playback.as_ref().is_some_and(|play| {
        play.selected_subtitle.is_some() && play.sync_enabled && play.sync.is_none()
    }) {
        configure_sync(shared, &mut state);
    }
    if state.playback.is_some()
        && !state
            .playback
            .as_ref()
            .is_some_and(|playback| playback.panel)
    {
        state.player_note("");
    }
    restore_tracks(&state);
    update_timeline(&state);
}

fn release_preloaded_media(state: &App) {
    let video = state.video();
    let _ = video.remove_attribute("src");
    video.set_preload("metadata");
    video.load();
}

enum FeedSelection {
    Explore,
    Preferred,
    Measured(Option<Format>),
}

fn stream_urls(state: &App, item: &Item) -> Option<StreamUrls> {
    let catalog = state
        .model
        .get(&Kind::Live)
        .or_else(|| state.catalog_previews.get(&Kind::Live));
    if item.kind == Kind::Live
        && !item.epg_id.trim().is_empty()
        && let Some(catalog) = catalog
        && let Some((id, format, startup_ms)) =
            state
                .live_feeds
                .best_feed(&state.source_key, &item.id, &item.epg_id, io::now(), |id| {
                    catalog.equivalent_live_feed(item, id).is_some()
                })
        && let Some(donor) = catalog.equivalent_live_feed(item, id)
        && let Some(mut streams) = feed_urls(state, donor, FeedSelection::Measured(format))
        && let Some(route) = &mut streams.live_route
        && let Some(original) = feed_urls(state, item, FeedSelection::Preferred)
    {
        route.fallback = Some(Box::new(original));
        // The TV can still be starting its decoder on the first click from the cached list.
        let minimum_ms = if state.model.contains_key(&Kind::Live) {
            2000
        } else {
            4500
        };
        route.donor_limit_ms = Some(startup_ms.saturating_add(1500).clamp(minimum_ms, 12_000));
        if startup_ms >= FEED_TRIAL_THRESHOLD_MS
            && let Some(mut trial) = feed_urls(state, item, FeedSelection::Explore)
            && let Some(trial_route) = &mut trial.live_route
            && let Some(choice) = &mut trial_route.choice
            && choice.trial_against_feed(startup_ms)
        {
            // Recovery can still use the known original after the donor, without repeating the trial.
            if let Some(original) = &mut route.fallback {
                original.alternate = None;
            }
            trial_route.fallback = Some(Box::new(streams));
            return Some(trial);
        }
        return Some(streams);
    }
    feed_urls(state, item, FeedSelection::Explore)
}

fn feed_urls(state: &App, item: &Item, selection: FeedSelection) -> Option<StreamUrls> {
    let source = &state.settings.source;
    let mut primary = item.playback_url(source)?;
    let mut alternate = item.alternate_live_url(source, &primary);
    let key = state.library_key(item);
    let choice = if let Some(other) = &mut alternate {
        let advertised = if primary.path().ends_with(".ts") {
            Format::Ts
        } else {
            Format::Hls
        };
        let choice = match selection {
            FeedSelection::Explore => state.live_routes.select(&key, advertised, io::now()),
            FeedSelection::Preferred => state.live_routes.preferred(&key, advertised, io::now()),
            FeedSelection::Measured(Some(format)) => Choice {
                format,
                proven: true,
                trial_limit_ms: None,
            },
            FeedSelection::Measured(None) => return None,
        };
        if choice.format != advertised {
            std::mem::swap(&mut primary, other);
        }
        Some(choice)
    } else if matches!(selection, FeedSelection::Measured(Some(_))) {
        return None;
    } else {
        None
    };
    let live_route = item.kind.is_live_stream().then(|| LiveRoute {
        key,
        choice,
        source: state.source_key.clone(),
        physical_id: item.id.clone(),
        epg_id: item.epg_id.clone(),
        url: primary.clone(),
        startup: None,
        awaiting_picture: false,
        fallback: None,
        previous_urls: Vec::new(),
        donor_limit_ms: None,
    });
    Some(StreamUrls {
        primary,
        alternate,
        live_route,
    })
}

pub fn start(app: &Shared, item: Item, with_subtitles: bool) {
    let streams = stream_urls(&app.borrow(), &item);
    let Some(streams) = streams else {
        app.borrow()
            .note(tr("Deze titel heeft geen geldige afspeellink."));
        return;
    };
    start_media(app, item, streams, with_subtitles, false);
}

pub fn start_archive(
    app: &Shared,
    mut item: Item,
    title: &str,
    urls: (url::Url, Option<url::Url>),
) {
    item.kind = Kind::Movie;
    item.title = title.to_owned();
    let (primary, alternate) = urls;
    start_media(
        app,
        item,
        StreamUrls {
            primary,
            alternate,
            live_route: None,
        },
        false,
        true,
    );
}

fn start_media(app: &Shared, item: Item, streams: StreamUrls, with_subtitles: bool, archive: bool) {
    let StreamUrls {
        primary,
        alternate,
        live_route,
    } = streams;
    let live_item = item.kind == Kind::Live;
    let radio_item = item.kind == Kind::Radio;
    let can_retry = alternate.is_some()
        || live_route
            .as_ref()
            .is_some_and(|route| route.fallback.is_some());
    let (subtitle_choice, legacy_subtitle_key, version) = {
        let mut state = app.borrow_mut();
        let video = state.video();
        let selected_at = media_clock_ms();
        let preload = state
            .live_preload
            .take()
            .filter(|preload| {
                live_item && preload.revision == state.revision && preload.key == item.key()
            })
            .and_then(|preload| preload.stream)
            .filter(|stream| {
                stream.url == primary && video.src() == primary.as_str() && video.error().is_none()
            });
        if let Some(route) = &live_route
            && let Some(choice) = &route.choice
            && choice.trial_limit_ms.is_some()
        {
            state
                .live_routes
                .begin_trial(&route.key, choice.format, io::now());
        }
        let archive_guide = archive.then(|| state.live_guide.archive_return());
        live::clear(&mut state);
        let series_detail = state.detail.as_ref().filter(|detail| {
            item.kind == Kind::Episode
                && detail.item.kind == Kind::Series
                && detail.item.id == item.series_id
        });
        let series = series_detail.map(|detail| detail.item.clone());
        let previous_episode_key =
            series_detail.and_then(|detail| state.previous_episode_key(detail, &item));
        let resume_at = if archive {
            0.
        } else {
            series_detail.map_or_else(
                || state.position(&item),
                |detail| state.episode_position(detail, &item),
            )
        };
        let subtitle_key = if archive {
            None
        } else {
            subtitle_key(&state, &item)
        };
        let saved_subtitle = subtitle_key.as_deref().and_then(|key| {
            let id_key = state.library_key(&item);
            [
                Some(key),
                Some(id_key.as_str()),
                previous_episode_key.as_deref(),
            ]
            .into_iter()
            .flatten()
            .find_map(|candidate| {
                let choice = state.library.subtitle_choices.get(candidate)?;
                Some((
                    choice.clone(),
                    (candidate != key).then(|| candidate.to_owned()),
                ))
            })
        });
        let (subtitle_choice, legacy_subtitle_key) = saved_subtitle
            .map(|(choice, key)| (Some(choice), key))
            .unwrap_or_default();
        let version = state.next_sequence();
        let show_tracks = !item.kind.is_live_stream() && !archive;
        state.playback = Some(Playback {
            item,
            version,
            alternate_url: alternate,
            early_retry_url: None,
            live_route,
            stream_attempt: 0,
            stream_failed: false,
            stream_health: (live_item || radio_item).then(StreamHealth::default),
            archive,
            archive_guide,
            last_saved: resume_at,
            resume_at,
            previous_episode_key,
            series,
            started: false,
            completed: false,
            selected_subtitle: None,
            tracks_url: None,
            subtitle_timing: Timing::default(),
            subtitle_key,
            subtitle_choice: subtitle_choice.clone(),
            subtitle_load: 0,
            subtitle_results: Vec::new(),
            subtitle_search: 0,
            panel: false,
            controls_until: io::now() + 6,
            sync_enabled: true,
            sync: None,
            sync_notice: String::new(),
            awaiting_cross_origin: show_tracks,
        });
        if legacy_subtitle_key.is_some()
            && let Some(choice @ (SavedSubtitle::Off | SavedSubtitle::Embedded(_))) =
                subtitle_choice.clone()
        {
            remember_subtitle(&mut state, choice);
        }
        state
            .element("app")
            .unchecked_into::<HtmlElement>()
            .set_hidden(true);
        state
            .element("player")
            .unchecked_into::<HtmlElement>()
            .set_hidden(false);
        state
            .element("player-title")
            .set_text_content(state.playback.as_ref().map(|play| play.item.title.as_str()));
        let player = state.element("player");
        if live_item {
            let _ = player.set_attribute("data-live", "");
        } else {
            let _ = player.remove_attribute("data-live");
        }
        if radio_item {
            let _ = player.set_attribute("data-radio", "");
        } else {
            let _ = player.remove_attribute("data-radio");
        }
        let _ = player.remove_attribute("data-radio-playing");
        let _ = player.set_attribute(
            "aria-label",
            if radio_item {
                tr("Radiospeler")
            } else {
                tr("Videospeler")
            },
        );
        state
            .element("quick-guide-button")
            .unchecked_into::<HtmlElement>()
            .set_hidden(!live_item);
        state.player_note("");
        state.hide_live_loading();
        let _ = state
            .element("player")
            .set_attribute("data-controls", if live_item { "false" } else { "true" });
        if let Some(preload) = preload {
            if !state.document.hidden()
                && let Some(route) = state
                    .playback
                    .as_mut()
                    .and_then(|play| play.live_route.as_mut())
            {
                route.startup = preload.startup(&video, selected_at);
            }
        } else {
            if live_item || radio_item {
                let _ = video.pause();
            }
            video.set_preload("metadata");
            video.set_cross_origin(show_tracks.then_some("anonymous"));
            if let Some(route) = state
                .playback
                .as_mut()
                .and_then(|play| play.live_route.as_mut())
            {
                route.startup = Some(StartupClock::cold());
            }
            video.set_src(primary.as_str());
        }
        set_seek_available(&state, false);
        (subtitle_choice, legacy_subtitle_key, version)
    };
    super::load_visible_live_programmes(app);
    request_play(app);
    {
        let state = app.borrow();
        live::update_last_channel_action(&state);
        live::render_now(&state);
    }
    if can_retry || live_item || radio_item {
        watch_stream_start(app, version, 0, StreamSwitch::Deadline);
        if can_retry && (live_item || radio_item) {
            watch_stream_start(app, version, 0, StreamSwitch::NoMetadata);
        }
    }
    show_panel(app, with_subtitles);
    app.borrow().note_tuning();
    if let Some(SavedSubtitle::External { subtitle, timing }) = subtitle_choice {
        load_subtitles(
            app,
            vec![subtitle],
            SubtitleSelection::Restore {
                timing,
                legacy_key: legacy_subtitle_key,
            },
        );
    }
    if !with_subtitles {
        let state = app.borrow();
        focus(
            &state.document,
            &state.element(if live_item { "player" } else { "player-play" }),
        );
    }
}

fn controls_hidden(state: &App) -> bool {
    state
        .element("player")
        .get_attribute("data-controls")
        .as_deref()
        == Some("false")
}

fn focus_live_video(state: &App) {
    if !state.document.hidden()
        && controls_hidden(state)
        && !state.live_guide.is_open()
        && state
            .playback
            .as_ref()
            .is_some_and(|play| play.item.kind == Kind::Live && !play.panel)
    {
        focus(&state.document, &state.element("player"));
    }
}

pub(super) fn hide_controls(state: &App) {
    let player = state.element("player");
    let _ = player.set_attribute("data-controls", "false");
    if !state.document.hidden() {
        focus(&state.document, &player);
    }
}

pub fn dismiss_controls(app: &Shared) -> bool {
    let state = app.borrow();
    if !state.live_guide.is_open()
        && !controls_hidden(&state)
        && state.playback.as_ref().is_some_and(|play| !play.panel)
    {
        hide_controls(&state);
        return true;
    }
    false
}

pub fn handle_controls_key(app: &Shared, key: &str) -> bool {
    if !matches!(
        key,
        "Enter" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight"
    ) {
        return false;
    }
    {
        let state = app.borrow();
        if key != "Enter"
            && controls_hidden(&state)
            && state
                .playback
                .as_ref()
                .is_some_and(|play| play.item.kind == Kind::Live)
        {
            return false;
        }
    }
    show_controls(app)
}

pub fn show_controls(app: &Shared) -> bool {
    let mut state = app.borrow_mut();
    if state.live_guide.is_open() {
        return false;
    }
    let Some(play) = &mut state.playback else {
        return false;
    };
    play.controls_until = io::now() + 6;
    let player = state.element("player");
    let was_hidden = player.get_attribute("data-controls").as_deref() == Some("false");
    if was_hidden {
        let _ = player.set_attribute("data-controls", "true");
        update_timeline(&state);
        live::render_now(&state);
        focus(
            &state.document,
            &state.element(if player.has_attribute("data-live") {
                "quick-guide-button"
            } else {
                "player-play"
            }),
        );
    }
    was_hidden
}

pub fn stop(app: &Shared) {
    save_position(app, true, false);
    let (returning_to_guide, returning_to_shelf, return_item) = {
        let mut state = app.borrow_mut();
        live::clear(&mut state);
        state.hide_live_loading();
        let mut returning_to_guide = false;
        let resume_detail = state
            .detail
            .as_ref()
            .filter(|detail| detail.resume_from_shelf)
            .map(|detail| (detail.item.key(), detail.origin_page));
        let mut return_item = None;
        if let Some(play) = state.playback.take() {
            if state.detail.is_none() {
                return_item = Some(play.item.key());
            }
            if let Some(url) = play.tracks_url {
                let _ = web_sys::Url::revoke_object_url(&url);
            }
            if let Some(guide) = play.archive_guide {
                state.view = super::View::Guide;
                state.live_guide = guide;
                returning_to_guide = true;
            }
        }
        let video = state.video();
        let _ = video.pause();
        let _ = video.remove_attribute("src");
        video.set_preload("metadata");
        video.set_text_content(None);
        video.load();
        state
            .element("subtitle-panel")
            .unchecked_into::<HtmlElement>()
            .set_hidden(true);
        state
            .element("player")
            .unchecked_into::<HtmlElement>()
            .set_hidden(true);
        state
            .element("app")
            .unchecked_into::<HtmlElement>()
            .set_hidden(false);
        if let Some((key, page)) = resume_detail.as_ref() {
            state.detail = None;
            state.page = *page;
            return_item = Some(key.clone());
        }
        if state.detail.is_none() {
            ui::clamp_library_page(&mut state);
        }
        (returning_to_guide, resume_detail.is_some(), return_item)
    };
    super::load_visible_live_programmes(app);
    if returning_to_guide {
        ui::render(app);
        let state = app.borrow();
        if let Ok(Some(button)) = state
            .element("content")
            .query_selector(".guide-detail [data-cmd='live-guide-archive']")
        {
            focus(&state.document, &button);
            return;
        }
    }
    if returning_to_shelf {
        ui::render(app);
    } else {
        ui::render_content(app);
    }
    if !return_item.is_some_and(|key| {
        ui::focus_continue(app, &key)
            || ui::focus_result(app, &key)
            || ui::reveal_library_card(app, &key)
    }) {
        ui::focus_catalog(app);
    }
}

pub fn dispatch(app: &Shared, cmd: &str, value: &str) {
    match cmd {
        "stop" => stop(app),
        "pause" => toggle(app),
        "rewind" => skip(app, -10.),
        "forward" => skip(app, 30.),
        "subs" => show_panel(app, true),
        "subs-close" => show_panel(app, false),
        "subs-search" => search(app),
        "subs-best" => choose_best_subtitle(app),
        "subtitle-size" => set_subtitle_size(&mut app.borrow_mut(), value),
        "subs-pick" => {
            if let Ok(index) = value.parse::<usize>() {
                pick(app, index);
            }
        }
        "sidecar" => {
            if let Ok(index) = value.parse::<usize>() {
                let subtitle = app
                    .borrow()
                    .playback
                    .as_ref()
                    .and_then(|play| play.item.sidecars.get(index))
                    .and_then(sidecar_subtitle);
                if let Some(subtitle) = subtitle {
                    load_subtitles(app, vec![subtitle], SubtitleSelection::Manual);
                }
            }
        }
        "track" => {
            if let Ok(index) = value.parse::<u32>() {
                choose_embedded_track(&mut app.borrow_mut(), index);
            }
        }
        "track-off" => {
            let mut state = app.borrow_mut();
            clear_external(&mut state);
            remember_subtitle(&mut state, SavedSubtitle::Off);
            restore_tracks(&state);
            state.player_note(tr("Ondertitels uit"));
        }
        "sync-toggle" => {
            let mut state = app.borrow_mut();
            if let Some(play) = &mut state.playback {
                play.sync_enabled = !play.sync_enabled;
            }
            configure_sync(app, &mut state);
            remember_external(&mut state);
            refresh_tracks(&state);
        }
        "offset" => {
            if let Ok(delta) = value.parse::<f64>() {
                shift(app, delta);
            }
        }
        "audio" => {
            if let Ok(value) = value.parse::<u32>() {
                select_audio(&app.borrow(), value);
            }
        }
        _ => {}
    }
}

fn toggle(app: &Shared) {
    let retry = app
        .borrow()
        .playback
        .as_ref()
        .filter(|play| play.stream_failed)
        .map(|play| play.item.clone());
    if let Some(item) = retry {
        start(app, item, false);
        return;
    }
    let video = app.borrow().video();
    if video.paused() {
        let pending = {
            let mut state = app.borrow_mut();
            let Some(play) = &mut state.playback else {
                return;
            };
            play.stream_health.as_mut().and_then(|health| {
                health.user_paused = false;
                if video.error().is_some() {
                    play.started = false;
                    video.load();
                }
                (!play.started).then_some((
                    play.version,
                    play.stream_attempt,
                    play.alternate_url.is_some()
                        || play
                            .live_route
                            .as_ref()
                            .is_some_and(|route| route.fallback.is_some()),
                ))
            })
        };
        request_play(app);
        if let Some((version, attempt, has_alternate)) = pending {
            watch_stream_start(app, version, attempt, StreamSwitch::Deadline);
            if has_alternate {
                watch_stream_start(app, version, attempt, StreamSwitch::NoMetadata);
            }
        }
    } else {
        if let Some(play) = &mut app.borrow_mut().playback
            && let Some(health) = &mut play.stream_health
        {
            health.user_paused = true;
            health.observation = None;
            if let Some(route) = &mut play.live_route {
                route.startup = None;
            }
            play.stream_attempt += 1;
        }
        let _ = video.pause();
    }
}

fn request_play(app: &Shared) {
    let (video, version, attempt) = {
        let state = app.borrow();
        let Some(play) = state.playback.as_ref().filter(|play| !play.stream_failed) else {
            return;
        };
        (state.video(), play.version, play.stream_attempt)
    };
    let cross_origin = video.cross_origin();
    let Ok(promise) = video.play() else {
        return;
    };
    let app = app.clone();
    spawn_local(async move {
        let Err(error) = JsFuture::from(promise).await else {
            return;
        };
        let name = Reflect::get(&error, &JsValue::from_str("name"))
            .ok()
            .and_then(|name| name.as_string());
        if name.as_deref() == Some("AbortError") {
            return;
        }
        let state = app.borrow();
        if state.playback.as_ref().is_some_and(|play| {
            play.version == version && play.stream_attempt == attempt && !play.stream_failed
        }) && state.video().cross_origin() == cross_origin
        {
            state.player_note(tr(
                "Afspelen kon niet starten. Controleer de stream en probeer opnieuw.",
            ));
        }
    });
}

fn check_stream_health(app: &Shared) {
    let stalled_version = {
        let mut state = app.borrow_mut();
        if state
            .playback
            .as_ref()
            .is_none_or(|play| play.stream_health.is_none())
        {
            return;
        }
        let video = state.video();
        let visible = !state.document.hidden();
        let Some(play) = state
            .playback
            .as_mut()
            .filter(|play| play.started && !play.stream_failed)
        else {
            return;
        };
        let Some(health) = &mut play.stream_health else {
            return;
        };
        let position = video.current_time();
        if !visible
            || health.user_paused
            || video.paused()
            || video.seeking()
            || !position.is_finite()
        {
            health.observation = None;
            return;
        }
        health.stalled(position, io::now()).then_some(play.version)
    };
    if let Some(version) = stalled_version {
        reconnect_stream(app, version);
    }
}

fn reconnect_stream(app: &Shared, version: u64) {
    let attempt = {
        let mut state = app.borrow_mut();
        let video = state.video();
        let Some(play) = state
            .playback
            .as_mut()
            .filter(|play| play.version == version && play.started && !play.stream_failed)
        else {
            return;
        };
        let Some(health) = &mut play.stream_health else {
            return;
        };
        if health.user_paused {
            return;
        }
        let has_fallback_feed = play
            .live_route
            .as_ref()
            .is_some_and(|route| route.fallback.is_some());
        if health.retried || has_fallback_feed {
            play.started = false;
            drop(state);
            if !try_alternate_stream(app, version, StreamSwitch::Error) {
                fail_stream(app, version);
            }
            return;
        }
        health.retried = true;
        health.observation = None;
        if let Some(route) = &mut play.live_route {
            route.startup = None;
            route.awaiting_picture = false;
        }
        play.started = false;
        play.stream_attempt += 1;
        let attempt = play.stream_attempt;
        if play.item.kind == Kind::Live {
            state.show_live_loading(tr(STREAM_RECONNECTING));
        } else {
            state.player_note(tr(STREAM_RECONNECTING));
        }
        video.load();
        attempt
    };
    request_play(app);
    watch_stream_start(app, version, attempt, StreamSwitch::Deadline);
}

fn fail_stream(app: &Shared, version: u64) {
    let mut state = app.borrow_mut();
    let Some(play) = state
        .playback
        .as_mut()
        .filter(|play| play.version == version && !play.stream_failed)
    else {
        return;
    };
    if play.stream_health.is_some() {
        play.stream_failed = true;
        let video = state.video();
        let _ = video.pause();
        let _ = video.remove_attribute("src");
        video.load();
    }
    state.hide_live_loading();
    state.player_note(tr(STREAM_FAILED));
}

fn try_alternate_stream(app: &Shared, version: u64, reason: StreamSwitch) -> bool {
    let (attempt, has_alternate) = {
        let mut state = app.borrow_mut();
        let video = state.video();
        let visible = !state.document.hidden();
        let App {
            playback,
            live_routes,
            live_feeds,
            live_preview,
            ..
        } = &mut *state;
        let Some(play) = playback
            .as_mut()
            .filter(|play| play.version == version && !play.started && !play.stream_failed)
        else {
            return false;
        };
        if visible && let Some(route) = &play.live_route {
            let now = io::now();
            if let Some(choice) = &route.choice {
                live_routes.failure(&route.key, choice.format, now);
                io::save_private(LIVE_ROUTES_KEY, live_routes);
            }
            if live_feeds.failure(
                &route.source,
                &route.physical_id,
                now,
                route.choice.as_ref().map(|choice| choice.format),
            ) {
                io::save_private(LIVE_FEEDS_KEY, live_feeds);
                live_preview.feed_changed(app, &route.epg_id);
            }
        }
        let mut previous_urls = Vec::new();
        let fallback = play.live_route.as_mut().and_then(|route| {
            previous_urls = std::mem::take(&mut route.previous_urls);
            previous_urls.push(route.url.clone());
            route.fallback.take()
        });
        let (url, has_alternate) = if let Some(fallback) = fallback {
            play.live_route = fallback.live_route;
            play.alternate_url = fallback.alternate;
            play.early_retry_url = None;
            let can_retry = play.alternate_url.is_some()
                || play
                    .live_route
                    .as_ref()
                    .is_some_and(|route| route.fallback.is_some());
            (fallback.primary, can_retry)
        } else {
            let next = play.alternate_url.take().or_else(|| {
                let slow_url = play.early_retry_url.take();
                slow_url.filter(|_| reason == StreamSwitch::Error)
            });
            let Some(url) = next else {
                return false;
            };
            // Give a slow primary one more chance if the early alternate fails outright.
            if reason == StreamSwitch::NoMetadata {
                play.early_retry_url = http_url(&video.src());
            }
            if let Some(choice) = play
                .live_route
                .as_mut()
                .and_then(|route| route.choice.as_mut())
            {
                choice.format = choice.format.other();
                choice.trial_limit_ms = None;
            }
            (url, false)
        };
        if let Some(route) = &mut play.live_route {
            route.url = url.clone();
            route.startup = (!previous_urls.contains(&url)).then(StartupClock::cold);
            route.previous_urls = previous_urls;
            route.awaiting_picture = false;
        }
        play.stream_attempt += 1;
        if let Some(health) = &mut play.stream_health {
            health.retried = false;
            health.observation = None;
        }
        let attempt = play.stream_attempt;
        state.note_tuning();
        video.set_src(url.as_str());
        (attempt, has_alternate)
    };
    request_play(app);
    watch_stream_start(app, version, attempt, StreamSwitch::Deadline);
    if has_alternate {
        watch_stream_start(app, version, attempt, StreamSwitch::NoMetadata);
    }
    true
}

fn watch_stream_start(app: &Shared, version: u64, attempt: u64, reason: StreamSwitch) {
    let early = reason == StreamSwitch::NoMetadata;
    let (timeout, trial) = if early {
        let state = app.borrow();
        match state
            .playback
            .as_ref()
            .and_then(|play| play.live_route.as_ref())
        {
            Some(route) if let Some(limit) = route.trial_limit_ms() => (limit as i32, true),
            Some(route) if route.choice.as_ref().is_some_and(|choice| choice.proven) => {
                (KNOWN_LIVE_ROUTE_TIMEOUT_MS, false)
            }
            _ => (LIVE_NO_METADATA_TIMEOUT_MS, false),
        }
    } else {
        (STREAM_START_TIMEOUT_MS, false)
    };
    let app = app.clone();
    spawn_local(async move {
        io::delay(timeout).await;
        {
            let state = app.borrow();
            if !state.playback.as_ref().is_some_and(|play| {
                play.version == version
                    && play.stream_attempt == attempt
                    && !play.started
                    && !play.stream_failed
            }) || (early
                && !trial
                && state.video().ready_state() != HtmlMediaElement::HAVE_NOTHING)
            {
                return;
            }
        }
        if !try_alternate_stream(&app, version, reason) && !early {
            fail_stream(&app, version);
        }
    });
}

fn skip(app: &Shared, delta: f64) {
    let state = app.borrow();
    seek_to(&state, state.video().current_time() + delta);
}

pub fn seek_key(app: &Shared, key: &str) -> bool {
    match key {
        "ArrowLeft" => skip(app, -10.),
        "ArrowRight" => skip(app, 10.),
        _ => return false,
    }
    true
}

fn seek_to(state: &App, time: f64) {
    if time.is_finite()
        && let Some((start, end)) = seek_bounds(state)
    {
        state.video().set_current_time(time.clamp(start, end));
        update_timeline(state);
    }
}

fn seek_bounds(state: &App) -> Option<(f64, f64)> {
    if state
        .playback
        .as_ref()
        .is_none_or(|play| play.item.kind.is_live_stream())
    {
        return None;
    }
    let ranges = state.video().seekable();
    let last = ranges.length().checked_sub(1)?;
    let start = ranges.start(0).ok()?;
    let end = ranges.end(last).ok()?;
    (start.is_finite() && end.is_finite() && end > start).then_some((start, end))
}

fn set_seek_available(state: &App, available: bool) {
    state
        .element("player-timeline")
        .unchecked_into::<HtmlElement>()
        .set_hidden(!available);
    for id in ["player-rewind", "player-forward"] {
        state
            .element(id)
            .unchecked_into::<web_sys::HtmlButtonElement>()
            .set_disabled(!available);
    }
}

fn update_timeline(state: &App) {
    if state
        .playback
        .as_ref()
        .is_none_or(|play| play.item.kind.is_live_stream())
        || controls_hidden(state)
    {
        return;
    }
    let bounds = seek_bounds(state);
    set_seek_available(state, bounds.is_some());
    let Some((start, end)) = bounds else {
        return;
    };
    let current = state.video().current_time().clamp(start, end);
    let seek: HtmlInputElement = state.element("player-seek").unchecked_into();
    seek.set_min(&start.to_string());
    seek.set_max(&end.to_string());
    seek.set_value_as_number(current);
    let elapsed = ui::playback_time(current - start);
    let total = ui::playback_time(end - start);
    state
        .element("player-elapsed")
        .set_text_content(Some(&elapsed));
    state
        .element("player-duration")
        .set_text_content(Some(&total));
    let _ = seek.set_attribute("aria-valuetext", &format!("{elapsed} / {total}"));
}

fn save_position(app: &Shared, force: bool, completed: bool) {
    let mut state = app.borrow_mut();
    let Some(play) = state.playback.as_ref().filter(|play| {
        !play.archive && !play.completed && matches!(play.item.kind, Kind::Movie | Kind::Episode)
    }) else {
        return;
    };
    let episode = play.item.kind == Kind::Episode;
    if episode && !play.started {
        return;
    }
    let video = state.video();
    let current = video.current_time();
    let completed = completed || video.ended();
    let save_interval = if episode { 5. } else { 15. };
    if (episode && !completed && video.seeking())
        || (!force && !completed && (current - play.last_saved).abs() < save_interval)
        || (!current.is_finite() && !completed)
    {
        return;
    }
    let duration = video.duration();
    let duration = if duration.is_finite() && duration > 0. {
        duration
    } else {
        0.
    };
    let finished_for_library = completed
        || (!episode
            && duration > 0.
            && !video.seeking()
            && current >= duration * 0.9
            && duration - current < 15.);
    let Some(play) = &mut state.playback else {
        return;
    };
    let item = play.item.clone();
    let series = play.series.clone();
    let previous_episode_key = play.previous_episode_key.take();
    play.last_saved = current;
    play.completed = completed;
    let key = state.library_key(&item);
    if finished_for_library && !item.id.is_empty() {
        state.library.watched.insert(key.clone());
    }
    if let Some(series) = series {
        if let Some(old_key) = previous_episode_key {
            state.library.positions.remove(&old_key);
            state.library.recent.remove(&old_key);
        }
        let series_key = state.library_key(&series);
        let App {
            library,
            detail,
            source_key,
            ..
        } = &mut *state;
        if !library.series.contains_key(&series_key)
            && let Some(detail) = detail
        {
            for old_episode in &detail.episodes {
                library
                    .recent
                    .remove(&format!("{source_key}:{}", old_episode.key()));
            }
        }
        library.recent.remove(&key);
        if completed || current < 1. {
            library.positions.remove(&key);
        } else {
            library.positions.insert(key, current);
        }
        let progress = SeriesProgress {
            series,
            episode_id: item.id,
            season: item.season,
            episode: item.episode,
            finished: completed,
        };
        let has_next = detail
            .as_ref()
            .filter(|_| completed)
            .map(|detail| progress.target(&detail.episodes).is_some());
        library.watching.insert(
            series_key.clone(),
            super::WatchDetails {
                updated: io::now(),
                duration,
                has_next,
            },
        );
        library.series.insert(series_key, progress);
    } else if finished_for_library {
        state.library.positions.remove(&key);
        state.library.recent.remove(&key);
        state.library.watching.remove(&key);
    } else if current >= 5. && current.is_finite() {
        state.library.positions.insert(key.clone(), current);
        state.library.watching.insert(
            key.clone(),
            super::WatchDetails {
                updated: io::now(),
                duration,
                has_next: None,
            },
        );
        state.library.recent.insert(key, item);
    } else {
        return;
    }
    io::save_private(LIBRARY_KEY, &state.library);
}

impl App {
    fn note_tuning(&self) {
        let Some(play) = &self.playback else { return };
        match play.item.kind {
            Kind::Live if !play.started && !play.stream_failed => {
                let index = play.version as usize % LIVE_LOADING_MESSAGES.len();
                self.show_live_loading(tr(LIVE_LOADING_MESSAGES[index]));
            }
            Kind::Radio => self.player_note(tr(RADIO_LOADING)),
            _ => {}
        }
    }

    fn show_live_loading(&self, message: &str) {
        self.player_note("");
        self.element("stream-loading-title")
            .set_text_content(self.playback.as_ref().map(|play| play.item.title.as_str()));
        self.element("stream-loading-message")
            .set_text_content(Some(message));
        self.element("stream-loading")
            .unchecked_into::<HtmlElement>()
            .set_hidden(false);
    }

    fn hide_live_loading(&self) {
        self.element("stream-loading")
            .unchecked_into::<HtmlElement>()
            .set_hidden(true);
    }

    pub(super) fn player_note(&self, message: &str) {
        let player = self.element("player");
        if message.is_empty() {
            let _ = player.remove_attribute("data-notice");
        } else {
            let _ = player.set_attribute("data-notice", "");
        }
        self.element("player-notice")
            .set_text_content(Some(message));
        if self.playback.as_ref().is_some_and(|play| play.panel) {
            self.element("subtitle-notice")
                .set_text_content(Some(message));
        }
    }
}

pub fn show_panel(app: &Shared, visible: bool) {
    let mut state = app.borrow_mut();
    let player = state.element("player");
    let Some(play) = &mut state.playback else {
        return;
    };
    let was_visible = play.panel;
    play.panel = visible;
    if !visible && play.subtitle_load == play.subtitle_search {
        play.subtitle_load = 0;
    }
    play.controls_until = io::now() + 6;
    if visible {
        let _ = player.set_attribute("data-subtitles-open", "");
    } else {
        let _ = player.remove_attribute("data-subtitles-open");
    }
    let default_query = subtitle_title(if play.item.kind == Kind::Episode {
        &play.item.series_title
    } else {
        &play.item.title
    });
    if visible || was_visible {
        let _ = player.set_attribute("data-controls", "true");
    }
    state
        .element("subtitle-panel")
        .unchecked_into::<HtmlElement>()
        .set_hidden(!visible);
    state
        .element("player-actions")
        .unchecked_into::<HtmlElement>()
        .set_hidden(visible);
    if visible {
        if !was_visible {
            state.element("subtitle-body").set_scroll_top(0);
        }
        let input = state
            .element("subtitle-query")
            .unchecked_into::<HtmlInputElement>();
        input.set_value(&default_query);
        let _ = input.set_attribute("data-original", &default_query);
        refresh_tracks(&state);
        render_results(&state);
        state.player_note("");
        let first = state
            .element("subtitle-tracks")
            .query_selector("button[aria-pressed=true]:not([data-cmd=track-off])")
            .ok()
            .flatten()
            .unwrap_or_else(|| state.element("subtitle-close"));
        focus(&state.document, &first);
    } else {
        state.player_note("");
        if was_visible {
            focus(&state.document, &state.element("player-subs"));
        }
    }
}

fn set_subtitle_size(state: &mut App, value: &str) {
    let Some(size) = [
        SubtitleSize::Original,
        SubtitleSize::Standard,
        SubtitleSize::Large,
    ]
    .into_iter()
    .find(|size| size.value() == value) else {
        return;
    };
    if size == state.settings.subtitle_size {
        return;
    }
    let mut settings = state.settings.clone();
    settings.subtitle_size = size;
    if io::save_private(CONFIG_KEY, &settings) {
        state.settings = settings;
        render_subtitle_size(state);
    } else {
        state.player_note(tr("Instellingen opslaan mislukt op dit apparaat."));
    }
}

fn render_subtitle_size(state: &App) {
    let value = state.settings.subtitle_size.value();
    let _ = state
        .element("video")
        .set_attribute("data-subtitle-size", value);
    if let Ok(buttons) = state.element("subtitle-size").query_selector_all("button") {
        for index in 0..buttons.length() {
            if let Some(button) = buttons
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            {
                let selected = button.get_attribute("data-value").as_deref() == Some(value);
                let _ =
                    button.set_attribute("aria-pressed", if selected { "true" } else { "false" });
            }
        }
    }
}

fn audio_tracks(state: &App) -> Option<(JsValue, u32)> {
    let list = Reflect::get(&state.video(), &JsValue::from_str("audioTracks")).ok()?;
    if list.is_null() || list.is_undefined() {
        return None;
    }
    let length = Reflect::get(&list, &JsValue::from_str("length"))
        .ok()?
        .as_f64()? as u32;
    (length > 1).then_some((list, length))
}

fn select_audio(state: &App, wanted: u32) {
    let Some((tracks, count)) = audio_tracks(state) else {
        return;
    };
    for index in 0..count {
        if let Ok(track) = Reflect::get(&tracks, &JsValue::from_f64(index as f64)) {
            let _ = Reflect::set(
                &track,
                &JsValue::from_str("enabled"),
                &JsValue::from_bool(index == wanted),
            );
        }
    }
    state.player_note(tr("Audiospoor gewijzigd"));
}

impl EmbeddedTrack {
    fn from_track(track: &TextTrack) -> Option<Self> {
        if !matches!(
            track.kind(),
            TextTrackKind::Subtitles | TextTrackKind::Captions
        ) {
            return None;
        }
        let label = track.label();
        let tag = track.language();
        let language = match language(tag.split('-').next().unwrap_or(&tag)) {
            detected @ (Language::Dutch | Language::English) => detected,
            _ => language(label.split_whitespace().next().unwrap_or("")),
        };
        matches!(language, Language::Dutch | Language::English).then(|| Self {
            id: track.id(),
            cc: track.kind() == TextTrackKind::Captions
                || label.to_ascii_lowercase().contains(" cc"),
            label,
            language,
        })
    }
}

fn subtitle_key(state: &App, item: &Item) -> Option<String> {
    if !matches!(item.kind, Kind::Movie | Kind::Episode) || item.id.is_empty() {
        return None;
    }
    let id_key = state.library_key(item);
    if item.kind == Kind::Episode
        && item.episode != 0
        && let Some(detail) = &state.detail
        && detail.item.kind == Kind::Series
        && detail.item.id == item.series_id
        && detail
            .episodes
            .iter()
            .filter(|other| (other.season, other.episode) == (item.season, item.episode))
            .take(2)
            .count()
            == 1
    {
        return Some(format!(
            "{}:Series:{}:S{}E{}",
            state.source_key, item.series_id, item.season, item.episode
        ));
    }
    Some(id_key)
}

fn remember_subtitle(state: &mut App, choice: SavedSubtitle) {
    let Some(play) = &mut state.playback else {
        return;
    };
    if play.subtitle_choice.as_ref() != Some(&choice) {
        play.subtitle_choice = Some(choice.clone());
    }
    let Some(key) = &play.subtitle_key else {
        return;
    };
    let mut timing_changed = false;
    if let SavedSubtitle::External { subtitle, timing } = &choice {
        let timing_key = subtitle_cache_key(key, &subtitle.download);
        if timing.clock == Timing::default() && timing.auto {
            timing_changed = state.library.subtitle_timings.remove(&timing_key).is_some();
        } else if state.library.subtitle_timings.get(&timing_key) != Some(timing) {
            state.library.subtitle_timings.insert(timing_key, *timing);
            timing_changed = true;
        }
    }
    let choice_changed = state.library.subtitle_choices.get(key) != Some(&choice);
    if choice_changed {
        state.library.subtitle_choices.insert(key.clone(), choice);
    }
    if choice_changed || timing_changed {
        io::save_private(LIBRARY_KEY, &state.library);
    }
}

fn remember_external(state: &mut App) {
    let Some(play) = &state.playback else {
        return;
    };
    let Some((subtitle, _)) = &play.selected_subtitle else {
        return;
    };
    let choice = SavedSubtitle::External {
        subtitle: subtitle.clone(),
        timing: SubtitleTiming {
            clock: play.subtitle_timing,
            auto: play.sync_enabled,
        },
    };
    remember_subtitle(state, choice);
}

fn external_track(state: &App) -> Option<TextTrack> {
    state
        .video()
        .query_selector("track[data-online]")
        .ok()
        .flatten()?
        .dyn_into::<HtmlTrackElement>()
        .ok()?
        .track()
}

fn clear_external(state: &mut App) {
    let Some(play) = &mut state.playback else {
        return;
    };
    play.subtitle_load = 0;
    play.selected_subtitle = None;
    play.subtitle_timing = Timing::default();
    play.sync = None;
    if let Some(url) = play.tracks_url.take() {
        let _ = web_sys::Url::revoke_object_url(&url);
    }
    if let Ok(Some(track)) = state.video().query_selector("track[data-online]") {
        track.remove();
    }
}

fn choose_embedded_track(state: &mut App, index: u32) {
    let Some(track) = state
        .video()
        .text_tracks()
        .and_then(|tracks| tracks.get(index))
    else {
        return;
    };
    if external_track(state).as_ref() == Some(&track) {
        return;
    }
    let Some(choice) = EmbeddedTrack::from_track(&track) else {
        return;
    };
    clear_external(state);
    remember_subtitle(state, SavedSubtitle::Embedded(choice));
    show_only_track(state, Some(&track));
    refresh_tracks(state);
}

fn restore_tracks(state: &App) {
    if let Some(choice) = state
        .playback
        .as_ref()
        .and_then(|play| play.subtitle_choice.as_ref())
        && let Some(tracks) = state.video().text_tracks()
    {
        let selected = match choice {
            SavedSubtitle::Off => None,
            SavedSubtitle::External { .. } => external_track(state),
            SavedSubtitle::Embedded(saved) => {
                let candidates: Vec<_> = (0..tracks.length())
                    .filter_map(|index| {
                        let track = tracks.get(index)?;
                        let info = EmbeddedTrack::from_track(&track)?;
                        (info.language == saved.language && info.cc == saved.cc)
                            .then_some((track, info))
                    })
                    .collect();
                candidates
                    .iter()
                    .find(|(_, info)| info == saved)
                    .or_else(|| {
                        candidates
                            .iter()
                            .find(|(_, info)| !saved.label.is_empty() && info.label == saved.label)
                    })
                    .or_else(|| (candidates.len() == 1).then(|| &candidates[0]))
                    .map(|(track, _)| track.clone())
            }
        };
        show_only_track(state, selected.as_ref());
    }
    refresh_tracks(state);
}

fn show_only_track(state: &App, selected: Option<&TextTrack>) {
    if let Some(tracks) = state.video().text_tracks() {
        for index in 0..tracks.length() {
            if let Some(track) = tracks.get(index) {
                let mode = if selected == Some(&track) {
                    TextTrackMode::Showing
                } else {
                    TextTrackMode::Disabled
                };
                if track.mode() != mode {
                    track.set_mode(mode);
                }
            }
        }
    }
}

fn refresh_tracks(state: &App) {
    if !state.playback.as_ref().is_some_and(|play| play.panel) {
        return;
    }
    let panel = state.element("subtitle-tracks");
    let active = state.document.active_element().and_then(|element| {
        (panel.contains(Some(&element))).then(|| {
            (
                element.get_attribute("data-cmd"),
                element.get_attribute("data-value"),
            )
        })
    });
    panel.set_text_content(None);
    let off = state.button(&panel, tr("Uit"), "track-off", "", "");
    let external = external_track(state);
    let mut has_active_track = false;
    let mut has_dutch_track = false;
    if let Some(tracks) = state.video().text_tracks() {
        for index in 0..tracks.length() {
            let Some(track) = tracks.get(index) else {
                continue;
            };
            let showing = track.mode() == TextTrackMode::Showing;
            has_active_track |= showing
                && matches!(
                    track.kind(),
                    TextTrackKind::Subtitles | TextTrackKind::Captions
                );
            if external.as_ref() == Some(&track) {
                continue;
            }
            let Some(info) = EmbeddedTrack::from_track(&track) else {
                continue;
            };
            let language = if info.language == Language::Dutch {
                has_dutch_track = true;
                tr("Nederlands")
            } else {
                tr("English")
            };
            let cc = if info.cc { " CC" } else { "" };
            let button = state.button(
                &panel,
                &format!("{language}{cc}"),
                "track",
                &index.to_string(),
                "",
            );
            let _ = button.set_attribute("aria-pressed", if showing { "true" } else { "false" });
        }
    }
    let _ = off.set_attribute(
        "aria-pressed",
        if has_active_track { "false" } else { "true" },
    );
    let can_search = subtitle_search_enabled(state);
    state
        .element("subtitle-online")
        .unchecked_into::<HtmlElement>()
        .set_hidden(!(can_search || has_dutch_track));
    for id in [
        "subtitle-online-label",
        "subtitle-query",
        "subtitle-search-button",
    ] {
        state
            .element(id)
            .unchecked_into::<HtmlElement>()
            .set_hidden(!can_search);
    }
    if let Some(play) = &state.playback {
        for (index, subtitle) in play.item.sidecars.iter().enumerate() {
            if let Some(subtitle) = sidecar_subtitle(subtitle) {
                let button = state.button(
                    &panel,
                    &subtitle.label_with(tr),
                    "sidecar",
                    &index.to_string(),
                    "",
                );
                if play
                    .selected_subtitle
                    .as_ref()
                    .is_some_and(|(selected, _)| selected.download == subtitle.download)
                {
                    let _ = button.set_attribute("aria-pressed", "true");
                }
            }
        }
    }
    if let Some((tracks, length)) = audio_tracks(state) {
        for index in 0..length {
            if let Ok(track) = Reflect::get(&tracks, &JsValue::from_f64(index as f64)) {
                let label = Reflect::get(&track, &JsValue::from_str("language"))
                    .ok()
                    .and_then(|value| value.as_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| (index + 1).to_string());
                state.button(
                    &panel,
                    &message("Audio · {label}", &[("label", &label)]),
                    "audio",
                    &index.to_string(),
                    "",
                );
            }
        }
    }
    if let Some(play) = &state.playback
        && let Some((selected, _)) = &play.selected_subtitle
    {
        if !matches!(selected.download, Download::Sidecar(_)) {
            state.node(&panel, "span", "active-subtitle", &selected.label_with(tr));
        }
        let timing = state.node(&panel, "div", "subtitle-timing", "");
        let shift = state.node(&timing, "div", "subtitle-shift", "");
        let _ = shift.set_attribute("data-subtitle-row", "");
        state.button(
            &shift,
            &format!("{} ½ s", tr("Eerder")),
            "offset",
            "-0.5",
            "",
        );
        let now = state.video().current_time();
        state.node(
            &shift,
            "span",
            "subtitle-offset",
            &format!("{:+.1} s", now - play.subtitle_timing.subtitle_time(now)),
        );
        state.button(&shift, &format!("{} ½ s", tr("Later")), "offset", "0.5", "");
        let sync = state.node(&timing, "div", "subtitle-sync", "");
        let auto = state.button(&sync, "Auto-sync", "sync-toggle", "", "");
        let _ = auto.set_attribute(
            "aria-pressed",
            if play.sync_enabled { "true" } else { "false" },
        );
        state.node(&sync, "span", "muted sync-status", &play.sync_notice);
    }
    if let Some((command, value)) = active
        && let Ok(buttons) = panel.query_selector_all("button")
    {
        for index in 0..buttons.length() {
            let Some(element) = buttons
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            else {
                continue;
            };
            if element.get_attribute("data-cmd") == command
                && element.get_attribute("data-value") == value
            {
                focus(&state.document, &element);
                break;
            }
        }
    }
}

pub fn search(app: &Shared) {
    search_subtitles(app, false);
}

fn subtitle_search_enabled(state: &App) -> bool {
    state
        .playback
        .as_ref()
        .is_some_and(|play| !play.item.kind.is_live_stream())
        && (!state.settings.opensubtitles.is_empty() || !state.settings.subdl.is_empty())
}

fn choose_best_subtitle(app: &Shared) {
    let mut state = app.borrow_mut();
    if state
        .playback
        .as_ref()
        .is_none_or(|play| !play.panel || play.item.kind.is_live_stream())
    {
        return;
    }
    let embedded = state.video().text_tracks().and_then(|tracks| {
        let external = external_track(&state);
        (0..tracks.length()).find(|&index| {
            tracks.get(index).is_some_and(|track| {
                external.as_ref() != Some(&track)
                    && EmbeddedTrack::from_track(&track)
                        .is_some_and(|info| info.language == Language::Dutch)
            })
        })
    });
    if let Some(index) = embedded {
        choose_embedded_track(&mut state, index);
        drop(state);
        show_panel(app, false);
    } else if subtitle_search_enabled(&state) {
        drop(state);
        search_subtitles(app, true);
    }
}

fn search_subtitles(app: &Shared, best: bool) {
    let (item, version, request, os, subdl, query) = {
        let mut state = app.borrow_mut();
        let input: HtmlInputElement = state.element("subtitle-query").unchecked_into();
        let value = input.value();
        let query = if input.get_attribute("data-original").as_deref() == Some(value.as_str()) {
            String::new()
        } else {
            value
        };
        let request = state.next_sequence();
        let Some(play) = &mut state.playback else {
            return;
        };
        if !play.panel || play.item.kind.is_live_stream() {
            return;
        }
        if best {
            play.subtitle_load = request;
        } else if play.subtitle_load == play.subtitle_search {
            play.subtitle_load = 0;
        }
        play.subtitle_search = request;
        play.subtitle_results.clear();
        let item = play.item.clone();
        let version = play.version;
        state.player_note(tr("Ondertitels zoeken…"));
        render_results(&state);
        (
            item,
            version,
            request,
            state.settings.opensubtitles.clone(),
            state.settings.subdl.clone(),
            query,
        )
    };
    let app = app.clone();
    spawn_local(async move {
        let results = sources::find_subtitles(&item, &os, &subdl, &query).await;
        let mut state = app.borrow_mut();
        let Some(play) = &mut state.playback else {
            return;
        };
        if play.version != version
            || play.subtitle_search != request
            || (best && play.subtitle_load != request)
        {
            return;
        }
        let mut fallback = None;
        let message = match results {
            Ok(matches) if matches.results.is_empty() => {
                tr("Geen Nederlandse of Engelse ondertitels gevonden.").to_owned()
            }
            Ok(matches) => {
                play.subtitle_results = matches.results;
                if best && !matches.checked_subdl && !subdl.is_empty() {
                    fallback = Some(SubDlFallback {
                        api_key: subdl,
                        query,
                    });
                }
                String::new()
            }
            Err(error) => error,
        };
        let candidates: Vec<_> = if best {
            play.subtitle_results.iter().take(2).cloned().collect()
        } else {
            Vec::new()
        };
        let panel_visible = play.panel;
        render_results(&state);
        if panel_visible {
            state.player_note(&message);
        }
        drop(state);
        if !candidates.is_empty() {
            load_subtitles(
                &app,
                candidates,
                SubtitleSelection::Best { request, fallback },
            );
        }
    });
}

fn render_results(state: &App) {
    let area = state.element("subtitle-results");
    area.set_text_content(None);
    if let Some(play) = &state.playback {
        for (index, row) in play.subtitle_results.iter().enumerate() {
            state.button(
                &area,
                &row.label_with(tr),
                "subs-pick",
                &index.to_string(),
                "subtitle-result",
            );
        }
    }
}

fn pick(app: &Shared, index: usize) {
    let subtitle = app
        .borrow()
        .playback
        .as_ref()
        .and_then(|play| play.subtitle_results.get(index))
        .cloned();
    if let Some(subtitle) = subtitle {
        load_subtitles(app, vec![subtitle], SubtitleSelection::Manual);
    }
}

fn sidecar_subtitle(track: &crate::catalog::Sidecar) -> Option<Subtitle> {
    let url = http_url(&track.url)?;
    let language = language(&track.lang);
    if !matches!(language, Language::Dutch | Language::English) {
        return None;
    }
    Some(Subtitle {
        name: track.name.clone(),
        language,
        cc: track.name.to_ascii_lowercase().contains(" cc"),
        download: Download::Sidecar(url),
        downloads: None,
    })
}

enum SubtitleSelection {
    Manual,
    Restore {
        timing: SubtitleTiming,
        legacy_key: Option<String>,
    },
    Best {
        request: u64,
        fallback: Option<SubDlFallback>,
    },
}

struct SubDlFallback {
    api_key: String,
    query: String,
}

fn subtitle_request_active(state: &App, version: u64, request: u64) -> bool {
    state
        .playback
        .as_ref()
        .is_some_and(|play| play.version == version && play.subtitle_load == request)
}

fn subtitle_cache_key(key: &str, download: &Download) -> String {
    let mut hash = DefaultHasher::new();
    download.hash(&mut hash);
    format!("subtitle:{key}:{:016x}", hash.finish())
}

async fn cached_subtitle(key: &str) -> Option<String> {
    let data = io::cached_raw(key).await?;
    serde_json::from_str::<(f64, String)>(&data)
        .ok()
        .map(|(_, body)| body)
}

fn load_subtitles(app: &Shared, candidates: Vec<Subtitle>, selection: SubtitleSelection) {
    let best = matches!(selection, SubtitleSelection::Best { .. });
    let (restored, legacy_key) = match &selection {
        SubtitleSelection::Restore { timing, legacy_key } => (Some(*timing), legacy_key.clone()),
        _ => (None, None),
    };
    let (version, request, os, title_key) = {
        let mut state = app.borrow_mut();
        let request = match &selection {
            SubtitleSelection::Best { request, .. } => *request,
            _ => state.next_sequence(),
        };
        let Some(play) = &mut state.playback else {
            return;
        };
        if best && play.subtitle_load != request {
            return;
        }
        play.subtitle_load = request;
        let version = play.version;
        let title_key = play.subtitle_key.clone();
        if restored.is_none() {
            state.player_note(tr("Ondertitels downloaden…"));
        }
        (
            version,
            request,
            state.settings.opensubtitles.clone(),
            title_key,
        )
    };
    let mut fallback = match selection {
        SubtitleSelection::Best { fallback, .. } => fallback,
        _ => None,
    };
    let app = app.clone();
    spawn_local(async move {
        let mut error = String::new();
        let mut candidates = candidates.into_iter();
        loop {
            let subtitle = if let Some(subtitle) = candidates.next() {
                subtitle
            } else if let Some(fallback) = fallback.take() {
                let item =
                    {
                        let state = app.borrow();
                        let Some(play) = state.playback.as_ref().filter(|play| {
                            play.version == version && play.subtitle_load == request
                        }) else {
                            return;
                        };
                        play.item.clone()
                    };
                let subtitles =
                    sources::find_subdl(&item, &fallback.api_key, &fallback.query).await;
                let mut state = app.borrow_mut();
                if !subtitle_request_active(&state, version, request) {
                    return;
                }
                let Ok(subtitles) = subtitles else {
                    break;
                };
                let Some(subtitle) = subtitles.first().cloned() else {
                    break;
                };
                if let Some(play) = &mut state.playback {
                    play.subtitle_results.extend(subtitles);
                    subtitles::rank(&item, &mut play.subtitle_results);
                }
                render_results(&state);
                subtitle
            } else {
                break;
            };
            let cache_key = title_key
                .as_ref()
                .map(|key| subtitle_cache_key(key, &subtitle.download));
            let mut cached = if let Some(key) = &cache_key {
                cached_subtitle(key).await
            } else {
                None
            };
            let cache_hit = cached.is_some();
            if !cache_hit && let Some(key) = &legacy_key {
                cached = cached_subtitle(&subtitle_cache_key(key, &subtitle.download)).await;
            }
            if !subtitle_request_active(&app.borrow(), version, request) {
                return;
            }
            let result = match cached {
                Some(body) => Ok(body),
                None => sources::subtitle_file(&subtitle, &os).await,
            };
            let body_to_cache = {
                let mut state = app.borrow_mut();
                if !subtitle_request_active(&state, version, request) {
                    return;
                }
                let body = match result {
                    Ok(body) => body,
                    Err(reason) => {
                        error = reason;
                        continue;
                    }
                };
                if best && !subtitles::fits_duration(&body, state.video().duration()) {
                    error = tr("De geteste ondertitel past niet bij de speelduur.").to_owned();
                    continue;
                }
                let body_to_cache = (!cache_hit && cache_key.is_some() && body.len() <= 1_048_576)
                    .then(|| body.clone());
                let timing = restored.or_else(|| {
                    state
                        .library
                        .subtitle_timings
                        .get(cache_key.as_ref()?)
                        .copied()
                });
                let Some(play) = &mut state.playback else {
                    return;
                };
                let timing = timing.or_else(|| {
                    play.selected_subtitle
                        .as_ref()
                        .filter(|(current, _)| current.download == subtitle.download)
                        .map(|_| SubtitleTiming {
                            clock: play.subtitle_timing,
                            auto: play.sync_enabled,
                        })
                });
                let previous = play.selected_subtitle.replace((subtitle, body));
                let previous_timing = play.subtitle_timing;
                play.subtitle_timing =
                    timing.map_or_else(Timing::default, |timing| timing.clock.bounded());
                if !apply_online(&mut state) {
                    if let Some(play) = &mut state.playback {
                        play.selected_subtitle = previous;
                        play.subtitle_timing = previous_timing;
                    }
                    error = tr("Dit ondertitelbestand kan niet worden gelezen.").to_owned();
                    continue;
                }
                if let Some(play) = &mut state.playback {
                    play.sync = None;
                    play.sync_enabled = timing.is_none_or(|timing| timing.auto);
                }
                configure_sync(&app, &mut state);
                remember_external(&mut state);
                body_to_cache
            };
            if restored.is_none() {
                show_panel(&app, false);
            }
            if let (Some(key), Some(body)) = (cache_key, body_to_cache) {
                io::put(&key, &body).await;
            }
            return;
        }
        let state = app.borrow();
        if restored.is_none() && subtitle_request_active(&state, version, request) {
            state.player_note(&error);
        }
    });
}

fn shift(app: &Shared, delta: f64) {
    let mut state = app.borrow_mut();
    let Some(play) = state
        .playback
        .as_mut()
        .filter(|play| play.selected_subtitle.is_some())
    else {
        return;
    };
    play.subtitle_timing = Timing {
        offset: play.subtitle_timing.offset + delta,
        ..play.subtitle_timing
    }
    .bounded();
    disable_sync(&mut state);
    if apply_online(&mut state) {
        remember_external(&mut state);
    }
}

fn disable_sync(state: &mut App) {
    if let Some(play) = &mut state.playback {
        play.sync_enabled = false;
        play.sync = None;
        play.sync_notice = tr("Automatische synchronisatie uit").to_owned();
    }
}

fn configure_sync(app: &Shared, state: &mut App) {
    let video = state.video();
    let Some(play) = &mut state.playback else {
        return;
    };
    if !play.sync_enabled {
        disable_sync(state);
        return;
    }
    let Some((_, body)) = &play.selected_subtitle else {
        return;
    };
    let cues = subtitles::sync_cues(body);
    let app = std::rc::Rc::downgrade(app);
    let sample = move || {
        if let Some(app) = app.upgrade() {
            sample_sync(&app);
        }
    };
    match super::timing::AudioSync::start(&video, cues, sample) {
        Ok(sync) => {
            play.sync = Some(sync);
            play.sync_notice = tr("Timing meten tijdens afspelen…").to_owned();
        }
        Err(_) => {
            play.sync = None;
            play.sync_notice = tr(SYNC_UNAVAILABLE).to_owned();
        }
    }
}

fn sample_sync(app: &Shared) {
    let mut state = app.borrow_mut();
    if state
        .playback
        .as_ref()
        .is_none_or(|play| play.sync.is_none())
    {
        return;
    }
    let video = state.video();
    if video.paused()
        || video.seeking()
        || video.ready_state() < HtmlMediaElement::HAVE_CURRENT_DATA
    {
        return;
    }
    let Some(play) = &mut state.playback else {
        return;
    };
    let Some(sync) = &mut play.sync else {
        return;
    };
    let time = video.current_time();
    if !sync.available() {
        if play.sync_notice != tr(SYNC_UNAVAILABLE) {
            sync.skip(time);
            play.sync_notice = tr(SYNC_UNAVAILABLE).to_owned();
            refresh_tracks(&state);
        }
        return;
    }
    if play.sync_notice == tr(SYNC_UNAVAILABLE) {
        play.sync_notice = tr("Timing meten tijdens afspelen…").to_owned();
        refresh_tracks(&state);
        return;
    }
    let Some(estimate) = sync.sample(time, play.subtitle_timing) else {
        return;
    };
    let previous_notice = play.sync_notice.clone();
    let mut apply = false;
    if let Some(estimate) = estimate {
        let previous = play.subtitle_timing;
        if previous.rate != estimate.rate || (estimate.offset - previous.offset).abs() >= 0.25 {
            play.subtitle_timing = estimate;
            play.sync_notice = message(
                "Automatisch bijgesteld: {offset} s",
                &[(
                    "offset",
                    &format!("{:+.1}", time - estimate.subtitle_time(time)),
                )],
            );
            apply = true;
        } else {
            play.sync_notice = tr("Timing gecontroleerd").to_owned();
        }
    } else {
        play.sync_notice = tr("Geen betrouwbare afwijking gemeten").to_owned();
    }
    let notice_changed = play.sync_notice != previous_notice;
    if apply && apply_online(&mut state) {
        remember_external(&mut state);
    } else if notice_changed {
        refresh_tracks(&state);
    }
}

fn apply_online(state: &mut App) -> bool {
    let Some(play) = &mut state.playback else {
        return false;
    };
    let Some((subtitle, body)) = &play.selected_subtitle else {
        return false;
    };
    let Some(vtt) = subtitles::retime(body, play.subtitle_timing) else {
        return false;
    };
    let body = Array::of1(&JsValue::from_str(&vtt));
    let options = BlobPropertyBag::new();
    options.set_type("text/vtt");
    let Some(url) = Blob::new_with_str_sequence_and_options(&body, &options)
        .ok()
        .and_then(|file| web_sys::Url::create_object_url_with_blob(&file).ok())
    else {
        return false;
    };
    if let Some(old) = play.tracks_url.replace(url.clone()) {
        let _ = web_sys::Url::revoke_object_url(&old);
    }
    let name = subtitle.label_with(tr);
    let lang = if subtitle.language == Language::Dutch {
        "nl"
    } else {
        "en"
    };
    let video = state.video();
    if let Ok(Some(track)) = video.query_selector("track[data-online]") {
        track.remove();
    }
    show_only_track(state, None);
    append_track(state, &name, lang, &url, true);
    refresh_tracks(state);
    true
}

fn append_track(state: &App, name: &str, lang: &str, url: &str, online: bool) {
    let track = state
        .document
        .create_element("track")
        .expect("track element")
        .unchecked_into::<HtmlTrackElement>();
    track.set_kind(if name.to_ascii_lowercase().contains(" cc") {
        "captions"
    } else {
        "subtitles"
    });
    track.set_label(name);
    track.set_srclang(lang);
    track.set_src(url);
    track.set_default(online);
    if online {
        let _ = track.set_attribute("data-online", "true");
    }
    let _ = state.video().append_child(&track);
    if online && let Some(track) = track.track() {
        track.set_mode(TextTrackMode::Showing);
    }
}
