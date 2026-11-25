use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
use url::Url;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Live,
    #[default]
    Movie,
    Series,
    Episode,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Item {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub category: String,
    pub poster: String,
    pub stream: String,
    pub epg_id: String,
    pub plot: String,
    pub imdb: String,
    pub year: String,
    pub series_title: String,
    pub season: u32,
    pub episode: u32,
    pub sidecars: Vec<Sidecar>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sidecar {
    pub name: String,
    pub lang: String,
    pub url: String,
}

impl Item {
    pub fn key(&self) -> String {
        format!("{:?}:{}", self.kind, self.id)
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Source {
    pub server: String,
    pub username: String,
    pub password: String,
    pub playlist: String,
    pub guide: String,
}

impl Source {
    pub fn configured(&self) -> bool {
        !self.playlist.trim().is_empty() || !self.server.trim().is_empty()
    }

    pub fn api(&self, action: &str, parameter: Option<(&str, &str)>) -> Option<Url> {
        let mut url = self.endpoint("player_api.php")?;
        let mut query = url.query_pairs_mut();
        query.append_pair("username", &self.username);
        query.append_pair("password", &self.password);
        query.append_pair("action", action);
        if let Some((name, value)) = parameter {
            query.append_pair(name, value);
        }
        drop(query);
        Some(url)
    }

    pub fn guide_url(&self) -> Option<Url> {
        if !self.guide.trim().is_empty() {
            return http_url(&self.guide);
        }
        let mut url = self.endpoint("xmltv.php")?;
        url.query_pairs_mut()
            .append_pair("username", &self.username)
            .append_pair("password", &self.password);
        Some(url)
    }

    pub fn playback(&self, kind: Kind, id: &str, extension: &str) -> Option<Url> {
        if id.is_empty() {
            return None;
        }
        let extension =
            if !extension.is_empty() && extension.chars().all(|c| c.is_ascii_alphanumeric()) {
                extension
            } else {
                match kind {
                    Kind::Live => "m3u8",
                    _ => "mp4",
                }
            };
        let prefix = match kind {
            Kind::Live => "live",
            Kind::Movie => "movie",
            Kind::Episode => "series",
            Kind::Series => return None,
        };
        let mut url = self.endpoint(prefix)?;
        url.path_segments_mut().ok()?.extend([
            self.username.as_str(),
            self.password.as_str(),
            &format!("{id}.{extension}"),
        ]);
        Some(url)
    }

    fn endpoint(&self, part: &str) -> Option<Url> {
        let mut url = http_url(&self.server)?;
        url.set_query(None);
        url.set_fragment(None);
        url.path_segments_mut().ok()?.pop_if_empty().push(part);
        Some(url)
    }
}

pub fn http_url(value: &str) -> Option<Url> {
    let url = Url::parse(value.trim()).ok()?;
    (matches!(url.scheme(), "https" | "http") && url.host().is_some()).then_some(url)
}

pub fn field(value: &Value, path: &str) -> String {
    match &value[path] {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn first_field(value: &Value, names: &[&str]) -> String {
    names
        .iter()
        .map(|name| field(value, name))
        .find(|text| !text.is_empty())
        .unwrap_or_default()
}

pub fn xtream_catalog(
    source: &Source,
    kind: Kind,
    categories: &Value,
    streams: &Value,
) -> Vec<Item> {
    let categories: HashMap<_, _> = categories
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| (field(entry, "category_id"), field(entry, "category_name")))
        .collect();
    streams
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = field(
                entry,
                if kind == Kind::Series {
                    "series_id"
                } else {
                    "stream_id"
                },
            );
            let title = field(entry, "name");
            if id.is_empty() || title.is_empty() {
                return None;
            }
            Some(Item {
                stream: source
                    .playback(kind, &id, &field(entry, "container_extension"))
                    .map(|url| url.to_string())
                    .unwrap_or_default(),
                id,
                kind,
                title,
                category: categories
                    .get(&field(entry, "category_id"))
                    .cloned()
                    .unwrap_or_default(),
                poster: first_field(entry, &["stream_icon", "cover"]),
                epg_id: field(entry, "epg_channel_id"),
                plot: field(entry, "plot"),
                imdb: first_field(entry, &["imdb_id", "imdb"]),
                year: first_field(entry, &["year", "releaseDate"]),
                ..Item::default()
            })
        })
        .collect()
}

pub fn xtream_episodes(source: &Source, series: &Item, payload: &Value) -> Vec<Item> {
    let mut episodes: Vec<_> = payload["episodes"]
        .as_object()
        .into_iter()
        .flatten()
        .flat_map(|(season, entries)| {
            entries
                .as_array()
                .into_iter()
                .flatten()
                .map(move |entry| (season, entry))
        })
        .filter_map(|(season, entry)| {
            let id = field(entry, "id");
            let stream =
                source.playback(Kind::Episode, &id, &field(entry, "container_extension"))?;
            Some(Item {
                id,
                kind: Kind::Episode,
                title: field(entry, "title"),
                category: series.category.clone(),
                poster: first_field(&entry["info"], &["movie_image", "cover"]),
                stream: stream.to_string(),
                plot: field(&entry["info"], "plot"),
                imdb: field(&payload["info"], "imdb_id"),
                sidecars: sidecars(&entry["info"]),
                year: series.year.clone(),
                series_title: series.title.clone(),
                season: season.parse().unwrap_or(0),
                episode: field(entry, "episode_num").parse().unwrap_or(0),
                ..Item::default()
            })
        })
        .collect();
    episodes.sort_by_key(|item| (item.season, item.episode));
    episodes
}

pub fn merge_movie_details(item: &mut Item, payload: &Value) {
    let info = &payload["info"];
    let plot = first_field(info, &["plot", "description"]);
    let imdb = first_field(info, &["imdb_id", "imdb", "imdbId"]);
    let year = first_field(info, &["year", "releasedate", "release_date"]);
    if !plot.is_empty() {
        item.plot = plot;
    }
    if !imdb.is_empty() {
        item.imdb = imdb;
    }
    if !year.is_empty() {
        item.year = year;
    }
    item.sidecars = sidecars(info);
}

fn sidecars(info: &Value) -> Vec<Sidecar> {
    info["subtitles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let url = first_field(entry, &["url", "src"]);
            http_url(&url)?;
            Some(Sidecar {
                name: first_field(entry, &["title", "label", "name"]),
                lang: first_field(entry, &["language", "lang"]),
                url,
            })
        })
        .collect()
}

pub fn fold(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value
        .nfkd()
        .filter(|ch| !is_combining_mark(*ch))
        .flat_map(char::to_lowercase)
    {
        if ch.is_alphanumeric() {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.truncate(out.trim_end().len());
    out
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Language {
    Dutch,
    English,
    Other,
    Unspecified,
}

pub fn language(value: &str) -> Language {
    match value.trim().to_ascii_lowercase().as_str() {
        "nl" | "nld" | "dut" | "nederlands" | "dutch" | "vlaams" | "be-nl" => Language::Dutch,
        "en" | "eng" | "english" | "en-us" | "en-gb" | "engels" => Language::English,
        "" | "und" | "unk" | "multi" | "dual" => Language::Unspecified,
        _ => Language::Other,
    }
}

fn language_tag(title: &str) -> Language {
    let title = title.trim_start();
    let tag = if let Some(rest) = title.strip_prefix(['|', '[', '(']) {
        rest.split(['|', ']', ')']).next().unwrap_or_default()
    } else if let Some((tag, _)) = title.split_once(" - ").or_else(|| title.split_once(" | ")) {
        tag
    } else if let Some((tag, _)) = title.split_once(':') {
        tag
    } else {
        title
            .split_whitespace()
            .next()
            .filter(|word| word.len() == 2 && word.chars().all(|c| c.is_ascii_uppercase()))
            .unwrap_or_default()
    };
    match tag.trim().to_ascii_uppercase().as_str() {
        "NL" | "NLD" | "DUTCH" | "NEDERLANDS" | "VLAAMS" | "BE-NL" => Language::Dutch,
        "EN" | "ENG" | "ENGLISH" | "ENGELS" | "UK" | "US" | "USA" | "GB" | "CA" | "AU" => {
            Language::English
        }
        "DE" | "DEU" | "GER" | "FR" | "FRA" | "ES" | "SPA" | "IT" | "ITA" | "PT" | "POR" | "PL"
        | "POL" | "TR" | "TUR" | "AR" | "ARA" | "RU" | "RUS" | "UA" | "UKR" | "SE" | "SV"
        | "NO" | "DK" | "DA" | "FI" | "GR" | "EL" | "RO" | "BG" | "HR" | "CZ" | "SK" | "HU"
        | "IN" | "HI" | "CN" | "ZH" | "JP" | "JA" | "KR" | "KO" | "VN" | "VI" | "TH" | "ID"
        | "MY" | "PK" | "IR" => Language::Other,
        _ => Language::Unspecified,
    }
}

pub fn preferred_content(item: &Item) -> bool {
    match language_tag(&item.title) {
        Language::Dutch | Language::English => true,
        Language::Other => false,
        Language::Unspecified => language_tag(&item.category) != Language::Other,
    }
}

pub fn subtitle_title(title: &str) -> String {
    let title = title.trim();
    let without_tag = if title.starts_with('|') {
        title
            .get(1..)
            .and_then(|rest| rest.split_once('|'))
            .map(|(_, title)| title)
            .unwrap_or(title)
    } else if matches!(language_tag(title), Language::Dutch | Language::English) {
        title
            .split_once(" - ")
            .map(|(_, title)| title)
            .unwrap_or(title)
    } else {
        title
    };
    if let Some((name, suffix)) = without_tag.rsplit_once(" (")
        && suffix.len() == 5
        && suffix.ends_with(')')
        && suffix[..4].chars().all(|ch| ch.is_ascii_digit())
    {
        return name.trim().to_owned();
    }
    without_tag.trim().to_owned()
}

pub struct Catalog {
    pub items: Vec<Item>,
    normalized: Vec<String>,
    preferred: Vec<bool>,
    categories: Vec<String>,
    preferred_categories: Vec<String>,
}

impl Catalog {
    pub fn new(items: Vec<Item>) -> Self {
        let normalized = items.iter().map(|item| fold(&item.title)).collect();
        let preferred = items.iter().map(preferred_content).collect::<Vec<_>>();
        let mut categories = BTreeSet::new();
        let mut preferred_categories = BTreeSet::new();
        for (item, preferred) in items.iter().zip(&preferred) {
            if item.category.is_empty() {
                continue;
            }
            categories.insert(item.category.as_str());
            if *preferred {
                preferred_categories.insert(item.category.as_str());
            }
        }
        let categories = categories.into_iter().map(str::to_owned).collect();
        let preferred_categories = preferred_categories
            .into_iter()
            .map(str::to_owned)
            .collect();
        Self {
            items,
            normalized,
            preferred,
            categories,
            preferred_categories,
        }
    }

    pub fn search(
        &self,
        query: &str,
        category: &str,
        preferred: bool,
        limit: usize,
    ) -> (Vec<usize>, usize) {
        let query = fold(query);
        let tokens: Vec<_> = query.split_whitespace().collect();
        let mut count = 0;
        let mut buckets: [Vec<usize>; 4] = std::array::from_fn(|_| Vec::new());
        for (index, title) in self.normalized.iter().enumerate() {
            if (!category.is_empty() && category != self.items[index].category)
                || (preferred && !self.preferred[index])
                || !tokens.iter().all(|token| title.contains(token))
            {
                continue;
            }
            count += 1;
            let rank = if query.is_empty() || title == &query {
                0
            } else if title.starts_with(&query) {
                1
            } else if title.contains(&query) {
                2
            } else {
                3
            };
            let bucket = &mut buckets[rank];
            if bucket.len() < limit {
                bucket.push(index);
            }
        }
        (buckets.into_iter().flatten().take(limit).collect(), count)
    }

    pub fn categories(&self, preferred: bool) -> &[String] {
        if preferred {
            &self.preferred_categories
        } else {
            &self.categories
        }
    }
}
