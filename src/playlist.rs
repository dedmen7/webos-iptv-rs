use crate::catalog::{Item, Kind, http_url};
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use url::Url;

pub fn parse(input: &str, base: &Url) -> Vec<Item> {
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    let mut pending = None;
    for line in input.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            pending = entry(info);
        } else if !line.is_empty()
            && !line.starts_with('#')
            && let Some(mut item) = pending.take()
            && let Ok(url) = base.join(line)
            && matches!(url.scheme(), "http" | "https")
            && seen.insert(url.to_string())
        {
            item.stream = url.to_string();
            let mut hash = DefaultHasher::new();
            item.stream.hash(&mut hash);
            item.id = format!("m3u-{:016x}", hash.finish());
            output.push(item);
        }
    }
    output
}

pub fn guide_url(input: &str, base: &Url) -> Option<Url> {
    let header = input
        .lines()
        .find(|line| line.trim_start_matches('\u{feff}').starts_with("#EXTM3U"))?;
    let properties = attributes(header);
    ["url-tvg", "x-tvg-url", "tvg-url"]
        .iter()
        .filter_map(|key| properties.get(*key))
        .flat_map(|value| value.split(','))
        .find_map(|value| {
            http_url(value).or_else(|| {
                base.join(value.trim())
                    .ok()
                    .filter(|u| matches!(u.scheme(), "http" | "https"))
            })
        })
}

fn entry(info: &str) -> Option<Item> {
    let mut quoted = false;
    let separator = info.char_indices().find_map(|(index, ch)| {
        if ch == '"' {
            quoted = !quoted;
        }
        (ch == ',' && !quoted).then_some(index)
    })?;
    let attrs = attributes(&info[..separator]);
    let title = info[separator + 1..].trim();
    let title = if title.is_empty() {
        attrs.get("tvg-name").copied().unwrap_or_default()
    } else {
        title
    };
    if title.is_empty() {
        return None;
    }
    Some(Item {
        title: title.to_owned(),
        kind: Kind::Live,
        epg_id: attrs.get("tvg-id").copied().unwrap_or_default().to_owned(),
        poster: attrs
            .get("tvg-logo")
            .copied()
            .unwrap_or_default()
            .to_owned(),
        category: attrs
            .get("group-title")
            .copied()
            .unwrap_or_default()
            .to_owned(),
        ..Item::default()
    })
}

fn attributes(mut text: &str) -> HashMap<&str, &str> {
    let mut output = HashMap::new();
    while let Some((left, right)) = text.split_once('=') {
        let name = left.split_whitespace().next_back().unwrap_or_default();
        let mut rest = right.trim_start();
        let value = if let Some(quoted) = rest.strip_prefix('"') {
            let Some((value, after)) = quoted.split_once('"') else {
                break;
            };
            rest = after;
            value
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let value = &rest[..end];
            rest = &rest[end..];
            value
        };
        output.insert(name, value);
        text = rest;
    }
    output
}
