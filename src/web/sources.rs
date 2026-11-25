use super::io;
use crate::{
    catalog::{self, Item, Kind, Source},
    guide, playlist,
    subtitles::{self, Download, Subtitle},
};
use futures::join;
use serde_json::{Value, json};
use url::Url;

async fn json(url: &Url, headers: &[(&str, &str)], body: Option<&str>) -> Result<Value, String> {
    let response = io::text(url.as_str(), headers, body).await?;
    serde_json::from_str(&response).map_err(|_| "De bron gaf geen geldige JSON terug".to_owned())
}

pub async fn catalog(source: &Source, kind: Kind) -> Result<Vec<Item>, String> {
    if kind == Kind::Live && !source.playlist.trim().is_empty() {
        let url = catalog::http_url(&source.playlist).ok_or("Ongeldige M3U-link")?;
        let response = io::text(url.as_str(), &[], None).await?;
        let items = playlist::parse(&response, &url);
        return (!items.is_empty())
            .then_some(items)
            .ok_or_else(|| "Geen kanalen gevonden in deze M3U-lijst".to_owned());
    }
    let (streams, categories) = match kind {
        Kind::Live => ("get_live_streams", "get_live_categories"),
        Kind::Movie => ("get_vod_streams", "get_vod_categories"),
        Kind::Series => ("get_series", "get_series_categories"),
        Kind::Episode => return Err("Open een serie om afleveringen te laden".to_owned()),
    };
    let stream_url = source
        .api(streams, None)
        .ok_or("Voeg een Xtream-account toe in Instellingen")?;
    let category_url = source.api(categories, None).ok_or("Ongeldige server-URL")?;
    let (streams, categories) = join!(json(&stream_url, &[], None), json(&category_url, &[], None));
    let streams = streams?;
    if !streams.is_array() {
        return Err(
            "De aanbieder gaf geen catalogus terug. Controleer je inloggegevens.".to_owned(),
        );
    }
    Ok(catalog::xtream_catalog(
        source,
        kind,
        &categories.unwrap_or(Value::Null),
        &streams,
    ))
}

pub async fn episodes(source: &Source, item: &Item) -> Result<Vec<Item>, String> {
    let url = source
        .api("get_series_info", Some(("series_id", &item.id)))
        .ok_or("Selecteer een Xtream-account")?;
    let details = json(&url, &[], None).await?;
    Ok(catalog::xtream_episodes(source, item, &details))
}

pub async fn movie(source: &Source, item: &mut Item) -> Result<(), String> {
    let url = source
        .api("get_vod_info", Some(("vod_id", &item.id)))
        .ok_or("Selecteer een Xtream-account")?;
    let payload = json(&url, &[], None).await?;
    catalog::merge_movie_details(item, &payload);
    Ok(())
}

pub async fn guide(source: &Source) -> Result<Vec<guide::Programme>, String> {
    let url = if (source.playlist.trim().is_empty() || !source.guide.trim().is_empty())
        && let Some(url) = source.guide_url()
    {
        url
    } else {
        let base = catalog::http_url(&source.playlist)
            .ok_or("Stel een XMLTV-link in om de gids te laden")?;
        let response = io::text(base.as_str(), &[], None).await?;
        playlist::guide_url(&response, &base).ok_or("De M3U-lijst bevat geen XMLTV-link")?
    };
    let response = io::text(url.as_str(), &[], None).await?;
    let now = io::now();
    guide::parse(&response, now - 86_400, now + 7 * 86_400)
        .map_err(|_| "Dit bestand is geen geldige XMLTV-gids".to_owned())
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

pub async fn find_subtitles(
    item: &Item,
    os_key: &str,
    subdl_key: &str,
    manual: &str,
) -> Result<Vec<Subtitle>, String> {
    let mut matches = Vec::new();
    let mut error = None;
    let mut successful = false;
    if !os_key.is_empty() {
        let url = subtitles::search_url(item, None, manual);
        match json(&url, &os_headers(os_key), None).await {
            Ok(body) => {
                successful = true;
                matches = subtitles::read_opensubtitles(&body);
            }
            Err(reason) => error = Some(format!("OpenSubtitles: {reason}")),
        }
    }
    if !subdl_key.is_empty() && subtitles::needs_fallback(&matches) {
        let url = subtitles::search_url(item, Some(subdl_key), manual);
        match json(&url, &[], None).await {
            Ok(body) if body["status"].as_bool() == Some(false) => {
                error = Some("SubDL accepteerde de aanvraag niet".to_owned());
            }
            Ok(body) => {
                successful = true;
                matches.extend(subtitles::read_subdl(&body, item));
            }
            Err(reason) => error = Some(format!("SubDL: {reason}")),
        }
    }
    if !successful {
        return Err(error.unwrap_or_else(|| "Voeg een API-sleutel toe bij Instellingen".to_owned()));
    }
    subtitles::rank(&mut matches);
    Ok(matches)
}

pub async fn subtitle_file(subtitle: &Subtitle, os_key: &str) -> Result<String, String> {
    let url = match &subtitle.download {
        Download::SubDl(url) | Download::Sidecar(url) => url.clone(),
        Download::OpenSubtitles(id) => {
            let endpoint = Url::parse("https://api.opensubtitles.com/api/v1/download")
                .expect("constant provider URL");
            let id: u32 = id.parse().map_err(|_| "Ongeldig ondertitel-ID")?;
            let payload = json!({"file_id": id}).to_string();
            let response = json(&endpoint, &os_headers(os_key), Some(&payload)).await?;
            let url = catalog::http_url(&catalog::field(&response, "link"))
                .ok_or("Geen downloadlink ontvangen")?;
            if url.scheme() != "https" {
                return Err("De ondertitelbron geeft geen beveiligde downloadlink".to_owned());
            }
            url
        }
    };
    io::text(url.as_str(), &[], None).await
}
