use chrono::{NaiveDateTime, TimeZone, Utc};
use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Programme {
    #[serde(rename = "@channel")]
    pub channel: String,
    #[serde(
        rename = "@catchup-id",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub catchup_id: String,
    #[serde(rename = "@start")]
    pub start: String,
    #[serde(rename = "@stop", default)]
    pub stop: String,
    #[serde(default)]
    pub title: Vec<String>,
    #[serde(rename = "desc", default)]
    pub description: Vec<String>,
    #[serde(default)]
    pub start_unix: i64,
    #[serde(default)]
    pub stop_unix: i64,
}

impl Programme {
    pub fn name(&self) -> &str {
        self.title.first().map_or("", String::as_str)
    }
}

pub fn parse(input: &str, from: i64, until: i64) -> Result<Vec<Programme>, quick_xml::Error> {
    let mut reader = Reader::from_str(input);
    reader.config_mut().trim_text(true);
    let mut output = Vec::new();
    loop {
        let before = reader.buffer_position() as usize;
        match reader.read_event()? {
            Event::Start(start) if start.name().as_ref() == "programme" => {
                reader.read_to_end(start.name())?;
                let after = reader.buffer_position() as usize;
                if let Some(fragment) = input.get(before..after)
                    && let Ok(mut programme) = quick_xml::de::from_str::<Programme>(fragment)
                    && let Some(start) = unix_time(&programme.start)
                {
                    let stop = unix_time(&programme.stop).unwrap_or(start + 3600);
                    if stop >= from && start <= until {
                        programme.start_unix = start;
                        programme.stop_unix = stop;
                        output.push(programme);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    output.sort_by_key(|programme| programme.start_unix);
    Ok(output)
}

pub fn unix_time(time: &str) -> Option<i64> {
    let time = time.trim();
    if let Ok(date) = chrono::DateTime::parse_from_str(time, "%Y%m%d%H%M%S %z") {
        return Some(date.timestamp());
    }
    let time = NaiveDateTime::parse_from_str(time, "%Y%m%d%H%M%S").ok()?;
    Some(Utc.from_utc_datetime(&time).timestamp())
}

pub fn now_and_next(
    programmes: &[Programme],
    now: i64,
) -> (Option<&Programme>, Option<&Programme>) {
    let boundary = programmes.partition_point(|programme| programme.start_unix <= now);
    let current = programmes[..boundary]
        .iter()
        .rev()
        .find(|programme| programme.stop_unix > now);
    (current, programmes.get(boundary))
}

pub fn overlap(programme: &Programme, window: std::ops::Range<i64>) -> Option<(f64, f64)> {
    let start = programme.start_unix.max(window.start);
    let end = programme.stop_unix.min(window.end);
    if end <= start {
        return None;
    }
    let duration = (window.end - window.start) as f64;
    Some((
        (start - window.start) as f64 / duration,
        (end - start) as f64 / duration,
    ))
}
