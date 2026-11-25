use crate::catalog::{Item, Kind, Language, field, fold, language, subtitle_title};
use serde_json::Value;
use std::collections::HashSet;
use url::Url;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Download {
    OpenSubtitles(String),
    SubDl(Url),
    Sidecar(Url),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subtitle {
    pub name: String,
    pub language: Language,
    pub cc: bool,
    pub download: Download,
}

impl Subtitle {
    pub fn label(&self) -> String {
        format!(
            "{}{} · {} · {}",
            match self.language {
                Language::Dutch => "Nederlands",
                _ => "English",
            },
            if self.cc { " CC" } else { "" },
            match self.download {
                Download::OpenSubtitles(_) => "OpenSubtitles",
                Download::SubDl(_) => "SubDL",
                Download::Sidecar(_) => "Aanbieder",
            },
            self.name
        )
    }
}

pub fn search_url(item: &Item, subdl_key: Option<&str>, manual: &str) -> Url {
    let mut url = Url::parse(if subdl_key.is_some() {
        "https://api.subdl.com/api/v1/subtitles"
    } else {
        "https://api.opensubtitles.com/api/v1/subtitles"
    })
    .expect("constant provider URL");

    let query = if manual.trim().is_empty() {
        subtitle_title(
            if item.kind == Kind::Episode && !item.series_title.is_empty() {
                &item.series_title
            } else {
                &item.title
            },
        )
    } else {
        manual.trim().to_owned()
    };
    let episode = item.kind == Kind::Episode;
    let imdb = item.imdb.trim().trim_start_matches("tt");
    let has_imdb =
        manual.trim().is_empty() && !imdb.is_empty() && imdb.chars().all(|ch| ch.is_ascii_digit());
    let mut params = url.query_pairs_mut();
    if let Some(api_key) = subdl_key {
        params
            .append_pair("api_key", api_key)
            .append_pair("languages", "NL,EN")
            .append_pair("unpack", "1")
            .append_pair("subs_per_page", "30")
            .append_pair("type", if episode { "tv" } else { "movie" });
        if has_imdb {
            params.append_pair("imdb_id", &format!("tt{imdb}"));
        } else {
            params.append_pair("film_name", &query);
        }
    } else {
        params
            .append_pair("languages", "nl,en")
            .append_pair("type", if episode { "episode" } else { "movie" });
        if has_imdb {
            params.append_pair(if episode { "parent_imdb_id" } else { "imdb_id" }, imdb);
        } else {
            params.append_pair("query", &query);
        }
    }
    if episode && item.season > 0 && item.episode > 0 {
        params
            .append_pair("season_number", &item.season.to_string())
            .append_pair("episode_number", &item.episode.to_string());
    } else if !episode
        && manual.trim().is_empty()
        && let Some(year) = item.year.get(..4)
        && year.chars().all(|ch| ch.is_ascii_digit())
    {
        params.append_pair("year", year);
    }
    drop(params);
    url
}

pub fn read_opensubtitles(body: &Value) -> Vec<Subtitle> {
    body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let attributes = &row["attributes"];
            let language = language(&field(attributes, "language"));
            if !matches!(language, Language::Dutch | Language::English) {
                return None;
            }
            let file = attributes["files"].as_array()?.first()?;
            let id = field(file, "file_id");
            if id.is_empty() {
                return None;
            }
            let mut name = field(attributes, "release");
            if name.is_empty() {
                name = field(file, "file_name");
            }
            Some(Subtitle {
                name,
                language,
                cc: attributes["hearing_impaired"].as_bool().unwrap_or(false),
                download: Download::OpenSubtitles(id),
            })
        })
        .collect()
}

pub fn read_subdl(body: &Value, item: &Item) -> Vec<Subtitle> {
    body["subtitles"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|row| {
            row["unpack_files"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|file| {
                    let language = language(&field(file, "language"));
                    if !matches!(language, Language::Dutch | Language::English) {
                        return None;
                    }
                    if item.kind == Kind::Episode
                        && item.season > 0
                        && item.episode > 0
                        && (field(file, "season").parse::<u32>().ok() != Some(item.season)
                            || field(file, "episode").parse::<u32>().ok() != Some(item.episode))
                    {
                        return None;
                    }
                    let format = field(file, "format").to_ascii_lowercase();
                    if !matches!(format.as_str(), "srt" | "vtt") {
                        return None;
                    }
                    let suffix = field(file, "url");
                    let url = Url::parse(&format!("https://dl.subdl.com{suffix}")).ok()?;
                    if !suffix.starts_with("/subtitle/") || url.host_str() != Some("dl.subdl.com") {
                        return None;
                    }
                    let mut name = field(file, "release_name");
                    if name.is_empty() {
                        name = field(file, "name");
                    }
                    Some(Subtitle {
                        name,
                        language,
                        cc: file["hi"].as_bool().unwrap_or(false),
                        download: Download::SubDl(url),
                    })
                })
        })
        .collect()
}

pub fn needs_fallback(results: &[Subtitle]) -> bool {
    !results.iter().any(|subtitle| {
        subtitle.language == Language::Dutch
            || (subtitle.language == Language::English && subtitle.cc)
    })
}

pub fn rank(results: &mut Vec<Subtitle>) {
    results.sort_by_key(|subtitle| match (subtitle.language, subtitle.cc) {
        (Language::Dutch, false) => 0,
        (Language::Dutch, true) => 1,
        (Language::English, true) => 2,
        _ => 3,
    });
    let mut seen = HashSet::new();
    results.retain(|entry| {
        let release = if entry.name.trim().is_empty() {
            format!("{:?}", entry.download)
        } else {
            fold(&entry.name)
        };
        seen.insert((entry.language, entry.cc, release))
    });
    results.truncate(60);
}

pub fn webvtt(input: &str, offset: f64) -> Option<String> {
    let input = input
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut output = String::from("WEBVTT\n\n");
    let mut cues = 0;
    for block in input.split("\n\n") {
        let mut lines = block.lines().skip_while(|line| !line.contains("-->"));
        let Some(timeline) = lines.next() else {
            continue;
        };
        let Some((start, end)) = timeline.split_once("-->") else {
            continue;
        };
        let Some(start) = seconds(start.trim()) else {
            continue;
        };
        let Some(end) = end.split_whitespace().next().and_then(seconds) else {
            continue;
        };
        let start = start + offset;
        let end = end + offset;
        if end <= 0.0 || start >= end {
            continue;
        }
        let text = lines.collect::<Vec<_>>().join("\n");
        if text.trim().is_empty() {
            continue;
        }
        output.push_str(&format!(
            "{} --> {}\n{}\n\n",
            timestamp(start.max(0.0)),
            timestamp(end),
            text
        ));
        cues += 1;
    }
    (cues > 0).then_some(output)
}

fn seconds(time: &str) -> Option<f64> {
    let mut result = 0.;
    for part in time.replace(',', ".").split(':') {
        result = result * 60. + part.parse::<f64>().ok()?;
    }
    result.is_finite().then_some(result)
}

fn timestamp(time: f64) -> String {
    let ms = (time * 1000.).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}
