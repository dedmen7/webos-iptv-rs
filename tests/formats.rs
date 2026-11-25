use serde_json::json;
use url::Url;
use webos_iptv_rs::{
    catalog::{Catalog, Item, Kind, Source, preferred_content, xtream_catalog, xtream_episodes},
    guide, playlist,
    subtitles::{needs_fallback, rank, read_opensubtitles, read_subdl, search_url, webvtt},
};

#[test]
fn xtream_keeps_provider_base_and_escapes_credentials() {
    let source = Source {
        server: "https://example.test/provider/".into(),
        username: "user/name".into(),
        password: "p&ss?".into(),
        ..Source::default()
    };
    let api = source.api("get_vod_info", Some(("vod_id", "77"))).unwrap();
    assert_eq!(api.path(), "/provider/player_api.php");
    assert!(
        api.query_pairs()
            .any(|(key, value)| key == "password" && value == "p&ss?")
    );
    let items = xtream_catalog(
        &source,
        Kind::Movie,
        &json!([{ "category_id": "3", "category_name": "NL - Films" }]),
        &json!([{ "stream_id": 77, "name": "Voorbeeld", "category_id": 3, "container_extension": "mkv" }]),
    );
    assert_eq!(items[0].category, "NL - Films");
    assert_eq!(
        items[0].stream,
        "https://example.test/provider/movie/user%2Fname/p&ss%3F/77.mkv"
    );
}

#[test]
fn xtream_episode_prefix_and_season_order() {
    let source = Source {
        server: "https://example.test".into(),
        ..Source::default()
    };
    let series = Item {
        title: "Voorbeeldserie".into(),
        kind: Kind::Series,
        ..Item::default()
    };
    let episodes = xtream_episodes(
        &source,
        &series,
        &json!({
            "episodes": {
                "10": [{"id": "10", "episode_num": 1, "title": "Later"}],
                "2": [{"id": "2", "episode_num": 1, "title": "Eerder"}]
            }
        }),
    );
    assert_eq!(
        episodes.iter().map(|item| item.season).collect::<Vec<_>>(),
        [2, 10]
    );
    assert_eq!(episodes[0].series_title, "Voorbeeldserie");
    assert!(episodes[0].stream.contains("/series/"));
}

#[test]
fn playlist_handles_quoted_commas_relative_paths_and_unsafe_urls() {
    let base = Url::parse("https://example.test/path/list.m3u").unwrap();
    let data = "\u{feff}#EXTM3U x-tvg-url=\"guide.xml\"\n#EXTINF:-1 tvg-id=\"one\" group-title=\"NL, Nieuws\",Zender 1\nclip.m3u8\n#EXTINF:-1,Duplicate\nclip.m3u8\n#EXTINF:-1,Unsafe\njavascript:alert(1)";
    let items = playlist::parse(data, &base);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].category, "NL, Nieuws");
    assert_eq!(items[0].epg_id, "one");
    assert_eq!(items[0].stream, "https://example.test/path/clip.m3u8");
    assert_eq!(
        playlist::guide_url(data, &base).unwrap().path(),
        "/path/guide.xml"
    );
}

#[test]
fn programme_parser_applies_timezone_and_ignores_out_of_range_data() {
    let start = guide::unix_time("20261125210000 +0100").unwrap();
    let schedules = guide::parse(
        r#"<tv>
        <programme start="20261125210000 +0100" stop="20261125220000 +0100" channel="one"><title>Nieuws &amp; weer</title><desc>Laat</desc></programme>
        <programme start="20261101010000 +0000" channel="one"><title>Oud</title></programme>
        </tv>"#,
        start,
        start + 7200,
    ).unwrap();
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0].name(), "Nieuws & weer");
    assert_eq!(
        schedules[0].start_unix,
        guide::unix_time("20261125200000 +0000").unwrap()
    );
}

#[test]
fn query_prioritizes_matches_and_removes_known_foreign_languages() {
    let items = [
        "NL - De Vuurbeker",
        "DE - De Vuurbeker",
        "Vuurbéker",
        "EN - The Vuurbeker",
    ]
    .into_iter()
    .map(|title| Item {
        title: title.into(),
        ..Item::default()
    })
    .collect();
    let catalog = Catalog::new(items);
    let (indices, count) = catalog.search("vuurbeker", "", true, 2);
    assert_eq!(count, 3);
    assert_eq!(indices, [2, 0]);
    assert!(!preferred_content(&catalog.items[1]));
}

#[test]
fn subtitles_fall_back_for_plain_english_and_keep_the_requested_episode() {
    let item = Item {
        kind: Kind::Episode,
        season: 1,
        episode: 2,
        series_title: "NL - Voorbeeld".into(),
        ..Item::default()
    };
    let plain = read_opensubtitles(&json!({
        "data": [{"attributes": { "language":"en", "release":"SD", "hearing_impaired":false, "files":[{"file_id":3}] }}]
    }));
    assert!(needs_fallback(&plain));
    let fallback = read_subdl(
        &json!({ "subtitles": [{"unpack_files": [
            {"name":"Mis", "season":1,"episode":1,"format":"srt","language":"NL","url":"/subtitle/123/mis"},
            {"name":"Juist","season":1,"episode":2,"format":"srt","language":"NL","url":"/subtitle/123/juist"},
            {"name":"Anders","season":1,"episode":2,"format":"srt","language":"FR","url":"/subtitle/123/anders"}
        ]}]}),
        &item,
    );
    assert_eq!(fallback.len(), 1);
    assert!(!needs_fallback(&fallback));
    let mut combined = plain;
    combined.extend(fallback);
    rank(&mut combined);
    assert!(combined[0].label().contains("Nederlands · SubDL"));
    let query = search_url(&item, Some("example"), "");
    assert!(
        query
            .query_pairs()
            .any(|(key, value)| key == "episode_number" && value == "2")
    );
}

#[test]
fn srt_is_converted_to_valid_webvtt_and_offset_is_applied() {
    let track = webvtt("1\r\n00:00:01,100 --> 00:00:02,450\r\nHallo!\r\n", -0.5).unwrap();
    assert_eq!(track, "WEBVTT\n\n00:00:00.600 --> 00:00:01.950\nHallo!\n\n");
}
