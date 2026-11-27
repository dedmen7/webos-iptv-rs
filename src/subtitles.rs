use crate::catalog::{Item, Kind, Language, field, fold, language, subtitle_title};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Reverse;
use std::collections::HashSet;
use url::Url;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Download {
    OpenSubtitles(String),
    SubDl(Url),
    Sidecar(Url),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Subtitle {
    pub name: String,
    pub language: Language,
    pub cc: bool,
    pub download: Download,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<u64>,
}

impl Subtitle {
    pub fn label(&self) -> String {
        self.label_with(|text| text)
    }

    pub fn label_with(&self, translate: impl Fn(&'static str) -> &'static str) -> String {
        format!(
            "{}{} · {} · {}",
            match self.language {
                Language::Dutch => translate("Nederlands"),
                _ => translate("English"),
            },
            if self.cc { " CC" } else { "" },
            match self.download {
                Download::OpenSubtitles(_) => "OpenSubtitles",
                Download::SubDl(_) => "SubDL",
                Download::Sidecar(_) => translate("Aanbieder"),
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

pub fn read_opensubtitles(body: &Value, item: &Item) -> Vec<Subtitle> {
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
            let feature = &attributes["feature_details"];
            if item.kind == Kind::Episode
                && [
                    ("season_number", item.season),
                    ("episode_number", item.episode),
                ]
                .into_iter()
                .any(|(key, expected)| {
                    expected > 0
                        && field(feature, key)
                            .parse::<u32>()
                            .is_ok_and(|number| number != expected)
                })
            {
                return None;
            }
            let file = attributes["files"].as_array()?.first()?;
            let id = field(file, "file_id");
            if id.is_empty() {
                return None;
            }
            Some(Subtitle {
                name: release_name(field(attributes, "release"), field(file, "file_name")),
                language,
                cc: attributes["hearing_impaired"].as_bool().unwrap_or(false),
                download: Download::OpenSubtitles(id),
                downloads: field(attributes, "download_count").parse().ok(),
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
                    Some(Subtitle {
                        name: release_name(field(file, "release_name"), field(file, "name")),
                        language,
                        cc: file["hi"].as_bool().unwrap_or(false),
                        download: Download::SubDl(url),
                        downloads: None,
                    })
                })
        })
        .collect()
}

pub fn needs_fallback(item: &Item, results: &[Subtitle]) -> bool {
    let stream_release = item_release(item);
    !results.iter().any(|subtitle| {
        language_priority(subtitle) < 2
            && release_fit(stream_release, &subtitle.name) != ReleaseFit::Conflict
    })
}

pub fn rank(item: &Item, results: &mut Vec<Subtitle>) {
    let stream_release = item_release(item);
    results.sort_by_cached_key(|subtitle| {
        (
            release_fit(stream_release, &subtitle.name),
            language_priority(subtitle),
            Reverse(subtitle.downloads),
            subtitle.language == Language::Dutch && subtitle.cc,
        )
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

fn language_priority(subtitle: &Subtitle) -> u8 {
    match (subtitle.language, subtitle.cc) {
        (Language::Dutch, _) => 0,
        (Language::English, true) => 1,
        _ => 2,
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Release {
    source: Option<&'static str>,
    edition: Option<&'static str>,
}

fn release_name(name: String, filename: String) -> String {
    if name.is_empty()
        || (release(&name) == Release::default() && release(&filename) != Release::default())
    {
        filename
    } else {
        name
    }
}

fn release(name: &str) -> Release {
    let normalized = format!(" {} ", fold(name));
    let matches = |tokens: &[&str]| tokens.iter().any(|token| normalized.contains(token));
    let source = [
        ("web-dl", &[" web dl ", " webdl "][..]),
        ("web-rip", &[" web rip ", " webrip "][..]),
        (
            "blu-ray",
            &[" blu ray ", " bluray ", " bdrip ", " brrip "][..],
        ),
        ("dvd", &[" dvd rip ", " dvdrip ", " dvd "][..]),
        ("broadcast", &[" hdtv ", " pdtv "][..]),
    ]
    .into_iter()
    .find_map(|(value, tokens)| matches(tokens).then_some(value));
    let edition = [
        ("director", &[" director s cut ", " directors cut "][..]),
        ("extended", &[" extended "][..]),
        ("theatrical", &[" theatrical "][..]),
        ("unrated", &[" unrated "][..]),
    ]
    .into_iter()
    .find_map(|(value, tokens)| matches(tokens).then_some(value));
    Release { source, edition }
}

fn item_release(item: &Item) -> Release {
    let title = release(&item.title);
    let filename = Url::parse(&item.stream)
        .ok()
        .and_then(|url| url.path_segments()?.next_back().map(release))
        .unwrap_or_default();
    Release {
        source: filename.source.or(title.source),
        edition: filename.edition.or(title.edition),
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum ReleaseFit {
    Match(Reverse<usize>),
    Unknown,
    Conflict,
}

fn release_fit(expected: Release, name: &str) -> ReleaseFit {
    let candidate = release(name);
    let hints = [
        expected.source.zip(candidate.source),
        expected.edition.zip(candidate.edition),
    ];
    let known = hints.iter().flatten().count();
    if hints
        .iter()
        .flatten()
        .any(|(expected, found)| expected != found)
    {
        ReleaseFit::Conflict
    } else if known > 0 {
        ReleaseFit::Match(Reverse(known))
    } else {
        ReleaseFit::Unknown
    }
}

fn cues_and_text(input: &str) -> Vec<(crate::sync::Cue, String)> {
    let normalized = input
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut cues = Vec::new();
    for block in normalized.split("\n\n") {
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
        let text = lines.collect::<Vec<_>>().join("\n");
        if start < end && !text.trim().is_empty() {
            cues.push((crate::sync::Cue { start, end }, text));
        }
    }
    cues.sort_by(|(left, _), (right, _)| left.start.total_cmp(&right.start));
    cues
}

pub fn sync_cues(input: &str) -> Vec<crate::sync::Cue> {
    cues_and_text(input)
        .into_iter()
        .map(|(cue, _)| cue)
        .collect()
}

pub fn fits_duration(input: &str, duration: f64) -> bool {
    let cues = sync_cues(input);
    if cues.is_empty() {
        return false;
    }
    if !duration.is_finite() || duration <= 0. {
        return true;
    }
    let tolerance = (duration * 0.02).clamp(6., 15.);
    let outside = cues
        .iter()
        .filter(|cue| cue.end > duration + tolerance)
        .count();
    cues[0].start < duration && outside <= (cues.len() / 20).max(1)
}

pub fn webvtt(input: &str, offset: f64) -> Option<String> {
    retime(
        input,
        crate::sync::Timing {
            offset,
            ..Default::default()
        },
    )
}

pub fn retime(input: &str, timing: crate::sync::Timing) -> Option<String> {
    let mut output = String::from("WEBVTT\n\n");
    let mut count = 0;
    for (cue, text) in cues_and_text(input) {
        let start = timing.video_time(cue.start);
        let end = timing.video_time(cue.end);
        if end <= 0. {
            continue;
        }
        output.push_str(&format!(
            "{} --> {}\n{}\n\n",
            timestamp(start.max(0.)),
            timestamp(end),
            text
        ));
        count += 1;
    }
    (count > 0).then_some(output)
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
