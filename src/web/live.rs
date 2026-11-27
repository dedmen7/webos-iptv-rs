use super::i18n::{message, tr};
use super::{
    App, LIBRARY_KEY, Shared, View, focus, io, load_catalog, load_guide, player, sources, ui,
};
use crate::{
    catalog::{Item, Kind, fold},
    guide::{Programme, now_and_next, overlap},
};
use js_sys::Date;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, HtmlElement, HtmlInputElement};

const WINDOW: i64 = 2 * 60 * 60;
const TIME_STEP: i64 = 30 * 60;
const QUICK_CHANNELS: usize = 8;
const GUIDE_CHANNELS: usize = 6;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Panel {
    Quick,
    Timeline,
}

#[derive(Clone, Default)]
pub(super) struct State {
    panel: Option<Panel>,
    page: usize,
    start: Option<i64>,
    query: String,
    selected: Option<(String, i64)>,
}

impl State {
    pub fn is_open(&self) -> bool {
        self.panel.is_some()
    }

    pub fn quick_page(&self) -> Option<usize> {
        (self.panel == Some(Panel::Quick)).then_some(self.page)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn archive_return(&self) -> Self {
        Self {
            panel: None,
            query: String::new(),
            ..self.clone()
        }
    }

    pub fn clear_selection_for_search(&mut self) {
        self.selected = None;
        self.page = 0;
    }

    fn page_size(&self) -> usize {
        if self.panel == Some(Panel::Quick) {
            QUICK_CHANNELS
        } else {
            GUIDE_CHANNELS
        }
    }

    fn from(&self) -> i64 {
        self.start.unwrap_or_else(window_now)
    }
}

fn window_now() -> i64 {
    io::now().div_euclid(TIME_STEP) * TIME_STEP
}

pub(super) fn time(unix: i64) -> String {
    let date = Date::new(&JsValue::from_f64(unix as f64 * 1000.));
    format!("{:02}:{:02}", date.get_hours(), date.get_minutes())
}

fn day(unix: i64) -> String {
    let date = Date::new(&JsValue::from_f64(unix as f64 * 1000.));
    format!("{:02}-{:02}", date.get_date(), date.get_month() + 1)
}

fn live_item(state: &App) -> Option<&Item> {
    state
        .playback
        .as_ref()
        .map(|play| &play.item)
        .filter(|item| item.kind == Kind::Live)
}

fn channels(state: &App) -> impl Iterator<Item = &Item> {
    state
        .model
        .get(&Kind::Live)
        .into_iter()
        .flat_map(|catalog| catalog.visible_items(state.settings.preferred))
}

pub(super) fn quick_page_channels(state: &App, page: usize) -> Vec<String> {
    let now = io::now();
    channels(state)
        .skip(page * QUICK_CHANNELS)
        .take(QUICK_CHANNELS)
        .filter(|item| {
            let (current, next) = programmes_for(state, &item.id, &item.epg_id, now);
            current.is_none() || next.is_none()
        })
        .map(|item| item.id.clone())
        .collect()
}

fn channel(state: &App, id: &str) -> Option<Item> {
    state
        .model
        .get(&Kind::Live)?
        .items
        .iter()
        .find(|item| item.id == id)
        .cloned()
}

fn schedule<'a>(state: &'a App, item: &Item) -> &'a [Programme] {
    schedule_by_epg_id(state, &item.epg_id)
}

fn schedule_by_epg_id<'a>(state: &'a App, epg_id: &str) -> &'a [Programme] {
    state
        .guide
        .as_ref()
        .and_then(|guide| guide.get(&epg_id.trim().to_ascii_lowercase()))
        .map(Vec::as_slice)
        .unwrap_or_default()
}

pub(super) fn programmes_for<'a>(
    state: &'a App,
    id: &str,
    epg_id: &str,
    now: i64,
) -> (Option<&'a Programme>, Option<&'a Programme>) {
    let full = schedule_by_epg_id(state, epg_id);
    let short = state
        .live_programmes
        .get(id)
        .map_or(&[][..], |(_, programmes)| programmes.as_slice());
    let current = now_and_next(full, now)
        .0
        .or_else(|| now_and_next(short, now).0);
    let after = current.map_or_else(|| now.saturating_add(1), |programme| programme.stop_unix);
    let next = [full, short]
        .into_iter()
        .filter_map(|programmes| {
            programmes.get(programmes.partition_point(|programme| programme.start_unix < after))
        })
        .min_by_key(|programme| programme.start_unix);
    (current, next)
}

pub fn record_playing(state: &mut App) {
    let Some(profile) = state.settings.active_source else {
        return;
    };
    let Some(item) = live_item(state).cloned() else {
        return;
    };
    if state
        .last_live
        .as_ref()
        .is_none_or(|last| last.id != item.id)
    {
        state.previous_live = state.last_live.replace(item.clone());
        update_last_channel_action(state);
    }
    if state.library.last_channel.get(&profile) != Some(&item.id) {
        state.library.last_channel.insert(profile, item.id.clone());
        io::save_private(LIBRARY_KEY, &state.library);
    }
}

pub fn remembered_channel(state: &App) -> Option<&Item> {
    let id = state
        .library
        .last_channel
        .get(&state.settings.active_source?)?;
    state
        .model
        .get(&Kind::Live)?
        .visible_items(state.settings.preferred)
        .find(|item| item.id == *id)
}

pub fn resume(app: &Shared) {
    let channel = remembered_channel(&app.borrow()).cloned();
    if let Some(channel) = channel {
        player::start(app, channel, false);
    }
}

fn neighbors<'a>(state: &'a App, item: &Item) -> Option<(&'a Item, &'a Item)> {
    state
        .model
        .get(&Kind::Live)
        .or_else(|| state.catalog_previews.get(&Kind::Live))?
        .live_neighbors_for_source(item, state.settings.preferred, &state.settings.source)
}

pub fn update_last_channel_action(state: &App) {
    state
        .element("last-channel")
        .unchecked_into::<HtmlElement>()
        .set_hidden(previous_channel(state).is_none());
}

fn previous_channel(state: &App) -> Option<&Item> {
    let active = live_item(state)?;
    state
        .last_live
        .as_ref()
        .filter(|last| last.id != active.id)
        .or(state.previous_live.as_ref())
        .filter(|item| item.id != active.id)
}

fn zap(app: &Shared, forward: bool) {
    let target = {
        let state = app.borrow();
        live_item(&state)
            .and_then(|item| neighbors(&state, item))
            .map(|(previous, next)| if forward { next } else { previous })
            .cloned()
    };
    if let Some(target) = target {
        player::start(app, target, false);
    }
}

pub fn clear(state: &mut App) {
    state.live_guide.reset();
    let _ = state.element("player").remove_attribute("data-guide-open");
    state
        .element("live-guide-modal")
        .unchecked_into::<HtmlElement>()
        .set_hidden(true);
}

pub fn handle_key(app: &Shared, key: &str) -> bool {
    if !matches!(
        key,
        "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" | "ChannelUp" | "ChannelDown"
    ) {
        return false;
    }
    let controls_hidden = {
        let state = app.borrow();
        if live_item(&state).is_none()
            || state.live_guide.is_open()
            || state.playback.as_ref().is_some_and(|play| play.panel)
        {
            return false;
        }
        state
            .element("player")
            .get_attribute("data-controls")
            .as_deref()
            == Some("false")
    };
    match key {
        "ChannelUp" | "ChannelDown" => {
            zap(app, key == "ChannelUp");
            true
        }
        "ArrowUp" => {
            open(app, Panel::Quick);
            true
        }
        "ArrowDown" | "ArrowLeft" | "ArrowRight" if controls_hidden => player::show_controls(app),
        _ => false,
    }
}

pub fn set_query(app: &Shared, query: String) {
    {
        let mut state = app.borrow_mut();
        if !state.live_guide.is_open() {
            return;
        }
        state.live_guide.query = query;
        state.live_guide.selected = None;
        state.live_guide.page = 0;
    }
    render_modal_results(app);
}

pub fn dispatch(app: &Shared, command: &str, value: &str) {
    match command {
        "live-guide-quick" => open(app, Panel::Quick),
        "live-guide-full" => open(app, Panel::Timeline),
        "live-guide-close" => close(app),
        "live-guide-refresh" => {
            load_catalog(app, Kind::Live, true);
            load_guide(app, true);
        }
        "live-guide-tune" => {
            let item = channel(&app.borrow(), value);
            if let Some(item) = item {
                if live_item(&app.borrow()).is_some_and(|active| active.id == item.id) {
                    close(app);
                } else {
                    player::start(app, item, false);
                }
            }
        }
        "live-guide-archive" => start_archive(app),
        "previous-live" => {
            let item = previous_channel(&app.borrow()).cloned();
            if let Some(item) = item {
                player::start(app, item, false);
            }
        }
        "live-guide-programme" => {
            let selected = value
                .rsplit_once(':')
                .and_then(|(id, time)| Some((id.to_owned(), time.parse().ok()?)));
            if selected.is_some() {
                app.borrow_mut().live_guide.selected = selected;
                redraw(app);
                let state = app.borrow();
                let root = state.element(if state.live_guide.is_open() {
                    "live-guide-content"
                } else {
                    "content"
                });
                if let Ok(Some(button)) =
                    root.query_selector(".guide-detail [data-cmd='live-guide-archive'], .guide-detail [data-cmd='live-guide-detail-back']")
                {
                    focus(&state.document, &button);
                }
            }
        }
        "live-guide-detail-back" => {
            let previous = app.borrow_mut().live_guide.selected.take();
            redraw(app);
            if let Some((id, start)) = previous {
                let state = app.borrow();
                let root = state.element(if state.live_guide.is_open() {
                    "live-guide-content"
                } else {
                    "content"
                });
                focus_command(
                    &state,
                    &root,
                    "live-guide-programme",
                    &format!("{id}:{start}"),
                );
            }
        }
        "live-guide-page" => {
            {
                let mut state = app.borrow_mut();
                let guide = &mut state.live_guide;
                guide.page = if value == "next" {
                    guide.page + 1
                } else {
                    guide.page.saturating_sub(1)
                };
                guide.selected = None;
            }
            super::load_visible_live_programmes(app);
            redraw(app);
        }
        "live-guide-time" => {
            {
                let mut state = app.borrow_mut();
                let guide = &mut state.live_guide;
                let offset = match value {
                    "prev" => -WINDOW,
                    "next" => WINDOW,
                    "prev-day" => -86_400,
                    "next-day" => 86_400,
                    _ => 0,
                };
                guide.start = (offset != 0).then(|| {
                    (guide.from() + offset).clamp(
                        window_now() - sources::GUIDE_DAYS * 86_400,
                        window_now() + sources::GUIDE_DAYS * 86_400 - WINDOW,
                    )
                });
                guide.selected = None;
            }
            redraw(app);
        }
        _ => {}
    }
}

pub fn back(app: &Shared) -> bool {
    let selector = {
        let state = app.borrow();
        (state.live_guide.is_open() || (state.view == View::Guide && state.playback.is_none()))
            && state.live_guide.selected.is_some()
    };
    if selector {
        dispatch(app, "live-guide-detail-back", "");
        return true;
    }
    if app.borrow().live_guide.is_open() {
        close(app);
        return true;
    }
    false
}

fn can_replay(item: &Item, programme: &Programme, now: i64) -> bool {
    item.archive.as_ref().is_some_and(|archive| {
        archive.available(programme.start_unix, programme.stop_unix, now)
            && (matches!(archive, crate::catalog::Archive::Xtream { .. })
                || archive.template_url(item, programme, now).is_some())
    })
}

fn archive_notice(state: &App, message: &str) {
    let root = state.element(if state.live_guide.is_open() {
        "live-guide-content"
    } else {
        "content"
    });
    if let Ok(Some(status)) = root.query_selector(".archive-notice") {
        status.set_text_content(Some(message));
    }
}

fn start_archive(app: &Shared) {
    let (item, programme, source, revision, request, selection) = {
        let mut state = app.borrow_mut();
        let Some(selection) = state.live_guide.selected.clone() else {
            return;
        };
        let Some(item) = channel(&state, &selection.0) else {
            return;
        };
        let Some(programme) = schedule(&state, &item)
            .iter()
            .find(|entry| entry.start_unix == selection.1 && can_replay(&item, entry, io::now()))
            .cloned()
        else {
            return;
        };
        archive_notice(&state, tr("Afspelen…"));
        let request = state.next_sequence();
        (
            item,
            programme,
            state.settings.source.clone(),
            state.revision,
            request,
            selection,
        )
    };
    let app = app.clone();
    spawn_local(async move {
        let urls = sources::archive_urls(&source, &item, &programme).await;
        {
            let state = app.borrow();
            if state.revision != revision
                || state.sequence != request
                || state.live_guide.selected.as_ref() != Some(&selection)
            {
                return;
            }
        }
        match urls {
            Ok(urls) => player::start_archive(&app, item, programme.name(), urls),
            Err(error) => archive_notice(&app.borrow(), &error),
        }
    });
}

fn open(app: &Shared, panel: Panel) {
    {
        let mut state = app.borrow_mut();
        let Some(item) = live_item(&state) else {
            return;
        };
        let page_size = if panel == Panel::Quick {
            QUICK_CHANNELS
        } else {
            GUIDE_CHANNELS
        };
        let index = channels(&state)
            .position(|channel| channel.id == item.id)
            .unwrap_or(0);
        state.live_guide = State {
            panel: Some(panel),
            page: index / page_size,
            ..State::default()
        };
        let _ = state.element("player").set_attribute("data-guide-open", "");
        state
            .element("live-guide-modal")
            .unchecked_into::<HtmlElement>()
            .set_hidden(false);
        state
            .element("live-guide-search")
            .unchecked_into::<HtmlInputElement>()
            .set_value("");
        state
            .element("live-guide-search")
            .unchecked_into::<HtmlInputElement>()
            .set_hidden(panel == Panel::Quick);
        state
            .element("live-guide-heading")
            .set_text_content(Some(if panel == Panel::Quick {
                tr("Snelle gids")
            } else {
                tr("Programmagids")
            }));
        state
            .element("live-guide-switch")
            .set_text_content(Some(if panel == Panel::Quick {
                tr("Hele gids")
            } else {
                tr("Snelle gids")
            }));
        let _ = state.element("live-guide-switch").set_attribute(
            "data-cmd",
            if panel == Panel::Quick {
                "live-guide-full"
            } else {
                "live-guide-quick"
            },
        );
    }
    load_catalog(app, Kind::Live, false);
    super::load_visible_live_programmes(app);
    if panel == Panel::Timeline {
        load_guide(app, false);
    }
    render_modal_results(app);
    let state = app.borrow();
    let root = state.element("live-guide-content");
    if let Some(item) = live_item(&state)
        && focus_command(&state, &root, "live-guide-tune", &item.id)
    {
        return;
    }
    focus(&state.document, &state.element("live-guide-close"));
}

fn close(app: &Shared) {
    let mut state = app.borrow_mut();
    if !state.live_guide.is_open() {
        return;
    }
    clear(&mut state);
    player::hide_controls(&state);
    drop(state);
    super::load_visible_live_programmes(app);
}

fn redraw(app: &Shared) {
    if app.borrow().live_guide.is_open() {
        render_modal_results(app);
    } else {
        ui::render_content(app);
    }
}

fn focus_command(state: &App, root: &Element, expected_cmd: &str, expected_value: &str) -> bool {
    let Ok(buttons) = root.query_selector_all("button[data-cmd]") else {
        return false;
    };
    for i in 0..buttons.length() {
        let Some(button) = buttons
            .item(i)
            .and_then(|node| node.dyn_into::<Element>().ok())
        else {
            continue;
        };
        if button.get_attribute("data-cmd").as_deref() == Some(expected_cmd)
            && button.get_attribute("data-value").as_deref() == Some(expected_value)
        {
            focus(&state.document, &button);
            return true;
        }
    }
    false
}

pub fn refresh(app: &Shared) {
    {
        let state = app.borrow();
        update_last_channel_action(&state);
        render_now(&state);
    }
    if app.borrow().live_guide.is_open() {
        render_modal_results(app);
    }
}

pub fn render_now(state: &App) {
    let root = state.element("live-now");
    let Some(item) = live_item(state) else {
        root.unchecked_into::<HtmlElement>().set_hidden(true);
        return;
    };
    root.clone()
        .unchecked_into::<HtmlElement>()
        .set_hidden(false);
    root.set_text_content(None);
    let now = io::now();
    let (current, next) = programmes_for(state, &item.id, &item.epg_id, now);
    let title = state.node(&root, "div", "live-current-channel", "");
    state.node(&title, "span", "live-indicator", "LIVE");
    state.node(&title, "strong", "", &item.title);
    if current.is_none() && next.is_none() {
        let hint = if state.guide.is_none() && !state.guide_pending && state.guide_error.is_none() {
            tr("Programma-informatie via Snelle gids.")
        } else {
            epg_message(state)
        };
        state.node(&root, "p", "live-epg-hint", hint);
        return;
    }
    let pair = state.node(&root, "div", "live-programmes", "");
    if let Some(programme) = current {
        let tile = programme_summary(state, &pair, tr("Nu"), programme);
        progress(state, &tile, programme, now);
    } else {
        state.node(
            &pair,
            "p",
            "live-programme-card muted",
            tr("Geen lopend programma in de gids."),
        );
    }
    if let Some(programme) = next {
        programme_summary(state, &pair, tr("Straks"), programme);
    }
}

fn programme_summary(
    state: &App,
    parent: &Element,
    caption: &str,
    programme: &Programme,
) -> Element {
    let card = state.node(parent, "div", "live-programme-card", "");
    let range = format!(
        "{caption} · {}–{}",
        time(programme.start_unix),
        time(programme.stop_unix)
    );
    state.node(&card, "span", "live-programme-time", &range);
    state.node(&card, "strong", "", programme.name());
    card
}

pub(super) fn progress_track(state: &App, parent: &Element) -> Element {
    let track = state.node(parent, "span", "live-progress", "");
    let _ = track.set_attribute("role", "progressbar");
    let _ = track.set_attribute("aria-label", tr("Voortgang programma"));
    state.node(&track, "span", "", "");
    track
}

pub(super) fn update_progress(track: &Element, programme: Option<&Programme>, now: i64) {
    let hidden = programme.is_none();
    if track.has_attribute("hidden") != hidden
        && let Some(track) = track.dyn_ref::<HtmlElement>()
    {
        track.set_hidden(hidden);
    }
    let Some(programme) = programme else { return };
    let length = programme
        .stop_unix
        .saturating_sub(programme.start_unix)
        .max(1) as f64;
    let passed = now.saturating_sub(programme.start_unix);
    set_progress_fraction(track, passed as f64 / length);
}

pub(super) fn set_progress_fraction(track: &Element, fraction: f64) {
    let percent = (fraction * 100.).clamp(0., 100.);
    let _ = track.set_attribute("aria-valuenow", &format!("{percent:.0}"));
    if let Some(fill) = track.first_element_child() {
        let _ = fill.set_attribute("style", &format!("width:{percent:.2}%"));
    }
}

fn progress(state: &App, parent: &Element, programme: &Programme, now: i64) {
    let track = progress_track(state, parent);
    update_progress(&track, Some(programme), now);
    if parent.class_name() == "live-programme-card" {
        let left = ((programme.stop_unix - now).max(0) + 59) / 60;
        state.node(
            parent,
            "span",
            "live-programme-time",
            &message("Nog {minutes} min", &[("minutes", &left.to_string())]),
        );
    }
}

fn epg_message(state: &App) -> &str {
    if state.guide_pending || state.live_programmes_pending.is_some() {
        tr("Programma-informatie ophalen…")
    } else {
        tr("Geen programma-informatie beschikbaar.")
    }
}

fn render_modal_results(app: &Shared) {
    let state = app.borrow();
    let root = state.element("live-guide-content");
    let key = state
        .document
        .active_element()
        .filter(|element| root.contains(Some(element)))
        .and_then(|element| ui::focus_identity(&element));
    super::poster::discard_under(&root);
    root.set_text_content(None);
    if state.live_guide.panel == Some(Panel::Quick) {
        render_quick(&state, &root);
    } else {
        render_timeline(&state, &root);
    }
    if let Some((cmd, value)) = key
        && !focus_command(&state, &root, &cmd, &value)
        && let Ok(Some(next)) = root.query_selector(
            "button[data-cmd='live-guide-tune'], button[data-cmd='live-guide-programme']",
        )
    {
        focus(&state.document, &next);
    }
}

pub fn render_guide(state: &App, parent: &Element) {
    let header = state.header(parent, tr("Programmagids"), "");
    if !state.settings.source.configured() {
        state.button(&header, tr("Bron instellen"), "view", "settings", "primary");
        return;
    }
    state.button(&header, tr("Verversen"), "live-guide-refresh", "", "");
    if let Some(error) = &state.guide_error {
        state.placeholder(parent, error);
    }
    render_timeline(state, parent);
}

fn render_quick(state: &App, parent: &Element) {
    let offset = state.live_guide.page * QUICK_CHANNELS;
    let mut visible = Vec::with_capacity(QUICK_CHANNELS);
    let mut count = 0;
    for (index, item) in channels(state).enumerate() {
        if (offset..offset + QUICK_CHANNELS).contains(&index) {
            visible.push(item);
        }
        count += 1;
    }
    if count == 0 {
        state.placeholder(
            parent,
            if state.pending.contains(&Kind::Live) {
                tr("Zenders laden…")
            } else {
                tr("Geen zenders beschikbaar.")
            },
        );
        return;
    }
    let tiles = state.node(parent, "div", "quick-channel-grid", "");
    let now = io::now();
    for item in visible {
        let tile = tune_tile(state, &tiles, item, "quick-channel");
        let (current, next) = programmes_for(state, &item.id, &item.epg_id, now);
        if let Some(current) = current {
            state.node(&tile, "strong", "quick-programme-title", current.name());
            progress(state, &tile, current, now);
        } else {
            let message = if state.guide.is_none()
                && !state.guide_pending
                && !state.settings.source.playlist.is_empty()
            {
                tr("Programma-informatie via Hele gids.")
            } else {
                epg_message(state)
            };
            state.node(&tile, "span", "live-programme-time", message);
        }
        if let Some(next) = next {
            state.node(
                &tile,
                "span",
                "live-programme-time quick-next",
                &format!("{} · {}", time(next.start_unix), next.name()),
            );
        }
    }
    pages(state, parent, count);
}

fn tune_tile(state: &App, parent: &Element, item: &Item, class: &str) -> Element {
    let button = state.button(parent, "", "live-guide-tune", &item.id, class);
    let top = state.node(&button, "span", "guide-channel-heading", "");
    if !item.poster.is_empty() || !item.poster_fallback.is_empty() {
        state.poster(&top, item);
    }
    state.node(&top, "strong", "guide-channel-name", &item.title);
    if live_item(state).is_some_and(|active| active.id == item.id) {
        let _ = button.set_attribute("aria-current", "true");
    }
    button
}

fn render_timeline(state: &App, parent: &Element) {
    if let Some((id, start)) = &state.live_guide.selected {
        programme_detail(state, parent, id, *start);
    }
    let channels = channels(state).collect::<Vec<_>>();
    if channels.is_empty() {
        state.placeholder(
            parent,
            if state.pending.contains(&Kind::Live) {
                tr("Zenders laden…")
            } else {
                tr("Geen zenders beschikbaar.")
            },
        );
        return;
    }
    let query = if state.live_guide.is_open() {
        &state.live_guide.query
    } else {
        &state.query
    };
    if !query.trim().is_empty() {
        programme_search(state, parent, &channels, query);
        return;
    }
    let start = state.live_guide.from();
    let end = start + WINDOW;
    let bar = state.node(parent, "div", "guide-toolbar", "");
    state.button(&bar, tr("Vorige dag"), "live-guide-time", "prev-day", "");
    state.button(&bar, tr("Eerder"), "live-guide-time", "prev", "");
    state.button(&bar, tr("Nu"), "live-guide-time", "now", "");
    state.button(&bar, tr("Later"), "live-guide-time", "next", "");
    state.button(&bar, tr("Volgende dag"), "live-guide-time", "next-day", "");
    state.node(
        &bar,
        "span",
        "live-programme-time",
        &format!("{} · {}–{}", day(start), time(start), time(end)),
    );
    let timeline = state.node(parent, "section", "programme-timeline", "");
    let heading = state.node(&timeline, "div", "guide-row guide-hours", "");
    state.node(&heading, "span", "", tr("Zender"));
    let ruler = state.node(&heading, "div", "guide-ruler", "");
    for timestamp in (start..end).step_by(TIME_STEP as usize) {
        state.node(&ruler, "time", "", &time(timestamp));
    }
    let now = io::now();
    let count = state.live_guide.page_size();
    for item in channels
        .iter()
        .skip(state.live_guide.page * count)
        .take(count)
    {
        let row = state.node(&timeline, "div", "guide-row", "");
        tune_tile(state, &row, item, "guide-channel");
        let grid = state.node(&row, "div", "guide-track", "");
        let programmes = schedule(state, item);
        let mut visible = false;
        let first = programmes.partition_point(|entry| entry.start_unix < start - 43_200);
        for programme in programmes[first..]
            .iter()
            .take_while(|entry| entry.start_unix < end)
        {
            let Some((left, width)) = overlap(programme, start..end) else {
                continue;
            };
            visible = true;
            let tile = programme_tile(state, &grid, item, programme, "guide-programme");
            let _ = tile.set_attribute(
                "style",
                &format!("left:{:.3}%;width:{:.3}%", left * 100., width * 100.),
            );
            if (programme.start_unix..programme.stop_unix).contains(&now) {
                let _ = tile.set_attribute("aria-current", "time");
            }
        }
        if !visible {
            state.node(&grid, "p", "guide-missing", epg_message(state));
        }
        if (start..end).contains(&now) {
            let mark = state.node(&grid, "span", "guide-now-marker", "");
            let _ = mark.set_attribute(
                "style",
                &format!("left:{:.3}%", (now - start) as f64 / WINDOW as f64 * 100.),
            );
        }
    }
    pages(state, parent, channels.len());
}

fn programme_tile(
    state: &App,
    parent: &Element,
    item: &Item,
    programme: &Programme,
    class: &str,
) -> Element {
    let key = format!("{}:{}", item.id, programme.start_unix);
    let button = state.button(parent, "", "live-guide-programme", &key, class);
    state.node(
        &button,
        "span",
        "live-programme-time",
        &format!(
            "{}–{}",
            time(programme.start_unix),
            time(programme.stop_unix)
        ),
    );
    state.node(&button, "strong", "", programme.name());
    if can_replay(item, programme, io::now()) {
        let _ = button.set_attribute("data-replay", "");
        let label = format!("{} · {}", programme.name(), tr("Terugkijken"));
        let _ = button.set_attribute("aria-label", &label);
    }
    button
}

fn programme_search(state: &App, parent: &Element, channels: &[&Item], query: &str) {
    let query = fold(query);
    if query.is_empty() {
        return;
    }
    let words: Vec<_> = query.split_whitespace().collect();
    let now = io::now();
    let results = state.node(parent, "div", "guide-search-grid", "");
    let mut count = 0;
    for item in channels {
        for programme in schedule(state, item)
            .iter()
            .filter(|entry| entry.stop_unix > now || can_replay(item, entry, now))
        {
            let label = fold(programme.name());
            if !words.iter().all(|word| label.contains(word)) {
                continue;
            }
            let tile = programme_tile(state, &results, item, programme, "guide-result-tile");
            state.node(
                &tile,
                "span",
                "live-programme-time",
                &format!("{} · {}", day(programme.start_unix), item.title),
            );
            count += 1;
            if count == 60 {
                return;
            }
        }
    }
    if count == 0 {
        state.placeholder(
            parent,
            if state.guide_pending {
                tr("Programma’s laden…")
            } else {
                tr("Geen programma's gevonden.")
            },
        );
    }
}

fn programme_detail(state: &App, parent: &Element, id: &str, start: i64) {
    let Some(item) = channel(state, id) else {
        return;
    };
    let Some(programme) = schedule(state, &item)
        .iter()
        .find(|entry| entry.start_unix == start)
    else {
        return;
    };
    let card = state.node(parent, "article", "guide-detail", "");
    let info = state.node(&card, "div", "", "");
    state.node(
        &info,
        "span",
        "live-programme-time",
        &format!(
            "{} · {}–{}",
            item.title,
            time(start),
            time(programme.stop_unix)
        ),
    );
    state.node(&info, "h2", "", programme.name());
    if let Some(description) = programme.description.first() {
        state.node(&info, "p", "", description);
    }
    let actions = state.node(&card, "div", "actions", "");
    if can_replay(&item, programme, io::now()) {
        state.button(
            &actions,
            tr("Terugkijken"),
            "live-guide-archive",
            "",
            "primary",
        );
        let status = state.node(&card, "p", "notice archive-notice", "");
        let _ = status.set_attribute("role", "status");
    }
    state.button(&actions, tr("Kijk live"), "live-guide-tune", id, "primary");
    state.button(&actions, tr("Terug"), "live-guide-detail-back", "", "");
}

fn pages(state: &App, parent: &Element, count: usize) {
    let per_page = state.live_guide.page_size();
    if count <= per_page {
        return;
    }
    let bar = state.node(parent, "div", "pagination guide-pagination", "");
    if state.live_guide.page > 0 {
        state.button(&bar, tr("Vorige zenders"), "live-guide-page", "prev", "");
    }
    state.node(
        &bar,
        "span",
        "live-programme-time",
        &message(
            "{page} van {total}",
            &[
                ("page", &(state.live_guide.page + 1).to_string()),
                ("total", &count.div_ceil(per_page).to_string()),
            ],
        ),
    );
    if (state.live_guide.page + 1) * per_page < count {
        state.button(&bar, tr("Volgende zenders"), "live-guide-page", "next", "");
    }
}
