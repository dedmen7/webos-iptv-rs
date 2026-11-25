use super::{App, PAGE_SIZE, Shared, View, focus, io};
use crate::catalog::{Catalog, Item, Kind, http_url, preferred_content};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlInputElement, HtmlSelectElement};

pub fn render(app: &Shared) {
    let state = app.borrow();
    let nav = state.element("navigation");
    for (index, view) in View::ALL.iter().enumerate() {
        if let Some(link) = nav.children().item(index as u32) {
            let _ = link.set_attribute(
                "aria-current",
                if state.view == *view { "page" } else { "false" },
            );
        }
    }
    mark_filter(&state);
    let searchable = !matches!(state.view, View::Settings | View::Guide) && state.detail.is_none();
    let search = state.element("search").unchecked_into::<HtmlInputElement>();
    search.set_hidden(!searchable);
    search.set_placeholder(&format!("Zoeken in {}", state.view.name().to_lowercase()));
    drop(state);
    render_content(app);
}

pub(super) fn mark_filter(state: &App) {
    let filter = state.element("preferred");
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
            View::Guide => state.render_guide(&content),
            _ => state.render_browse(&content),
        }
    }
    if let Some(key) = focus_key {
        let tags = content.query_selector_all("button, input, select").ok();
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
                    break;
                }
            }
        }
    }
}

pub fn render_search(app: &Shared) {
    let state = app.borrow();
    if let Some(results) = state.document.get_element_by_id("search-results") {
        super::poster::discard_under(&results);
        results.set_text_content(None);
        state.render_results(&results);
    }
}

fn focus_identity(element: &Element) -> Option<(String, String)> {
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

    fn header(&self, parent: &Element, title: &str, caption: &str) -> Element {
        let header = self.node(parent, "div", "heading", "");
        let text = self.node(&header, "div", "heading-copy", "");
        if !caption.is_empty() {
            self.node(&text, "p", "eyebrow", caption);
        }
        self.node(&text, "h1", "", title);
        header
    }

    fn placeholder(&self, parent: &Element, text: &str) {
        self.node(parent, "p", "empty", text);
    }

    fn poster(&self, parent: &Element, item: &Item) {
        let artwork = self.node(parent, "span", "artwork", "");
        if let Some(url) = http_url(&item.poster) {
            let image = self.node(&artwork, "img", "", "");
            let _ = image.set_attribute("alt", "");
            let _ = image.set_attribute("decoding", "async");
            let _ = image.set_attribute("loading", "lazy");
            let _ = image.set_attribute("referrerpolicy", "no-referrer");
            super::poster::mount(&image, url.as_str());
        }
    }

    fn render_settings(&self, parent: &Element) {
        self.header(parent, "Instellingen", "");
        let form = self.node(parent, "section", "settings-grid", "");
        let source = self.node(&form, "div", "settings-card", "");
        self.node(&source, "h2", "", "Tv-bron");
        self.entry_field(
            &source,
            "source-server",
            "Xtream-server",
            &self.settings.source.server,
            "url",
        );
        let pair = self.node(&source, "div", "field-pair", "");
        self.entry_field(
            &pair,
            "source-user",
            "Gebruikersnaam",
            &self.settings.source.username,
            "text",
        );
        self.entry_field(
            &pair,
            "source-pass",
            "Wachtwoord",
            &self.settings.source.password,
            "password",
        );
        self.entry_field(
            &source,
            "source-m3u",
            "M3U-link",
            &self.settings.source.playlist,
            "url",
        );
        self.entry_field(
            &source,
            "source-epg",
            "XMLTV-link",
            &self.settings.source.guide,
            "url",
        );
        let subtitles = self.node(&form, "div", "settings-card", "");
        self.node(&subtitles, "h2", "", "Online ondertitels");
        self.entry_field(
            &subtitles,
            "source-os",
            "OpenSubtitles API-sleutel",
            &self.settings.opensubtitles,
            "password",
        );
        self.entry_field(
            &subtitles,
            "source-subdl",
            "SubDL API-sleutel",
            &self.settings.subdl,
            "password",
        );
        self.button(parent, "Opslaan", "save", "", "primary settings-save");
    }

    fn render_browse(&self, parent: &Element) {
        let caption = match self.view {
            View::Live => "Televisie",
            View::Movies | View::Series => "Op aanvraag",
            _ => "",
        };
        let header = self.header(parent, self.view.name(), caption);
        if !self.settings.source.configured() {
            self.button(&header, "Bron instellen", "view", "settings", "primary");
            return;
        }
        if let Some(kind) = self.view.kind() {
            self.button(&header, "Verversen", "refresh", "", "");
            if let Some(catalog) = self.model.get(&kind) {
                let category = self.selector(parent, "category", "Categorie");
                self.option(&category, "", "Alle categorieën");
                for name in catalog.categories(self.settings.preferred) {
                    self.option(&category, name, name);
                }
                category.set_value(&self.category);
            }
        }
        let results = self.node(parent, "section", "", "");
        results.set_id("search-results");
        self.render_results(&results);
    }

    fn render_results(&self, parent: &Element) {
        if self.view == View::Library {
            let prefix = format!("{}:", self.source_key);
            let mut items: Vec<_> = self
                .library
                .recent
                .iter()
                .chain(&self.library.favorites)
                .filter(|(key, _)| key.starts_with(&prefix))
                .map(|(_, value)| value.clone())
                .collect();
            items.sort_by(|left, right| left.title.cmp(&right.title));
            let mut seen = std::collections::HashSet::new();
            items.retain(|item| seen.insert(item.key()));
            self.render_items(parent, &Catalog::new(items));
        } else if let Some(kind) = self.view.kind() {
            if let Some(error) = self.failures.get(&kind) {
                self.placeholder(parent, error);
            }
            if let Some(catalog) = self.model.get(&kind) {
                self.render_items(parent, catalog);
            } else if self.pending.contains(&kind) {
                self.placeholder(parent, "Catalogus laden…");
            }
        }
    }

    fn render_items(&self, parent: &Element, catalog: &Catalog) {
        let limit = (self.page + 1) * PAGE_SIZE;
        let (hits, count) =
            catalog.search(&self.query, &self.category, self.settings.preferred, limit);
        if count == 0 {
            self.placeholder(
                parent,
                if self.query.trim().is_empty() {
                    "Geen titels in deze lijst."
                } else {
                    "Geen resultaten gevonden."
                },
            );
            return;
        }
        let limit = (self.page * PAGE_SIZE).min(hits.len());
        let tiles = self.node(
            parent,
            "div",
            if self.view == View::Live {
                "tiles live-tiles"
            } else {
                "tiles"
            },
            "",
        );
        for index in &hits[limit..] {
            let item = &catalog.items[*index];
            let tile = self.button(&tiles, "", "item", &item.key(), "tile");
            self.poster(&tile, item);
            self.node(&tile, "span", "tile-title", &item.title);
            if self.position(item) > 0. {
                self.node(&tile, "span", "tile-meta", "Verder kijken");
            } else if !item.category.is_empty() {
                self.node(&tile, "span", "tile-meta", &item.category);
            }
        }
        self.pagination(parent, count, hits.len());
    }

    fn pagination(&self, parent: &Element, count: usize, end: usize) {
        let footer = self.node(parent, "div", "pagination", "");
        if self.page > 0 {
            self.button(&footer, "Vorige", "page", "prev", "");
        }
        self.node(
            &footer,
            "span",
            "muted",
            &format!("{}–{} van {}", self.page * PAGE_SIZE + 1, end, count),
        );
        if end < count {
            self.button(&footer, "Volgende", "page", "next", "");
        }
    }

    fn render_detail(&self, parent: &Element, detail: &super::Detail) {
        let item = &detail.item;
        let hero = self.node(parent, "section", "detail", "");
        self.poster(&hero, item);
        let copy = self.node(&hero, "div", "detail-copy", "");
        self.button(&copy, "Terug", "back", "", "back-button");
        self.header(
            &copy,
            &item.title,
            if item.kind == Kind::Series {
                "Serie"
            } else {
                "Film"
            },
        );
        if !item.plot.is_empty() {
            self.node(&copy, "p", "plot", &item.plot);
        }
        let actions = self.node(&copy, "div", "actions", "");
        if item.kind == Kind::Movie {
            self.button(
                &actions,
                if self.position(item) > 0. {
                    "Verder kijken"
                } else {
                    "Afspelen"
                },
                "watch",
                "",
                "primary",
            );
            let subtitles = self.button(&actions, "Ondertitels", "watch-subtitles", "", "");
            if detail.loading {
                let _ = subtitles.set_attribute("disabled", "");
            }
        }
        let saved = self.library.favorites.contains_key(&self.library_key(item));
        self.button(
            &actions,
            if saved {
                "Verwijder uit mijn lijst"
            } else {
                "Bewaar in mijn lijst"
            },
            "save-title",
            &item.key(),
            "",
        );
        if item.kind != Kind::Series {
            return;
        }
        if detail.loading {
            self.placeholder(parent, "Afleveringen laden…");
            return;
        }
        if let Some(error) = &detail.error {
            self.placeholder(parent, error);
            return;
        }
        if detail.episodes.is_empty() {
            self.placeholder(parent, "Geen afleveringen gevonden.");
            return;
        }
        let select = self.selector(parent, "series-season", "Seizoen");
        let mut previous = None;
        for season in detail.episodes.iter().map(|episode| episode.season) {
            if previous != Some(season) {
                self.option(&select, &season.to_string(), &format!("Seizoen {season}"));
            }
            previous = Some(season);
        }
        if let Some(season) = detail.season {
            select.set_value(&season.to_string());
        }
        let episodes: Vec<_> = detail
            .episodes
            .iter()
            .filter(|item| detail.season.is_none_or(|season| item.season == season))
            .collect();
        let list = self.node(parent, "div", "episode-list", "");
        for episode in episodes.iter().skip(self.page * PAGE_SIZE).take(PAGE_SIZE) {
            let title = format!("{} · {}", episode.episode, episode.title);
            self.button(&list, &title, "episode", &episode.id, "episode");
        }
        if episodes.len() > PAGE_SIZE {
            self.pagination(
                parent,
                episodes.len(),
                ((self.page + 1) * PAGE_SIZE).min(episodes.len()),
            );
        }
    }

    fn render_guide(&self, parent: &Element) {
        let header = self.header(parent, "Programmagids", "");
        self.button(&header, "Verversen", "refresh", "", "");
        if let Some(error) = &self.guide_error {
            self.placeholder(parent, error);
        }
        if self.guide_pending {
            self.placeholder(parent, "Programma’s laden…");
        }
        if !self.settings.source.configured() {
            self.button(&header, "Bron instellen", "view", "settings", "primary");
            return;
        }
        let (Some(guide), Some(catalog)) = (&self.guide, self.model.get(&Kind::Live)) else {
            return;
        };
        let channels: Vec<_> = catalog
            .items
            .iter()
            .filter(|item| {
                !item.epg_id.is_empty()
                    && guide.contains_key(&item.epg_id)
                    && (!self.settings.preferred || preferred_content(item))
            })
            .collect();
        if channels.is_empty() {
            self.placeholder(parent, "Geen gidsinformatie gevonden voor deze kanalen.");
            return;
        }
        let select = self.selector(parent, "guide-channel", "Zender");
        let active = channels
            .iter()
            .copied()
            .find(|item| item.id == self.guide_channel)
            .unwrap_or(channels[0]);
        for item in channels {
            self.option(&select, &item.id, &item.title);
        }
        select.set_value(&active.id);
        self.button(
            parent,
            "Kijk live",
            "item",
            &active.key(),
            "primary guide-play",
        );
        let list = self.node(parent, "div", "guide-grid", "");
        let now = io::now();
        if let Some(programmes) = guide.get(&active.epg_id) {
            for programme in programmes
                .iter()
                .filter(|entry| entry.stop_unix > now)
                .take(30)
            {
                let row = self.node(&list, "article", "programme", "");
                let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(
                    programme.start_unix as f64 * 1000.,
                ));
                self.node(
                    &row,
                    "time",
                    "",
                    &format!("{:02}:{:02}", date.get_hours(), date.get_minutes()),
                );
                self.node(&row, "h2", "", programme.name());
                if let Some(description) = programme.description.first() {
                    self.node(&row, "p", "", description);
                }
            }
        }
    }

    fn selector(&self, parent: &Element, id: &str, label: &str) -> HtmlSelectElement {
        let wrapper = self.node(parent, "label", "selector", label);
        let element = self
            .node(&wrapper, "select", "", "")
            .unchecked_into::<HtmlSelectElement>();
        element.set_id(id);
        element
    }

    fn option(&self, select: &HtmlSelectElement, value: &str, label: &str) {
        let option = self.node(select, "option", "", label);
        let _ = option.set_attribute("value", value);
    }
}

pub fn focus_catalog(app: &Shared) {
    let state = app.borrow();
    if let Ok(Some(next)) = state
        .element("content")
        .query_selector("button, select, input")
    {
        focus(&state.document, &next);
    }
}

pub fn navigate(app: &Shared, key: &str) -> bool {
    let direction = match key {
        "ArrowLeft" => (-1., 0.),
        "ArrowRight" => (1., 0.),
        "ArrowUp" => (0., -1.),
        "ArrowDown" => (0., 1.),
        _ => return false,
    };
    let state = app.borrow();
    let doc: &Document = &state.document;
    let active = doc.active_element();
    let current = active
        .as_ref()
        .filter(|element| matches!(element.tag_name().as_str(), "BUTTON" | "INPUT" | "SELECT"))
        .map(Element::get_bounding_client_rect);
    let root = if state.playback.is_some() {
        let panel = state.playback.as_ref().is_some_and(|play| play.panel);
        state.element(if panel {
            "subtitle-panel"
        } else {
            "player-actions"
        })
    } else {
        state.element("app")
    };
    let Ok(candidates) = root.query_selector_all(
        "button:not([disabled]), input:not([disabled]), select:not([disabled])",
    ) else {
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
        focus(doc, &element);
        return true;
    }
    false
}
