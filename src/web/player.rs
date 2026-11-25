use super::{App, LIBRARY_KEY, Playback, Shared, focus, io, on, sources, ui};
use crate::{
    catalog::{Item, Kind, Language, http_url, language, subtitle_title},
    subtitles::{self, Download, Subtitle},
};
use js_sys::{Array, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    Blob, BlobPropertyBag, HtmlElement, HtmlInputElement, HtmlTrackElement, TextTrackMode,
};

pub fn wire(app: &Shared) -> Result<(), JsValue> {
    let video = app.borrow().video();
    let shared = app.clone();
    on(&video, "loadedmetadata", move |_| {
        let state = shared.borrow();
        if let Some(play) = &state.playback {
            let video = state.video();
            if play.resume_at > 0.
                && video.duration().is_finite()
                && play.resume_at < video.duration() - 10.
            {
                video.set_current_time(play.resume_at);
            }
        }
        refresh_tracks(&state);
    })?;
    let shared = app.clone();
    on(&video, "timeupdate", move |_| {
        save_position(&shared, false, false)
    })?;
    let shared = app.clone();
    on(&video, "ended", move |_| {
        save_position(&shared, true, true);
        shared.borrow().player_note("Einde van deze titel");
    })?;
    let shared = app.clone();
    on(&video, "error", move |_| {
        let state = shared.borrow();
        if state.playback.is_some() {
            state.player_note("Deze stream kon niet worden afgespeeld.");
        }
    })?;
    let shared = app.clone();
    let tracks = video.text_tracks().ok_or("Ondertitelsporen ontbreken")?;
    on(&tracks, "addtrack", move |_| {
        refresh_tracks(&shared.borrow())
    })?;
    let shared = app.clone();
    on(&video, "playing", move |_| {
        let state = shared.borrow();
        if state.playback.is_some()
            && !state
                .playback
                .as_ref()
                .is_some_and(|playback| playback.panel)
        {
            state.player_note("");
        }
        refresh_tracks(&state);
    })?;
    let shared = app.clone();
    on(&video, "click", move |_| {
        if !show_controls(&shared) {
            toggle(&shared);
        }
    })?;
    let shared = app.clone();
    let player = app.borrow().element("player");
    on(&player, "mousemove", move |_| {
        show_controls(&shared);
    })?;
    let app = app.clone();
    let shared_for_sync = app.clone();
    let hide_controls = Closure::<dyn FnMut()>::new(move || {
        let state = app.borrow();
        if state
            .playback
            .as_ref()
            .is_some_and(|play| !play.panel && play.controls_until < io::now())
            && !state.video().paused()
        {
            let _ = state
                .element("player")
                .set_attribute("data-controls", "false");
        }
    });
    web_sys::window()
        .ok_or("Browservenster ontbreekt")?
        .set_interval_with_callback_and_timeout_and_arguments_0(
            hide_controls.as_ref().unchecked_ref(),
            1000,
        )?;
    hide_controls.forget();
    let samples = shared_for_sync.clone();
    let sample = Closure::<dyn FnMut()>::new(move || sample_sync(&samples));
    web_sys::window()
        .ok_or("Browservenster ontbreekt")?
        .set_interval_with_callback_and_timeout_and_arguments_0(
            sample.as_ref().unchecked_ref(),
            100,
        )?;
    sample.forget();
    let shifted = shared_for_sync.clone();
    on(&video, "seeking", move |_| {
        let mut state = shifted.borrow_mut();
        let time = state.video().current_time();
        if let Some(timing) = state.playback.as_mut().and_then(|play| play.sync.as_mut()) {
            timing.skip(time);
        }
    })?;
    Ok(())
}

pub fn start(app: &Shared, item: Item, with_subtitles: bool) {
    let Some(url) = http_url(&item.stream) else {
        app.borrow()
            .note("Deze titel heeft geen geldige afspeellink.");
        return;
    };
    {
        let mut state = app.borrow_mut();
        let resume_at = state.position(&item);
        let version = state.next_sequence();
        let show_tracks = item.kind != Kind::Live;
        state.playback = Some(Playback {
            item,
            version,
            last_saved: resume_at,
            resume_at,
            selected_subtitle: None,
            tracks_url: None,
            subtitle_offset: 0.,
            subtitle_results: Vec::new(),
            subtitle_search: 0,
            panel: false,
            controls_until: io::now() + 6,
            sync_enabled: true,
            sync: None,
            sync_notice: String::new(),
        });
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
        state
            .element("player-save")
            .unchecked_into::<HtmlElement>()
            .set_hidden(show_tracks);
        state.player_note("");
        let _ = state
            .element("player")
            .set_attribute("data-controls", "true");
        let video = state.video();
        video.set_src(url.as_str());
    }
    request_play(app);
    show_panel(app, with_subtitles);
    if !with_subtitles {
        let state = app.borrow();
        focus(&state.document, &state.element("player-play"));
    }
}

pub fn show_controls(app: &Shared) -> bool {
    let mut state = app.borrow_mut();
    let Some(play) = &mut state.playback else {
        return false;
    };
    play.controls_until = io::now() + 6;
    let player = state.element("player");
    let was_hidden = player.get_attribute("data-controls").as_deref() == Some("false");
    if was_hidden {
        let _ = player.set_attribute("data-controls", "true");
        focus(&state.document, &state.element("player-play"));
    }
    was_hidden
}

pub fn stop(app: &Shared) {
    save_position(app, true, false);
    {
        let mut state = app.borrow_mut();
        if let Some(play) = state.playback.take()
            && let Some(url) = play.tracks_url
        {
            let _ = web_sys::Url::revoke_object_url(&url);
        }
        let video = state.video();
        let _ = video.pause();
        let _ = video.remove_attribute("src");
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
    }
    ui::render_content(app);
    ui::focus_catalog(app);
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
                    load_subtitle(app, subtitle);
                }
            }
        }
        "track" => {
            if let Ok(index) = value.parse::<u32>() {
                let mut state = app.borrow_mut();
                disable_sync(&mut state);
                select_track(&state, index);
            }
        }
        "track-off" => {
            let mut state = app.borrow_mut();
            disable_sync(&mut state);
            if let Some(tracks) = state.video().text_tracks() {
                for index in 0..tracks.length() {
                    if let Some(track) = tracks.get(index) {
                        track.set_mode(TextTrackMode::Disabled);
                    }
                }
            }
            state.player_note("Ondertitels uit");
        }
        "sync-toggle" => {
            let mut state = app.borrow_mut();
            if let Some(play) = &mut state.playback {
                play.sync_enabled = !play.sync_enabled;
            }
            configure_sync(&mut state);
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
        "save-live" => {
            let item = app.borrow().playback.as_ref().map(|play| play.item.clone());
            if let Some(item) = item {
                let mut state = app.borrow_mut();
                let key = state.library_key(&item);
                let message = if state.library.favorites.remove(&key).is_none() {
                    state.library.favorites.insert(key, item);
                    "Zender bewaard"
                } else {
                    "Zender verwijderd"
                };
                io::save(LIBRARY_KEY, &state.library);
                state.player_note(message);
            }
        }
        _ => {}
    }
}

fn toggle(app: &Shared) {
    let video = app.borrow().video();
    if video.paused() {
        request_play(app);
    } else {
        let _ = video.pause();
    }
}

fn request_play(app: &Shared) {
    let (video, version) = {
        let state = app.borrow();
        let Some(play) = &state.playback else {
            return;
        };
        (state.video(), play.version)
    };
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
        if state
            .playback
            .as_ref()
            .is_some_and(|play| play.version == version)
        {
            state
                .player_note("Afspelen kon niet starten. Controleer de stream en probeer opnieuw.");
        }
    });
}

fn skip(app: &Shared, delta: f64) {
    let state = app.borrow();
    let video = state.video();
    if video.duration().is_finite() {
        video.set_current_time((video.current_time() + delta).clamp(0., video.duration()));
    }
}

fn save_position(app: &Shared, force: bool, completed: bool) {
    let mut state = app.borrow_mut();
    let video = state.video();
    let current = video.current_time();
    let Some(play) = &mut state.playback else {
        return;
    };
    if !matches!(play.item.kind, Kind::Movie | Kind::Episode)
        || (!force && (current - play.last_saved).abs() < 15.)
    {
        return;
    }
    let item = play.item.clone();
    play.last_saved = current;
    let key = state.library_key(&item);
    if completed
        || (video.duration().is_finite() && current > 0. && video.duration() - current < 15.)
    {
        state.library.positions.remove(&key);
        state.library.recent.remove(&key);
    } else if current >= 5. && current.is_finite() {
        state.library.positions.insert(key.clone(), current);
        state.library.recent.insert(key, item);
    } else {
        return;
    }
    io::save(LIBRARY_KEY, &state.library);
}

impl App {
    fn player_note(&self, message: &str) {
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
    let Some(play) = &mut state.playback else {
        return;
    };
    play.panel = visible;
    play.controls_until = io::now() + 6;
    let default_query = subtitle_title(if play.item.kind == Kind::Episode {
        &play.item.series_title
    } else {
        &play.item.title
    });
    let _ = state
        .element("player")
        .set_attribute("data-controls", "true");
    let can_search = state
        .playback
        .as_ref()
        .is_some_and(|play| play.item.kind != Kind::Live)
        && (!state.settings.opensubtitles.is_empty() || !state.settings.subdl.is_empty());
    state
        .element("subtitle-panel")
        .unchecked_into::<HtmlElement>()
        .set_hidden(!visible);
    state
        .element("subtitle-online")
        .unchecked_into::<HtmlElement>()
        .set_hidden(!can_search);
    state
        .element("player-actions")
        .unchecked_into::<HtmlElement>()
        .set_hidden(visible);
    if visible {
        let input = state
            .element("subtitle-query")
            .unchecked_into::<HtmlInputElement>();
        input.set_value(&default_query);
        let _ = input.set_attribute("data-original", &default_query);
        refresh_tracks(&state);
        render_results(&state);
        state.player_note("");
        let first = if can_search {
            state.element("subtitle-search-button")
        } else {
            state.element("subtitle-close")
        };
        focus(&state.document, &first);
    } else {
        state.player_note("");
        focus(&state.document, &state.element("player-subs"));
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
    state.player_note("Audiospoor gewijzigd");
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
    state.button(&panel, "Uit", "track-off", "", "");
    if let Some(tracks) = state.video().text_tracks() {
        for index in 0..tracks.length() {
            let Some(track) = tracks.get(index) else {
                continue;
            };
            let language = match language(&track.language()) {
                Language::Dutch => "Nederlands",
                Language::English => "English",
                _ => match language(track.label().split_whitespace().next().unwrap_or("")) {
                    Language::Dutch => "Nederlands",
                    Language::English => "English",
                    _ => continue,
                },
            };
            let cc = if track.kind() == web_sys::TextTrackKind::Captions
                || track.label().to_ascii_lowercase().contains(" cc")
            {
                " CC"
            } else {
                ""
            };
            let current = if track.mode() == TextTrackMode::Showing {
                " ✓"
            } else {
                ""
            };
            state.button(
                &panel,
                &format!("{language}{cc}{current}"),
                "track",
                &index.to_string(),
                "",
            );
        }
    }
    if let Some(play) = &state.playback {
        for (index, subtitle) in play.item.sidecars.iter().enumerate() {
            if let Some(subtitle) = sidecar_subtitle(subtitle) {
                state.button(&panel, &subtitle.label(), "sidecar", &index.to_string(), "");
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
                    &format!("Audio · {label}"),
                    "audio",
                    &index.to_string(),
                    "",
                );
            }
        }
    }
    if let Some(play) = &state.playback
        && play.selected_subtitle.is_some()
    {
        state.button(&panel, "−0,5 s", "offset", "-0.5", "");
        state.node(
            &panel,
            "span",
            "muted",
            &format!("{:+.1} s", play.subtitle_offset),
        );
        state.button(&panel, "+0,5 s", "offset", "0.5", "");
        let auto = state.button(&panel, "Auto-sync", "sync-toggle", "", "");
        let _ = auto.set_attribute(
            "aria-pressed",
            if play.sync_enabled { "true" } else { "false" },
        );
        state.node(&panel, "span", "muted sync-status", &play.sync_notice);
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

fn select_track(state: &App, selected: u32) {
    if let Some(tracks) = state.video().text_tracks() {
        for index in 0..tracks.length() {
            if let Some(track) = tracks.get(index) {
                track.set_mode(if index == selected {
                    TextTrackMode::Showing
                } else {
                    TextTrackMode::Disabled
                });
            }
        }
    }
    refresh_tracks(state);
}

pub fn search(app: &Shared) {
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
        play.subtitle_search = request;
        play.subtitle_results.clear();
        let item = play.item.clone();
        let version = play.version;
        state.player_note("Ondertitels zoeken…");
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
        if play.version != version || play.subtitle_search != request {
            return;
        }
        let message = match results {
            Ok(results) if results.is_empty() => {
                "Geen Nederlandse of Engelse ondertitels gevonden.".to_owned()
            }
            Ok(results) => {
                play.subtitle_results = results;
                String::new()
            }
            Err(error) => error,
        };
        render_results(&state);
        state.player_note(&message);
    });
}

fn render_results(state: &App) {
    let area = state.element("subtitle-results");
    area.set_text_content(None);
    if let Some(play) = &state.playback {
        for (index, row) in play.subtitle_results.iter().enumerate() {
            state.button(
                &area,
                &row.label(),
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
        load_subtitle(app, subtitle);
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
    })
}

fn load_subtitle(app: &Shared, subtitle: Subtitle) {
    let (version, request, os) = {
        let mut state = app.borrow_mut();
        let request = state.next_sequence();
        let Some(play) = &mut state.playback else {
            return;
        };
        play.subtitle_search = request;
        let version = play.version;
        state.player_note("Ondertitels downloaden…");
        (version, request, state.settings.opensubtitles.clone())
    };
    let app = app.clone();
    spawn_local(async move {
        let data = sources::subtitle_file(&subtitle, &os).await;
        let mut state = app.borrow_mut();
        let Some(play) = &mut state.playback else {
            return;
        };
        if play.version != version || play.subtitle_search != request {
            return;
        }
        match data {
            Ok(body) => {
                play.selected_subtitle = Some((subtitle, body));
                play.subtitle_offset = 0.;
                play.sync = None;
                let message = if apply_online(&mut state) {
                    configure_sync(&mut state);
                    "Ondertitels geselecteerd"
                } else {
                    "Dit ondertitelbestand kan niet worden gelezen."
                };
                state.player_note(message);
                if message == "Ondertitels geselecteerd" {
                    drop(state);
                    show_panel(&app, false);
                }
            }
            Err(error) => state.player_note(&error),
        }
    });
}

fn shift(app: &Shared, delta: f64) {
    let mut state = app.borrow_mut();
    disable_sync(&mut state);
    let Some(play) = &mut state.playback else {
        return;
    };
    play.subtitle_offset = (play.subtitle_offset + delta).clamp(-120., 120.);
    let _ = apply_online(&mut state);
}

fn disable_sync(state: &mut App) {
    if let Some(play) = &mut state.playback {
        play.sync_enabled = false;
        play.sync = None;
        play.sync_notice = "Automatische synchronisatie uit".to_owned();
    }
}

fn configure_sync(state: &mut App) {
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
    match super::timing::AudioSync::start(&video, cues) {
        Ok(sync) => {
            play.sync = Some(sync);
            play.sync_notice = "Timing meten tijdens afspelen…".to_owned();
        }
        Err(_) => {
            play.sync = None;
            play.sync_notice =
                "Automatische controle niet beschikbaar voor deze stream.".to_owned();
        }
    }
}

fn sample_sync(app: &Shared) {
    let mut state = app.borrow_mut();
    let video = state.video();
    if video.paused() || video.seeking() {
        return;
    }
    let Some(play) = &mut state.playback else {
        return;
    };
    let Some(estimate) = play
        .sync
        .as_mut()
        .and_then(|sync| sync.sample(video.current_time(), play.subtitle_offset))
    else {
        return;
    };
    let mut apply = false;
    if let Some(estimate) = estimate {
        let difference = estimate.offset - play.subtitle_offset;
        if difference.abs() >= 0.25 {
            play.subtitle_offset = estimate.offset.clamp(-120., 120.);
            play.sync_notice = format!("Automatisch bijgesteld: {:+.1} s", play.subtitle_offset);
            apply = true;
        } else {
            play.sync_notice = "Timing gecontroleerd".to_owned();
        }
    } else {
        play.sync_notice = "Geen betrouwbare afwijking gemeten".to_owned();
    }
    if apply {
        let _ = apply_online(&mut state);
    } else {
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
    let Some(vtt) = subtitles::webvtt(body, play.subtitle_offset) else {
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
    let name = subtitle.label();
    let lang = if subtitle.language == Language::Dutch {
        "nl"
    } else {
        "en"
    };
    let video = state.video();
    if let Ok(Some(track)) = video.query_selector("track[data-online]") {
        track.remove();
    }
    if let Some(tracks) = video.text_tracks() {
        for index in 0..tracks.length() {
            if let Some(track) = tracks.get(index) {
                track.set_mode(TextTrackMode::Disabled);
            }
        }
    }
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
