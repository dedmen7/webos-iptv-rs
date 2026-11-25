mod io;
mod player;
mod poster;
mod sources;
mod ui;

use crate::{
    catalog::{self, Catalog, Item, Kind, Source},
    guide::Programme,
    subtitles::Subtitle,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    Document, Element, Event, HtmlElement, HtmlInputElement, HtmlSelectElement, HtmlVideoElement,
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
    preferred: bool,
    opensubtitles: String,
    subdl: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Library {
    favorites: HashMap<String, Item>,
    positions: HashMap<String, f64>,
    recent: HashMap<String, Item>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum View {
    Live,
    Guide,
    Movies,
    Series,
    Library,
    Settings,
}

impl View {
    const ALL: [Self; 6] = [
        Self::Live,
        Self::Guide,
        Self::Movies,
        Self::Series,
        Self::Library,
        Self::Settings,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Live => "Live",
            Self::Guide => "Gids",
            Self::Movies => "Films",
            Self::Series => "Series",
            Self::Library => "Mijn lijst",
            Self::Settings => "Instellingen",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Guide => "guide",
            Self::Movies => "movies",
            Self::Series => "series",
            Self::Library => "library",
            Self::Settings => "settings",
        }
    }

    fn kind(self) -> Option<Kind> {
        match self {
            Self::Live | Self::Guide => Some(Kind::Live),
            Self::Movies => Some(Kind::Movie),
            Self::Series => Some(Kind::Series),
            _ => None,
        }
    }
}

struct Detail {
    item: Item,
    episodes: Vec<Item>,
    season: Option<u32>,
    loading: bool,
    error: Option<String>,
    version: u64,
}

struct Playback {
    item: Item,
    version: u64,
    last_saved: f64,
    resume_at: f64,
    selected_subtitle: Option<(Subtitle, String)>,
    tracks_url: Option<String>,
    subtitle_offset: f64,
    subtitle_results: Vec<Subtitle>,
    subtitle_search: u64,
    panel: bool,
    controls_until: i64,
}

struct App {
    document: Document,
    settings: Settings,
    library: Library,
    source_key: String,
    view: View,
    model: HashMap<Kind, Catalog>,
    pending: HashSet<Kind>,
    failures: HashMap<Kind, String>,
    query: String,
    category: String,
    page: usize,
    detail: Option<Detail>,
    playback: Option<Playback>,
    guide: Option<HashMap<String, Vec<Programme>>>,
    guide_pending: bool,
    guide_error: Option<String>,
    guide_channel: String,
    revision: u64,
    sequence: u64,
}

impl App {
    fn element(&self, id: &str) -> Element {
        self.document.get_element_by_id(id).expect("app element")
    }

    fn video(&self) -> HtmlVideoElement {
        self.element("video").unchecked_into()
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

    fn note(&self, message: &str) {
        self.element("notice").set_text_content(Some(message));
    }

    fn catalog_key(&self, kind: Kind) -> String {
        format!("{}:{kind:?}", self.source_key)
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

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or("Document ontbreekt")?;
    let settings: Settings = io::saved(CONFIG_KEY).unwrap_or_default();
    let view = if settings.source.configured() {
        View::Live
    } else {
        View::Settings
    };
    let source_key = App::source_fingerprint(&settings.source);
    let app = Rc::new(RefCell::new(App {
        document: document.clone(),
        settings,
        library: io::saved(LIBRARY_KEY).unwrap_or_default(),
        source_key,
        view,
        model: HashMap::new(),
        pending: HashSet::new(),
        failures: HashMap::new(),
        query: String::new(),
        category: String::new(),
        page: 0,
        detail: None,
        playback: None,
        guide: None,
        guide_pending: false,
        guide_error: None,
        guide_channel: String::new(),
        revision: 0,
        sequence: 0,
    }));
    wire(&app)?;
    player::wire(&app)?;
    ui::render(&app);
    document
        .document_element()
        .ok_or("Pagina ontbreekt")?
        .set_attribute("data-ready", "true")?;
    open_view(&app, view);
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
        let Ok(Some(button)) = target.closest("[data-cmd]") else {
            return;
        };
        let command = button.get_attribute("data-cmd").unwrap_or_default();
        let value = button.get_attribute("data-value").unwrap_or_default();
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
            let mut app = input_app.borrow_mut();
            app.query = input.value();
            app.page = 0;
            drop(app);
            ui::render_search(&input_app);
        }
    })?;
    let select_app = app.clone();
    on(&document, "change", move |event| {
        let Some(select) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlSelectElement>().ok())
        else {
            return;
        };
        {
            let mut app = select_app.borrow_mut();
            match select.id().as_str() {
                "category" => {
                    app.category = select.value();
                    app.page = 0;
                }
                "guide-channel" => app.guide_channel = select.value(),
                "series-season" => {
                    if let Some(detail) = &mut app.detail {
                        detail.season = select.value().parse().ok();
                    }
                    app.page = 0;
                }
                _ => return,
            }
        }
        ui::render_content(&select_app);
    })?;
    let key_app = app.clone();
    on(&document, "keydown", move |event| {
        let Ok(event) = event.dyn_into::<KeyboardEvent>() else {
            return;
        };
        let key = event.key();
        let key = if key.is_empty() || key == "Unidentified" {
            match event.key_code() {
                13 => "Enter".to_owned(),
                37 => "ArrowLeft".to_owned(),
                38 => "ArrowUp".to_owned(),
                39 => "ArrowRight".to_owned(),
                40 => "ArrowDown".to_owned(),
                _ => key,
            }
        } else {
            key
        };
        if key == "Escape" || event.key_code() == 461 {
            event.prevent_default();
            back(&key_app);
            return;
        }
        if player::show_controls(&key_app)
            && matches!(
                key.as_str(),
                "Enter" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight"
            )
        {
            event.prevent_default();
            return;
        }
        let target = event
            .target()
            .and_then(|element| element.dyn_into::<Element>().ok());
        let target_id = target.as_ref().map(Element::id).unwrap_or_default();
        if key == "Enter" && target_id == "subtitle-query" {
            event.prevent_default();
            player::search(&key_app);
            return;
        }
        let tag = target.as_ref().map(Element::tag_name);
        if tag
            .as_deref()
            .is_some_and(|tag| matches!(tag, "INPUT" | "SELECT"))
            && (!matches!(key.as_str(), "ArrowUp" | "ArrowDown")
                || tag.as_deref() == Some("SELECT"))
        {
            return;
        }
        if ui::navigate(&key_app, &key) {
            event.prevent_default();
        }
    })?;
    Ok(())
}

fn dispatch(app: &Shared, cmd: &str, value: &str) {
    match cmd {
        "view" => {
            if let Some(view) = View::ALL.into_iter().find(|view| view.id() == value) {
                open_view(app, view);
            }
        }
        "preferred" => {
            let mut state = app.borrow_mut();
            state.settings.preferred = !state.settings.preferred;
            state.category.clear();
            state.page = 0;
            io::save(CONFIG_KEY, &state.settings);
            ui::mark_filter(&state);
            let settings_open = state.view == View::Settings;
            drop(state);
            if !settings_open {
                ui::render_content(app);
            }
        }
        "save" => save_settings(app),
        "refresh" => {
            let view = app.borrow().view;
            if let Some(kind) = view.kind() {
                load_catalog(app, kind, true);
            }
            if view == View::Guide {
                load_guide(app, true);
            }
        }
        "page" => {
            let mut state = app.borrow_mut();
            state.page = if value == "prev" {
                state.page.saturating_sub(1)
            } else {
                state.page + 1
            };
            drop(state);
            ui::render_content(app);
            ui::focus_catalog(app);
        }
        "item" => select_item(app, value),
        "episode" => {
            let item = app.borrow().detail.as_ref().and_then(|detail| {
                detail
                    .episodes
                    .iter()
                    .find(|item| item.id == value)
                    .cloned()
            });
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
        "back" => back(app),
        command => player::dispatch(app, command, value),
    }
}

fn back(app: &Shared) {
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
        return;
    }
    let mut state = app.borrow_mut();
    if state.detail.take().is_some() {
        drop(state);
        ui::render(app);
        ui::focus_catalog(app);
    } else if state.view != View::Live && state.settings.source.configured() {
        drop(state);
        open_view(app, View::Live);
    }
}

fn open_view(app: &Shared, view: View) {
    {
        let mut state = app.borrow_mut();
        state.view = view;
        state.detail = None;
        state.category.clear();
        state.page = 0;
        state.query.clear();
        state.note("");
        let input: HtmlInputElement = state.element("search").unchecked_into();
        input.set_value("");
    }
    ui::render(app);
    if let Some(kind) = view.kind() {
        load_catalog(app, kind, false);
    }
    if view == View::Guide {
        load_guide(app, false);
    }
}

fn save_settings(app: &Shared) {
    let mut state = app.borrow_mut();
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
    if !source.configured() {
        state.note("Vul een Xtream-server of een M3U-link in.");
        return;
    }
    if [&source.server, &source.playlist, &source.guide]
        .iter()
        .any(|url| !url.is_empty() && catalog::http_url(url).is_none())
    {
        state.note("Gebruik een geldige HTTP- of HTTPS-link.");
        return;
    }
    if !source.server.is_empty() && (source.username.is_empty() || source.password.is_empty()) {
        state.note("Vul ook de Xtream-gebruikersnaam en het wachtwoord in.");
        return;
    }
    let settings = Settings {
        source,
        opensubtitles: field("source-os"),
        subdl: field("source-subdl"),
        preferred: state.settings.preferred,
    };
    if !io::save(CONFIG_KEY, &settings) {
        state.note("Instellingen opslaan mislukt op dit apparaat.");
        return;
    }
    state.source_key = App::source_fingerprint(&settings.source);
    state.settings = settings;
    state.revision += 1;
    state.model.clear();
    state.pending.clear();
    state.failures.clear();
    state.guide = None;
    state.guide_pending = false;
    state.guide_error = None;
    state.guide_channel.clear();
    drop(state);
    open_view(app, View::Live);
}

fn maybe_render(app: &Shared, kind: Kind) {
    let update = {
        let state = app.borrow();
        state.view.kind() == Some(kind) && state.detail.is_none() && state.playback.is_none()
    };
    if update {
        ui::render_content(app);
    }
}

fn load_catalog(app: &Shared, kind: Kind, refresh: bool) {
    let (source, key, revision) = {
        let mut state = app.borrow_mut();
        if !state.settings.source.configured()
            || state.pending.contains(&kind)
            || (!refresh && (state.model.contains_key(&kind) || state.failures.contains_key(&kind)))
        {
            return;
        }
        state.pending.insert(kind);
        state.failures.remove(&kind);
        (
            state.settings.source.clone(),
            state.catalog_key(kind),
            state.revision,
        )
    };
    maybe_render(app, kind);
    let app = app.clone();
    spawn_local(async move {
        let cached = if refresh {
            None
        } else {
            io::cached::<Vec<Item>>(&key).await
        };
        let data = match cached {
            Some(items) => Ok(items),
            None => {
                let result = sources::catalog(&source, kind).await;
                if let Ok(items) = &result {
                    let cache_key = key.clone();
                    let data = items.clone();
                    spawn_local(async move {
                        io::put(&cache_key, &data).await;
                    });
                }
                result
            }
        };
        {
            let mut state = app.borrow_mut();
            if state.revision != revision {
                return;
            }
            state.pending.remove(&kind);
            match data {
                Ok(items) => {
                    state.model.insert(kind, Catalog::new(items));
                }
                Err(error) => {
                    state.failures.insert(kind, error);
                }
            }
        }
        maybe_render(&app, kind);
    });
}

fn load_guide(app: &Shared, refresh: bool) {
    let (source, revision) = {
        let mut state = app.borrow_mut();
        if !state.settings.source.configured()
            || state.guide_pending
            || (!refresh && (state.guide.is_some() || state.guide_error.is_some()))
        {
            return;
        }
        state.guide_pending = true;
        state.guide_error = None;
        (state.settings.source.clone(), state.revision)
    };
    maybe_render(app, Kind::Live);
    let app = app.clone();
    spawn_local(async move {
        let result = sources::guide(&source).await;
        let mut state = app.borrow_mut();
        if state.revision != revision {
            return;
        }
        state.guide_pending = false;
        match result {
            Ok(programmes) => {
                let mut map: HashMap<String, Vec<Programme>> = HashMap::new();
                for programme in programmes {
                    map.entry(programme.channel.clone())
                        .or_default()
                        .push(programme);
                }
                state.guide = Some(map);
            }
            Err(error) => state.guide_error = Some(error),
        }
        let visible = state.view == View::Guide;
        drop(state);
        if visible {
            ui::render_content(&app);
        }
    });
}

fn find_item(state: &App, key: &str) -> Option<Item> {
    state
        .view
        .kind()
        .and_then(|kind| state.model.get(&kind))
        .and_then(|catalog| catalog.items.iter().find(|item| item.key() == key))
        .cloned()
        .or_else(|| {
            let key = format!("{}:{key}", state.source_key);
            state
                .library
                .favorites
                .get(&key)
                .or_else(|| state.library.recent.get(&key))
                .cloned()
        })
}

fn flip_favorite(app: &Shared, key: &str) {
    let mut state = app.borrow_mut();
    let item = state
        .detail
        .as_ref()
        .map(|detail| detail.item.clone())
        .or_else(|| find_item(&state, key));
    let Some(item) = item else {
        return;
    };
    let key = state.library_key(&item);
    if state.library.favorites.remove(&key).is_none() {
        state.library.favorites.insert(key, item);
    }
    io::save(LIBRARY_KEY, &state.library);
    drop(state);
    ui::render_content(app);
}

fn select_item(app: &Shared, key: &str) {
    let item = find_item(&app.borrow(), key);
    let Some(item) = item else {
        return;
    };
    if matches!(item.kind, Kind::Live | Kind::Episode) {
        player::start(app, item, false);
        return;
    }
    let (version, source) = {
        let mut state = app.borrow_mut();
        let version = state.next_sequence();
        state.page = 0;
        state.detail = Some(Detail {
            item: item.clone(),
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
            sources::episodes(&source, &item).await
        } else {
            let _ = sources::movie(&source, &mut item).await;
            Ok(Vec::new())
        };
        let mut state = app.borrow_mut();
        let Some(detail) = &mut state.detail else {
            return;
        };
        if detail.version != version {
            return;
        }
        detail.item = item;
        detail.loading = false;
        match episodes {
            Ok(list) => {
                detail.season = list.first().map(|episode| episode.season);
                detail.episodes = list;
            }
            Err(error) => detail.error = Some(error),
        }
        let visible = state.playback.is_none();
        drop(state);
        if visible {
            ui::render_content(&app);
        }
    });
}

fn focus(document: &Document, element: &Element) {
    if let Some(element) = element.dyn_ref::<HtmlElement>() {
        let _ = element.focus();
        let options = web_sys::ScrollIntoViewOptions::new();
        options.set_block(web_sys::ScrollLogicalPosition::Nearest);
        options.set_inline(web_sys::ScrollLogicalPosition::Nearest);
        element.scroll_into_view_with_scroll_into_view_options(&options);
    }
    let _ = document;
}
