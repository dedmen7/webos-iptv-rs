use super::i18n::{message, tr};
use super::{App, PAGE_SIZE, Shared, SourceRefresh, View, focus, picker::Choice};
use crate::catalog::{Catalog, Item, Kind};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlInputElement};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum LibrarySection {
    #[default]
    All,
    Movies,
    Series,
}

impl LibrarySection {
    const GROUPS: [Self; 2] = [Self::Movies, Self::Series];

    fn id(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Movies => "movies",
            Self::Series => "series",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => tr("Alles"),
            Self::Movies => tr("Films"),
            Self::Series => tr("Series"),
        }
    }

    fn includes(self, kind: Kind) -> bool {
        match self {
            Self::All => !kind.is_live_stream(),
            Self::Movies => kind == Kind::Movie,
            Self::Series => matches!(kind, Kind::Series | Kind::Episode),
        }
    }
}

pub(super) fn select_library_section(app: &Shared, id: &str) {
    let Some(section) = std::iter::once(LibrarySection::All)
        .chain(LibrarySection::GROUPS)
        .find(|section| section.id() == id)
    else {
        return;
    };
    {
        let mut state = app.borrow_mut();
        if state.view != View::Library || state.detail.is_some() {
            return;
        }
        state.library_section = section;
        state.page = 0;
    }
    render_content(app);
}

fn episode_label(season: u32, episode: u32) -> String {
    format!("S{season:02}E{episode:02}")
}

pub(super) fn playback_time(seconds: f64) -> String {
    let seconds = seconds as u64;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn movie_metadata(item: &Item) -> String {
    let year = item.year.get(..4).filter(|year| {
        year.bytes().all(|byte| byte.is_ascii_digit()) && !item.title.contains(year)
    });
    [year.unwrap_or_default(), item.category.as_str()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

pub fn render(app: &Shared) {
    let state = app.borrow();
    let nav = state.element("navigation");
    let active_tab = if state.view == View::Guide {
        View::Live
    } else {
        state.view
    };
    for (index, view) in View::ALL.iter().enumerate() {
        if let Some(link) = nav.children().item(index as u32) {
            let _ = link.set_attribute(
                "aria-current",
                if active_tab == *view { "page" } else { "false" },
            );
        }
    }
    mark_filter(&state);
    let searchable = state.view != View::Settings && state.detail.is_none();
    let search = state.element("search").unchecked_into::<HtmlInputElement>();
    search.set_hidden(!searchable);
    state
        .element("search-keys")
        .unchecked_into::<web_sys::HtmlElement>()
        .set_hidden(!searchable);
    search.set_placeholder(&message(
        "Zoeken in {section}",
        &[("section", state.view.name())],
    ));
    drop(state);
    render_content(app);
}

pub(super) fn mark_filter(state: &App) {
    let filter = state
        .element("preferred")
        .unchecked_into::<web_sys::HtmlElement>();
    filter.set_hidden(state.view == View::Library);
    let _ = filter.set_attribute(
        "aria-pressed",
        if state.settings.preferred {
            "true"
        } else {
            "false"
        },
    );
}

pub fn render_content(app: &Shared) {
    super::player::queue_live_preload(app);
    super::load_visible_live_programmes(app);
    let state = app.borrow();
    let content = state.element("content");
    let focus_key = state.document.active_element().and_then(|element| {
        if !content.contains(Some(&element)) {
            return None;
        }
        focus_identity(&element)
    });
    super::poster::discard_under(&content);
    content.set_text_content(None);
    if let Some(detail) = &state.detail {
        state.render_detail(&content, detail);
    } else {
        match state.view {
            View::Settings => state.render_settings(&content),
            View::Guide => super::live::render_guide(&state, &content),
            _ => state.render_browse(&content),
        }
    }
    super::picker::refresh(&state);
    if let Some(key) = focus_key {
        let tags = content.query_selector_all("button, input").ok();
        if let Some(tags) = tags {
            for idx in 0..tags.length() {
                let Some(element) = tags
                    .item(idx)
                    .and_then(|node| node.dyn_into::<Element>().ok())
                else {
                    continue;
                };
                if focus_identity(&element) == Some(key.clone()) {
                    focus(&state.document, &element);
                    return;
                }
            }
        }
        if (key.0 == "item" || (key.0 == "id" && key.1 == Choice::Category.id()))
            && let Some(first) = result_element(&state, None).or_else(|| {
                state
                    .element("navigation")
                    .query_selector("[aria-current=page]")
                    .ok()
                    .flatten()
            })
        {
            focus(&state.document, &first);
        }
    }
}

pub fn render_search(app: &Shared) {
    super::player::queue_live_preload(app);
    super::load_visible_live_programmes(app);
    let state = app.borrow();
    if state.view == View::Guide {
        drop(state);
        render_content(app);
        return;
    }
    if let Some(results) = state.document.get_element_by_id("search-results") {
        super::poster::discard_under(&results);
        results.set_text_content(None);
        state.render_results(&results);
    }
}

pub(super) fn focus_identity(element: &Element) -> Option<(String, String)> {
    if !element.id().is_empty() {
        return Some(("id".into(), element.id()));
    }
    Some((
        element.get_attribute("data-cmd")?,
        element.get_attribute("data-value").unwrap_or_default(),
    ))
}

impl App {
    pub(super) fn node(&self, parent: &Element, tag: &str, class: &str, text: &str) -> Element {
        let element = self.document.create_element(tag).expect("valid element");
        if !class.is_empty() {
            element.set_class_name(class);
        }
        if !text.is_empty() {
            element.set_text_content(Some(text));
        }
        parent.append_child(&element).expect("valid DOM parent");
        element
    }

    pub(super) fn button(
        &self,
        parent: &Element,
        label: &str,
        cmd: &str,
        value: &str,
        class: &str,
    ) -> Element {
        let button = self.node(parent, "button", class, label);
        let _ = button.set_attribute("type", "button");
        let _ = button.set_attribute("data-cmd", cmd);
        let _ = button.set_attribute("data-value", value);
        button
    }

    fn entry_field(&self, parent: &Element, id: &str, caption: &str, value: &str, kind: &str) {
        let label = self.node(parent, "label", "field", caption);
        let input = self
            .node(&label, "input", "", "")
            .unchecked_into::<HtmlInputElement>();
        input.set_id(id);
        input.set_type(kind);
        input.set_value(value);
        input.set_autocomplete("off");
        let _ = input.set_attribute("spellcheck", "false");
    }

    pub(super) fn header(&self, parent: &Element, title: &str, caption: &str) -> Element {
        let header = self.node(parent, "div", "heading", "");
        let text = self.node(&header, "div", "heading-copy", "");
        if !caption.is_empty() {
            self.node(&text, "p", "eyebrow", caption);
        }
        self.node(&text, "h1", "", title);
        header
    }

    pub(super) fn placeholder(&self, parent: &Element, text: &str) {
        self.node(parent, "p", "empty", text);
    }

    pub(super) fn poster(&self, parent: &Element, item: &Item) {
        let artwork = self.node(parent, "span", "artwork", "");
        let (url, fallback) = item.poster_urls(&self.settings.source);
        if let Some(url) = url {
            let image = self.node(&artwork, "img", "", "");
            let _ = image.set_attribute("alt", "");
            let _ = image.set_attribute("decoding", "async");
            let _ = image.set_attribute("referrerpolicy", "no-referrer");
            super::poster::mount(&image, &url, fallback.as_ref());
        } else if item.kind == Kind::Radio {
            let mark = self.node(&artwork, "span", "radio-mark", "♫");
            let _ = mark.set_attribute("aria-hidden", "true");
        }
    }

    fn render_settings(&self, parent: &Element) {
        let first_run = !self.settings.source.configured();
        self.header(
            parent,
            if first_run {
                tr("Orbis instellen")
            } else {
                tr("Instellingen")
            },
            "",
        );
        let form = self.node(parent, "section", "settings-grid", "");
        let card_class = if first_run {
            "settings-card setup-source"
        } else {
            "settings-card"
        };
        let source = self.node(&form, "div", card_class, "");
        self.node(&source, "h2", "", tr("Tv-bron"));
        let source_actions = self.node(&source, "div", "actions settings-source-actions", "");
        source_actions.set_id("source-actions");
        let source_status = self.node(&source, "p", "notice", "");
        source_status.set_id("source-refresh-status");
        let _ = source_status.set_attribute("role", "status");
        if first_run {
            let _ = source_actions.set_attribute("hidden", "");
            self.node(
                &source,
                "p",
                "muted",
                tr("Kies wat je van je IPTV-aanbieder hebt gekregen."),
            );
            let methods = self.node(&source, "div", "actions setup-methods", "");
            self.button(&methods, tr("Xtream-account"), "setup-method", "xtream", "")
                .set_id("setup-xtream");
            self.button(&methods, tr("M3U-link"), "setup-method", "m3u", "")
                .set_id("setup-m3u");
        } else {
            refresh_source_actions(self);
        }
        let blank = crate::catalog::Source::default();
        let current = if self.adding_source {
            &blank
        } else {
            &self.settings.source
        };
        let name = if self.adding_source {
            ""
        } else {
            self.settings
                .active_profile()
                .map(|profile| profile.name.as_str())
                .unwrap_or_default()
        };
        self.entry_field(&source, "source-name", tr("Naam"), name, "text");
        self.entry_field(
            &source,
            "source-server",
            tr("Xtream-server"),
            &current.server,
            "url",
        );
        let pair = self.node(&source, "div", "field-pair", "");
        pair.set_id("source-credentials");
        self.entry_field(
            &pair,
            "source-user",
            tr("Gebruikersnaam"),
            &current.username,
            "text",
        );
        self.entry_field(
            &pair,
            "source-pass",
            tr("Wachtwoord"),
            &current.password,
            "password",
        );
        self.entry_field(
            &source,
            "source-m3u",
            tr("M3U-link"),
            &current.playlist,
            "url",
        );
        self.entry_field(
            &source,
            "source-epg",
            tr("XMLTV-link"),
            &current.guide,
            "url",
        );
        let subtitles = self.node(&form, "div", "settings-card", "");
        self.node(&subtitles, "h2", "", tr("Online ondertitels"));
        self.entry_field(
            &subtitles,
            "source-os",
            tr("OpenSubtitles API-sleutel"),
            &self.settings.opensubtitles,
            "password",
        );
        self.entry_field(
            &subtitles,
            "source-subdl",
            tr("SubDL API-sleutel"),
            &self.settings.subdl,
            "password",
        );
        let groups = self.node(&form, "div", "settings-card", "");
        self.node(&groups, "h2", "", tr("Zendergroepen"));
        let editor = self.node(&groups, "div", "", "");
        editor.set_id("group-settings");
        if first_run {
            let _ = subtitles.set_attribute("hidden", "");
            let _ = groups.set_attribute("hidden", "");
            for optional in ["source-name", "source-epg"] {
                if let Some(label) = self.element(optional).parent_element() {
                    let _ = label.set_attribute("hidden", "");
                }
            }
            show_setup_fields(self);
        } else {
            refresh_group_editor(self);
        }
        self.render_privacy(&form);
        self.button(parent, tr("Opslaan"), "save", "", "primary settings-save");
    }

    fn render_privacy(&self, parent: &Element) {
        let card = self.node(parent, "section", "settings-card", "");
        let button = self.button(&card, tr("Privacy"), "privacy", "", "");
        button.set_id("privacy-toggle");
        let _ = button.set_attribute("aria-controls", "privacy-policy");
        let _ = button.set_attribute("aria-expanded", "false");
        let policy = self.node(&card, "div", "muted", "");
        policy.set_id("privacy-policy");
        let _ = policy.set_attribute("hidden", "");
        for text in [
            "Orbis bewaart je tv-bronnen, ondertitelsleutels en kijkgeschiedenis versleuteld op je tv. Orbis gebruikt geen advertenties of gebruiksstatistieken.",
            "Je tv vraagt lijsten, programma’s, afbeeldingen en streams rechtstreeks op bij je aanbieder of de bijbehorende servers. Voor online ondertitels vraagt je tv met jouw sleutel een titel of aflevering op bij OpenSubtitles of SubDL.",
            "Onder Tv-bron kun je een bron verwijderen. Je kunt je ondertitelsleutels wissen onder Online ondertitels. Verwijder de appgegevens op je tv om ook lokaal opgeslagen kijkgegevens en caches te wissen.",
            "Voor vragen over privacy kun je contact opnemen met dedmen7 via GitHub.",
        ] {
            self.node(&policy, "p", "", tr(text));
        }
    }

    fn render_browse(&self, parent: &Element) {
        let header = self.header(parent, self.view.name(), "");
        if self.view != View::Radio && !self.settings.source.configured() {
            self.button(&header, tr("Bron instellen"), "view", "settings", "primary");
            return;
        }
        if self.view == View::Library {
            self.render_library_tabs(parent);
        }
        if let Some(kind) = self.view.kind() {
            let toolbar = self.node(parent, "div", "catalog-toolbar", "");
            if let Some(options) = super::picker::browse_options(self, kind) {
                let selected = options.label(self.catalog_filter.borrowed());
                let field = self.node(&toolbar, "div", "selector", options.heading);
                super::picker::trigger(self, &field, super::picker::Choice::Category, selected);
            }
            if kind == Kind::Live
                && let Some(channel) = super::live::remembered_channel(self)
            {
                let actions = self.node(&toolbar, "div", "catalog-actions", "");
                self.button(
                    &actions,
                    tr("Laatst bekeken"),
                    "live-resume",
                    &channel.id,
                    "catalog-resume",
                );
            }
        }
        let results = self.node(parent, "section", "", "");
        results.set_id("search-results");
        self.render_results(&results);
    }

    fn render_library_tabs(&self, parent: &Element) {
        let tabs = self.node(parent, "div", "library-tabs", "");
        for section in std::iter::once(LibrarySection::All).chain(LibrarySection::GROUPS) {
            let tab = self.button(&tabs, section.label(), "library-section", section.id(), "");
            let _ = tab.set_attribute(
                "aria-pressed",
                if self.library_section == section {
                    "true"
                } else {
                    "false"
                },
            );
        }
    }

    fn library_catalog(&self) -> Catalog {
        let prefix = format!("{}:", self.source_key);
        let mut seen = std::collections::HashSet::new();
        let mut items: Vec<_> = self
            .library
            .series
            .iter()
            .map(|(key, progress)| (key, &progress.series))
            .chain(&self.library.recent)
            .chain(&self.library.favorites)
            .filter(|(key, item)| {
                key.starts_with(&prefix)
                    && self.library_section.includes(item.kind)
                    && seen.insert(key.as_str())
            })
            .collect();
        let updated = |key: &str| {
            self.library
                .watching
                .get(key)
                .map_or(0, |watch| watch.updated)
        };
        items.sort_by(|(left_key, left), (right_key, right)| {
            updated(right_key)
                .cmp(&updated(left_key))
                .then_with(|| left.title.cmp(&right.title))
                .then_with(|| left_key.cmp(right_key))
        });
        Catalog::new(items.into_iter().map(|(_, item)| item.clone()).collect())
    }

    fn render_results(&self, parent: &Element) {
        if self.view == View::Library {
            self.render_items(parent, &self.library_catalog(), false);
        } else if let Some(kind) = self.view.kind() {
            if matches!(kind, Kind::Movie | Kind::Series)
                && self.page == 0
                && self.query.trim().is_empty()
                && self.catalog_filter.is_all()
            {
                self.render_continue(parent, kind);
            }
            if let Some(error) = self.failures.get(&kind) {
                self.placeholder(parent, error);
            }
            if let Some(catalog) = self.browse_catalog(kind) {
                self.render_items(parent, catalog, false);
            } else {
                self.render_incomplete_catalog(parent, kind);
            }
        }
    }

    fn render_incomplete_catalog(&self, parent: &Element, kind: Kind) {
        let partial = self.vod_partial.get(&kind);
        let preview = self.catalog_previews.get(&kind);
        match (partial, preview) {
            (Some(partial), Some(preview))
                if !self.query.is_empty() || !self.catalog_filter.is_all() =>
            {
                let items = partial.search_with_preview(
                    preview,
                    &self.query,
                    self.catalog_filter.borrowed(),
                    self.settings.preferred,
                    PAGE_SIZE,
                );
                if !items.is_empty() {
                    self.render_catalog_items(parent, &items);
                } else if !self.catalog_loading(kind) {
                    self.placeholder(parent, tr("Geen resultaten gevonden."));
                }
            }
            _ => {
                if let Some(catalog) = preview.or(partial) {
                    self.render_items(parent, catalog, true);
                }
            }
        }
        if self.catalog_loading(kind) {
            self.placeholder(parent, tr("Catalogus laden…"));
        }
    }

    fn render_items(&self, parent: &Element, catalog: &Catalog, preview: bool) {
        let limit = (self.page + 1) * PAGE_SIZE;
        let preferred = self.view != View::Library && self.settings.preferred;
        let (hits, count) = catalog.search_filtered(
            &self.query,
            self.catalog_filter.borrowed(),
            preferred,
            limit,
        );
        if count == 0 {
            if preview
                && self
                    .view
                    .kind()
                    .is_some_and(|kind| self.catalog_loading(kind))
            {
                return;
            }
            self.placeholder(
                parent,
                if self.query.trim().is_empty() {
                    tr("Geen titels in deze lijst.")
                } else {
                    tr("Geen resultaten gevonden.")
                },
            );
            return;
        }
        let first = (self.page * PAGE_SIZE).min(hits.len());
        let items: Vec<_> = hits[first..]
            .iter()
            .map(|index| &catalog.items[*index])
            .collect();
        if self.view == View::Library {
            self.render_library_items(parent, &items);
        } else {
            self.render_catalog_items(parent, &items);
        }
        if !preview {
            self.pagination(parent, count, hits.len());
        }
    }

    fn render_library_items(&self, parent: &Element, items: &[&Item]) {
        for section in LibrarySection::GROUPS {
            let mut titles = items
                .iter()
                .copied()
                .filter(|item| section.includes(item.kind))
                .peekable();
            if titles.peek().is_none() {
                continue;
            }
            let shelf = self.node(parent, "section", "library-shelf", "");
            let _ = shelf.set_attribute("data-library-section", section.id());
            if self.library_section == LibrarySection::All {
                self.node(&shelf, "h2", "", section.label());
            }
            let grid = self.node(&shelf, "div", "library-grid", "");
            for item in titles {
                self.render_library_card(&grid, item);
            }
        }
    }

    fn render_library_card(&self, parent: &Element, item: &Item) {
        let key = self.library_key(item);
        let watch = self.library.watching.get(&key);
        let (episode, position) = if let Some(progress) = self.library.series.get(&key) {
            let code = episode_label(progress.season, progress.episode);
            if progress.finished {
                let has_next = watch.and_then(|watch| watch.has_next) != Some(false);
                let label = if has_next {
                    "Volgende aflevering · na {episode}"
                } else {
                    "Laatst bekeken · {episode}"
                };
                (message(label, &[("episode", &code)]), 0.)
            } else {
                (code, self.saved_episode_position(progress))
            }
        } else {
            let code = if item.kind == Kind::Episode {
                let episode = episode_label(item.season, item.episode);
                if item.series_title.is_empty() {
                    episode
                } else {
                    format!("{} · {episode}", item.series_title)
                }
            } else {
                String::new()
            };
            (code, self.position(item))
        };
        let in_progress = position.is_finite() && position > 0.;
        let watched = !in_progress && self.is_watched(item);
        let fraction = if watched {
            Some(1.)
        } else if in_progress {
            watch
                .map(|watch| watch.duration)
                .filter(|duration| duration.is_finite() && *duration > 0.)
                .map(|duration| (position / duration).clamp(0., 1.))
        } else {
            None
        };
        let card = self.button(parent, "", "item", &item.key(), "tile library-card");
        let header = self.node(&card, "span", "library-card-header", "");
        self.node(&header, "span", "library-card-title", &item.title);
        if self.library.favorites.contains_key(&key) {
            let star = self.node(&header, "span", "saved-mark", "★");
            let _ = star.set_attribute("aria-hidden", "true");
        }
        let status = self.node(&card, "span", "library-card-status", "");
        let resume = if in_progress {
            message("Verder vanaf {time}", &[("time", &playback_time(position))])
        } else {
            String::new()
        };
        let metadata = match (episode.is_empty(), resume.is_empty()) {
            (false, false) => format!("{episode} · {resume}"),
            (false, true) => episode,
            (true, false) => resume,
            (true, true) => movie_metadata(item),
        };
        if !metadata.is_empty() {
            self.node(&status, "span", "library-card-meta", &metadata);
        }
        if watched {
            self.node(&status, "span", "library-card-badge", tr("Gezien"));
        } else if let Some(fraction) = fraction {
            let percent = format!("{:.0}%", fraction * 100.);
            self.node(&status, "span", "library-card-badge", &percent);
        }
        if let Some(fraction) = fraction {
            let track = super::live::progress_track(self, &card);
            let _ = track.set_attribute("aria-label", tr("Kijkvoortgang"));
            super::live::set_progress_fraction(&track, fraction);
        }
    }

    fn render_continue(&self, parent: &Element, kind: Kind) {
        let prefix = format!("{}:", self.source_key);
        let mut items: Vec<_> = self
            .library
            .recent
            .iter()
            .chain(
                self.library
                    .series
                    .iter()
                    .map(|(key, progress)| (key, &progress.series)),
            )
            .filter(|(key, item)| {
                key.starts_with(&prefix)
                    && (item.kind == kind || (kind == Kind::Series && item.kind == Kind::Episode))
            })
            .filter_map(|(key, item)| {
                if item.kind == Kind::Episode
                    && self.library.series.iter().any(|(key, progress)| {
                        key.starts_with(&prefix)
                            && if item.series_id.is_empty() {
                                !item.series_title.is_empty()
                                    && progress.series.title == item.series_title
                            } else {
                                progress.series.id == item.series_id
                            }
                    })
                {
                    return None;
                }
                let watch = self.library.watching.get(key);
                let (subtitle, position) = match self.library.series.get(key) {
                    Some(progress) if progress.finished => {
                        if watch.and_then(|watch| watch.has_next) == Some(false) {
                            return None;
                        }
                        let episode = episode_label(progress.season, progress.episode);
                        (
                            message(
                                "Volgende aflevering · na {episode}",
                                &[("episode", &episode)],
                            ),
                            0.,
                        )
                    }
                    Some(progress) => (
                        episode_label(progress.season, progress.episode),
                        self.saved_episode_position(progress),
                    ),
                    None if item.kind == Kind::Episode && self.position(item) > 0. => (
                        episode_label(item.season, item.episode),
                        self.position(item),
                    ),
                    None => (String::new(), self.position(item)),
                };
                if (position <= 0. && subtitle.is_empty()) || !position.is_finite() {
                    return None;
                }
                let updated = watch.map_or(0, |watch| watch.updated);
                let duration = watch.map_or(0., |watch| watch.duration);
                Some((item, subtitle, position, duration, updated))
            })
            .collect();
        items.sort_by(|left, right| {
            right
                .4
                .cmp(&left.4)
                .then_with(|| {
                    (right.0.season, right.0.episode).cmp(&(left.0.season, left.0.episode))
                })
                .then_with(|| left.0.title.cmp(&right.0.title))
                .then_with(|| left.0.id.cmp(&right.0.id))
        });
        if items.is_empty() {
            return;
        }
        let section = self.node(parent, "section", "continue-section", "");
        self.node(&section, "h2", "", tr("Verder kijken"));
        let cards = self.node(&section, "div", "continue-grid", "");
        let mut episode_parents = std::collections::HashSet::new();
        let visible = items.into_iter().filter(|(item, ..)| {
            item.kind != Kind::Episode
                || episode_parents.insert((
                    item.series_id.as_str(),
                    if item.series_id.is_empty() {
                        item.series_title.as_str()
                    } else {
                        ""
                    },
                    if item.series_id.is_empty() && item.series_title.is_empty() {
                        item.id.as_str()
                    } else {
                        ""
                    },
                ))
        });
        for (item, subtitle, position, duration, _) in visible.take(3) {
            let card = self.button(&cards, "", "continue-item", &item.key(), "continue-card");
            let title = if item.kind == Kind::Episode && !item.series_title.is_empty() {
                &item.series_title
            } else {
                &item.title
            };
            self.node(&card, "span", "continue-title", title);
            if !subtitle.is_empty() {
                self.node(&card, "span", "continue-meta", &subtitle);
            }
            if position > 0. {
                self.node(
                    &card,
                    "span",
                    "continue-meta",
                    &message("Verder vanaf {time}", &[("time", &playback_time(position))]),
                );
                if duration > 0. {
                    let track = super::live::progress_track(self, &card);
                    super::live::set_progress_fraction(&track, position / duration);
                }
            }
        }
    }

    fn render_catalog_items(&self, parent: &Element, items: &[&Item]) {
        let live = self.view == View::Live;
        if live {
            let columns = self.node(parent, "div", "programme-columns", "");
            for label in [tr("Zender"), tr("Nu"), tr("Straks")] {
                self.node(&columns, "span", "", label);
            }
        }
        let list = self.node(
            parent,
            "div",
            if live {
                "result-list live-results"
            } else {
                "result-list media-results"
            },
            "",
        );
        for item in items {
            let row = self.button(&list, "", "item", &item.key(), "tile result-row");
            let title = self.node(&row, "span", "result-title", "");
            self.node(&title, "span", "tile-title", &item.title);
            let saved = !item.kind.is_live_stream()
                && self.library.favorites.contains_key(&self.library_key(item));
            if saved {
                let star = self.node(&title, "span", "saved-mark", "★");
                let _ = star.set_attribute("aria-hidden", "true");
            }
            if live {
                let _ = row.set_attribute("data-epg-id", &item.epg_id);
                let _ = row.set_attribute("data-category", &item.category);
                let current = self.node(&row, "span", "result-current", "");
                self.node(&current, "span", "result-programme", "");
                let timeline = self.node(&current, "span", "result-timeline", "");
                super::live::progress_track(self, &timeline);
                self.node(&timeline, "span", "result-meta result-time", "");
                self.node(&row, "span", "result-next", "");
                show_programme(self, &row);
            } else {
                let metadata = movie_metadata(item);
                self.node(&row, "span", "result-meta", &metadata);
                let status = if self.is_watched(item) {
                    tr("Gezien").to_owned()
                } else if let Some(progress) = self.series_progress(item) {
                    let episode = episode_label(progress.season, progress.episode);
                    let label = if progress.finished {
                        tr("Laatst bekeken")
                    } else {
                        tr("Verder kijken")
                    };
                    format!("{label} · {episode}")
                } else if self.position(item) > 0. {
                    message(
                        "Verder vanaf {time}",
                        &[("time", &playback_time(self.position(item)))],
                    )
                } else {
                    String::new()
                };
                self.node(&row, "span", "result-meta result-status", &status);
            }
        }
    }

    fn pagination(&self, parent: &Element, count: usize, end: usize) {
        let footer = self.node(parent, "div", "pagination", "");
        if self.page > 0 {
            self.button(&footer, tr("Vorige"), "page", "prev", "");
        }
        self.node(
            &footer,
            "span",
            "muted",
            &message(
                "{start}–{end} van {total}",
                &[
                    ("start", &(self.page * PAGE_SIZE + 1).to_string()),
                    ("end", &end.to_string()),
                    ("total", &count.to_string()),
                ],
            ),
        );
        if end < count {
            self.button(&footer, tr("Volgende"), "page", "next", "");
        }
    }

    fn render_detail(&self, parent: &Element, detail: &super::Detail) {
        let item = &detail.item;
        let series = item.kind == Kind::Series;
        let hero = self.node(parent, "section", "detail text-detail", "");
        let copy = self.node(&hero, "div", "detail-copy", "");
        self.button(&copy, tr("Terug"), "back", "", "back-button");
        self.header(
            &copy,
            &item.title,
            if series { tr("Serie") } else { tr("Film") },
        );
        let metadata = movie_metadata(item);
        if !metadata.is_empty() {
            self.node(&copy, "p", "result-meta", &metadata);
        }
        if !item.plot.is_empty() {
            self.node(&copy, "p", "plot", &item.plot);
        }
        let actions = self.node(&copy, "div", "actions", "");
        let series_target = self.series_target(item, &detail.episodes);
        if item.kind == Kind::Movie {
            let watched = self.is_watched(item);
            self.button(
                &actions,
                if self.position(item) > 0. {
                    tr("Verder kijken")
                } else {
                    tr("Afspelen")
                },
                "watch",
                "",
                "primary",
            );
            let subtitles = self.button(&actions, tr("Ondertitels"), "watch-subtitles", "", "");
            if detail.loading {
                let _ = subtitles.set_attribute("disabled", "");
            }
            if !item.id.is_empty() {
                self.button(
                    &actions,
                    if watched {
                        tr("Markeer als ongezien")
                    } else {
                        tr("Markeer als gezien")
                    },
                    "toggle-watched",
                    "",
                    "",
                );
            }
            if watched {
                self.node(&copy, "p", "resume-time", tr("Gezien"));
            }
        } else if let Some((episode, next)) = series_target {
            let label = episode_label(episode.season, episode.episode);
            let action = if next {
                tr("Volgende aflevering")
            } else {
                tr("Verder kijken")
            };
            self.button(
                &actions,
                &format!("{action} · {label}"),
                "series-resume",
                "",
                "primary",
            );
        }
        let saved = self.library.favorites.contains_key(&self.library_key(item));
        self.button(
            &actions,
            favorite_label(saved),
            "save-title",
            &item.key(),
            "",
        );
        if item.kind != Kind::Series {
            return;
        }
        if let Some((episode, false)) = series_target {
            let position = self.episode_position(detail, episode);
            if position >= 1. {
                self.node(
                    &copy,
                    "p",
                    "resume-time",
                    &message("Verder vanaf {time}", &[("time", &playback_time(position))]),
                );
            }
        } else if !detail.loading
            && detail.error.is_none()
            && let Some(progress) = self.series_progress(item)
            && progress.finished
            && series_target.is_none()
        {
            let episode = episode_label(progress.season, progress.episode);
            self.node(
                &copy,
                "p",
                "resume-time",
                &message("Laatst bekeken · {episode}", &[("episode", &episode)]),
            );
        }
        if detail.loading {
            self.placeholder(parent, tr("Afleveringen laden…"));
            return;
        }
        if let Some(error) = &detail.error {
            self.placeholder(parent, error);
            return;
        }
        if detail.episodes.is_empty() {
            self.placeholder(parent, tr("Geen afleveringen gevonden."));
            return;
        }
        let field = self.node(parent, "div", "selector", "");
        let season = detail.season.unwrap_or(detail.episodes[0].season);
        super::picker::trigger(
            self,
            &field,
            super::picker::Choice::Season,
            &message("Seizoen {season}", &[("season", &season.to_string())]),
        );
        let episodes: Vec<_> = detail
            .episodes
            .iter()
            .filter(|item| detail.season.is_none_or(|season| item.season == season))
            .collect();
        let list = self.node(parent, "div", "episode-list", "");
        for episode in episodes.iter().skip(self.page * PAGE_SIZE).take(PAGE_SIZE) {
            let tile = self.button(&list, "", "episode", &episode.id, "episode");
            let code = episode_label(episode.season, episode.episode);
            self.node(&tile, "span", "episode-code", &code);
            if !episode.title.is_empty() {
                self.node(&tile, "span", "episode-title", &episode.title);
            }
            if self.is_watched(episode) {
                self.node(&tile, "span", "episode-title", tr("Gezien"));
            }
            if series_target.is_some_and(|(target, _)| target.id == episode.id) {
                let _ = tile.set_attribute("data-resume", "");
            }
        }
        if episodes.len() > PAGE_SIZE {
            self.pagination(
                parent,
                episodes.len(),
                ((self.page + 1) * PAGE_SIZE).min(episodes.len()),
            );
        }
    }
}

pub(super) fn show_setup_fields(state: &App) {
    for (id, selected) in [
        ("setup-xtream", !state.setup_m3u),
        ("setup-m3u", state.setup_m3u),
    ] {
        let _ = state
            .element(id)
            .set_attribute("aria-pressed", if selected { "true" } else { "false" });
    }
    for (id, hidden) in [
        ("source-server", state.setup_m3u),
        ("source-m3u", !state.setup_m3u),
    ] {
        if let Some(label) = state.element(id).parent_element() {
            label
                .unchecked_into::<web_sys::HtmlElement>()
                .set_hidden(hidden);
        }
    }
    state
        .element("source-credentials")
        .unchecked_into::<web_sys::HtmlElement>()
        .set_hidden(state.setup_m3u);
}

fn live_rows(state: &App) -> impl Iterator<Item = Element> {
    let rows = state
        .element("content")
        .query_selector_all(".live-results .result-row")
        .ok();
    let len = rows.as_ref().map_or(0, web_sys::NodeList::length);
    (0..len).filter_map(move |index| rows.as_ref()?.item(index)?.dyn_into::<Element>().ok())
}

fn live_id(row: &Element) -> Option<String> {
    row.get_attribute("data-value")?
        .strip_prefix("Live:")
        .map(str::to_owned)
}

pub(super) fn visible_channels_needing_programmes(state: &App) -> Vec<String> {
    let height = state
        .document
        .default_view()
        .and_then(|window| window.inner_height().ok())
        .and_then(|height| height.as_f64())
        .unwrap_or(1080.);
    live_rows(state)
        .filter(|row| {
            let rect = row.get_bounding_client_rect();
            rect.bottom() > 0. && rect.top() < height
        })
        .filter_map(|row| {
            let id = live_id(&row)?;
            let epg_id = row.get_attribute("data-epg-id").unwrap_or_default();
            let (current, next) =
                super::live::programmes_for(state, &id, &epg_id, super::io::now());
            (current.is_none() || next.is_none()).then_some(id)
        })
        .collect()
}

pub(super) fn update_live_programme(state: &App, id: &str) {
    if let Some(row) = live_rows(state).find(|row| live_id(row).as_deref() == Some(id)) {
        show_programme(state, &row);
    }
}

pub(super) fn update_live_programmes(state: &App) {
    if super::live_list_visible(state) {
        for row in live_rows(state) {
            show_programme(state, &row);
        }
    }
}

fn show_programme(state: &App, row: &Element) {
    let Some(id) = live_id(row) else { return };
    let epg_id = row.get_attribute("data-epg-id").unwrap_or_default();
    let now = super::io::now();
    let (current, next) = super::live::programmes_for(state, &id, &epg_id, now);
    let category = row.get_attribute("data-category").unwrap_or_default();
    let label = current.map_or(category.as_str(), crate::guide::Programme::name);
    set_result_text(row, ".result-programme", label);
    let times = current.map(|programme| {
        format!(
            "{}–{}",
            super::live::time(programme.start_unix),
            super::live::time(programme.stop_unix)
        )
    });
    set_result_text(row, ".result-time", times.as_deref().unwrap_or_default());
    if let Ok(Some(track)) = row.query_selector(".live-progress") {
        super::live::update_progress(&track, current, now);
    }
    let next_label = next.map(|programme| {
        format!(
            "{} · {}",
            super::live::time(programme.start_unix),
            programme.name()
        )
    });
    set_result_text(
        row,
        ".result-next",
        next_label.as_deref().unwrap_or_default(),
    );
}

fn set_result_text(row: &Element, selector: &str, value: &str) {
    if let Ok(Some(text)) = row.query_selector(selector)
        && text.text_content().as_deref() != Some(value)
    {
        text.set_text_content(Some(value));
    }
}

pub(super) fn clamp_library_page(state: &mut App) {
    if state.view == View::Library && state.page > 0 {
        let (_, count) = state.library_catalog().search(&state.query, "", false, 0);
        state.page = state.page.min(count.saturating_sub(1) / PAGE_SIZE);
    }
}

pub fn focus_catalog(app: &Shared) {
    let state = app.borrow();
    let content = state.element("content");
    let resume = content
        .query_selector("[data-cmd=series-resume]")
        .ok()
        .flatten();
    let next = resume
        .or_else(|| content.query_selector("button, input").ok().flatten())
        .or_else(|| {
            state
                .element("navigation")
                .query_selector("[aria-current=page]")
                .ok()
                .flatten()
        });
    if let Some(next) = next {
        focus(&state.document, &next);
    }
}

fn favorite_label(saved: bool) -> &'static str {
    if saved {
        tr("Verwijder uit mijn lijst")
    } else {
        tr("Bewaar in mijn lijst")
    }
}

pub(super) fn focus_continue(app: &Shared, key: &str) -> bool {
    let state = app.borrow();
    let Ok(cards) = state
        .element("content")
        .query_selector_all(".continue-card")
    else {
        return false;
    };
    for index in 0..cards.length() {
        let Some(card) = cards
            .item(index)
            .and_then(|node| node.dyn_into::<Element>().ok())
        else {
            continue;
        };
        if card.get_attribute("data-value").as_deref() == Some(key) {
            focus(&state.document, &card);
            return true;
        }
    }
    false
}

pub fn focus_result(app: &Shared, key: &str) -> bool {
    focus_search_result(app, Some(key))
}

pub(super) fn reveal_library_card(app: &Shared, key: &str) -> bool {
    {
        let mut state = app.borrow_mut();
        if state.view != View::Library || state.detail.is_some() {
            return false;
        }
        let catalog = state.library_catalog();
        let (matches, _) = catalog.search(&state.query, "", false, usize::MAX);
        let Some(position) = matches
            .into_iter()
            .position(|index| catalog.items[index].key() == key)
        else {
            return false;
        };
        let page = position / PAGE_SIZE;
        if state.page == page {
            return false;
        }
        state.page = page;
    }
    render_content(app);
    focus_result(app, key)
}

pub fn focus_first_result(app: &Shared) -> bool {
    focus_search_result(app, None)
}

pub(super) fn focus_last_result(app: &Shared) -> bool {
    let state = app.borrow();
    let row = state
        .element("content")
        .query_selector_all("#search-results .tile")
        .ok()
        .and_then(|items| items.item(items.length().checked_sub(1)?))
        .and_then(|item| item.dyn_into::<Element>().ok());
    if let Some(row) = row {
        focus(&state.document, &row);
        true
    } else {
        false
    }
}

pub fn leave_search(app: &Shared) {
    if focus_first_result(app) {
        return;
    }
    let state = app.borrow();
    let active_view = state
        .element("navigation")
        .query_selector("[aria-current=page]")
        .ok()
        .flatten();
    let document = state.document.clone();
    drop(state);
    if let Some(active_view) = active_view {
        focus(&document, &active_view);
    }
}

fn focus_search_result(app: &Shared, key: Option<&str>) -> bool {
    let state = app.borrow();
    let result = result_element(&state, key);
    let document = state.document.clone();
    drop(state);
    if let Some(result) = result {
        focus(&document, &result);
        return true;
    }
    false
}

pub(super) fn result_element(state: &App, key: Option<&str>) -> Option<Element> {
    let matches = state
        .element("content")
        .query_selector_all("#search-results .tile, .guide-result-tile")
        .ok()?;
    for index in 0..matches.length() {
        let Some(element) = matches
            .item(index)
            .and_then(|item| item.dyn_into::<Element>().ok())
        else {
            continue;
        };
        if key.is_none_or(|key| element.get_attribute("data-value").as_deref() == Some(key)) {
            return Some(element);
        }
    }
    None
}

pub fn refresh_group_editor(state: &App) {
    let Some(editor) = state.document.get_element_by_id("group-settings") else {
        return;
    };
    let active = state
        .document
        .active_element()
        .filter(|element| editor.contains(Some(element)))
        .map(|element| element.id());
    editor.set_text_content(None);
    let Some(catalog) = state.model.get(&Kind::Live) else {
        if state.settings.source.configured() {
            state.placeholder(&editor, tr("Zenders laden…"));
        }
        return;
    };
    if state.adding_source || catalog.categories(false).is_empty() {
        return;
    }
    let prefs = state.live_groups().cloned().unwrap_or_default();
    let groups = prefs.ordered(catalog.categories(false));
    let selection = groups.iter().position(|name| *name == state.editing_group);
    let label = selection.map_or(tr("Zendergroep kiezen"), |index| groups[index]);
    let row = state.node(&editor, "div", "actions settings-source-actions", "");
    super::picker::trigger(state, &row, Choice::Group, label);
    if let Some(index) = selection {
        let toggle = if prefs.hidden.contains(label) {
            tr("Tonen")
        } else {
            tr("Verbergen")
        };
        for (cmd, name, disabled) in [
            ("group-toggle", toggle, false),
            ("group-up", tr("Hoger"), index == 0),
            ("group-down", tr("Lager"), index + 1 == groups.len()),
        ] {
            let button = state.button(&row, name, cmd, "", "");
            button.set_id(cmd);
            if disabled {
                let _ = button.set_attribute("disabled", "");
            }
        }
    }
    if let Some(active) = active {
        let target = state
            .document
            .get_element_by_id(&active)
            .filter(|target| !target.has_attribute("disabled"))
            .unwrap_or_else(|| state.element("group-edit"));
        focus(&state.document, &target);
    }
}

pub fn refresh_source_actions(state: &App) {
    let Some(actions) = state.document.get_element_by_id("source-actions") else {
        return;
    };
    actions.set_text_content(None);
    if state.adding_source {
        state.button(&actions, tr("Annuleren"), "source-cancel", "", "");
        update_source_refresh(state);
        return;
    }
    if !state.settings.source.configured() {
        return;
    }
    let add = state.button(&actions, tr("Bron toevoegen"), "source-new", "", "");
    add.set_id("source-new");
    state
        .button(&actions, tr("Verversen"), "source-refresh", "", "")
        .set_id("source-refresh");
    for profile in &state.settings.sources {
        let id = profile.id.to_string();
        let row = state.node(&actions, "div", "actions source-profile", "");
        if Some(profile.id) == state.settings.active_source {
            state.node(&row, "span", "active-source", &profile.name);
        } else {
            state.button(&row, &profile.name, "source-switch", &id, "");
        }
        let remove = state.button(&row, tr("Verwijderen"), "source-remove", &id, "");
        let _ = remove.set_attribute(
            "aria-label",
            &message("Verwijder {name}", &[("name", &profile.name)]),
        );
    }
    update_source_refresh(state);
}

pub(super) fn update_source_refresh(state: &App) {
    let Some(status) = state.document.get_element_by_id("source-refresh-status") else {
        return;
    };
    let message = if let Some(button) = state.document.get_element_by_id("source-refresh") {
        let running = state.source_refresh_busy();
        let _ = button.set_attribute("aria-disabled", if running { "true" } else { "false" });
        if running {
            tr("Catalogus laden…")
        } else {
            match &state.source_refresh {
                SourceRefresh::Finished(Ok(())) => tr("Catalogus bijgewerkt."),
                SourceRefresh::Finished(Err(error)) => error,
                _ => "",
            }
        }
    } else {
        ""
    };
    if status.text_content().as_deref() != Some(message) {
        status.set_text_content(Some(message));
    }
}

pub(super) fn toggle_privacy(state: &App) {
    if let Some(policy) = state.document.get_element_by_id("privacy-policy") {
        let policy: web_sys::HtmlElement = policy.unchecked_into();
        let expanded = policy.hidden();
        policy.set_hidden(!expanded);
        let _ = state
            .element("privacy-toggle")
            .set_attribute("aria-expanded", if expanded { "true" } else { "false" });
    }
}

#[derive(Default)]
pub(super) struct LiveListFocus {
    last_row: Option<String>,
    pointer: Option<(i32, i32)>,
}

fn live_row(node: &Element) -> Option<Element> {
    node.closest(".live-results .result-row").ok()?
}

pub(super) fn track_live_list_focus(app: &Shared, event: &web_sys::Event, pointer: bool) {
    let Ok(state) = app.try_borrow() else { return };
    if !super::live_list_visible(&state)
        || super::picker::is_open(&state)
        || super::search_keyboard::is_open(&state)
    {
        return;
    }
    let mut navigation = state.live_list_focus.borrow_mut();
    if pointer {
        navigation.pointer = event
            .dyn_ref::<web_sys::MouseEvent>()
            .map(|event| (event.client_x(), event.client_y()));
    } else {
        navigation.pointer = None;
        let key = event
            .target()
            .and_then(|node| node.dyn_into::<Element>().ok())
            .and_then(|node| live_row(&node))
            .and_then(|row| row.get_attribute("data-value"));
        if key.is_some() {
            navigation.last_row = key;
        }
    }
}

pub(super) fn focus_pointed_live_row(app: &Shared) {
    let Ok(state) = app.try_borrow() else { return };
    if !super::live_list_visible(&state)
        || state.keyboard_visible.get()
        || super::picker::is_open(&state)
        || super::search_keyboard::is_open(&state)
    {
        return;
    }
    let pointer = state.live_list_focus.borrow_mut().pointer.take();
    if state
        .document
        .active_element()
        .is_some_and(|active| active.tag_name() == "INPUT")
    {
        return;
    }
    let pointed = pointer
        .and_then(|(x, y)| state.document.element_from_point(x as f32, y as f32))
        .and_then(|node| live_row(&node));
    let document = state.document.clone();
    drop(state);
    if let Some(row) = pointed {
        focus(&document, &row);
    }
}

fn page_button(state: &App, value: &str) -> Option<Element> {
    state
        .element("search-results")
        .query_selector(&format!(
            ".pagination [data-cmd='page'][data-value='{value}']"
        ))
        .ok()?
}

fn navigate_result_rows(app: &Shared, active: &Element, key: &str) -> bool {
    use super::PageChange;
    let forward = key == "ArrowDown";
    let state = app.borrow();
    if active
        .matches("#search-results .result-row")
        .unwrap_or(false)
    {
        let neighbor = if forward {
            active.next_element_sibling()
        } else {
            active.previous_element_sibling()
        };
        if let Some(neighbor) = neighbor {
            let document = state.document.clone();
            drop(state);
            focus(&document, &neighbor);
            return true;
        }
    } else {
        let pager = if forward { "next" } else { "prev" };
        if active.get_attribute("data-cmd").as_deref() != Some("page")
            || active.get_attribute("data-value").as_deref() != Some(pager)
            || active.closest("#search-results").ok().flatten().is_none()
        {
            return false;
        }
    }
    let pager = if forward { "next" } else { "prev" };
    if page_button(&state, pager).is_none() {
        return forward;
    }
    drop(state);
    super::turn_page(
        app,
        if forward {
            PageChange::Next
        } else {
            PageChange::PreviousEnd
        },
    );
    true
}

fn enter_live_list(app: &Shared, key: &str) -> bool {
    let state = app.borrow();
    if !super::live_list_visible(&state) || super::search_keyboard::is_open(&state) {
        return false;
    }
    let active = state.document.active_element();
    let lost_focus = active
        .as_ref()
        .is_none_or(|element| matches!(element.tag_name().as_str(), "BODY" | "HTML"));
    if lost_focus {
        let remembered = state.live_list_focus.borrow().last_row.clone();
        if let Some(row) = remembered
            .as_deref()
            .and_then(|key| result_element(&state, Some(key)))
        {
            let document = state.document.clone();
            drop(state);
            focus(&document, &row);
            return false;
        }
    } else if key != "ArrowDown"
        || active.as_ref().is_none_or(|element| {
            !element
                .matches("#navigation [data-value='live'][aria-current='page']")
                .unwrap_or(false)
        })
    {
        return false;
    }
    let first = result_element(&state, None);
    let document = state.document.clone();
    drop(state);
    if let Some(first) = first {
        focus(&document, &first);
        return true;
    }
    false
}

pub fn navigate(app: &Shared, key: &str) -> bool {
    let direction = match key {
        "ArrowLeft" => (-1., 0.),
        "ArrowRight" => (1., 0.),
        "ArrowUp" => (0., -1.),
        "ArrowDown" => (0., 1.),
        _ => return false,
    };
    if matches!(key, "ArrowUp" | "ArrowDown") {
        focus_pointed_live_row(app);
        if enter_live_list(app, key) {
            return true;
        }
        let active = app.borrow().document.active_element();
        if let Some(active) = active
            && navigate_result_rows(app, &active, key)
        {
            return true;
        }
    }
    let state = app.borrow();
    let doc: &Document = &state.document;
    let active = doc.active_element();
    if matches!(key, "ArrowLeft" | "ArrowRight")
        && active
            .as_ref()
            .is_some_and(|row| row.matches(".result-row").unwrap_or(false))
    {
        let page = if key == "ArrowLeft" { "prev" } else { "next" };
        let available = state
            .element("content")
            .query_selector(&format!("[data-cmd='page'][data-value='{page}']"))
            .ok()
            .flatten()
            .is_some();
        drop(state);
        if available {
            super::dispatch(app, "page", page);
        }
        return true;
    }
    if let Some(active) = &active
        && let Some(target) = picker_boundary(&state, active, key)
    {
        focus(doc, &target);
        return true;
    }
    let current = active
        .as_ref()
        .filter(|element| matches!(element.tag_name().as_str(), "BUTTON" | "INPUT"))
        .map(Element::get_bounding_client_rect);
    let search_keyboard = super::search_keyboard::is_open(&state);
    let active_row = active.as_ref().and_then(|element| element.parent_element());
    let root = if search_keyboard {
        state.element("search-keyboard")
    } else if state.live_guide.is_open() {
        state.element("live-guide-modal")
    } else if state.playback.is_some() {
        let panel = state.playback.as_ref().is_some_and(|play| play.panel);
        state.element(if panel {
            "subtitle-panel"
        } else {
            "player-actions"
        })
    } else {
        state.element("app")
    };
    let Ok(candidates) = root.query_selector_all("button:not([disabled]), input:not([disabled])")
    else {
        return false;
    };
    let mut best: Option<(Element, f64)> = None;
    for index in 0..candidates.length() {
        let Some(element) = candidates
            .item(index)
            .and_then(|node| node.dyn_into::<Element>().ok())
        else {
            continue;
        };
        if active.as_ref() == Some(&element) {
            continue;
        }
        if search_keyboard
            && direction.0 != 0.
            && active_row.is_some()
            && active_row != element.parent_element()
        {
            continue;
        }
        let rect = element.get_bounding_client_rect();
        if rect.width() <= 0. || rect.height() <= 0. {
            continue;
        }
        let score = if let Some(current) = &current {
            let x = rect.x() + rect.width() / 2. - current.x() - current.width() / 2.;
            let y = rect.y() + rect.height() / 2. - current.y() - current.height() / 2.;
            let forward = x * direction.0 + y * direction.1;
            if forward <= 2. {
                continue;
            }
            let across = (x * direction.1 - y * direction.0).abs();
            forward + across * 3.
        } else {
            index as f64
        };
        if best
            .as_ref()
            .is_none_or(|(_, best_score)| score < *best_score)
        {
            best = Some((element, score));
        }
    }
    if let Some((element, _)) = best {
        if search_keyboard {
            super::search_keyboard::focus_key(doc, &element);
        } else {
            focus(doc, &element);
        }
        return true;
    }
    false
}

pub(super) fn first_below_picker(state: &App, choice: Choice) -> Option<Element> {
    match choice {
        Choice::Category => result_element(state, None),
        Choice::Season => state.element("content").query_selector(".episode").ok()?,
        Choice::Group => None,
    }
}

fn picker_boundary(state: &App, active: &Element, key: &str) -> Option<Element> {
    if key == "ArrowDown" && state.view == View::Library && state.detail.is_none() {
        match active.get_attribute("data-cmd").as_deref() {
            Some("view") if active.get_attribute("data-value").as_deref() == Some("library") => {
                return state
                    .element("content")
                    .query_selector(".library-tabs button[aria-pressed=true]")
                    .ok()
                    .flatten();
            }
            Some("library-section") => return result_element(state, None),
            _ => {}
        }
    }
    match key {
        "ArrowDown" if active.get_attribute("data-cmd").as_deref() == Some("continue-item") => {
            result_element(state, None)
        }
        "ArrowDown" => {
            let choice = Choice::from_id(&active.id())?;
            if matches!(choice, Choice::Category)
                && let Ok(Some(card)) = state.element("content").query_selector(".continue-card")
            {
                return Some(card);
            }
            first_below_picker(state, choice)
        }
        "ArrowUp" => {
            let choice = match active.get_attribute("data-cmd")?.as_str() {
                "episode" => Choice::Season,
                "item" => Choice::Category,
                _ => return None,
            };
            let first_tile = active.parent_element()?.first_element_child()?;
            let first_y = first_tile.get_bounding_client_rect().y();
            let active_y = active.get_bounding_client_rect().y();
            if (first_y - active_y).abs() >= 2. {
                return None;
            }
            if matches!(choice, Choice::Category)
                && let Ok(Some(card)) = state.element("content").query_selector(".continue-card")
            {
                return Some(card);
            }
            state.document.get_element_by_id(choice.id())
        }
        _ => None,
    }
}
