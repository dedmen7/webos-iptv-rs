use super::i18n::tr;
use super::{App, Shared, focus, set_search, ui};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, HtmlInputElement};

pub fn build(state: &App) {
    let keys = state.element("search-keyboard-keys");
    for letters in ["1234567890", "QWERTYUIOP", "ASDFGHJKL", "ZXCVBNM"] {
        let row = state.node(&keys, "div", "search-keyboard-row", "");
        for letter in letters.chars() {
            let letter = letter.to_string();
            state.button(&row, &letter, "search-key", &letter, "");
        }
    }
    let row = state.node(&keys, "div", "search-keyboard-row", "");
    state.button(&row, tr("Wissen"), "search-key", "delete", "");
    state.button(&row, tr("Spatie"), "search-key", "space", "");
    state.button(&row, tr("Zoeken"), "search-key", "done", "primary");
}

pub fn is_open(state: &App) -> bool {
    !state
        .element("search-keyboard")
        .unchecked_into::<HtmlElement>()
        .hidden()
}

pub fn open(app: &Shared) {
    let state = app.borrow();
    let input = state.element("search").unchecked_into::<HtmlInputElement>();
    let document = state.document.clone();
    let panel = state
        .element("search-keyboard")
        .unchecked_into::<HtmlElement>();
    let trigger = state.element("search-keys");
    let title = state.element("search-keyboard-title");
    let first = panel
        .query_selector("[data-cmd=search-key][data-value=Q]")
        .ok()
        .flatten();
    drop(state);
    if input.hidden() {
        return;
    }
    let _ = input.blur();
    title.set_text_content(Some(&input.placeholder()));
    panel.set_hidden(false);
    let _ = trigger.set_attribute("aria-expanded", "true");
    update_query(app);
    if let Some(first) = first {
        focus_key(&document, &first);
    }
}

pub fn hide(state: &App) {
    state
        .element("search-keyboard")
        .unchecked_into::<HtmlElement>()
        .set_hidden(true);
    let _ = state
        .element("search-keys")
        .set_attribute("aria-expanded", "false");
}

pub fn close(app: &Shared, show_results: bool) {
    hide(&app.borrow());
    if !show_results || !ui::focus_first_result(app) {
        let state = app.borrow();
        focus(&state.document, &state.element("search-keys"));
    }
}

pub fn cycle(app: &Shared, backward: bool) {
    let state = app.borrow();
    let Ok(keys) = state
        .element("search-keyboard")
        .query_selector_all("button")
    else {
        return;
    };
    let active = state.document.active_element();
    let count = keys.length();
    let current =
        (0..count).find(|&index| keys.item(index).and_then(|node| node.dyn_into().ok()) == active);
    let index = match (current, backward) {
        (Some(index), false) => (index + 1) % count,
        (Some(index), true) => (index + count - 1) % count,
        (None, false) => 0,
        (None, true) => count.saturating_sub(1),
    };
    if let Some(key) = keys.item(index).and_then(|node| node.dyn_into().ok()) {
        focus_key(&state.document, &key);
    }
}

pub fn focus_key(document: &Document, key: &Element) {
    mark_key(document, key);
    focus(document, key);
}

pub fn mark_key(document: &Document, key: &Element) {
    if let Ok(Some(previous)) = document.query_selector("#search-keyboard [data-key-focused]") {
        let _ = previous.remove_attribute("data-key-focused");
    }
    let _ = key.set_attribute("data-key-focused", "");
}

pub fn press(app: &Shared, key: &str) {
    if key == "done" {
        close(app, true);
        return;
    }
    let input = app
        .borrow()
        .element("search")
        .unchecked_into::<HtmlInputElement>();
    let mut query = input.value();
    match key {
        "delete" => {
            query.pop();
        }
        "space" => query.push(' '),
        character => query.push_str(character),
    }
    input.set_value(&query);
    set_search(app, query);
    update_query(app);
}

fn update_query(app: &Shared) {
    let state = app.borrow();
    state
        .element("search-keyboard-query")
        .set_text_content(Some(&state.query));
}
