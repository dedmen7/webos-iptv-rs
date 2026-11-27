use std::{cell::Cell, collections::HashMap};
use wasm_bindgen::JsValue;
use web_sys::{Document, Element};

#[derive(Clone, Copy)]
enum Language {
    English,
    German,
    French,
    Dutch,
}

impl Language {
    fn from_locale(locale: &str) -> Self {
        match locale
            .trim()
            .split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "de" => Self::German,
            "fr" => Self::French,
            "nl" => Self::Dutch,
            _ => Self::English,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::German => "de",
            Self::French => "fr",
            Self::Dutch => "nl",
        }
    }
}

thread_local! {
    static LANGUAGE: Cell<Language> = const { Cell::new(Language::English) };
    static TRANSLATIONS: HashMap<&'static str, [&'static str; 3]> = include_str!("translations.tsv")
        .lines()
        .map(|line| {
            let mut fields = line.split('\t');
            let dutch = fields.next().unwrap();
            let translations = std::array::from_fn(|_| fields.next().expect("translation"));
            (dutch, translations)
        })
        .collect();
}

pub(super) fn tr(dutch: &str) -> &str {
    let column = match LANGUAGE.with(Cell::get) {
        Language::English => 0,
        Language::German => 1,
        Language::French => 2,
        Language::Dutch => return dutch,
    };
    TRANSLATIONS.with(|entries| entries.get(dutch).map_or(dutch, |text| text[column]))
}

pub(super) fn message(dutch: &str, values: &[(&str, &str)]) -> String {
    let mut template = tr(dutch);
    let mut output = String::with_capacity(template.len());
    while let Some((before, field)) = template.split_once('{') {
        let Some((name, following)) = field.split_once('}') else {
            break;
        };
        output.push_str(before);
        if let Some((_, value)) = values.iter().find(|(key, _)| *key == name) {
            output.push_str(value);
        } else {
            output.push('{');
            output.push_str(name);
            output.push('}');
        }
        template = following;
    }
    output.push_str(template);
    output
}

pub(super) fn initialize(document: &Document) {
    let Some(window) = document.default_view() else {
        return;
    };
    let system_locale = ["webOSSystem", "PalmSystem"].iter().find_map(|system| {
        let system = js_sys::Reflect::get(&window, &JsValue::from_str(system)).ok()?;
        js_sys::Reflect::get(&system, &JsValue::from_str("locale"))
            .ok()?
            .as_string()
            .filter(|value| !value.trim().is_empty())
    });
    let locale = system_locale
        .or_else(|| window.navigator().language())
        .unwrap_or_default();
    let language = Language::from_locale(&locale);
    LANGUAGE.with(|selected| selected.set(language));
    if let Some(root) = document.document_element() {
        let _ = root.set_attribute("lang", language.tag());
    }
    for (marker, attribute) in [
        ("data-i18n", None),
        ("data-i18n-aria", Some("aria-label")),
        ("data-i18n-placeholder", Some("placeholder")),
    ] {
        let Ok(nodes) = document.query_selector_all(&format!("[{marker}]")) else {
            continue;
        };
        for index in 0..nodes.length() {
            use wasm_bindgen::JsCast;
            let Some(node) = nodes
                .item(index)
                .and_then(|node| node.dyn_into::<Element>().ok())
            else {
                continue;
            };
            if let Some(key) = node.get_attribute(marker) {
                if let Some(attribute) = attribute {
                    let _ = node.set_attribute(attribute, tr(&key));
                } else {
                    node.set_text_content(Some(tr(&key)));
                }
            }
        }
    }
}
