use super::i18n::{message, tr};
use super::{App, Shared, focus, ui};
use crate::catalog::{BrowseFilter, Genre, Kind};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, KeyboardEvent};

pub(super) struct BrowseOptions<'a> {
    pub heading: &'static str,
    all_label: &'static str,
    pub selections: Vec<BrowseFilter<&'a str>>,
}

impl BrowseOptions<'_> {
    pub fn label<'a>(&self, selection: BrowseFilter<&'a str>) -> &'a str {
        match selection {
            BrowseFilter::All => self.all_label,
            BrowseFilter::Category(name) => name,
            BrowseFilter::Genre(genre) => tr(genre_label(genre)),
        }
    }
}

pub(super) fn browse_options(state: &App, kind: Kind) -> Option<BrowseOptions<'_>> {
    let catalog = state.browse_catalog(kind)?;
    let preferred = state.settings.preferred;
    let genres = catalog.genres(preferred);
    let categories = catalog.categories(preferred);
    if genres.len() > 1 && (categories.len() < 2 || catalog.genre_covers_at_least_half(preferred)) {
        let mut selections: Vec<_> = genres.iter().copied().collect();
        selections.sort_by_key(|&genre| tr(genre_label(genre)));
        return Some(BrowseOptions {
            heading: tr("Genre"),
            all_label: tr("Alle genres"),
            selections: selections.into_iter().map(BrowseFilter::Genre).collect(),
        });
    }

    let groups = (kind == Kind::Live).then(|| state.live_groups()).flatten();
    let names = if let Some(groups) = groups {
        groups
            .ordered(categories)
            .into_iter()
            .filter(|name| !groups.hidden.contains(*name))
            .collect::<Vec<_>>()
    } else {
        categories.iter().map(String::as_str).collect()
    };
    (names.len() > 1).then(|| BrowseOptions {
        heading: tr("Categorie"),
        all_label: tr("Alle categorieën"),
        selections: names.into_iter().map(BrowseFilter::Category).collect(),
    })
}

fn genre_label(genre: Genre) -> &'static str {
    match genre {
        Genre::Action => "Actie",
        Genre::Adventure => "Avontuur",
        Genre::Animation => "Animatie",
        Genre::Comedy => "Komedie",
        Genre::Crime => "Misdaad",
        Genre::Documentary => "Documentaire",
        Genre::Drama => "Drama",
        Genre::Family => "Familie",
        Genre::Fantasy => "Fantasy",
        Genre::History => "Geschiedenis",
        Genre::Horror => "Horror",
        Genre::Mystery => "Mysterie",
        Genre::Romance => "Romantiek",
        Genre::ScienceFiction => "Sciencefiction",
        Genre::Thriller => "Thriller",
        Genre::War => "Oorlog",
        Genre::Western => "Western",
    }
}

fn filter_key(filter: BrowseFilter<&str>) -> String {
    match filter {
        BrowseFilter::All => String::new(),
        BrowseFilter::Category(name) => name.to_owned(),
        BrowseFilter::Genre(genre) => format!("genre:{}", genre_label(genre)),
    }
}

#[derive(Clone, Copy)]
pub(super) enum Choice {
    Category,
    Season,
    Group,
}

impl Choice {
    pub(super) fn from_id(id: &str) -> Option<Self> {
        match id {
            "category" => Some(Self::Category),
            "series-season" => Some(Self::Season),
            "group-edit" => Some(Self::Group),
            _ => None,
        }
    }

    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Category => "category",
            Self::Season => "series-season",
            Self::Group => "group-edit",
        }
    }

    fn title(self, state: &App) -> &'static str {
        match self {
            Self::Category => state
                .view
                .kind()
                .and_then(|kind| browse_options(state, kind))
                .map_or(tr("Categorie"), |options| options.heading),
            Self::Season => tr("Seizoen"),
            Self::Group => tr("Zendergroepen"),
        }
    }

    fn options(self, state: &App) -> Option<(String, Vec<(String, String)>)> {
        match self {
            Self::Category => {
                let browse = browse_options(state, state.view.kind()?)?;
                let options = std::iter::once(BrowseFilter::All)
                    .chain(browse.selections.iter().copied())
                    .map(|filter| (filter_key(filter), browse.label(filter).to_owned()))
                    .collect();
                Some((filter_key(state.catalog_filter.borrowed()), options))
            }
            Self::Season => {
                let detail = state.detail.as_ref()?;
                let first = detail.episodes.first()?.season;
                let mut seasons: Vec<_> = detail
                    .episodes
                    .iter()
                    .map(|episode| episode.season)
                    .collect();
                seasons.dedup();
                let options = seasons
                    .into_iter()
                    .map(|season| {
                        (
                            season.to_string(),
                            message("Seizoen {season}", &[("season", &season.to_string())]),
                        )
                    })
                    .collect();
                Some((detail.season.unwrap_or(first).to_string(), options))
            }
            Self::Group => {
                let catalog = state.model.get(&crate::catalog::Kind::Live)?;
                let groups = state.live_groups().cloned().unwrap_or_default();
                let options = groups
                    .ordered(catalog.categories(false))
                    .into_iter()
                    .map(|name| (name.to_owned(), name.to_owned()))
                    .collect();
                Some((state.editing_group.clone(), options))
            }
        }
    }
}

fn dialog(state: &App) -> HtmlElement {
    state.element("catalog-picker").unchecked_into()
}

pub fn is_open(state: &App) -> bool {
    !dialog(state).hidden()
}

pub fn owns(state: &App, button: &Element) -> bool {
    dialog(state).contains(Some(button))
}

pub(super) fn trigger(state: &App, parent: &Element, choice: Choice, label: &str) {
    let button = state.button(parent, label, "picker-open", choice.id(), "");
    button.set_id(choice.id());
    let _ = button.set_attribute("aria-haspopup", "dialog");
    let _ = button.set_attribute("aria-controls", "catalog-picker");
    let expanded = dialog(state).get_attribute("data-trigger").as_deref() == Some(choice.id());
    let _ = button.set_attribute("aria-expanded", if expanded { "true" } else { "false" });
}

pub fn open(app: &Shared, id: &str) {
    let state = app.borrow();
    if Choice::from_id(id).is_none() {
        return;
    }
    let picker = dialog(&state);
    let _ = picker.set_attribute("data-trigger", id);
    picker.set_hidden(false);
    refresh(&state);
}

pub(super) fn refresh(state: &App) {
    let picker = dialog(state);
    let Some(id) = picker.get_attribute("data-trigger") else {
        return;
    };
    let Some(choice) = Choice::from_id(&id) else {
        return;
    };
    let Some(trigger) = state.document.get_element_by_id(&id) else {
        hide(state);
        if matches!(choice, Choice::Category)
            && let Some(target) = super::ui::result_element(state, None).or_else(|| {
                state
                    .element("navigation")
                    .query_selector("[aria-current=page]")
                    .ok()
                    .flatten()
            })
        {
            focus(&state.document, &target);
        }
        return;
    };
    let Some((selected, choices)) = choice.options(state) else {
        hide(state);
        focus(&state.document, &trigger);
        return;
    };
    let _ = trigger.set_attribute("aria-expanded", "true");
    let options = state.element("catalog-picker-options");
    let existing = options.children();
    if existing.length() as usize == choices.len()
        && choices.iter().enumerate().all(|(index, (value, _))| {
            existing.item(index as u32).is_some_and(|button| {
                button.get_attribute("data-value").as_deref() == Some(value.as_str())
            })
        })
    {
        return;
    }
    let active = state.document.active_element();
    let focused_value = active
        .as_ref()
        .filter(|element| options.contains(Some(element)))
        .and_then(|element| element.get_attribute("data-value"));
    let keep_close = active.is_some_and(|element| element.id() == "catalog-picker-close");
    options.set_text_content(None);
    state
        .element("catalog-picker-title")
        .set_text_content(Some(choice.title(state)));
    let mut current = None;
    let mut fallback = None;
    for (value, label) in choices {
        let option = state.button(&options, &label, "picker-select", &value, "");
        let selected = value == selected;
        let _ = option.set_attribute("aria-pressed", if selected { "true" } else { "false" });
        if focused_value.as_ref() == Some(&value) {
            current = Some(option.clone());
        }
        if selected || fallback.is_none() {
            fallback = Some(option);
        }
    }
    if !keep_close && let Some(option) = current.or(fallback) {
        focus(&state.document, &option);
    }
}

pub fn hide(state: &App) {
    let picker = dialog(state);
    if let Some(id) = picker.get_attribute("data-trigger")
        && let Some(trigger) = state.document.get_element_by_id(&id)
    {
        let _ = trigger.set_attribute("aria-expanded", "false");
    }
    let _ = picker.remove_attribute("data-trigger");
    picker.set_hidden(true);
    state
        .element("catalog-picker-options")
        .set_text_content(None);
}

pub fn close(app: &Shared) {
    let state = app.borrow();
    let trigger = dialog(&state)
        .get_attribute("data-trigger")
        .and_then(|id| state.document.get_element_by_id(&id));
    hide(&state);
    if let Some(trigger) = trigger {
        focus(&state.document, &trigger);
    }
}

pub fn restore_focus(app: &Shared) -> bool {
    let state = app.borrow();
    if state
        .document
        .active_element()
        .is_some_and(|element| owns(&state, &element))
    {
        return false;
    }
    let options = state.element("catalog-picker-options");
    let next = options
        .query_selector("[aria-pressed=true]")
        .ok()
        .flatten()
        .or_else(|| options.first_element_child())
        .unwrap_or_else(|| state.element("catalog-picker-close"));
    focus(&state.document, &next);
    true
}

pub fn select(app: &Shared, value: &str) {
    let choice = {
        let mut state = app.borrow_mut();
        let Some(choice) = dialog(&state)
            .get_attribute("data-trigger")
            .as_deref()
            .and_then(Choice::from_id)
        else {
            return;
        };
        let changed = match choice {
            Choice::Category => {
                let next = if value.is_empty() {
                    BrowseFilter::All
                } else {
                    let Some(options) = state
                        .view
                        .kind()
                        .and_then(|kind| browse_options(&state, kind))
                    else {
                        return;
                    };
                    match options
                        .selections
                        .into_iter()
                        .find(|&filter| filter_key(filter) == value)
                    {
                        Some(BrowseFilter::Category(name)) => {
                            BrowseFilter::Category(name.to_owned())
                        }
                        Some(BrowseFilter::Genre(genre)) => BrowseFilter::Genre(genre),
                        _ => return,
                    }
                };
                let changed = state.catalog_filter != next;
                state.catalog_filter = next;
                changed
            }
            Choice::Season => {
                let Some(detail) = state.detail.as_mut() else {
                    return;
                };
                let Ok(season) = value.parse() else {
                    return;
                };
                let changed = detail.season != Some(season);
                detail.season = Some(season);
                changed
            }
            Choice::Group => {
                state.editing_group = value.to_owned();
                false
            }
        };
        hide(&state);
        if changed {
            state.page = 0;
        }
        choice
    };
    if matches!(choice, Choice::Group) {
        ui::refresh_group_editor(&app.borrow());
    } else {
        ui::render_content(app);
    }
    let state = app.borrow();
    if let Some(target) = ui::first_below_picker(&state, choice)
        .or_else(|| state.document.get_element_by_id(choice.id()))
    {
        focus(&state.document, &target);
    }
}

pub fn move_focus(app: &Shared, event: &KeyboardEvent, key: &str) {
    let state = app.borrow();
    let close = state.element("catalog-picker-close");
    let options = state.element("catalog-picker-options");
    let backwards = key == "ArrowUp" || (key == "Tab" && event.shift_key());
    let active = state.document.active_element();
    let next = match active {
        Some(active) if active == close && backwards && key == "Tab" => {
            options.last_element_child()
        }
        Some(active) if active == close && backwards => None,
        Some(active) if active == close => options.first_element_child(),
        Some(active) if options.contains(Some(&active)) && backwards => {
            active.previous_element_sibling().or(Some(close))
        }
        Some(active) if options.contains(Some(&active)) => active
            .next_element_sibling()
            .or_else(|| (key == "Tab").then_some(close)),
        _ => Some(close),
    };
    if let Some(target) = next {
        focus(&state.document, &target);
    }
}
