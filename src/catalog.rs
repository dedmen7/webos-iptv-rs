use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Mutex;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
use url::Url;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Live,
    Radio,
    #[default]
    Movie,
    Series,
    Episode,
}

impl Kind {
    pub fn is_live_stream(self) -> bool {
        matches!(self, Self::Live | Self::Radio)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Item {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub category: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub category_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<Genre>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub poster: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub poster_fallback: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stream: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub extension: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub epg_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive: Option<Archive>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub plot: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub imdb: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub year: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub series_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub series_title: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub season: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub episode: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sidecars: Vec<Sidecar>,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Genre {
    Action,
    Adventure,
    Animation,
    Comedy,
    Crime,
    Documentary,
    Drama,
    Family,
    Fantasy,
    History,
    Horror,
    Mystery,
    Romance,
    ScienceFiction,
    Thriller,
    War,
    Western,
}

impl Genre {
    fn parse(label: &str) -> Option<Self> {
        match fold(label).as_str() {
            "action" | "actie" | "aktion" => Some(Self::Action),
            "adventure" | "avontuur" | "abenteuer" | "aventure" => Some(Self::Adventure),
            "animation" | "animatie" | "animationen" => Some(Self::Animation),
            "comedy" | "komedie" | "komodie" | "comedie" => Some(Self::Comedy),
            "crime" | "misdaad" | "krimi" | "policier" => Some(Self::Crime),
            "documentary" | "documentaries" | "documentaire" | "dokumentation" => {
                Some(Self::Documentary)
            }
            "drama" | "drame" => Some(Self::Drama),
            "family" | "familie" | "familial" => Some(Self::Family),
            "fantasy" | "fantasie" | "fantastique" => Some(Self::Fantasy),
            "history" | "historical" | "geschiedenis" | "geschichte" | "historique" => {
                Some(Self::History)
            }
            "horror" | "horreur" => Some(Self::Horror),
            "mystery" | "mysterie" | "mystere" => Some(Self::Mystery),
            "romance" | "romantiek" | "liebesfilm" | "romantisch" => Some(Self::Romance),
            "sci fi" | "scifi" | "science fiction" | "sciencefiction" => Some(Self::ScienceFiction),
            "thriller" | "thrillers" => Some(Self::Thriller),
            "war" | "oorlog" | "krieg" | "guerre" => Some(Self::War),
            "western" | "westerns" => Some(Self::Western),
            _ => None,
        }
    }

    pub(crate) fn from_feed(value: &Value) -> Vec<Self> {
        let labels = value.as_str().into_iter().chain(
            value
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
        let mut genres: Vec<_> = labels
            .flat_map(|label| label.split([',', '/', '|', ';', '&']))
            .filter_map(Self::parse)
            .collect();
        genres.sort_unstable();
        genres.dedup();
        genres
    }

    fn from_group(group: &str) -> Option<Self> {
        let label = if language_tag(group) != Language::Unspecified {
            let (_, label) = group
                .trim_start_matches([' ', '|', '[', '('])
                .split_once([' ', '|', ']', ')', ':'])?;
            label.trim_start_matches([' ', '-', '|', ':'])
        } else {
            group
        };
        Self::parse(label)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    pub name: String,
    pub lang: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum Archive {
    Xtream {
        days: u32,
    },
    Template {
        days: u32,
        url: String,
        append: bool,
    },
}

impl Archive {
    pub fn available(&self, start: i64, stop: i64, now: i64) -> bool {
        let (Self::Xtream { days } | Self::Template { days, .. }) = self;
        stop > start
            && stop <= now
            && (*days == 0 || start >= now.saturating_sub(i64::from(*days) * 86_400))
    }

    pub fn template_url(
        &self,
        item: &Item,
        programme: &crate::guide::Programme,
        now: i64,
    ) -> Option<Url> {
        let Self::Template { url, append, .. } = self else {
            return None;
        };
        let mut target = if *append {
            if let Some(params) = url.strip_prefix(['?', '&']) {
                let (base, fragment) = item.stream.split_once('#').unwrap_or((&item.stream, ""));
                let separator = if base.ends_with(['?', '&']) {
                    ""
                } else if base.contains('?') {
                    "&"
                } else {
                    "?"
                };
                let hash = if fragment.is_empty() { "" } else { "#" };
                format!("{base}{separator}{params}{hash}{fragment}")
            } else {
                format!("{}{url}", item.stream)
            }
        } else {
            url.clone()
        };
        if (target.contains("{catchup-id}") || target.contains("{catchup_id}"))
            && programme.catchup_id.is_empty()
        {
            return None;
        }
        let identifier: String =
            url::form_urlencoded::byte_serialize(programme.catchup_id.as_bytes()).collect();
        for (variable, value) in [
            ("utc", programme.start_unix.to_string()),
            ("start", programme.start_unix.to_string()),
            ("utcend", programme.stop_unix.to_string()),
            ("end", programme.stop_unix.to_string()),
            (
                "duration",
                (programme.stop_unix - programme.start_unix).to_string(),
            ),
            (
                "durmin",
                ((programme.stop_unix - programme.start_unix + 59) / 60).to_string(),
            ),
            ("now", now.to_string()),
            ("timestamp", now.to_string()),
            (
                "offset",
                now.saturating_sub(programme.start_unix).to_string(),
            ),
            ("catchup-id", identifier.clone()),
            ("catchup_id", identifier),
        ] {
            target = target
                .replace(&format!("${{{variable}}}"), &value)
                .replace(&format!("{{{variable}}}"), &value);
        }
        if target.contains(['{', '}']) {
            return None;
        }
        http_url(&target).or_else(|| {
            http_url(&item.stream)?
                .join(&target)
                .ok()
                .filter(|url| matches!(url.scheme(), "http" | "https"))
        })
    }
}

impl Item {
    pub fn poster_urls(&self, source: &Source) -> (Option<Url>, Option<Url>) {
        let primary = source.poster_url(self.kind, &self.poster);
        let fallback = source.poster_url(self.kind, &self.poster_fallback);
        let Some(primary) = primary else {
            return (fallback, None);
        };
        if self.kind == Kind::Movie
            && let Some(direct) = tmdb_movie_poster(&primary)
        {
            let alternate = fallback.filter(|url| url != &direct).unwrap_or(primary);
            return (Some(direct), Some(alternate));
        }
        let fallback = fallback.filter(|url| url != &primary);
        (Some(primary), fallback)
    }

    pub fn key(&self) -> String {
        format!("{:?}:{}", self.kind, self.id)
    }

    pub fn playback_url(&self, source: &Source) -> Option<Url> {
        if self.stream.is_empty()
            && (self.kind == Kind::Movie
                || (self.kind.is_live_stream() && source.playlist.trim().is_empty()))
        {
            source.playback(self.kind, &self.id, &self.extension)
        } else {
            http_url(&self.stream)
        }
    }

    pub fn alternate_live_url(&self, source: &Source, primary: &Url) -> Option<Url> {
        if !self.kind.is_live_stream() {
            return None;
        }
        if source.playlist.trim().is_empty()
            && let (Some(hls), Some(transport_stream)) = (
                source.playback(Kind::Live, &self.id, "m3u8"),
                source.playback(Kind::Live, &self.id, "ts"),
            )
        {
            if *primary == hls {
                return Some(transport_stream);
            }
            if *primary == transport_stream {
                return Some(hls);
            }
        }
        let mut path = primary.path_segments()?.rev();
        let (id, extension) = path.next()?.rsplit_once('.')?;
        let password = path.next()?;
        let username = path.next()?;
        if path.next()? != "live"
            || username.is_empty()
            || password.is_empty()
            || id.is_empty()
            || !id.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let other = match extension {
            "m3u8" => "ts",
            "ts" => "m3u8",
            _ => return None,
        };
        let mut alternate = primary.clone();
        alternate
            .path_segments_mut()
            .ok()?
            .pop()
            .push(&format!("{id}.{other}"));
        Some(alternate)
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
    fn poster_url(&self, kind: Kind, raw: &str) -> Option<Url> {
        let raw = raw.trim();
        http_url(raw).or_else(|| {
            if !relative_poster(raw) {
                return None;
            }
            let base = if kind.is_live_stream() && !self.playlist.trim().is_empty() {
                http_url(&self.playlist)
            } else {
                self.endpoint("player_api.php")
            }?;
            let url = base.join(raw).ok()?;
            (matches!(url.scheme(), "http" | "https") && url.host().is_some()).then_some(url)
        })
    }

    pub fn configured(&self) -> bool {
        !self.playlist.trim().is_empty() || !self.server.trim().is_empty()
    }

    pub fn api(&self, action: &str, parameter: Option<(&str, &str)>) -> Option<Url> {
        let mut url = self.endpoint("player_api.php")?;
        let mut query = url.query_pairs_mut();
        query.append_pair("username", &self.username);
        query.append_pair("password", &self.password);
        if !action.is_empty() {
            query.append_pair("action", action);
        }
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
                    Kind::Live | Kind::Radio => "m3u8",
                    _ => "mp4",
                }
            };
        let prefix = match kind {
            Kind::Live | Kind::Radio => "live",
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

    pub fn archive_url(&self, id: &str, start: &str, minutes: i64, extension: &str) -> Option<Url> {
        let mut url = self.endpoint("timeshift")?;
        url.path_segments_mut().ok()?.extend([
            self.username.as_str(),
            self.password.as_str(),
            &minutes.to_string(),
            start,
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

fn tmdb_movie_poster(provider_url: &Url) -> Option<Url> {
    if provider_url.scheme() != "http"
        || !matches!(provider_url.host(), Some(url::Host::Ipv4(_)))
        || !provider_url.username().is_empty()
        || provider_url.password().is_some()
        || provider_url.query().is_some()
    {
        return None;
    }
    // Some Xtream artwork addresses mirror the public TMDB filename under this path.
    let file = provider_url.path().strip_prefix("/images/movies//")?;
    let (name, extension) = file.rsplit_once('.')?;
    if !(26..=27).contains(&name.len())
        || !name.bytes().all(|byte| byte.is_ascii_alphanumeric())
        || !matches!(extension, "jpg" | "jpeg" | "png" | "webp")
    {
        return None;
    }
    http_url(&format!("https://image.tmdb.org/t/p/w342/{file}"))
}

fn relative_poster(raw: &str) -> bool {
    let raw = raw.trim();
    !raw.is_empty()
        && !matches!(raw, "." | ".." | "-" | "/" | "//")
        && !["null", "false", "undefined", "none", "n/a"]
            .iter()
            .any(|invalid| raw.eq_ignore_ascii_case(invalid))
        && !raw.bytes().all(|byte| byte.is_ascii_digit())
        && !raw.starts_with(['#', '?'])
        && !raw.split('/').next().is_some_and(|part| part.contains(':'))
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
    let categories = category_names(categories);
    streams
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let category = categories
                .get(&field(entry, "category_id"))
                .map(String::as_str)
                .unwrap_or_default();
            xtream_item(Some(source), kind, entry, category)
        })
        .collect()
}

pub(crate) fn category_names(categories: &Value) -> HashMap<String, String> {
    categories
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| (field(entry, "category_id"), field(entry, "category_name")))
        .collect()
}

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn xtream_live_row(entry: &Value, categories: &HashMap<String, String>) -> Option<Item> {
    let category = categories
        .get(&field(entry, "category_id"))
        .map_or("", String::as_str);
    xtream_item(None, Kind::Live, entry, category)
}

fn xtream_item(source: Option<&Source>, kind: Kind, entry: &Value, category: &str) -> Option<Item> {
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
    let extension = field(entry, "container_extension");
    let kind = if kind == Kind::Live
        && (field(entry, "stream_type").eq_ignore_ascii_case("radio")
            || entry["is_radio"].as_bool() == Some(true)
            || radio_flag(&field(entry, "is_radio"))
            || radio_category(category)
            || radio_extension(&extension))
    {
        Kind::Radio
    } else {
        kind
    };
    let stream = if kind.is_live_stream()
        && let Some(source) = source
    {
        source
            .playback(kind, &id, &extension)
            .map(|url| url.to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    Some(Item {
        stream,
        extension,
        id,
        kind,
        title,
        category: category.to_owned(),
        category_id: if kind.is_live_stream() {
            field(entry, "category_id")
        } else {
            String::new()
        },
        genres: Genre::from_feed(&entry["genre"]),
        poster: field(entry, "stream_icon"),
        poster_fallback: field(entry, "cover"),
        epg_id: field(entry, "epg_channel_id"),
        archive: (kind == Kind::Live
            && (entry["tv_archive"].as_bool() == Some(true) || field(entry, "tv_archive") == "1"))
            .then(|| Archive::Xtream {
                days: field(entry, "tv_archive_duration").parse().unwrap_or(0),
            }),
        plot: field(entry, "plot"),
        imdb: first_field(entry, &["imdb_id", "imdb"]),
        year: first_field(entry, &["year", "releaseDate"]),
        ..Item::default()
    })
}

pub(crate) fn radio_flag(value: &str) -> bool {
    value.eq_ignore_ascii_case("true") || value == "1"
}

pub(crate) fn radio_extension(value: &str) -> bool {
    ["mp3", "aac", "flac", "opus", "m4a"]
        .iter()
        .any(|extension| value.eq_ignore_ascii_case(extension))
}

pub(crate) fn radio_category(category: &str) -> bool {
    let words = || category.split(|ch: char| !ch.is_alphabetic());
    words().any(|word| {
        ["radio", "radios", "radiozenders"]
            .iter()
            .any(|name| word.eq_ignore_ascii_case(name))
    }) && !words().any(|word| {
        ["tv", "television", "televisie"]
            .iter()
            .any(|name| word.eq_ignore_ascii_case(name))
    })
}

pub fn preview_categories(
    categories: &Value,
    preferred: bool,
) -> impl Iterator<Item = (String, String)> + '_ {
    categories
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(move |row| {
            let id = field(row, "category_id");
            let name = field(row, "category_name");
            (!id.is_empty() && (!preferred || language_tag(&name) != Language::Other))
                .then_some((id, name))
        })
}

pub fn xtream_movie_preview(
    source: &Source,
    rows: &Value,
    category: (&str, &str),
    preferred: bool,
    limit: usize,
) -> Vec<Item> {
    rows.as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let reported_category = field(row, "category_id");
            if !reported_category.is_empty() && reported_category != category.0 {
                return None;
            }
            let item = xtream_item(Some(source), Kind::Movie, row, category.1)?;
            (!preferred || preferred_content(&item)).then_some(item)
        })
        .take(limit)
        .collect()
}

pub fn lead_with_preview(items: Vec<Item>, preview: &[Item]) -> Vec<Item> {
    if preview.is_empty()
        || (items.len() >= preview.len()
            && items
                .iter()
                .zip(preview)
                .all(|(item, first)| item.id == first.id))
    {
        return items;
    }
    let positions: HashMap<_, _> = preview
        .iter()
        .enumerate()
        .map(|(index, item)| (item.id.as_str(), index))
        .collect();
    let mut first: Vec<Option<Item>> = (0..preview.len()).map(|_| None).collect();
    let mut remainder = Vec::with_capacity(items.len());
    for item in items {
        if let Some(&index) = positions.get(item.id.as_str())
            && first[index].is_none()
        {
            first[index] = Some(item);
        } else {
            remainder.push(item);
        }
    }
    first.into_iter().flatten().chain(remainder).collect()
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
                poster: field(&entry["info"], "movie_image"),
                poster_fallback: field(&entry["info"], "cover"),
                stream: stream.to_string(),
                plot: field(&entry["info"], "plot"),
                imdb: field(&payload["info"], "imdb_id"),
                sidecars: sidecars(&entry["info"]),
                year: series.year.clone(),
                series_id: series.id.clone(),
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
    let mut posters = [
        std::mem::take(&mut item.poster),
        std::mem::take(&mut item.poster_fallback),
        field(info, "movie_image"),
        field(info, "cover"),
    ]
    .into_iter()
    .filter(|poster| relative_poster(poster) || http_url(poster).is_some());
    item.poster = posters.next().unwrap_or_default();
    item.poster_fallback = posters
        .find(|poster| poster != &item.poster)
        .unwrap_or_default();
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
    if value.is_ascii() {
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() {
                out.push(char::from(byte.to_ascii_lowercase()));
            } else if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        }
        out.truncate(out.trim_end().len());
        return out;
    }
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
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
    } else if let Some((tag, _)) = title.split_once(" - ").or_else(|| title.split_once('|')) {
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
    let tag = tag.trim().as_bytes();
    let mut uppercase = [0; b"NEDERLANDS".len()];
    let Some(uppercase) = uppercase.get_mut(..tag.len()) else {
        return Language::Unspecified;
    };
    uppercase.copy_from_slice(tag);
    uppercase.make_ascii_uppercase();
    match &*uppercase {
        b"NL" | b"NLD" | b"DUTCH" | b"NEDERLANDS" | b"VLAAMS" | b"BE-NL" => Language::Dutch,
        b"EN" | b"ENG" | b"ENGLISH" | b"ENGELS" | b"UK" | b"US" | b"USA" | b"GB" | b"CA"
        | b"AU" => Language::English,
        b"DE" | b"DEU" | b"GER" | b"FR" | b"FRA" | b"ES" | b"SPA" | b"IT" | b"ITA" | b"PT"
        | b"POR" | b"PL" | b"POL" | b"TR" | b"TUR" | b"AR" | b"ARA" | b"RU" | b"RUS" | b"UA"
        | b"UKR" | b"SE" | b"SV" | b"NO" | b"DK" | b"DA" | b"FI" | b"GR" | b"EL" | b"RO"
        | b"BG" | b"HR" | b"CZ" | b"SK" | b"HU" | b"IN" | b"HI" | b"CN" | b"ZH" | b"JP" | b"JA"
        | b"KR" | b"KO" | b"VN" | b"VI" | b"TH" | b"ID" | b"MY" | b"PK" | b"IR" => Language::Other,
        _ => Language::Unspecified,
    }
}

pub fn preferred_content(item: &Item) -> bool {
    title_preferred(&item.title, || {
        language_tag(&item.category) != Language::Other
    })
}

fn title_preferred(title: &str, category_preferred: impl FnOnce() -> bool) -> bool {
    match language_tag(title) {
        Language::Dutch | Language::English => true,
        Language::Other => false,
        Language::Unspecified => category_preferred(),
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

// One shortlist per catalog, bounded to 16 KiB of indices on a TV.
const SEARCH_SHORTLIST_LIMIT: usize = 4096;

struct SearchCandidates {
    query: String,
    preferred: bool,
    indices: Vec<usize>,
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Default)]
struct LiveFeedIndex {
    first: HashMap<Box<str>, usize>,
    duplicates: HashMap<usize, Vec<usize>>,
}

#[derive(Default)]
pub struct Catalog {
    pub items: Vec<Item>,
    normalized: Vec<String>,
    last_search: Mutex<Option<SearchCandidates>>,
    preferred: Vec<bool>,
    preferred_count: usize,
    categories: Vec<String>,
    preferred_categories: Vec<String>,
    genres: BTreeSet<Genre>,
    preferred_genres: BTreeSet<Genre>,
    tagged_count: usize,
    preferred_tagged_count: usize,
    preview_order: Vec<usize>,
    live_order: Option<Vec<usize>>,
    #[cfg(any(target_arch = "wasm32", test))]
    live_feed_index: std::sync::OnceLock<LiveFeedIndex>,
    #[cfg(any(target_arch = "wasm32", test))]
    preview_feed_routes: Vec<Item>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BrowseFilter<T> {
    #[default]
    All,
    Category(T),
    #[cfg(any(target_arch = "wasm32", test))]
    Genre(Genre),
}

impl<T> BrowseFilter<T> {
    pub(crate) fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }
}

impl<'a> BrowseFilter<&'a str> {
    fn from_category(category: &'a str) -> Self {
        if category.is_empty() {
            Self::All
        } else {
            Self::Category(category)
        }
    }

    fn matches(self, item: &Item) -> bool {
        match self {
            Self::All => true,
            Self::Category(category) => item.category == category,
            #[cfg(any(target_arch = "wasm32", test))]
            Self::Genre(genre) => item.genres.contains(&genre),
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl BrowseFilter<String> {
    pub(crate) fn borrowed(&self) -> BrowseFilter<&str> {
        match self {
            Self::All => BrowseFilter::All,
            Self::Category(name) => BrowseFilter::Category(name),
            Self::Genre(genre) => BrowseFilter::Genre(*genre),
        }
    }
}

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct LiveGroups {
    pub order: Vec<String>,
    pub hidden: HashSet<String>,
}

impl LiveGroups {
    pub fn ordered<'a>(&self, groups: &'a [String]) -> Vec<&'a str> {
        let ranks: HashMap<_, _> = self
            .order
            .iter()
            .enumerate()
            .map(|(i, name)| (name.as_str(), i))
            .collect();
        let mut groups: Vec<_> = groups.iter().map(String::as_str).collect();
        groups.sort_by_key(|name| ranks.get(name).copied().unwrap_or(usize::MAX));
        groups
    }
}

impl Catalog {
    pub fn new(items: Vec<Item>) -> Self {
        let mut catalog = Self::default();
        catalog.append(items);
        catalog
    }

    pub(crate) fn append(&mut self, mut batch: Vec<Item>) {
        #[cfg(any(target_arch = "wasm32", test))]
        {
            self.live_feed_index.take();
        }
        self.last_search = Mutex::default();
        self.normalized.reserve(batch.len());
        self.preferred.reserve(batch.len());
        struct Group {
            preferred_language: bool,
            has_preferred: bool,
            genre: Option<Genre>,
        }
        let mut groups = HashMap::new();
        for item in &mut batch {
            self.normalized.push(fold(&item.title));
            let group = groups
                .entry(item.category.as_str())
                .or_insert_with(|| Group {
                    preferred_language: language_tag(&item.category) != Language::Other,
                    genre: Genre::from_group(&item.category),
                    has_preferred: false,
                });
            let preferred = title_preferred(&item.title, || group.preferred_language);
            group.has_preferred |= preferred;
            self.preferred.push(preferred);
            self.preferred_count += usize::from(preferred);
            if matches!(item.kind, Kind::Movie | Kind::Series) {
                if item.genres.is_empty()
                    && let Some(genre) = group.genre
                {
                    item.genres.push(genre);
                }
                let tagged = !item.genres.is_empty();
                self.tagged_count += usize::from(tagged);
                self.preferred_tagged_count += usize::from(tagged && preferred);
                self.genres.extend(item.genres.iter().copied());
                if preferred {
                    self.preferred_genres.extend(item.genres.iter().copied());
                }
            }
        }
        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_unstable_by_key(|(category, _)| *category);
        for (category, group) in groups {
            if category.is_empty() {
                continue;
            }
            let insert = |available: &mut Vec<String>| {
                if let Err(index) = available.binary_search_by(|known| known.as_str().cmp(category))
                {
                    available.insert(index, category.to_owned());
                }
            };
            insert(&mut self.categories);
            if group.has_preferred {
                insert(&mut self.preferred_categories);
            }
        }
        if self.items.is_empty() {
            self.items = batch;
        } else {
            self.items.append(&mut batch);
        }
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn keep_preview_order(&mut self, preview: &[Item]) {
        let positions: HashMap<_, _> = preview
            .iter()
            .enumerate()
            .map(|(position, item)| (item.id.as_str(), position))
            .collect();
        let mut order = vec![None; preview.len()];
        let mut remaining = positions.len();
        for (index, item) in self.items.iter().enumerate() {
            if remaining == 0 {
                break;
            }
            if let Some(&position) = positions.get(item.id.as_str())
                && order[position].is_none()
            {
                order[position] = Some(index);
                remaining -= 1;
            }
        }
        self.preview_order = order.into_iter().flatten().collect();
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn has_match_from(
        &self,
        query: &str,
        filter: BrowseFilter<&str>,
        preferred: bool,
        from: usize,
    ) -> bool {
        let query = fold(query);
        self.normalized[from..]
            .iter()
            .enumerate()
            .any(|(offset, title)| {
                let index = offset + from;
                (!preferred || self.preferred[index])
                    && filter.matches(&self.items[index])
                    && query.split_whitespace().all(|word| title.contains(word))
            })
    }

    pub fn search(
        &self,
        query: &str,
        category: &str,
        preferred: bool,
        limit: usize,
    ) -> (Vec<usize>, usize) {
        self.search_filtered(
            query,
            BrowseFilter::from_category(category),
            preferred,
            limit,
        )
    }

    pub(crate) fn search_filtered(
        &self,
        query: &str,
        filter: BrowseFilter<&str>,
        preferred: bool,
        limit: usize,
    ) -> (Vec<usize>, usize) {
        let query = fold(query);
        if query.is_empty() && filter.is_all() && self.live_order.is_none() {
            let count = if preferred {
                self.preferred_count
            } else {
                self.items.len()
            };
            let leading = self.preview_order.as_slice();
            let hits = leading
                .iter()
                .copied()
                .filter(|&index| !preferred || self.preferred[index])
                .chain((0..self.items.len()).filter(|&index| {
                    (!preferred || self.preferred[index]) && !leading.contains(&index)
                }))
                .take(limit.min(count))
                .collect();
            return (hits, count);
        }
        if query.is_empty() && !filter.is_all() {
            let mut hits = Vec::new();
            let mut count = 0;
            for index in self.indices().filter(|&index| {
                (!preferred || self.preferred[index]) && filter.matches(&self.items[index])
            }) {
                count += 1;
                if hits.len() < limit {
                    hits.push(index);
                }
            }
            return (hits, count);
        }
        let (buckets, count) = self.search_buckets(&query, filter, preferred, limit);
        (buckets.into_iter().flatten().take(limit).collect(), count)
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn search_with_preview<'a>(
        &'a self,
        preview: &'a Self,
        query: &str,
        filter: BrowseFilter<&str>,
        preferred: bool,
        limit: usize,
    ) -> Vec<&'a Item> {
        let query = fold(query);
        let (loaded, _) = self.search_buckets(&query, filter, preferred, limit);
        let (early, _) = preview.search_buckets(&query, filter, preferred, limit);
        let mut seen = std::collections::HashSet::new();
        loaded
            .into_iter()
            .zip(early)
            .flat_map(|(loaded, early)| {
                loaded
                    .into_iter()
                    .map(|index| &self.items[index])
                    .chain(early.into_iter().map(|index| &preview.items[index]))
            })
            .filter(|item| item.id.is_empty() || seen.insert(item.id.as_str()))
            .take(limit)
            .collect()
    }

    fn search_buckets(
        &self,
        query: &str,
        filter: BrowseFilter<&str>,
        preferred: bool,
        limit: usize,
    ) -> ([Vec<usize>; 4], usize) {
        let tokens: Vec<_> = query.split_whitespace().collect();
        let mut count = 0;
        let mut buckets: [Vec<usize>; 4] = std::array::from_fn(|_| Vec::new());
        let mut searched_ids = std::collections::HashSet::new();
        let leading = if query.is_empty() && filter.is_all() {
            for &index in &self.preview_order {
                if !preferred || self.preferred[index] {
                    buckets[0].push(index);
                }
            }
            self.preview_order.as_slice()
        } else {
            &[]
        };
        let mut cache = (!query.is_empty() && filter.is_all())
            .then(|| self.last_search.try_lock().ok())
            .flatten();
        let previous = cache
            .as_deref()
            .and_then(Option::as_ref)
            .filter(|previous| {
                preferred == previous.preferred && query.starts_with(&previous.query)
            });

        let mut candidates = Vec::new();
        let visit = |index: usize| {
            let item = &self.items[index];
            let title = &self.normalized[index];
            if !filter.matches(item) || (preferred && !self.preferred[index]) {
                return;
            }
            let phrase_matches = query.is_empty() || title.contains(query);
            if !phrase_matches
                && (tokens.len() == 1 || !tokens.iter().all(|token| title.contains(token)))
            {
                return;
            }
            // Another row with the same provider ID may be the only match after the next key.
            if cache.is_some() && candidates.len() < SEARCH_SHORTLIST_LIMIT {
                candidates.push(index);
            }
            if !query.is_empty()
                && matches!(item.kind, Kind::Movie | Kind::Series)
                && !item.id.is_empty()
                && !searched_ids.insert((item.kind, item.id.as_str()))
            {
                return;
            }
            count += 1;
            let rank = if query.is_empty() || title == query {
                0
            } else if title.starts_with(query) {
                1
            } else if phrase_matches {
                2
            } else {
                3
            };
            let bucket = &mut buckets[rank];
            if bucket.len() < limit && !leading.contains(&index) {
                bucket.push(index);
            }
        };
        if let Some(previous) = previous {
            previous.indices.iter().copied().for_each(visit);
        } else {
            self.indices().for_each(visit);
        }
        if let Some(cache) = cache.as_deref_mut()
            && candidates.len() < SEARCH_SHORTLIST_LIMIT
        {
            *cache = Some(SearchCandidates {
                query: query.to_owned(),
                preferred,
                indices: candidates,
            });
        }
        (buckets, count)
    }

    pub fn visible_items(&self, preferred: bool) -> impl Iterator<Item = &Item> {
        self.indices()
            .filter(move |&index| !preferred || self.preferred[index])
            .map(|index| &self.items[index])
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn browse_position(
        &self,
        filter: BrowseFilter<&str>,
        preferred: bool,
        id: &str,
    ) -> Option<usize> {
        let leading = if filter.is_all() {
            self.preview_order.as_slice()
        } else {
            &[]
        };
        leading
            .iter()
            .copied()
            .chain(self.indices().filter(|index| !leading.contains(index)))
            .filter(|&index| {
                (!preferred || self.preferred[index]) && filter.matches(&self.items[index])
            })
            .position(|index| self.items[index].id == id)
    }

    fn indices(&self) -> impl Iterator<Item = usize> + '_ {
        let order = self.live_order.as_deref();
        (0..order.map_or(self.items.len(), <[usize]>::len))
            .map(move |index| order.map_or(index, |indices| indices[index]))
    }

    pub fn apply_live_groups(&mut self, settings: &LiveGroups) {
        #[cfg(any(target_arch = "wasm32", test))]
        {
            self.live_feed_index.take();
            self.preview_feed_routes
                .retain(|item| !settings.hidden.contains(&item.category));
        }
        self.last_search = Mutex::default();
        if settings.order.is_empty() && settings.hidden.is_empty() {
            self.live_order = None;
            return;
        }
        let group_ranks: HashMap<_, _> = settings
            .order
            .iter()
            .enumerate()
            .map(|(index, name)| (name.as_str(), index))
            .collect();
        let mut indices: Vec<_> = (0..self.items.len())
            .filter(|&index| !settings.hidden.contains(&self.items[index].category))
            .collect();
        indices.sort_by_key(|&index| {
            group_ranks
                .get(self.items[index].category.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
        self.live_order = Some(indices);
    }

    #[cfg(any(target_arch = "wasm32", test))]
    fn visible_live_feed_index(&self) -> &LiveFeedIndex {
        self.live_feed_index.get_or_init(|| {
            let mut index = LiveFeedIndex::default();
            for position in self
                .indices()
                .filter(|&pos| self.items[pos].kind == Kind::Live)
            {
                let id = self.items[position].id.as_str();
                if let Some(&first) = index.first.get(id) {
                    index.duplicates.entry(first).or_default().push(position);
                } else {
                    index.first.insert(id.into(), position);
                }
            }
            index
        })
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn equivalent_live_feed(&self, logical: &Item, candidate_id: &str) -> Option<&Item> {
        let epg = logical.epg_id.trim();
        if logical.kind != Kind::Live
            || logical.id == candidate_id
            || ["", "-", "0", "none", "null", "unknown", "n/a"]
                .iter()
                .any(|missing| epg.eq_ignore_ascii_case(missing))
        {
            return None;
        }
        let guide_matches = |item: &Item| item.epg_id.trim().eq_ignore_ascii_case(epg);
        let candidate = self
            .preview_feed_routes
            .iter()
            .find(|item| item.id == candidate_id && guide_matches(item))
            .or_else(|| {
                let visible = self.visible_live_feed_index();
                let &first = visible.first.get(candidate_id)?;
                std::iter::once(first)
                    .chain(
                        visible
                            .duplicates
                            .get(&first)
                            .into_iter()
                            .flatten()
                            .copied(),
                    )
                    .map(|pos| &self.items[pos])
                    .find(|item| guide_matches(item))
            })?;
        let name = live_feed_identity(&logical.title)?;
        (live_feed_identity(&candidate.title)? == name).then_some(candidate)
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn preview_feed_routes(
        &self,
        first_items: &[Item],
        feeds: &crate::live_feeds::Feeds,
        source: &str,
        now: i64,
    ) -> Vec<Item> {
        if !feeds.has_samples_for_source(source) {
            return Vec::new();
        }
        let mut routes = Vec::new();
        let mut included = HashSet::new();
        for logical in first_items {
            for id in feeds.preview_alternatives(source, &logical.id, &logical.epg_id, now) {
                if included.contains(id) {
                    continue;
                }
                let Some(donor) = self.equivalent_live_feed(logical, id) else {
                    continue;
                };
                included.insert(id);
                if first_items.iter().any(|shown| same_feed_row(shown, donor)) {
                    continue;
                }
                routes.push(Item {
                    id: donor.id.clone(),
                    kind: Kind::Live,
                    title: donor.title.clone(),
                    epg_id: donor.epg_id.clone(),
                    stream: donor.stream.clone(),
                    extension: donor.extension.clone(),
                    category: donor.category.clone(),
                    ..Item::default()
                });
            }
        }
        routes
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn set_preview_feed_routes(&mut self, mut routes: Vec<Item>) {
        routes.truncate(crate::live_routes::CHANNEL_LIMIT);
        routes.retain(|route| {
            route.kind == Kind::Live && !self.items.iter().any(|shown| same_feed_row(shown, route))
        });
        self.preview_feed_routes = routes;
    }

    pub fn live_neighbors(&self, current: &Item, preferred: bool) -> Option<(&Item, &Item)> {
        self.live_neighbors_with(current, preferred, |item| !item.stream.is_empty())
    }

    pub fn live_neighbors_for_source(
        &self,
        current: &Item,
        preferred: bool,
        source: &Source,
    ) -> Option<(&Item, &Item)> {
        let resolves_by_id =
            source.playlist.trim().is_empty() && http_url(&source.server).is_some();
        self.live_neighbors_with(current, preferred, |item| {
            !item.stream.is_empty() || (resolves_by_id && !item.id.is_empty())
        })
    }

    fn live_neighbors_with(
        &self,
        current: &Item,
        preferred: bool,
        can_play: impl Fn(&Item) -> bool,
    ) -> Option<(&Item, &Item)> {
        let channels = || {
            self.visible_items(preferred)
                .filter(|item| item.kind == Kind::Live && can_play(item))
        };
        if (!current.category_id.is_empty() || !current.category.is_empty())
            && let Some(pair) = live_neighbor_pair(
                channels().filter(|item| {
                    if current.category_id.is_empty() {
                        item.category == current.category
                    } else {
                        item.category_id == current.category_id
                    }
                }),
                &current.id,
            )
        {
            return Some(pair);
        }
        live_neighbor_pair(channels(), &current.id)
    }

    pub fn categories(&self, preferred: bool) -> &[String] {
        if preferred {
            &self.preferred_categories
        } else {
            &self.categories
        }
    }

    pub fn genres(&self, preferred: bool) -> &BTreeSet<Genre> {
        if preferred {
            &self.preferred_genres
        } else {
            &self.genres
        }
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn genre_covers_at_least_half(&self, preferred: bool) -> bool {
        if preferred {
            self.preferred_tagged_count >= self.preferred_count.div_ceil(2)
        } else {
            self.tagged_count >= self.items.len().div_ceil(2)
        }
    }
}

#[cfg(any(target_arch = "wasm32", test))]
fn same_feed_row(shown: &Item, donor: &Item) -> bool {
    shown.kind == donor.kind
        && shown.id == donor.id
        && shown.title == donor.title
        && shown
            .epg_id
            .trim()
            .eq_ignore_ascii_case(donor.epg_id.trim())
        && shown.stream == donor.stream
        && shown.extension == donor.extension
}

#[cfg(any(target_arch = "wasm32", test))]
fn live_feed_identity(title: &str) -> Option<(String, String)> {
    let normalized = title.nfkc().collect::<String>().to_lowercase();
    let (prefix, name) = normalized.split_once('|').unwrap_or(("", &normalized));
    let prefix = prefix.trim();
    let region = prefix
        .strip_suffix("-cw")
        .filter(|country| {
            country.len() == 2 && country.bytes().all(|byte| byte.is_ascii_alphabetic())
        })
        .unwrap_or(prefix);
    let mut words: Vec<_> = name.split_whitespace().collect();
    while words.last().is_some_and(|word| {
        matches!(
            word.trim_matches(['[', ']', '(', ')']),
            "raw" | "sd" | "hd" | "fhd" | "uhd" | "4k" | "8k" | "720p" | "1080p" | "2160p"
        )
    }) {
        words.pop();
    }
    let mut station = String::new();
    for word in words {
        let joins_channel_number = word.starts_with(|c: char| c.is_ascii_digit())
            && station.ends_with(char::is_alphabetic);
        if !station.is_empty() && !joins_channel_number {
            station.push(' ');
        }
        station.push_str(word);
    }
    (!station.is_empty()).then(|| (region.to_owned(), station))
}

fn live_neighbor_pair<'a>(
    mut channels: impl Iterator<Item = &'a Item>,
    current_id: &str,
) -> Option<(&'a Item, &'a Item)> {
    let first = channels.next()?;
    let mut seen = HashSet::from([first.id.as_str()]);
    if first.id == current_id {
        let next = channels.find(|item| item.id != current_id)?;
        seen.insert(next.id.as_str());
        let mut last = next;
        for item in channels {
            if seen.insert(item.id.as_str()) {
                last = item;
            }
        }
        return Some((last, next));
    }
    let mut distinct = channels.filter(|item| seen.insert(item.id.as_str()));
    let mut previous = first;
    while let Some(channel) = distinct.next() {
        if channel.id == current_id {
            return Some((previous, distinct.next().unwrap_or(first)));
        }
        previous = channel;
    }
    Some((previous, first))
}

#[cfg(test)]
mod catalog_index_tests {
    use super::{
        BrowseFilter, Catalog, Genre, Item, Kind, Source, category_names, xtream_live_row,
    };
    use serde_json::json;

    #[test]
    fn xtream_live_and_radio_resolve_playback_without_storing_authenticated_stream_urls() {
        let source = Source {
            server: "https://example.test".into(),
            username: "fixture-user".into(),
            password: "fixture-password".into(),
            ..Source::default()
        };
        let categories = category_names(&json!([{"category_id": 7, "category_name": "Radio"}]));
        let radio = xtream_live_row(&json!({"stream_id": 42, "name": "Radio", "category_id": 7, "container_extension": "mp3"}), &categories).unwrap();
        assert_eq!(radio.kind, Kind::Radio);
        assert!(radio.stream.is_empty());
        assert_eq!(
            radio.playback_url(&source).unwrap().path(),
            "/live/fixture-user/fixture-password/42.mp3"
        );
        let live =
            xtream_live_row(&json!({"stream_id": 43, "name": "Nieuws"}), &categories).unwrap();
        assert_eq!(live.kind, Kind::Live);
        assert!(live.stream.is_empty());
        assert_eq!(
            live.playback_url(&source).unwrap().path(),
            "/live/fixture-user/fixture-password/43.m3u8"
        );
    }

    #[test]
    fn substitute_feeds_require_the_same_station_region_guide_and_a_visible_channel() {
        let row = |id: &str, title: &str, epg: &str| Item {
            id: id.into(),
            title: title.into(),
            epg_id: epg.into(),
            category: id.into(),
            kind: Kind::Live,
            ..Item::default()
        };
        let logical = row("logical", "NL| Station1 4K", "station1.nl");
        let mut catalog = Catalog::new(vec![
            logical.clone(),
            row("fhd", "NL| Station 1 FHD", "station1.nl"),
            row("cable", "NL-CW| Station 1 ᴿᴬᵂ", "station1.nl"),
            row("extra", "NL| Station 1 Extra HD", "station1.nl"),
            row("delay", "NL| Station 1 +1 HD", "station1.nl"),
            row("region", "BE| Station 1 HD", "station1.nl"),
            row("edition", "NL-WEST| Station 1 HD", "station1.nl"),
            row("language", "NL-EN| Station 1 HD", "station1.nl"),
            row("other", "NL| Station 1 HD", "another.guide"),
            row("missing", "NL| Station 1 HD", ""),
        ]);
        for id in ["fhd", "cable"] {
            assert_eq!(catalog.equivalent_live_feed(&logical, id).unwrap().id, id);
        }
        for id in [
            "logical", "extra", "delay", "region", "edition", "language", "other", "missing",
        ] {
            assert!(catalog.equivalent_live_feed(&logical, id).is_none(), "{id}");
        }
        catalog.apply_live_groups(&super::LiveGroups {
            hidden: ["fhd".into()].into_iter().collect(),
            ..Default::default()
        });
        assert!(catalog.equivalent_live_feed(&logical, "fhd").is_none());
        assert!(
            catalog
                .equivalent_live_feed(&row("blank", "NL| Station 1", ""), "missing")
                .is_none()
        );
    }

    #[test]
    fn live_feed_lookup_tracks_duplicate_ids_across_catalog_and_visibility_updates() {
        let live = |id: &str, guide: &str, category: &str| Item {
            kind: Kind::Live,
            title: "Station".to_owned(),
            id: id.to_owned(),
            epg_id: guide.to_owned(),
            category: category.to_owned(),
            ..Item::default()
        };
        let logical = live("shown", "station", "main");
        let mut catalog = Catalog::new(vec![live("alt", "other", "group")]);
        assert!(catalog.equivalent_live_feed(&logical, "alt").is_none());
        catalog.append(vec![live("alt", "station", "eligible")]);
        assert_eq!(
            catalog
                .equivalent_live_feed(&logical, "alt")
                .unwrap()
                .category,
            "eligible"
        );
        catalog.apply_live_groups(&super::LiveGroups {
            hidden: ["eligible".to_owned()].into_iter().collect(),
            ..Default::default()
        });
        assert!(catalog.equivalent_live_feed(&logical, "alt").is_none());
    }

    #[test]
    fn off_page_feed_winners_are_cached_for_playback_only() {
        use crate::live_feeds::{FeedSample, Feeds};

        let station = |id: &str, category: &str| Item {
            id: id.to_owned(),
            title: "Station".to_owned(),
            epg_id: "station.guide".to_owned(),
            kind: Kind::Live,
            category: category.to_owned(),
            ..Item::default()
        };
        let first = vec![station("shown", "main"), station("duplicate", "main")];
        let mut fast = station("fast", "extra");
        fast.stream = "https://example.test/stream".to_owned();
        fast.poster = "unused-image".to_owned();
        let mut full = Catalog::new(first.clone());
        full.append(vec![fast.clone(), station("faster-hidden", "hidden")]);
        let groups = super::LiveGroups {
            hidden: ["hidden".to_owned()].into_iter().collect(),
            ..Default::default()
        };
        full.apply_live_groups(&groups);
        let mut timings = Feeds::default();
        for (id, startup_ms) in [
            ("shown", 6800),
            ("duplicate", 6400),
            ("fast", 900),
            ("faster-hidden", 700),
        ] {
            timings.record(
                "source",
                id,
                100,
                FeedSample {
                    startup_ms,
                    width: 1920,
                    height: 1080,
                    epg_id: "station.guide",
                    format: None,
                },
            );
        }
        assert!(timings.has_samples_for_source("source"));
        assert!(!timings.has_samples_for_source("other"));
        assert!(
            full.preview_feed_routes(&first, &timings, "other", 111)
                .is_empty()
        );
        assert!(
            full.preview_feed_routes(&first, &timings, "source", 800_000)
                .is_empty()
        );
        let routes = full.preview_feed_routes(&first, &timings, "source", 111);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].id, "fast");
        assert!(routes[0].poster.is_empty());
        let mut preview = Catalog::new(first.clone());
        preview.set_preview_feed_routes(routes);
        let ready = preview.equivalent_live_feed(&first[0], "fast").unwrap();
        assert_eq!(ready.stream, fast.stream);
        assert!(
            preview
                .equivalent_live_feed(&first[1], "faster-hidden")
                .is_none()
        );
        assert_eq!(preview.visible_items(false).count(), 2);
        assert_eq!(preview.search("Station", "", false, 10).0.len(), 2);
        preview.apply_live_groups(&super::LiveGroups {
            hidden: ["extra".to_owned()].into_iter().collect(),
            ..Default::default()
        });
        assert!(preview.equivalent_live_feed(&first[0], "fast").is_none());

        let mut on_page = first.clone();
        on_page.push(fast);
        assert!(
            full.preview_feed_routes(&on_page, &timings, "source", 111)
                .is_empty()
        );
        full.apply_live_groups(&super::LiveGroups::default());
        let unhidden = full.preview_feed_routes(&first, &timings, "source", 111);
        assert!(unhidden.iter().any(|feed| feed.id == "faster-hidden"));
    }

    #[test]
    fn preview_retains_alternatives_across_cooldown_and_duplicate_guide_ids() {
        use crate::live_feeds::{FeedSample, Feeds};
        use crate::live_routes::{FAILURE_COOLDOWN_SECS, SAMPLE_LIFETIME_SECS};

        let station = |id: &str, guide: &str| Item {
            kind: Kind::Live,
            id: id.to_owned(),
            title: "Station".to_owned(),
            epg_id: guide.to_owned(),
            ..Item::default()
        };
        let listed = vec![station("main", "guide"), station("speedy", "different")];
        let mut full = Catalog::new(listed.clone());
        full.append(vec![
            station("speedy", "guide"),
            station("backup", "guide"),
            station("slow", "guide"),
        ]);
        let mut timings = Feeds::default();
        for (id, at, ms) in [
            ("main", 150, 6500),
            ("speedy", 100, 900),
            ("backup", 150, 1500),
            ("slow", 150, 7200),
        ] {
            timings.record(
                "account",
                id,
                at,
                FeedSample {
                    format: None,
                    startup_ms: ms,
                    width: 640,
                    height: 360,
                    epg_id: "guide",
                },
            );
        }
        assert!(!timings.has_preview_candidate("account", &listed[1..], 160));
        assert!(timings.has_preview_candidate("account", &listed, 160));
        assert!(!timings.has_preview_candidate("account", &listed, 160 + SAMPLE_LIFETIME_SECS));
        timings.failure("account", "speedy", 160, None);
        let routes = full.preview_feed_routes(&listed, &timings, "account", 161);
        assert_eq!(
            routes.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["speedy", "backup"]
        );
        let mut restored = Catalog::new(listed.clone());
        restored.set_preview_feed_routes(routes);
        let choose = |now| {
            timings
                .best_feed("account", "main", "guide", now, |id| {
                    restored.equivalent_live_feed(&listed[0], id).is_some()
                })
                .map(|(id, _, _)| id)
        };
        assert_eq!(choose(161), Some("backup"));
        assert_eq!(choose(161 + FAILURE_COOLDOWN_SECS), Some("speedy"));
        assert_eq!(choose(101 + SAMPLE_LIFETIME_SECS), Some("backup"));
        assert_eq!(restored.visible_items(false).count(), 2);
    }

    #[test]
    fn zapping_stops_at_the_next_distinct_channel() {
        let items = ["a", "a", "b", "a", "b", "c", "d"].map(|id| Item {
            id: id.into(),
            ..Item::default()
        });
        let mut visited = 0;
        let channels = items.iter().inspect(|_| visited += 1);
        let (previous, next) = super::live_neighbor_pair(channels, "b").unwrap();
        assert_eq!((previous.id.as_str(), next.id.as_str()), ("a", "c"));
        assert_eq!(visited, 6);
    }

    #[test]
    fn zapping_finds_xtream_channels_without_stored_stream_urls() {
        let source = Source {
            server: "https://example.test".into(),
            ..Source::default()
        };
        let categories = Default::default();
        let entries = json!([
            {"stream_id": 1, "name": "Nieuws", "category_id": 7},
            {"stream_id": 2, "name": "Sport", "category_id": 7},
            {"stream_id": 3, "name": "Radio", "category_id": 7, "stream_type": "radio"},
            {"stream_id": 4, "name": "Films", "category_id": 8},
            {"stream_id": 5, "name": "Cultuur", "category_id": 7},
        ]);
        let items = entries
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| xtream_live_row(entry, &categories).unwrap())
            .collect();
        let catalog = Catalog::new(items);
        let (previous, next) = catalog
            .live_neighbors_for_source(&catalog.items[0], false, &source)
            .unwrap();
        assert_eq!(previous.id, "5");
        assert_eq!(next.id, "2");
        let playlist = Source {
            playlist: "https://example.test/list.m3u".into(),
            ..source
        };
        assert!(
            catalog
                .live_neighbors_for_source(&catalog.items[0], false, &playlist)
                .is_none()
        );
    }

    #[test]
    fn indexes_batches_and_keeps_the_visible_preview_without_discarding_titles() {
        let item = |id: &str, category: &str| Item {
            id: id.into(),
            title: format!("Voorbeeld {id}"),
            category: category.into(),
            ..Item::default()
        };
        let mut catalog = Catalog::new(vec![item("A", "NL - Films")]);
        catalog.append(vec![item("B", "DE - Filme"), item("C", "EN - Movies")]);
        catalog.keep_preview_order(&[item("C", "EN - Movies"), item("missing", "NL - Films")]);
        assert_eq!(catalog.search("", "", true, 5), (vec![2, 0], 2));
        assert_eq!(catalog.search("", "", true, 1), (vec![2], 2));
        assert_eq!(catalog.search("...", "", false, 0), (vec![], 3));
        assert_eq!(
            catalog.search("voorbeeld", "", false, 5),
            (vec![0, 1, 2], 3)
        );
        assert_eq!(catalog.categories(true), ["EN - Movies", "NL - Films"]);
    }

    #[test]
    fn narrowing_search_preserves_duplicate_variants_filters_and_new_batches() {
        let movie = |id: &str, title: &str| Item {
            id: id.to_owned(),
            title: title.to_owned(),
            ..Item::default()
        };
        let mut catalog = Catalog::new(vec![
            movie("1", "North wind"),
            movie("1", "North star"),
            movie("2", "DE - North star"),
        ]);
        assert_eq!(catalog.search("no", "", true, 1), (vec![0], 1));
        assert_eq!(catalog.search("north star", "", true, 1), (vec![1], 1));
        assert_eq!(catalog.search("north star", "", false, 2), (vec![1, 2], 2));
        catalog.append(vec![movie("3", "North stars")]);
        assert_eq!(catalog.search("north stars", "", true, 1), (vec![3], 1));
    }

    #[test]
    fn narrowing_a_broad_search_keeps_matches_beyond_the_shortlist() {
        let mut titles: Vec<_> = (0..super::SEARCH_SHORTLIST_LIMIT)
            .map(|index| Item {
                id: index.to_string(),
                title: "Valley".into(),
                ..Item::default()
            })
            .collect();
        let last = titles.len();
        titles.push(Item {
            title: "Valley lake".into(),
            ..Item::default()
        });
        let catalog = Catalog::new(titles);
        assert_eq!(catalog.search("val", "", false, 1), (vec![0], last + 1));
        assert_eq!(catalog.search("valley lake", "", false, 1), (vec![last], 1));
    }

    #[test]
    fn search_keeps_earlier_hits_when_later_catalog_batches_match() {
        let film = |id: &str, title: &str, category: &str| Item {
            id: id.into(),
            title: title.into(),
            category: category.into(),
            ..Item::default()
        };
        let preview = Catalog::new(vec![
            film("1", "Harry Potter", "NL - Films"),
            film("2", "Harry Potter 2", "NL - Films"),
        ]);
        let loaded = Catalog::new(vec![
            film("2", "Harry Potter 2", "NL - Films"),
            film("3", "Harry Potter 3", "EN - Movies"),
            film("4", "DE - Harry Potter 4", "DE - Filme"),
        ]);
        let ids = |items: Vec<&Item>| items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            ids(loaded.search_with_preview(&preview, "harry potter", BrowseFilter::All, true, 60)),
            ["1", "2", "3"]
        );
        assert_eq!(
            ids(loaded.search_with_preview(
                &preview,
                "harry potter",
                BrowseFilter::Category("NL - Films"),
                true,
                60
            )),
            ["1", "2"]
        );
        let full = Catalog::new(vec![
            film("1", "Harry Potter", "NL - Films"),
            film("2", "Harry Potter 2", "NL - Films"),
            film("2", "Harry Potter 2", "EN - Movies"),
        ]);
        assert_eq!(full.search("harry potter", "", true, 60), (vec![0, 1], 2));
        assert_eq!(
            full.search("harry potter", "EN - Movies", true, 60),
            (vec![2], 1)
        );
    }

    #[test]
    fn filters_normalized_genres_within_title_search_and_language_preferences() {
        let catalog = Catalog::new(vec![
            Item {
                id: "1".into(),
                title: "NL - Sterrenwacht".into(),
                category: "Provider Boxsets".into(),
                genres: Genre::from_feed(&json!("Sci-fi & Drama, science fiction")),
                ..Item::default()
            },
            Item {
                id: "2".into(),
                title: "EN - Starbound".into(),
                category: "|EN| Science Fiction".into(),
                ..Item::default()
            },
            Item {
                id: "3".into(),
                title: "DE - Dunkel".into(),
                genres: Genre::from_feed(&json!(["Thriller", "Sci-Fi"])),
                ..Item::default()
            },
            Item {
                id: "4".into(),
                title: "EN - North".into(),
                category: "NL - Thriller Picks".into(),
                ..Item::default()
            },
            Item {
                kind: Kind::Live,
                title: "Sci-fi channel".into(),
                category: "Sci-fi".into(),
                ..Item::default()
            },
        ]);

        let science_fiction = BrowseFilter::Genre(Genre::ScienceFiction);
        assert_eq!(
            catalog.search_filtered("", science_fiction, true, 60),
            (vec![0, 1], 2)
        );
        assert_eq!(
            catalog.search_filtered("star", science_fiction, false, 60),
            (vec![1], 1)
        );
        assert_eq!(
            catalog.search_filtered("", science_fiction, false, 60),
            (vec![0, 1, 2], 3)
        );
        assert_eq!(
            catalog.items[0].genres,
            [Genre::Drama, Genre::ScienceFiction]
        );
        assert!(catalog.items[3].genres.is_empty());
        assert_eq!(
            catalog.genres(true).iter().copied().collect::<Vec<_>>(),
            [Genre::Drama, Genre::ScienceFiction]
        );
        assert!(!catalog.genres(true).contains(&Genre::Thriller));
        assert!(catalog.genres(false).contains(&Genre::Thriller));
        assert!(catalog.genre_covers_at_least_half(true));
    }
}
