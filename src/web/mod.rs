mod i18n;
mod io;
mod live;
mod picker;
mod player;
mod poster;
mod search_keyboard;
mod sources;
mod timing;
mod ui;
mod vault;
mod vod;

use self::i18n::{message, tr};
use crate::{
    catalog::{self, BrowseFilter, Catalog, Item, Kind, Source},
    guide::Programme,
    series::SeriesProgress,
    subtitles::Subtitle,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    future::Future,
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    CustomEvent, Document, Element, Event, HtmlElement, HtmlInputElement, HtmlVideoElement,
    KeyboardEvent,
};

type Shared = Rc<RefCell<App>>;
const PAGE_SIZE: usize = 60;
const CONFIG_KEY: &str = "iptv-rs.settings";
const LIBRARY_KEY: &str = "iptv-rs.library";

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
struct Settings {
    source: Source,
    active_source: Option<u64>,
    next_source_id: u64,
    sources: Vec<SourceProfile>,
    preferred: bool,
    subtitle_size: player::SubtitleSize,
    opensubtitles: String,
    subdl: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct SourceProfile {
    id: u64,
    name: String,
    source: Source,
}

#[derive(Deserialize, Serialize)]
struct LivePreview {
    groups: Option<catalog::LiveGroups>,
    items: Vec<Item>,
}

impl Settings {
    fn migrate_sources(&mut self) {
        if self.sources.is_empty() && self.source.configured() {
            self.active_source = Some(1);
            self.sources.push(SourceProfile {
                id: 1,
                name: tr("Tv-bron").into(),
                source: self.source.clone(),
            });
        }
    }

    fn active_profile(&self) -> Option<&SourceProfile> {
        self.sources
            .iter()
            .find(|profile| Some(profile.id) == self.active_source)
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Library {
    favorites: HashMap<String, Item>,
    positions: HashMap<String, f64>,
    recent: HashMap<String, Item>,
    series: HashMap<String, SeriesProgress>,
    #[serde(skip_serializing_if = "HashSet::is_empty")]
    watched: HashSet<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    watching: HashMap<String, WatchDetails>,
    #[serde(
        rename = "episode_subtitles",
        skip_serializing_if = "HashMap::is_empty"
    )]
    subtitle_choices: HashMap<String, player::SavedSubtitle>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    subtitle_timings: HashMap<String, player::SubtitleTiming>,
    last_channel: HashMap<u64, String>,
    groups: HashMap<u64, catalog::LiveGroups>,
}

impl Library {
    fn restore_finished_episodes(&mut self) {
        for (series_key, progress) in &self.series {
            if progress.finished
                && !progress.episode_id.is_empty()
                && let Some((source, _)) = series_key.split_once(':')
            {
                self.watched
                    .insert(format!("{source}:Episode:{}", progress.episode_id));
            }
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct WatchDetails {
    updated: i64,
    duration: f64,
    has_next: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum View {
    Live,
    Guide,
    Radio,
    Movies,
    Series,
    Library,
    Settings,
}

impl View {
    const ALL: [Self; 6] = [
        Self::Live,
        Self::Radio,
        Self::Movies,
        Self::Series,
        Self::Library,
        Self::Settings,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Live => tr("Live"),
            Self::Guide => tr("Gids"),
            Self::Radio => tr("Radio"),
            Self::Movies => tr("Films"),
            Self::Series => tr("Series"),
            Self::Library => tr("Mijn lijst"),
            Self::Settings => tr("Instellingen"),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Guide => "guide",
            Self::Radio => "radio",
            Self::Movies => "movies",
            Self::Series => "series",
            Self::Library => "library",
            Self::Settings => "settings",
        }
    }

    fn kind(self) -> Option<Kind> {
        match self {
            Self::Live | Self::Guide => Some(Kind::Live),
            Self::Radio => Some(Kind::Radio),
            Self::Movies => Some(Kind::Movie),
            Self::Series => Some(Kind::Series),
            _ => None,
        }
    }
}

struct Detail {
    item: Item,
    origin_page: usize,
    resume_from_shelf: bool,
    episodes: Vec<Item>,
    season: Option<u32>,
    loading: bool,
    error: Option<String>,
    version: u64,
}

struct Playback {
    item: Item,
    version: u64,
    alternate_url: Option<url::Url>,
    early_retry_url: Option<url::Url>,
    live_route: Option<player::LiveRoute>,
    stream_attempt: u64,
    stream_failed: bool,
    stream_health: Option<player::StreamHealth>,
    archive: bool,
    archive_guide: Option<live::State>,
    last_saved: f64,
    resume_at: f64,
    previous_episode_key: Option<String>,
    series: Option<Item>,
    started: bool,
    completed: bool,
    selected_subtitle: Option<(Subtitle, String)>,
    tracks_url: Option<String>,
    subtitle_timing: crate::sync::Timing,
    subtitle_key: Option<String>,
    subtitle_choice: Option<player::SavedSubtitle>,
    subtitle_load: u64,
    subtitle_results: Vec<Subtitle>,
    subtitle_search: u64,
    panel: bool,
    controls_until: i64,
    sync_enabled: bool,
    sync: Option<timing::AudioSync>,
    sync_notice: String,
    awaiting_cross_origin: bool,
}

struct App {
    document: Document,
    settings: Settings,
    library: Library,
    live_routes: player::LiveRoutes,
    source_key: String,
    adding_source: bool,
    setup_m3u: bool,
    editing_group: String,
    view: View,
    library_section: ui::LibrarySection,
    model: HashMap<Kind, Catalog>,
    public_radio: Catalog,
    catalog_previews: HashMap<Kind, Catalog>,
    vod_partial: HashMap<Kind, Catalog>,
    pending: HashSet<Kind>,
    failures: HashMap<Kind, String>,
    source_refresh: SourceRefresh,
    query: String,
    keyboard_visible: Cell<bool>,
    catalog_filter: BrowseFilter<String>,
    page: usize,
    detail: Option<Detail>,
    playback: Option<Playback>,
    live_preload: Option<player::LivePreload>,
    live_list_focus: RefCell<ui::LiveListFocus>,
    guide: Option<HashMap<String, Vec<Programme>>>,
    live_programmes: HashMap<String, (i64, Vec<Programme>)>,
    live_programmes_pending: Option<(ProgrammeScope, futures::future::AbortHandle)>,
    guide_pending: bool,
    guide_error: Option<String>,
    live_guide: live::State,
    last_live: Option<Item>,
    previous_live: Option<Item>,
    revision: u64,
    sequence: u64,
}

#[derive(Default)]
enum SourceRefresh {
    #[default]
    Idle,
    Loading {
        remaining: HashSet<Kind>,
        error: Option<String>,
    },
    Finished(Result<(), String>),
}

impl App {
    fn element(&self, id: &str) -> Element {
        self.document.get_element_by_id(id).expect("app element")
    }

    fn video(&self) -> HtmlVideoElement {
        self.element("video").unchecked_into()
    }

    fn search_page(&self, kind: Kind, id: &str) -> Option<usize> {
        let catalog = self.browse_catalog(kind)?;
        if self.query.trim().is_empty() {
            return catalog
                .browse_position(self.catalog_filter.borrowed(), self.settings.preferred, id)
                .map(|position| position / PAGE_SIZE);
        }
        let (hits, _) = catalog.search_filtered(
            &self.query,
            self.catalog_filter.borrowed(),
            self.settings.preferred,
            usize::MAX,
        );
        hits.iter()
            .position(|&index| catalog.items[index].id == id)
            .map(|position| position / PAGE_SIZE)
    }

    fn browse_catalog(&self, kind: Kind) -> Option<&Catalog> {
        self.model
            .get(&kind)
            .or_else(|| (kind == Kind::Radio).then_some(&self.public_radio))
    }

    fn sync_browse_filter(&mut self, kind: Kind) {
        if self.view.kind() != Some(kind) {
            return;
        }
        if !self.catalog_filter.is_all()
            && picker::browse_options(self, kind)
                .is_none_or(|options| !options.selections.contains(&self.catalog_filter.borrowed()))
        {
            self.catalog_filter = BrowseFilter::All;
            self.page = 0;
        }
        if self.page > 0
            && let Some(catalog) = self.browse_catalog(kind)
        {
            let (_, count) = catalog.search_filtered(
                &self.query,
                self.catalog_filter.borrowed(),
                self.settings.preferred,
                0,
            );
            self.page = self.page.min(count.saturating_sub(1) / PAGE_SIZE);
        }
    }

    fn restore_catalog_page_for_focus(&mut self, kind: Kind) {
        if self.view.kind() == Some(kind)
            && self.detail.is_none()
            && let Some(active) = self.document.active_element()
            && let Some((command, key)) = ui::focus_identity(&active)
            && command == "item"
            && let Some((_, id)) = key.split_once(':')
            && let Some(page) = self.search_page(kind, id)
        {
            self.page = page;
        }
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }

    fn library_key(&self, item: &Item) -> String {
        format!("{}:{}", self.source_key, item.key())
    }

    fn position(&self, item: &Item) -> f64 {
        self.library
            .positions
            .get(&self.library_key(item))
            .copied()
            .unwrap_or(0.)
    }

    fn is_watched(&self, item: &Item) -> bool {
        matches!(item.kind, Kind::Movie | Kind::Episode)
            && self.library.watched.contains(&self.library_key(item))
    }

    fn series_progress(&self, series: &Item) -> Option<&SeriesProgress> {
        self.library.series.get(&self.library_key(series))
    }

    fn saved_episode_position(&self, progress: &SeriesProgress) -> f64 {
        self.library
            .positions
            .get(&format!(
                "{}:Episode:{}",
                self.source_key, progress.episode_id
            ))
            .copied()
            .unwrap_or_default()
    }

    fn series_target<'a>(&self, series: &Item, episodes: &'a [Item]) -> Option<(&'a Item, bool)> {
        if series.kind != Kind::Series {
            return None;
        }
        if let Some(progress) = self.series_progress(series) {
            return progress
                .target(episodes)
                .map(|episode| (episode, progress.finished));
        }
        episodes
            .iter()
            .rev()
            .find(|episode| self.position(episode) > 0.)
            .map(|episode| (episode, false))
    }

    fn previous_episode_key(&self, detail: &Detail, episode: &Item) -> Option<String> {
        if let Some(progress) = self.series_progress(&detail.item)
            && !progress.finished
            && progress.episode_id != episode.id
            && progress
                .target(&detail.episodes)
                .is_some_and(|target| target.id == episode.id)
        {
            return Some(format!(
                "{}:Episode:{}",
                self.source_key, progress.episode_id
            ));
        }
        None
    }

    fn episode_position(&self, detail: &Detail, episode: &Item) -> f64 {
        self.previous_episode_key(detail, episode)
            .and_then(|key| self.library.positions.get(&key).copied())
            .unwrap_or_else(|| self.position(episode))
    }

    fn note(&self, message: &str) {
        self.element("notice").set_text_content(Some(message));
    }

    fn source_catalog_kinds(&self) -> &'static [Kind] {
        if self.settings.source.server.trim().is_empty() {
            &[Kind::Live]
        } else {
            &[Kind::Live, Kind::Movie, Kind::Series]
        }
    }

    fn source_refresh_busy(&self) -> bool {
        matches!(self.source_refresh, SourceRefresh::Loading { .. })
            || self
                .source_catalog_kinds()
                .iter()
                .any(|kind| self.pending.contains(kind))
    }

    fn catalog_loading(&self, kind: Kind) -> bool {
        let kind = if kind == Kind::Radio {
            Kind::Live
        } else {
            kind
        };
        self.pending.contains(&kind)
            || (matches!(self.source_refresh, SourceRefresh::Loading { .. })
                && self.source_catalog_kinds().contains(&kind))
    }

    fn finish_source_refresh<T>(&mut self, kind: Kind, result: &Result<T, String>) {
        if let SourceRefresh::Loading { remaining, error } = &mut self.source_refresh
            && remaining.remove(&kind)
            && error.is_none()
        {
            *error = result.as_ref().err().cloned();
        }
    }

    fn catalog_key(&self, kind: Kind) -> String {
        if kind == Kind::Live {
            format!("{}:Live:groups-v3", self.source_key)
        } else {
            format!("{}:{kind:?}", self.source_key)
        }
    }

    fn live_groups(&self) -> Option<&catalog::LiveGroups> {
        self.library.groups.get(&self.settings.active_source?)
    }

    fn source_fingerprint(source: &Source) -> String {
        let mut hash = DefaultHasher::new();
        for field in [
            &source.server,
            &source.username,
            &source.password,
            &source.playlist,
            &source.guide,
        ] {
            field.hash(&mut hash);
        }
        format!("{:016x}", hash.finish())
    }
}

fn public_radio_catalog() -> Catalog {
    let stations = [
        ("radio1", "NPO Radio 1"),
        ("radio2", "NPO Radio 2"),
        ("3fm", "NPO 3FM"),
        ("radio4", "NPO Klassiek"),
        ("radio5", "NPO Radio 5"),
        ("funx", "NPO FunX"),
    ];
    Catalog::new(
        stations
            .into_iter()
            .map(|(id, title)| Item {
                id: format!("npo-{id}"),
                kind: Kind::Radio,
                title: title.to_owned(),
                category: "NPO".to_owned(),
                stream: format!("https://icecast.omroep.nl/{id}-bb-mp3"),
                ..Item::default()
            })
            .collect(),
    )
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or(tr("Document ontbreekt"))?;
    i18n::initialize(&document);
    spawn_local(async move {
        let loaded = async {
            vault::initialize().await?;
            let settings = vault::load(CONFIG_KEY).ok()?.unwrap_or_default();
            let library = vault::load(LIBRARY_KEY).ok()?.unwrap_or_default();
            io::remove("iptv-rs.poster-order");
            Some((settings, library))
        };
        if let Some((settings, library)) = loaded.await
            && boot(document.clone(), settings, library).is_ok()
        {
            return;
        }
        if let Some(initial) = document.get_element_by_id("initial") {
            let _ = initial.set_attribute("data-error", "");
        }
        if let Some(message) = document.get_element_by_id("initial-message") {
            message.set_text_content(Some(tr("Orbis kon de beveiligde opslag niet openen.")));
        }
    });
    Ok(())
}

fn boot(document: Document, mut settings: Settings, mut library: Library) -> Result<(), JsValue> {
    settings.migrate_sources();
    let view = if settings.source.configured() {
        View::Live
    } else {
        View::Settings
    };
    let source_key = App::source_fingerprint(&settings.source);
    library.restore_finished_episodes();
    let app = Rc::new(RefCell::new(App {
        document: document.clone(),
        settings,
        library,
        live_routes: player::LiveRoutes::load(),
        source_key,
        adding_source: false,
        setup_m3u: false,
        editing_group: String::new(),
        view,
        library_section: ui::LibrarySection::default(),
        model: HashMap::new(),
        public_radio: public_radio_catalog(),
        catalog_previews: HashMap::new(),
        vod_partial: HashMap::new(),
        pending: HashSet::new(),
        failures: HashMap::new(),
        source_refresh: SourceRefresh::default(),
        query: String::new(),
        keyboard_visible: Cell::new(false),
        catalog_filter: BrowseFilter::All,
        page: 0,
        detail: None,
        playback: None,
        live_preload: None,
        live_list_focus: RefCell::default(),
        guide: None,
        live_programmes: HashMap::new(),
        live_programmes_pending: None,
        guide_pending: false,
        guide_error: None,
        live_guide: live::State::default(),
        last_live: None,
        previous_live: None,
        revision: 0,
        sequence: 0,
    }));
    search_keyboard::build(&app.borrow());
    wire(&app)?;
    player::wire(&app)?;
    ui::render(&app);
    document
        .document_element()
        .ok_or(tr("Pagina ontbreekt"))?
        .set_attribute("data-ready", "true")?;
    open_view(&app, view);
    if let Some(choice) = document.get_element_by_id("setup-xtream") {
        focus(&document, &choice);
    }
    Ok(())
}

fn on(
    target: &web_sys::EventTarget,
    event: &str,
    handler: impl FnMut(Event) + 'static,
) -> Result<(), JsValue> {
    let callback = Closure::<dyn FnMut(Event)>::new(handler);
    target.add_event_listener_with_callback(event, callback.as_ref().unchecked_ref())?;
    callback.forget();
    Ok(())
}

fn wire(app: &Shared) -> Result<(), JsValue> {
    let document = app.borrow().document.clone();
    let click_app = app.clone();
    on(&document, "click", move |event| {
        let Some(target) = event
            .target()
            .and_then(|value| value.dyn_into::<Element>().ok())
        else {
            return;
        };
        {
            let state = click_app.borrow();
            if picker::is_open(&state) {
                if target.id() == "catalog-picker" {
                    picker::close(&click_app);
                    return;
                }
                if !picker::owns(&state, &target) {
                    return;
                }
            }
        }
        let Ok(Some(button)) = target.closest("[data-cmd]") else {
            return;
        };
        let command = button.get_attribute("data-cmd").unwrap_or_default();
        let value = button.get_attribute("data-value").unwrap_or_default();
        if command == "search-key" {
            search_keyboard::mark_key(&click_app.borrow().document, &button);
        }
        dispatch(&click_app, &command, &value);
    })?;
    let input_app = app.clone();
    on(&document, "input", move |event| {
        let Some(input) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
        else {
            return;
        };
        if input.id() == "search" {
            set_search(&input_app, input.value());
        }
        if input.id() == "live-guide-search" {
            live::set_query(&input_app, input.value());
        }
    })?;
    let search = app
        .borrow()
        .element("search")
        .unchecked_into::<HtmlInputElement>();
    let change_app = app.clone();
    on(&search, "change", move |event| {
        if let Some(input) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
        {
            set_search(&change_app, input.value());
        }
    })?;
    let keyboard_app = app.clone();
    on(&document, "keyboardStateChange", move |event| {
        if let Ok(event) = event.dyn_into::<CustomEvent>()
            && let Ok(value) =
                js_sys::Reflect::get(&event.detail(), &JsValue::from_str("visibility"))
            && let Some(visible) = value.as_bool()
        {
            keyboard_app.borrow().keyboard_visible.set(visible);
        }
    })?;
    let pointer_app = app.clone();
    on(&document, "mousemove", move |event| {
        ui::track_live_list_focus(&pointer_app, &event, true);
    })?;
    let focus_app = app.clone();
    on(&document, "focusin", move |event| {
        ui::track_live_list_focus(&focus_app, &event, false);
    })?;
    let cursor_app = app.clone();
    on(&document, "cursorStateChange", move |event| {
        if let Ok(event) = event.dyn_into::<CustomEvent>()
            && js_sys::Reflect::get(&event.detail(), &JsValue::from_str("visibility"))
                .ok()
                .and_then(|value| value.as_bool())
                == Some(false)
        {
            ui::focus_pointed_live_row(&cursor_app);
        }
    })?;
    let key_app = app.clone();
    let mut back_started_in_player = false;
    let keys = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Ok(event) = event.dyn_into::<KeyboardEvent>() else {
            return;
        };
        if event.key_code() == 1537 {
            ui::focus_pointed_live_row(&key_app);
            return;
        }
        let reported_key = event.key();
        let key = match event.key_code() {
            13 => "Enter",
            33 => "ChannelUp",
            34 => "ChannelDown",
            37 => "ArrowLeft",
            38 => "ArrowUp",
            39 => "ArrowRight",
            40 => "ArrowDown",
            _ => match reported_key.as_str() {
                "PageUp" => "ChannelUp",
                "PageDown" => "ChannelDown",
                "Left" => "ArrowLeft",
                "Up" => "ArrowUp",
                "Right" => "ArrowRight",
                "Down" => "ArrowDown",
                _ => &reported_key,
            },
        };
        let target = event
            .target()
            .and_then(|element| element.dyn_into::<Element>().ok());
        let target_id = target.as_ref().map(Element::id).unwrap_or_default();
        let remote_back = event.key_code() == 461;
        let back_key = key == "Escape" || remote_back;
        if back_key && !event.repeat() {
            back_started_in_player = key_app.borrow().playback.is_some();
        }
        if event.repeat()
            && ((back_key && back_started_in_player)
                || (key == "Enter"
                    && key_app
                        .borrow()
                        .playback
                        .as_ref()
                        .is_some_and(|play| play.item.kind == Kind::Live)))
        {
            event.prevent_default();
            return;
        }
        if picker::is_open(&key_app.borrow()) {
            match key {
                _ if back_key => picker::close(&key_app),
                "ArrowUp" | "ArrowDown" | "Tab" => picker::move_focus(&key_app, &event, key),
                "ArrowLeft" | "ArrowRight" => {}
                "Enter" if picker::restore_focus(&key_app) => {}
                _ => return,
            }
            event.prevent_default();
            return;
        }
        if search_keyboard::is_open(&key_app.borrow()) {
            if back_key {
                event.prevent_default();
                search_keyboard::close(&key_app, false);
            } else if key == "Tab" {
                event.prevent_default();
                search_keyboard::cycle(&key_app, event.shift_key());
            } else if ui::navigate(&key_app, key) {
                event.prevent_default();
            }
            return;
        }
        if target_id == "search" {
            let keyboard_visible = key_app.borrow().keyboard_visible.get();
            if event.is_composing() || (keyboard_visible && (back_key || key.starts_with("Arrow")))
            {
                return;
            }
            if back_key {
                event.prevent_default();
                ui::leave_search(&key_app);
                return;
            }
            if matches!(key, "Enter" | "ArrowDown") {
                if let Some(input) = target
                    .as_ref()
                    .and_then(|element| element.dyn_ref::<HtmlInputElement>())
                {
                    set_search(&key_app, input.value());
                    if key == "Enter" && !keyboard_visible && input.value().trim().is_empty() {
                        event.prevent_default();
                        search_keyboard::open(&key_app);
                        return;
                    }
                }
                if ui::focus_first_result(&key_app) {
                    event.prevent_default();
                    return;
                }
            }
        }
        if back_key {
            event.prevent_default();
            if !player::dismiss_controls(&key_app) && !back(&key_app) && remote_back {
                platform_back();
            }
            return;
        }
        if live::handle_key(&key_app, key) {
            event.prevent_default();
            event.stop_propagation();
            return;
        }
        if player::handle_controls_key(&key_app, key) {
            event.prevent_default();
            return;
        }
        if key == "Enter" && target_id == "subtitle-query" {
            event.prevent_default();
            player::search(&key_app);
            return;
        }
        if target_id == "player-seek" && player::seek_key(&key_app, key) {
            event.prevent_default();
            return;
        }
        let tag = target.as_ref().map(Element::tag_name);
        if tag.as_deref() == Some("INPUT") && !matches!(key, "ArrowUp" | "ArrowDown") {
            return;
        }
        if ui::navigate(&key_app, key) {
            event.prevent_default();
        }
    });
    document
        .default_view()
        .ok_or(tr("Browservenster ontbreekt"))?
        .add_event_listener_with_callback_and_bool(
            "keydown",
            keys.as_ref().unchecked_ref(),
            true,
        )?;
    keys.forget();
    let window = document
        .default_view()
        .ok_or(tr("Browservenster ontbreekt"))?;
    let scroll_app = app.clone();
    on(&window, "scroll", move |_| {
        load_visible_live_programmes(&scroll_app)
    })?;
    let live_app = app.clone();
    let refresh_programmes = Closure::<dyn FnMut()>::new(move || {
        ui::update_live_programmes(&live_app.borrow());
        load_visible_live_programmes(&live_app);
    });
    window.set_interval_with_callback_and_timeout_and_arguments_0(
        refresh_programmes.as_ref().unchecked_ref(),
        60_000,
    )?;
    refresh_programmes.forget();
    Ok(())
}

fn set_search(app: &Shared, query: String) {
    if app.borrow().query == query {
        return;
    }
    let mut state = app.borrow_mut();
    state.query = query;
    state.page = 0;
    *state.live_list_focus.get_mut() = ui::LiveListFocus::default();
    if state.view == View::Guide {
        state.live_guide.clear_selection_for_search();
    }
    drop(state);
    ui::render_search(app);
}

fn dispatch(app: &Shared, cmd: &str, value: &str) {
    match cmd {
        "picker-open" => picker::open(app, value),
        "picker-select" => picker::select(app, value),
        "picker-close" => picker::close(app),
        "view" => {
            if let Some(view) = View::ALL.into_iter().find(|view| view.id() == value) {
                open_view(app, view);
            }
        }
        "preferred" => {
            let mut state = app.borrow_mut();
            state.settings.preferred = !state.settings.preferred;
            state.catalog_filter = BrowseFilter::All;
            state.page = 0;
            io::save_private(CONFIG_KEY, &state.settings);
            ui::mark_filter(&state);
            let settings_open = state.view == View::Settings;
            drop(state);
            if !settings_open {
                ui::render_content(app);
            }
        }
        "save" => save_settings(app),
        "privacy" => ui::toggle_privacy(&app.borrow()),
        "setup-method" => {
            let mut state = app.borrow_mut();
            if state.view == View::Settings && !state.settings.source.configured() {
                state.setup_m3u = value == "m3u";
                ui::show_setup_fields(&state);
                let field = if state.setup_m3u {
                    "source-m3u"
                } else {
                    "source-server"
                };
                focus(&state.document, &state.element(field));
            }
        }
        "live-resume" => live::resume(app),
        "source-switch" => switch_source(app, value),
        "source-new" | "source-cancel" => {
            app.borrow_mut().adding_source = cmd == "source-new";
            ui::render_content(app);
            let state = app.borrow();
            let target = state
                .document
                .get_element_by_id("source-new")
                .unwrap_or_else(|| state.element("source-name"));
            focus(&state.document, &target);
        }
        "source-remove" => remove_source(app, value),
        "group-toggle" | "group-up" | "group-down" => change_group(app, cmd),
        "source-refresh" => refresh_source_catalogs(app),
        "page" => turn_page(
            app,
            if value == "prev" {
                PageChange::Previous
            } else {
                PageChange::Next
            },
        ),
        "item" | "continue-item" => select_item(app, value, cmd == "continue-item"),
        "search-keys" => search_keyboard::open(app),
        "search-key" => search_keyboard::press(app, value),
        "episode" | "series-resume" => {
            let item = {
                let state = app.borrow();
                state.detail.as_ref().and_then(|detail| {
                    if cmd == "series-resume" {
                        state
                            .series_target(&detail.item, &detail.episodes)
                            .map(|(item, _)| item)
                    } else {
                        detail.episodes.iter().find(|item| item.id == value)
                    }
                    .cloned()
                })
            };
            if let Some(item) = item {
                player::start(app, item, false);
            }
        }
        "watch" | "watch-subtitles" => {
            let item = app
                .borrow()
                .detail
                .as_ref()
                .map(|detail| detail.item.clone());
            if let Some(item) = item {
                player::start(app, item, cmd == "watch-subtitles");
            }
        }
        "save-title" => flip_favorite(app, value),
        "library-section" => ui::select_library_section(app, value),
        "toggle-watched" => toggle_movie_watched(app),
        "back" => {
            back(app);
        }
        command if command.starts_with("live-") || command == "previous-live" => {
            live::dispatch(app, command, value)
        }
        command => player::dispatch(app, command, value),
    }
}

fn platform_back() {
    let Some(window) = web_sys::window() else {
        return;
    };
    for name in ["webOSSystem", "PalmSystem"] {
        if let Ok(system) = js_sys::Reflect::get(window.as_ref(), &JsValue::from_str(name))
            && let Ok(method) = js_sys::Reflect::get(&system, &JsValue::from_str("platformBack"))
            && let Ok(method) = method.dyn_into::<js_sys::Function>()
        {
            let _ = method.call0(&system);
            return;
        }
    }
}

fn back(app: &Shared) -> bool {
    if live::back(app) {
        return true;
    }
    if app.borrow().playback.is_some() {
        if app
            .borrow()
            .playback
            .as_ref()
            .is_some_and(|playback| playback.panel)
        {
            player::show_panel(app, false);
        } else {
            player::stop(app);
        }
        return true;
    }
    let mut state = app.borrow_mut();
    if let Some(detail) = state.detail.take() {
        state.page = detail.origin_page;
        if let Some(kind) = state.view.kind() {
            state.sync_browse_filter(kind);
        } else {
            ui::clamp_library_page(&mut state);
        }
        drop(state);
        ui::render(app);
        let key = detail.item.key();
        let focused = (detail.resume_from_shelf && ui::focus_continue(app, &key))
            || ui::focus_result(app, &key);
        if !focused && !ui::reveal_library_card(app, &key) {
            locate_search_page(app, &detail.item);
            if !ui::focus_result(app, &key) && !ui::focus_first_result(app) {
                ui::focus_catalog(app);
            }
        }
        true
    } else if state.view != View::Live && state.settings.source.configured() {
        drop(state);
        open_view(app, View::Live);
        true
    } else {
        false
    }
}

fn locate_search_page(app: &Shared, item: &Item) {
    let mut state = app.borrow_mut();
    if state.view == View::Library {
        return;
    }
    let Some(page) = state.search_page(item.kind, &item.id) else {
        return;
    };
    if page != state.page {
        state.page = page;
        drop(state);
        ui::render_content(app);
    }
}

#[derive(Clone, Copy)]
enum PageChange {
    Next,
    Previous,
    PreviousEnd,
}

fn turn_page(app: &Shared, change: PageChange) {
    let list = {
        let mut state = app.borrow_mut();
        state.page = match change {
            PageChange::Next => state.page + 1,
            PageChange::Previous | PageChange::PreviousEnd => state.page.saturating_sub(1),
        };
        *state.live_list_focus.get_mut() = ui::LiveListFocus::default();
        state.detail.is_none() && state.view != View::Guide
    };
    ui::render_content(app);
    let focused = list
        && match change {
            PageChange::PreviousEnd => ui::focus_last_result(app),
            _ => ui::focus_first_result(app),
        };
    if !focused {
        ui::focus_catalog(app);
    }
}

fn open_view(app: &Shared, view: View) {
    {
        let mut state = app.borrow_mut();
        state.view = view;
        *state.live_list_focus.get_mut() = ui::LiveListFocus::default();
        state.adding_source = false;
        state.detail = None;
        state.live_guide.reset();
        state.catalog_filter = BrowseFilter::All;
        state.page = 0;
        state.query.clear();
        search_keyboard::hide(&state);
        picker::hide(&state);
        state.note("");
        let input: HtmlInputElement = state.element("search").unchecked_into();
        input.set_value("");
    }
    ui::render(app);
    if let Some(kind) = view.kind() {
        load_catalog(app, kind, false);
    } else if view == View::Settings {
        load_catalog(app, Kind::Live, false);
    }
    if view == View::Guide {
        load_guide(app, false);
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProgrammeScope {
    LiveList,
    QuickGuide(usize),
}

fn programme_scope(state: &App) -> Option<ProgrammeScope> {
    if let Some(page) = state.live_guide.quick_page() {
        return (!state.document.hidden()).then_some(ProgrammeScope::QuickGuide(page));
    }
    live_list_visible(state).then_some(ProgrammeScope::LiveList)
}

fn load_visible_live_programmes(app: &Shared) {
    let (scope, revision, registration) = {
        let mut state = app.borrow_mut();
        let scope = programme_scope(&state);
        if state
            .live_programmes_pending
            .as_ref()
            .is_some_and(|(pending, _)| Some(*pending) == scope)
        {
            return;
        }
        if let Some((_, pending)) = state.live_programmes_pending.take() {
            pending.abort();
        }
        let Some(scope) = scope else { return };
        if state.settings.source.server.is_empty() || !state.settings.source.playlist.is_empty() {
            return;
        }
        let (handle, registration) = futures::future::AbortHandle::new_pair();
        state.live_programmes_pending = Some((scope, handle));
        (scope, state.revision, registration)
    };
    let app = app.clone();
    let load = async move {
        io::delay(300).await;
        let (source, candidates) = {
            let mut state = app.borrow_mut();
            if state.revision != revision {
                return;
            }
            let now = io::now();
            state.live_programmes.retain(|_, (expires, programmes)| {
                *expires > now || programmes.iter().any(|programme| programme.stop_unix > now)
            });
            let mut candidates = match scope {
                ProgrammeScope::LiveList => ui::visible_channels_needing_programmes(&state),
                ProgrammeScope::QuickGuide(page) => live::quick_page_channels(&state, page),
            };
            candidates.retain(|id| {
                state
                    .live_programmes
                    .get(id)
                    .is_none_or(|(expires, _)| *expires <= now)
            });
            candidates.truncate(12);
            (state.settings.source.clone(), candidates)
        };
        use futures::StreamExt;
        let mut requests = futures::stream::iter(candidates.into_iter().map(|id| {
            let source = &source;
            async move {
                let programmes = sources::short_epg(source, &id).await;
                (id, programmes)
            }
        }))
        .buffer_unordered(3);
        let mut requested = false;
        while let Some((id, programmes)) = requests.next().await {
            let mut state = app.borrow_mut();
            if state.revision != revision {
                return;
            }
            if programme_scope(&state) != Some(scope) {
                break;
            }
            requested = true;
            let now = io::now();
            if let Some(programmes) = programmes {
                let (current, next) = crate::guide::now_and_next(&programmes, now);
                let expires = current
                    .map(|programme| programme.stop_unix)
                    .or_else(|| next.map(|programme| programme.start_unix.min(now + 180)))
                    .unwrap_or(now + 180);
                state
                    .live_programmes
                    .insert(id.clone(), (expires, programmes));
            } else {
                let cached = state.live_programmes.entry(id.clone()).or_default();
                cached.0 = now + 180;
            }
            if scope == ProgrammeScope::LiveList {
                ui::update_live_programme(&state, &id);
            }
        }
        let current = {
            let mut state = app.borrow_mut();
            state.live_programmes_pending = None;
            programme_scope(&state)
        };
        if current == Some(scope) && matches!(scope, ProgrammeScope::QuickGuide(_)) {
            live::refresh(&app);
        } else if current.is_some() && (requested || Some(scope) != current) {
            load_visible_live_programmes(&app);
        }
    };
    spawn_local(async move {
        let _ = futures::future::Abortable::new(load, registration).await;
    });
}

fn live_list_visible(state: &App) -> bool {
    state.view == View::Live && state.playback.is_none() && !state.document.hidden()
}

fn save_settings(app: &Shared) {
    let state = app.borrow();
    let field = |id| {
        state
            .element(id)
            .unchecked_into::<HtmlInputElement>()
            .value()
            .trim()
            .to_owned()
    };
    let source = Source {
        server: field("source-server"),
        username: field("source-user"),
        password: field("source-pass"),
        playlist: field("source-m3u"),
        guide: field("source-epg"),
    };
    let source = if state.settings.source.configured() {
        source
    } else if state.setup_m3u {
        Source {
            playlist: source.playlist,
            ..Source::default()
        }
    } else {
        Source {
            playlist: String::new(),
            ..source
        }
    };
    if !source.configured() {
        state.note(tr("Vul een Xtream-server of een M3U-link in."));
        return;
    }
    if [&source.server, &source.playlist, &source.guide]
        .iter()
        .any(|url| !url.is_empty() && catalog::http_url(url).is_none())
    {
        state.note(tr("Gebruik een geldige HTTP- of HTTPS-link."));
        return;
    }
    if !source.server.is_empty() && (source.username.is_empty() || source.password.is_empty()) {
        state.note(tr("Vul ook de Xtream-gebruikersnaam en het wachtwoord in."));
        return;
    }
    let mut settings = state.settings.clone();
    settings.opensubtitles = field("source-os");
    settings.subdl = field("source-subdl");
    let mut name = field("source-name");
    let id = if state.adding_source || settings.active_source.is_none() {
        settings.next_source_id.max(
            settings
                .sources
                .iter()
                .map(|profile| profile.id)
                .max()
                .unwrap_or(0)
                + 1,
        )
    } else {
        settings.active_source.unwrap_or(1)
    };
    if name.is_empty() {
        name = message("Tv-bron {id}", &[("id", &id.to_string())]);
    }
    if let Some(profile) = settings.sources.iter_mut().find(|profile| profile.id == id) {
        profile.name = name;
        profile.source = source.clone();
    } else {
        settings.sources.push(SourceProfile {
            id,
            name,
            source: source.clone(),
        });
    }
    settings.active_source = Some(id);
    settings.next_source_id = settings.next_source_id.max(id + 1);
    settings.source = source;
    drop(state);
    activate_source(app, settings);
}

fn switch_source(app: &Shared, value: &str) {
    let Ok(id) = value.parse::<u64>() else {
        return;
    };
    let mut settings = app.borrow().settings.clone();
    let Some(profile) = settings.sources.iter().find(|profile| profile.id == id) else {
        return;
    };
    settings.source = profile.source.clone();
    settings.active_source = Some(id);
    activate_source(app, settings);
}

fn remove_source(app: &Shared, value: &str) {
    let Ok(id) = value.parse::<u64>() else {
        return;
    };
    let state = app.borrow();
    let mut settings = state.settings.clone();
    settings.sources.retain(|profile| profile.id != id);
    drop(state);
    if settings.active_source == Some(id) {
        settings.active_source = settings.sources.first().map(|profile| profile.id);
        settings.source = settings
            .sources
            .first()
            .map(|profile| profile.source.clone())
            .unwrap_or_default();
        activate_source(app, settings);
        return;
    }
    if io::save_private(CONFIG_KEY, &settings) {
        app.borrow_mut().settings = settings;
        let state = app.borrow();
        ui::refresh_source_actions(&state);
        if let Some(target) = state.document.get_element_by_id("source-new") {
            focus(&state.document, &target);
        }
    } else {
        app.borrow()
            .note(tr("Instellingen opslaan mislukt op dit apparaat."));
    }
}

fn change_group(app: &Shared, command: &str) {
    let mut state = app.borrow_mut();
    let Some(id) = state.settings.active_source else {
        return;
    };
    let group = state.editing_group.clone();
    let Some(catalog) = state.model.get(&Kind::Live) else {
        return;
    };
    let available = catalog.categories(false);
    if !available.contains(&group) {
        return;
    }
    let mut settings = state.library.groups.get(&id).cloned().unwrap_or_default();
    if command == "group-toggle" {
        if !settings.hidden.remove(&group) {
            settings.hidden.insert(group);
        }
    } else {
        let mut order: Vec<_> = settings
            .ordered(available)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let Some(index) = order.iter().position(|name| name == &group) else {
            return;
        };
        let other = if command == "group-up" {
            index.saturating_sub(1)
        } else {
            (index + 1).min(order.len() - 1)
        };
        order.swap(index, other);
        order.extend(
            settings
                .order
                .iter()
                .filter(|name| !available.contains(name))
                .cloned(),
        );
        settings.order = order;
    }
    let old = state.library.groups.insert(id, settings.clone());
    if !io::save_private(LIBRARY_KEY, &state.library) {
        if let Some(old) = old {
            state.library.groups.insert(id, old);
        } else {
            state.library.groups.remove(&id);
        }
        state.note(tr("Instellingen opslaan mislukt op dit apparaat."));
        return;
    }
    if let Some(catalog) = state.model.get_mut(&Kind::Live) {
        catalog.apply_live_groups(&settings);
    }
    ui::refresh_group_editor(&state);
}

fn activate_source(app: &Shared, settings: Settings) {
    let mut state = app.borrow_mut();
    if !io::save_private(CONFIG_KEY, &settings) {
        state.note(tr("Instellingen opslaan mislukt op dit apparaat."));
        return;
    }
    let view = if settings.source.configured() {
        View::Live
    } else {
        View::Settings
    };
    state.source_key = App::source_fingerprint(&settings.source);
    state.settings = settings;
    state.library_section = ui::LibrarySection::default();
    state.adding_source = false;
    state.editing_group.clear();
    state.revision += 1;
    state.model.clear();
    state.catalog_previews.clear();
    state.vod_partial.clear();
    state.pending.clear();
    state.failures.clear();
    state.source_refresh = SourceRefresh::default();
    state.guide = None;
    state.live_programmes.clear();
    if let Some((_, pending)) = state.live_programmes_pending.take() {
        pending.abort();
    }
    state.guide_pending = false;
    state.guide_error = None;
    state.live_guide = live::State::default();
    state.last_live = None;
    state.previous_live = None;
    drop(state);
    open_view(app, view);
    let state = app.borrow();
    if let Ok(Some(selected)) = state
        .element("navigation")
        .query_selector("[aria-current=page]")
    {
        focus(&state.document, &selected);
    }
}

fn maybe_render(app: &Shared, kind: Kind) {
    let update = {
        let state = app.borrow();
        (state.view.kind() == Some(kind) || (kind == Kind::Live && state.view == View::Radio))
            && state.detail.is_none()
            && state.playback.is_none()
    };
    if update {
        ui::render_content(app);
    }
    ui::update_source_refresh(&app.borrow());
    if kind == Kind::Live {
        ui::refresh_group_editor(&app.borrow());
        live::refresh(app);
        if app.borrow().live_guide.quick_page().is_some() {
            load_visible_live_programmes(app);
        }
    }
}

fn refresh_source_catalogs(app: &Shared) {
    let (kinds, revision) = {
        let mut state = app.borrow_mut();
        if state.view != View::Settings
            || state.adding_source
            || !state.settings.source.configured()
            || state.source_refresh_busy()
        {
            return;
        }
        let kinds = state.source_catalog_kinds();
        state.source_refresh = SourceRefresh::Loading {
            remaining: kinds.iter().copied().collect(),
            error: None,
        };
        (kinds, state.revision)
    };
    let app = app.clone();
    spawn_local(async move {
        for &kind in kinds {
            if app.borrow().revision != revision {
                return;
            }
            if let Some(job) = catalog_job(&app, kind, true) {
                job.await;
            }
            io::after_frame().await;
        }
        let view_kind = {
            let mut state = app.borrow_mut();
            if state.revision != revision {
                return;
            }
            if let SourceRefresh::Loading { error, .. } = &mut state.source_refresh {
                state.source_refresh = SourceRefresh::Finished(error.take().map_or(Ok(()), Err));
                ui::update_source_refresh(&state);
            }
            state.view.kind()
        };
        if let Some(kind @ (Kind::Movie | Kind::Series)) = view_kind {
            load_catalog(&app, kind, false);
        }
    });
}

fn load_catalog(app: &Shared, kind: Kind, refresh: bool) {
    if matches!(app.borrow().source_refresh, SourceRefresh::Loading { .. }) {
        return;
    }
    if let Some(job) = catalog_job(app, kind, refresh) {
        spawn_local(job);
    }
}

fn catalog_job(
    app: &Shared,
    kind: Kind,
    refresh: bool,
) -> Option<impl Future<Output = ()> + use<>> {
    let kind = if kind == Kind::Radio {
        Kind::Live
    } else {
        kind
    };
    let (source, key, revision, preference, needs_preview, profile_id) = {
        let mut state = app.borrow_mut();
        if !state.settings.source.configured()
            || state.pending.contains(&kind)
            || (!refresh && (state.model.contains_key(&kind) || state.failures.contains_key(&kind)))
        {
            return None;
        }
        state.pending.insert(kind);
        state.failures.remove(&kind);
        state.vod_partial.remove(&kind);
        (
            state.settings.source.clone(),
            state.catalog_key(kind),
            state.revision,
            state.settings.preferred,
            !state.model.contains_key(&kind),
            state.settings.active_source.unwrap_or(0),
        )
    };
    maybe_render(app, kind);
    let app = app.clone();
    Some(async move {
        if matches!(kind, Kind::Movie | Kind::Series) {
            vod::load(
                &app,
                vod::Load {
                    source,
                    key,
                    kind,
                    revision,
                    preference,
                    refresh,
                    needs_preview,
                },
            )
            .await;
            return;
        }
        let preview_key = format!("{key}:first:{profile_id}");
        if needs_preview {
            let shown = restore_live_preview(&app, &preview_key, revision).await;
            if shown {
                io::after_frame().await;
            }
            let mut legacy = false;
            let mut cached = io::read_recent::<Vec<Item>>(&key, io::CATALOG_RETENTION_MS).await;
            if cached.is_none() && app.borrow().revision == revision {
                let old_key = key.trim_end_matches("groups-v3").to_owned() + "groups-v2";
                cached = io::read_recent::<Vec<Item>>(&old_key, io::CATALOG_RETENTION_MS).await;
                legacy = true;
            }
            if let Some(entry) = cached
                && !entry.value.is_empty()
            {
                let refreshing = refresh || legacy || entry.age_ms > io::CATALOG_FRESH_MS;
                publish_live_catalog(&app, revision, Ok(entry.value), refreshing);
                if !shown && !legacy {
                    store_live_cache(&app, revision, &key, &preview_key, Some(entry.timestamp_ms))
                        .await;
                }
                if !refreshing {
                    return;
                }
                io::after_frame().await;
            }
        }
        if app.borrow().revision != revision {
            return;
        }
        let result = sources::live_catalog(&source, || app.borrow().revision == revision).await;
        let downloaded = result
            .as_ref()
            .is_ok_and(|download| download.categorized && !download.items.is_empty());
        let data = result.and_then(|download| {
            if download.items.is_empty() {
                Err(tr("Geen zenders beschikbaar.").into())
            } else if !download.categorized && app.borrow().model.contains_key(&Kind::Live) {
                Err(tr("Zendergroepen konden niet worden geladen.").into())
            } else {
                Ok(download.items)
            }
        });
        publish_live_catalog(&app, revision, data, false);
        if downloaded {
            store_live_cache(&app, revision, &key, &preview_key, None).await;
        }
    })
}

async fn restore_live_preview(app: &Shared, key: &str, revision: u64) -> bool {
    let Some(entry) = io::read_recent::<LivePreview>(key, io::CATALOG_RETENTION_MS).await else {
        return false;
    };
    let mut state = app.borrow_mut();
    if state.revision != revision
        || state.live_groups() != entry.value.groups.as_ref()
        || entry.value.items.is_empty()
    {
        return false;
    }
    state
        .catalog_previews
        .insert(Kind::Live, Catalog::new(entry.value.items));
    drop(state);
    maybe_render(app, Kind::Live);
    true
}

async fn store_live_cache(
    app: &Shared,
    revision: u64,
    key: &str,
    preview_key: &str,
    snapshot_time: Option<f64>,
) {
    io::after_frame().await;
    let (preview, all) = {
        let state = app.borrow();
        let Some(catalog) = state
            .model
            .get(&Kind::Live)
            .filter(|_| state.revision == revision)
        else {
            return;
        };
        let mut seen = HashSet::new();
        let first_items = catalog
            .visible_items(false)
            .take(PAGE_SIZE)
            .chain(catalog.visible_items(true).take(PAGE_SIZE))
            .filter(|item| seen.insert(item.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let preview = LivePreview {
            groups: state.live_groups().cloned(),
            items: first_items,
        };
        (
            preview,
            snapshot_time.is_none().then(|| {
                catalog
                    .items
                    .iter()
                    .chain(
                        state
                            .model
                            .get(&Kind::Radio)
                            .into_iter()
                            .flat_map(|radio| &radio.items),
                    )
                    .cloned()
                    .collect::<Vec<_>>()
            }),
        )
    };
    if !preview.items.is_empty() {
        io::put_at(
            preview_key,
            &preview,
            snapshot_time.unwrap_or_else(js_sys::Date::now),
        )
        .await;
    }
    if let Some(all) = all
        && app.borrow().revision == revision
    {
        io::put(key, &all).await;
    }
}

fn publish_live_catalog(
    app: &Shared,
    revision: u64,
    data: Result<Vec<Item>, String>,
    refreshing: bool,
) {
    {
        let mut state = app.borrow_mut();
        if state.revision != revision {
            return;
        }
        if !refreshing {
            state.pending.remove(&Kind::Live);
            state.finish_source_refresh(Kind::Live, &data);
        }
        match data {
            Ok(items) => {
                let (radio, tv): (Vec<_>, Vec<_>) =
                    items.into_iter().partition(|item| item.kind == Kind::Radio);
                let mut catalog = Catalog::new(tv);
                if let Some(groups) = state.live_groups() {
                    catalog.apply_live_groups(groups);
                }
                state.catalog_previews.remove(&Kind::Live);
                state.model.insert(Kind::Live, catalog);
                state.model.remove(&Kind::Radio);
                if !radio.is_empty() {
                    state.model.insert(Kind::Radio, Catalog::new(radio));
                }
                for kind in [Kind::Live, Kind::Radio] {
                    state.sync_browse_filter(kind);
                    state.restore_catalog_page_for_focus(kind);
                }
            }
            Err(error) => {
                if !state.model.contains_key(&Kind::Live) {
                    state.failures.insert(Kind::Live, error);
                }
            }
        }
    }
    maybe_render(app, Kind::Live);
}

fn load_guide(app: &Shared, refresh: bool) {
    let (source, revision, key) = {
        let mut state = app.borrow_mut();
        if !state.settings.source.configured()
            || state.guide_pending
            || (!refresh && (state.guide.is_some() || state.guide_error.is_some()))
        {
            return;
        }
        state.guide_pending = true;
        state.guide_error = None;
        (
            state.settings.source.clone(),
            state.revision,
            format!("{}:guide:archive", state.source_key),
        )
    };
    if app.borrow().view == View::Guide {
        ui::render_content(app);
    }
    live::refresh(app);
    let app = app.clone();
    spawn_local(async move {
        if !refresh
            && let Some(entry) =
                io::read_recent::<Vec<Programme>>(&key, io::GUIDE_RETENTION_MS).await
        {
            let refreshing = entry.age_ms > io::GUIDE_FRESH_MS;
            publish_guide(&app, revision, Ok(entry.value), refreshing);
            if !refreshing {
                return;
            }
            io::after_frame().await;
        }
        if app.borrow().revision != revision {
            return;
        }
        let result = sources::guide(&source).await;
        let downloaded = result
            .as_ref()
            .is_ok_and(|programmes| !programmes.is_empty());
        publish_guide(&app, revision, result, false);
        if downloaded {
            io::after_frame().await;
            let copy = {
                let state = app.borrow();
                if state.revision != revision {
                    return;
                }
                state
                    .guide
                    .as_ref()
                    .map(|guide| guide.values().flatten().cloned().collect::<Vec<_>>())
            };
            if let Some(copy) = copy {
                io::put(&key, &copy).await;
            }
        }
    });
}

fn publish_guide(
    app: &Shared,
    revision: u64,
    result: Result<Vec<Programme>, String>,
    refreshing: bool,
) {
    let visible = {
        let mut state = app.borrow_mut();
        if state.revision != revision {
            return;
        }
        state.guide_pending = refreshing;
        match result {
            Ok(programmes) if programmes.is_empty() && state.guide.is_some() => {}
            Ok(programmes) => {
                let mut map: HashMap<String, Vec<Programme>> = HashMap::new();
                for programme in programmes {
                    map.entry(programme.channel.trim().to_ascii_lowercase())
                        .or_default()
                        .push(programme);
                }
                state.guide = Some(map);
            }
            Err(error) => {
                if state.guide.is_none() {
                    state.guide_error = Some(error);
                }
            }
        }
        matches!(state.view, View::Guide | View::Live) && state.playback.is_none()
    };
    if visible {
        ui::render_content(app);
    }
    live::refresh(app);
}

fn find_item(state: &App, key: &str) -> Option<Item> {
    state
        .view
        .kind()
        .and_then(|kind| {
            let (_, id) = key.split_once(':')?;
            [
                state.browse_catalog(kind),
                state.vod_partial.get(&kind),
                state.catalog_previews.get(&kind),
            ]
            .into_iter()
            .flatten()
            .find_map(|catalog| {
                catalog
                    .items
                    .iter()
                    .find(|item| item.id == id && item.key() == key)
            })
        })
        .cloned()
        .or_else(|| {
            let key = format!("{}:{key}", state.source_key);
            state
                .library
                .favorites
                .get(&key)
                .or_else(|| state.library.recent.get(&key))
                .or_else(|| {
                    state
                        .library
                        .series
                        .get(&key)
                        .map(|progress| &progress.series)
                })
                .cloned()
        })
}

fn flip_favorite(app: &Shared, key: &str) {
    let mut state = app.borrow_mut();
    if state.playback.is_some() {
        return;
    }
    let item = if let Some(detail) = &state.detail {
        (detail.item.key() == key).then(|| detail.item.clone())
    } else {
        find_item(&state, key)
    };
    let Some(item) = item.filter(|item| !item.kind.is_live_stream()) else {
        return;
    };
    let key = state.library_key(&item);
    let removed = state.library.favorites.remove(&key);
    if removed.is_none() {
        state.library.favorites.insert(key.clone(), item);
    }
    if !io::save_private(LIBRARY_KEY, &state.library) {
        if let Some(original) = removed {
            state.library.favorites.insert(key, original);
        } else {
            state.library.favorites.remove(&key);
        }
        state.note(tr("Mijn lijst opslaan mislukt op dit apparaat."));
        return;
    }
    drop(state);
    ui::render_content(app);
}

fn toggle_movie_watched(app: &Shared) {
    let mut state = app.borrow_mut();
    let Some(detail) = state.detail.as_ref().filter(|detail| {
        detail.item.kind == Kind::Movie && !detail.item.id.is_empty() && state.playback.is_none()
    }) else {
        return;
    };
    let key = state.library_key(&detail.item);
    let library = &mut state.library;
    if library.watched.remove(&key) {
        if !io::save_private(LIBRARY_KEY, library) {
            library.watched.insert(key);
            state.note(tr("Mijn lijst opslaan mislukt op dit apparaat."));
            return;
        }
    } else {
        library.watched.insert(key.clone());
        let position = library.positions.remove(&key);
        let recent = library.recent.remove(&key);
        let watching = library.watching.remove(&key);
        if !io::save_private(LIBRARY_KEY, library) {
            library.watched.remove(&key);
            library
                .positions
                .extend(position.map(|value| (key.clone(), value)));
            library
                .recent
                .extend(recent.map(|value| (key.clone(), value)));
            library.watching.extend(watching.map(|value| (key, value)));
            state.note(tr("Mijn lijst opslaan mislukt op dit apparaat."));
            return;
        }
    }
    state.note("");
    drop(state);
    ui::render_content(app);
}

fn select_item(app: &Shared, key: &str, resume: bool) {
    let item = {
        let state = app.borrow();
        resume
            .then(|| {
                state
                    .library
                    .recent
                    .get(&format!("{}:{key}", state.source_key))
                    .cloned()
            })
            .flatten()
            .or_else(|| find_item(&state, key))
    };
    let Some(item) = item else {
        return;
    };
    if item.kind.is_live_stream()
        || item.kind == Kind::Episode
        || (resume && item.kind == Kind::Movie)
    {
        player::start(app, item, false);
        return;
    }
    let (version, source) = {
        let mut state = app.borrow_mut();
        let version = state.next_sequence();
        let origin_page = state.page;
        state.page = 0;
        state.detail = Some(Detail {
            item: item.clone(),
            origin_page,
            resume_from_shelf: resume,
            episodes: Vec::new(),
            season: None,
            loading: true,
            error: None,
            version,
        });
        (version, state.settings.source.clone())
    };
    ui::render(app);
    ui::focus_catalog(app);
    let app = app.clone();
    spawn_local(async move {
        let mut item = item;
        let episodes = if item.kind == Kind::Series {
            sources::episodes(&source, &mut item).await
        } else {
            let _ = sources::movie(&source, &mut item).await;
            Ok(Vec::new())
        };
        let mut state = app.borrow_mut();
        let focus_resume = item.kind == Kind::Series
            && state
                .document
                .active_element()
                .is_some_and(|active| active.get_attribute("data-cmd").as_deref() == Some("back"));
        let season = episodes.as_ref().ok().and_then(|list| {
            state
                .series_target(&item, list)
                .map(|(episode, _)| episode)
                .or_else(|| list.first())
                .map(|episode| episode.season)
        });
        if state
            .detail
            .as_ref()
            .is_none_or(|detail| detail.version != version)
        {
            return;
        }
        if let Ok(episodes) = &episodes
            && let Some(progress) = state
                .series_progress(&item)
                .filter(|progress| progress.finished)
        {
            let has_next = Some(progress.target(episodes).is_some());
            let watched_episode = progress
                .matched_episode(episodes)
                .filter(|episode| !episode.id.is_empty())
                .map(|episode| state.library_key(episode));
            let restored = watched_episode.is_some_and(|key| state.library.watched.insert(key));
            let key = state.library_key(&item);
            let watch = state.library.watching.entry(key).or_default();
            if restored || watch.has_next != has_next {
                watch.has_next = has_next;
                io::save_private(LIBRARY_KEY, &state.library);
            }
        }
        let Some(detail) = &mut state.detail else {
            return;
        };
        detail.item = item;
        detail.loading = false;
        match episodes {
            Ok(list) => {
                detail.season = season;
                detail.episodes = list;
            }
            Err(error) => detail.error = Some(error),
        }
        let visible = state.playback.is_none();
        let resume_target = if resume && visible {
            state.detail.as_ref().and_then(|detail| {
                state
                    .series_target(&detail.item, &detail.episodes)
                    .map(|(item, _)| item.clone())
            })
        } else {
            None
        };
        drop(state);
        if let Some(episode) = resume_target {
            player::start(&app, episode, false);
        } else if visible {
            ui::render_content(&app);
            if focus_resume {
                ui::focus_catalog(&app);
            }
        }
    });
}

fn focus(document: &Document, element: &Element) {
    let Some(element) = element.dyn_ref::<HtmlElement>() else {
        return;
    };
    let _ = element.focus();
    let options = web_sys::ScrollIntoViewOptions::new();
    options.set_block(web_sys::ScrollLogicalPosition::Nearest);
    options.set_inline(web_sys::ScrollLogicalPosition::Nearest);
    element.scroll_into_view_with_scroll_into_view_options(&options);
    if element.closest("#content").ok().flatten().is_some()
        && let Ok(Some(topbar)) = document.query_selector(".topbar")
        && let Some(window) = document.default_view()
    {
        let overlap = topbar.get_bounding_client_rect().bottom() + 8.
            - element.get_bounding_client_rect().top();
        if overlap > 0. {
            window.scroll_by_with_x_and_y(0., -overlap);
        }
    }
}
