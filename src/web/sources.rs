use super::i18n::tr;
use super::io;
use crate::{
    catalog::{self, Item, Kind, Source},
    guide,
    json_array::JsonArray,
    playlist,
    subtitles::{self, Download, Subtitle},
};
use futures::join;
use serde::{Deserialize, de::IgnoredAny};
use serde_json::{Value, json};
use std::collections::HashMap;
use url::Url;

async fn json(url: &Url, headers: &[(&str, &str)], body: Option<&str>) -> Result<Value, String> {
    let response = io::text(url.as_str(), headers, body).await?;
    decode_json(&response)
}

fn decode_json(response: &str) -> Result<Value, String> {
    serde_json::from_str(response).map_err(|_| tr("De bron gaf geen geldige JSON terug").to_owned())
}

pub struct LiveDownload {
    pub items: Vec<Item>,
    pub categorized: bool,
}

pub async fn live_catalog(
    source: &Source,
    current: impl Fn() -> bool,
) -> Result<LiveDownload, String> {
    if !source.playlist.trim().is_empty() {
        let url = catalog::http_url(&source.playlist).ok_or(tr("Ongeldige M3U-link"))?;
        let response = io::text(url.as_str(), &[], None).await?;
        let items = playlist::parse(&response, &url);
        return (!items.is_empty())
            .then_some(LiveDownload {
                items,
                categorized: true,
            })
            .ok_or_else(|| tr("Geen kanalen gevonden in deze M3U-lijst").to_owned());
    }
    let stream_url = source
        .api("get_live_streams", None)
        .ok_or(tr("Voeg een Xtream-account toe in Instellingen"))?;
    let category_url = source
        .api("get_live_categories", None)
        .ok_or(tr("Ongeldige server-URL"))?;
    let (body, categories) = join!(
        io::CatalogBody::open(stream_url.as_str()),
        json(&category_url, &[], None)
    );
    let mut body = body?;
    let categories = categories.unwrap_or(Value::Null);
    let names = catalog::category_names(&categories);
    let mut parser = JsonArray::default();
    let mut items = Vec::new();
    let mut received = 0;
    let mut slice_bytes = 0;
    while current()
        && let Some(chunk) = body.next().await?
    {
        for bytes in chunk.chunks(VOD_PARSE_SLICE) {
            parser
                .feed(bytes, |row| {
                    let entry = serde_json::from_slice(row).map_err(|_| ())?;
                    received += 1;
                    if let Some(item) = catalog::xtream_live_row(&entry, &names) {
                        items.push(item);
                    }
                    Ok(true)
                })
                .map_err(|_| {
                    tr("De aanbieder gaf geen catalogus terug. Controleer je inloggegevens.")
                })?;
            slice_bytes += bytes.len();
            if slice_bytes >= VOD_PARSE_SLICE {
                slice_bytes = 0;
                io::delay(0).await;
                if !current() {
                    break;
                }
            }
        }
    }
    if !parser.finished() || (received != 0 && items.is_empty()) {
        return Err(
            tr("De aanbieder gaf geen catalogus terug. Controleer je inloggegevens.").to_owned(),
        );
    }
    let categorized = categories.as_array().is_some_and(|entries| {
        !entries.is_empty() || items.iter().all(|item| item.category_id.is_empty())
    });
    Ok(LiveDownload { items, categorized })
}

pub async fn vod_categories(source: &Source, kind: Kind) -> Result<Value, String> {
    let category_action = if kind == Kind::Movie {
        "get_vod_categories"
    } else {
        "get_series_categories"
    };
    let url = source
        .api(category_action, None)
        .ok_or(tr("Ongeldige server-URL"))?;
    json(&url, &[], None).await
}

pub struct VodFeed<'a> {
    pub source: &'a Source,
    pub kind: Kind,
    pub categories: &'a HashMap<String, String>,
    pub preview: Option<PreviewCategory<'a>>,
}

pub struct PreviewCategory<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub preferred: bool,
    pub limit: usize,
}

const VOD_BATCH_SIZE: usize = 768;
const VOD_PARSE_SLICE: usize = 128 * 1024;

pub async fn stream_vod(
    feed: VodFeed<'_>,
    mut publish: impl AsyncFnMut(Vec<Item>) -> bool,
) -> Result<(), String> {
    let action = if feed.kind == Kind::Movie {
        "get_vod_streams"
    } else {
        "get_series"
    };
    let parameter = feed
        .preview
        .as_ref()
        .map(|preview| ("category_id", preview.id));
    let url = feed
        .source
        .api(action, parameter)
        .ok_or(tr("Ongeldige server-URL"))?;
    let mut body = io::CatalogBody::open(url.as_str()).await?;
    let mut parser = JsonArray::default();
    let mut batch = Vec::with_capacity(VOD_BATCH_SIZE);
    let mut received = 0;
    let mut usable = 0;
    let mut processed_bytes = 0;
    while let Some(bytes) = body.next().await? {
        for bytes in bytes.chunks(VOD_PARSE_SLICE) {
            let more = parser
                .feed(bytes, |value| {
                    received += 1;
                    if let Some(item) = vod_item(&feed, value)? {
                        batch.push(item);
                        usable += 1;
                    }
                    Ok(feed
                        .preview
                        .as_ref()
                        .is_none_or(|preview| usable < preview.limit))
                })
                .map_err(|()| tr("De bron gaf geen geldige JSON terug"))?;
            if !more || (feed.preview.is_none() && batch.len() >= VOD_BATCH_SIZE) {
                let items = std::mem::replace(&mut batch, Vec::with_capacity(VOD_BATCH_SIZE));
                if !publish(items).await || !more {
                    return Ok(());
                }
            }
            processed_bytes += bytes.len();
            if processed_bytes >= VOD_PARSE_SLICE {
                processed_bytes = 0;
                io::delay(0).await;
            }
        }
    }
    if !parser.finished() {
        return Err(tr("De catalogus is onvolledig ontvangen").to_owned());
    }
    if feed.preview.is_none() && received != 0 && usable == 0 {
        return Err(tr("De aanbieder gaf geen bruikbare titels terug.").to_owned());
    }
    if !batch.is_empty() {
        publish(batch).await;
    }
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct VodRow {
    stream_id: Field,
    series_id: Field,
    name: Field,
    category_id: Field,
    stream_icon: Field,
    cover: Field,
    genre: Value,
    container_extension: Field,
    imdb_id: Field,
    imdb: Field,
    year: Field,
    #[serde(rename = "releaseDate")]
    release_date: Field,
}

#[derive(Default, Deserialize)]
#[serde(untagged)]
enum Field {
    Text(String),
    Number(serde_json::Number),
    Ignored(IgnoredAny),
    #[default]
    Empty,
}

impl Field {
    fn text(self) -> String {
        match self {
            Self::Text(value) if value.len() == value.trim().len() => value,
            Self::Text(value) => value.trim().to_owned(),
            Self::Number(value) => value.to_string(),
            Self::Empty | Self::Ignored(_) => String::new(),
        }
    }

    fn with_fallback(self, fallback: Self) -> String {
        let value = self.text();
        if value.is_empty() {
            fallback.text()
        } else {
            value
        }
    }
}

fn vod_item(feed: &VodFeed<'_>, bytes: &[u8]) -> Result<Option<Item>, ()> {
    if !bytes.starts_with(b"{") {
        serde_json::from_slice::<IgnoredAny>(bytes).map_err(|_| ())?;
        return Ok(None);
    }
    let row: VodRow = serde_json::from_slice(bytes).map_err(|_| ())?;
    let id = if feed.kind == Kind::Movie {
        row.stream_id
    } else {
        row.series_id
    }
    .text();
    let title = row.name.text();
    if id.is_empty() || title.is_empty() {
        return Ok(None);
    }
    let category_id = row.category_id.text();
    let category = if let Some(preview) = &feed.preview {
        if !category_id.is_empty() && category_id != preview.id {
            return Ok(None);
        }
        preview.label.to_owned()
    } else {
        feed.categories
            .get(&category_id)
            .cloned()
            .unwrap_or_default()
    };
    let item = Item {
        kind: feed.kind,
        id,
        title,
        category,
        genres: catalog::Genre::from_feed(&row.genre),
        poster: row.stream_icon.text(),
        poster_fallback: row.cover.text(),
        extension: row.container_extension.text(),
        imdb: row.imdb_id.with_fallback(row.imdb),
        year: row.year.with_fallback(row.release_date),
        ..Item::default()
    };
    Ok((!feed
        .preview
        .as_ref()
        .is_some_and(|preview| preview.preferred)
        || catalog::preferred_content(&item))
    .then_some(item))
}

pub async fn episodes(source: &Source, item: &mut Item) -> Result<Vec<Item>, String> {
    let url = source
        .api("get_series_info", Some(("series_id", &item.id)))
        .ok_or(tr("Selecteer een Xtream-account"))?;
    let details = json(&url, &[], None).await?;
    let info = &details["info"];
    if item.plot.is_empty() {
        item.plot = catalog::field(info, "plot");
    }
    if item.year.is_empty() {
        item.year = catalog::field(info, "year");
        if item.year.is_empty() {
            item.year = catalog::field(info, "releaseDate");
        }
    }
    Ok(catalog::xtream_episodes(source, item, &details))
}

pub async fn movie(source: &Source, item: &mut Item) -> Result<(), String> {
    let url = source
        .api("get_vod_info", Some(("vod_id", &item.id)))
        .ok_or(tr("Selecteer een Xtream-account"))?;
    let payload = json(&url, &[], None).await?;
    catalog::merge_movie_details(item, &payload);
    Ok(())
}

pub async fn short_epg(source: &Source, stream_id: &str) -> Option<Vec<guide::Programme>> {
    let mut url = source.api("get_short_epg", Some(("stream_id", stream_id)))?;
    url.query_pairs_mut().append_pair("limit", "2");
    let mut body = io::CatalogBody::open(url.as_str()).await.ok()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await.ok()? {
        if bytes.len() + chunk.len() > 128 * 1024 {
            return None;
        }
        bytes.extend(chunk);
    }
    let response: Value = serde_json::from_slice(&bytes).ok()?;
    let now = io::now();
    let window = web_sys::window()?;
    let mut programmes: Vec<_> = response["epg_listings"]
        .as_array()?
        .iter()
        .filter_map(|listing| {
            let start_unix = catalog::field(listing, "start_timestamp").parse().ok()?;
            let stop_unix = catalog::field(listing, "stop_timestamp").parse().ok()?;
            if start_unix >= stop_unix || stop_unix <= now {
                return None;
            }
            let raw = catalog::field(listing, "title");
            let title = window
                .atob(&raw)
                .ok()
                .and_then(|decoded| {
                    String::from_utf8(decoded.chars().map(|ch| ch as u8).collect()).ok()
                })
                .filter(|decoded| {
                    !decoded.trim().is_empty() && !decoded.chars().any(char::is_control)
                })
                .unwrap_or(raw);
            if title.is_empty() {
                return None;
            }
            Some(guide::Programme {
                channel: catalog::field(listing, "channel_id"),
                catchup_id: String::new(),
                start: String::new(),
                stop: String::new(),
                title: vec![title],
                description: Vec::new(),
                start_unix,
                stop_unix,
            })
        })
        .collect();
    programmes.sort_by_key(|programme| programme.start_unix);
    Some(programmes)
}

pub const GUIDE_DAYS: i64 = 7;

pub async fn guide(source: &Source) -> Result<Vec<guide::Programme>, String> {
    let url = if (source.playlist.trim().is_empty() || !source.guide.trim().is_empty())
        && let Some(url) = source.guide_url()
    {
        url
    } else {
        let base = catalog::http_url(&source.playlist)
            .ok_or(tr("Stel een XMLTV-link in om de gids te laden"))?;
        let response = io::text(base.as_str(), &[], None).await?;
        playlist::guide_url(&response, &base).ok_or(tr("De M3U-lijst bevat geen XMLTV-link"))?
    };
    let response = io::text(url.as_str(), &[], None).await?;
    let now = io::now();
    guide::parse(
        &response,
        now - GUIDE_DAYS * 86_400,
        now + GUIDE_DAYS * 86_400,
    )
    .map_err(|_| tr("Dit bestand is geen geldige XMLTV-gids").to_owned())
}

pub async fn archive_urls(
    source: &Source,
    item: &Item,
    programme: &guide::Programme,
) -> Result<(Url, Option<Url>), String> {
    let unavailable = tr("Dit programma is niet beschikbaar om terug te kijken.");
    let now = io::now();
    let archive = item
        .archive
        .as_ref()
        .filter(|archive| archive.available(programme.start_unix, programme.stop_unix, now))
        .ok_or(unavailable)?;
    if matches!(archive, catalog::Archive::Template { .. }) {
        return archive
            .template_url(item, programme, now)
            .map(|url| (url, None))
            .ok_or_else(|| unavailable.to_owned());
    }
    let lookup = source
        .api("get_simple_data_table", Some(("stream_id", &item.id)))
        .ok_or(unavailable)?;
    let listings = json(&lookup, &[], None).await.unwrap_or_default();
    let start = programme.start_unix;
    let matched = listings["epg_listings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let timestamp = catalog::field(entry, "start_timestamp")
                .parse::<i64>()
                .ok()?;
            let difference = timestamp.abs_diff(start);
            (difference <= 60).then_some((difference, entry))
        })
        .min_by_key(|(difference, _)| *difference)
        .map(|(_, entry)| entry);
    if matched.is_some_and(|entry| {
        entry["has_archive"].as_bool() == Some(false)
            || matches!(catalog::field(entry, "has_archive").as_str(), "0" | "false")
    }) {
        return Err(unavailable.to_owned());
    }
    let wall_time = matched.and_then(|entry| provider_time(&catalog::field(entry, "start")));
    let wall_time = if let Some(time) = wall_time {
        time
    } else {
        let account = source.api("", None).ok_or(unavailable)?;
        let details = json(&account, &[], None).await.unwrap_or_default();
        let clock = &details["server_info"];
        let offset = provider_time(&catalog::field(clock, "time_now"))
            .and_then(|wall_now| {
                let timestamp = catalog::field(clock, "timestamp_now").parse::<i64>().ok()?;
                Some((wall_now.and_utc().timestamp() - timestamp).clamp(-86_400, 86_400))
            })
            .unwrap_or(0);
        chrono::DateTime::from_timestamp(start.saturating_add(offset), 0)
            .ok_or(unavailable)?
            .naive_utc()
    };
    let wall_time = wall_time.format("%Y-%m-%d:%H-%M").to_string();
    let minutes = (programme.stop_unix - start + 59) / 60;
    let first = source
        .archive_url(&item.id, &wall_time, minutes, "m3u8")
        .ok_or(unavailable)?;
    let alternate = source.archive_url(&item.id, &wall_time, minutes, "ts");
    Ok((first, alternate))
}

fn provider_time(value: &str) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").ok()
}

fn os_headers(key: &str) -> [(&str, &str); 3] {
    [
        ("Api-Key", key),
        ("Content-Type", "application/json"),
        (
            "User-Agent",
            concat!("webos-iptv-rs v", env!("CARGO_PKG_VERSION")),
        ),
    ]
}

pub(super) struct SubtitleMatches {
    pub results: Vec<Subtitle>,
    pub checked_subdl: bool,
}

pub async fn find_subtitles(
    item: &Item,
    os_key: &str,
    subdl_key: &str,
    manual: &str,
) -> Result<SubtitleMatches, String> {
    let mut matches = Vec::new();
    let mut error = None;
    let mut successful = false;
    if !os_key.is_empty() {
        let url = subtitles::search_url(item, None, manual);
        match json(&url, &os_headers(os_key), None).await {
            Ok(body) => {
                successful = true;
                matches = subtitles::read_opensubtitles(&body, item);
            }
            Err(reason) => error = Some(format!("OpenSubtitles: {reason}")),
        }
    }
    let checked_subdl = !subdl_key.is_empty() && subtitles::needs_fallback(item, &matches);
    if checked_subdl {
        match find_subdl(item, subdl_key, manual).await {
            Ok(subtitles) => {
                successful = true;
                matches.extend(subtitles);
            }
            Err(reason) => error = Some(format!("SubDL: {reason}")),
        }
    }
    if !successful {
        return Err(
            error.unwrap_or_else(|| tr("Voeg een API-sleutel toe bij Instellingen").to_owned())
        );
    }
    subtitles::rank(item, &mut matches);
    Ok(SubtitleMatches {
        results: matches,
        checked_subdl,
    })
}

pub(super) async fn find_subdl(
    item: &Item,
    api_key: &str,
    manual: &str,
) -> Result<Vec<Subtitle>, String> {
    let url = subtitles::search_url(item, Some(api_key), manual);
    let body = json(&url, &[], None).await?;
    if body["status"].as_bool() == Some(false) {
        return Err(tr("SubDL accepteerde de aanvraag niet").to_owned());
    }
    let mut matches = subtitles::read_subdl(&body, item);
    subtitles::rank(item, &mut matches);
    Ok(matches)
}

pub async fn subtitle_file(subtitle: &Subtitle, os_key: &str) -> Result<String, String> {
    let url = match &subtitle.download {
        Download::SubDl(url) | Download::Sidecar(url) => url.clone(),
        Download::OpenSubtitles(id) => {
            let endpoint = Url::parse("https://api.opensubtitles.com/api/v1/download")
                .expect("constant provider URL");
            let id: u32 = id.parse().map_err(|_| tr("Ongeldig ondertitel-ID"))?;
            let payload = json!({"file_id": id}).to_string();
            let response = json(&endpoint, &os_headers(os_key), Some(&payload)).await?;
            let url = catalog::http_url(&catalog::field(&response, "link"))
                .ok_or(tr("Geen downloadlink ontvangen"))?;
            if url.scheme() != "https" {
                return Err(tr("De ondertitelbron geeft geen beveiligde downloadlink").to_owned());
            }
            url
        }
    };
    io::text(url.as_str(), &[], None).await
}
