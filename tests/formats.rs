use serde_json::json;
use url::Url;
use webos_iptv_rs::{
    catalog::{
        Catalog, Item, Kind, LiveGroups, Source, preferred_content, xtream_catalog, xtream_episodes,
    },
    guide, playlist,
    series::SeriesProgress,
    subtitles::{
        Download, fits_duration, needs_fallback, rank, read_opensubtitles, read_subdl, search_url,
        webvtt,
    },
};

#[test]
fn m3u_distinguishes_radio_from_television_in_mixed_playlists() {
    let source = Url::parse("https://example.test/playlist.m3u").unwrap();
    let items = playlist::parse(
        concat!(
            "#EXTM3U\n",
            "#EXTINF:-1 group-title=\"TV & Radio\",Studio TV\n/live/tv.m3u8\n",
            "#EXTINF:-1 group-title=\"Gemengd\" radio=\"TRUE\",Studio Radio\n/live/studio.m3u8\n",
            "#EXTINF:-1 group-title=\"NL - Radiozenders\",Zender FM\n/live/fm.ts\n",
            "#EXTINF:-1 group-title=\"Radio\" radio=\"false\",Studio webcam\n/live/webcam.ts\n",
            "#EXTINF:-1,Losse stream\n/live/audio.MP3?format=live\n",
        ),
        &source,
    );
    let kinds: Vec<_> = items.iter().map(|item| item.kind).collect();
    assert_eq!(
        kinds,
        [
            Kind::Live,
            Kind::Radio,
            Kind::Radio,
            Kind::Live,
            Kind::Radio
        ]
    );
}

#[test]
fn xtream_radio_keeps_live_stream_urls_and_skips_tv_replay() {
    let source = Source {
        server: "https://example.test/provider".into(),
        username: "fixture".into(),
        password: "fixture".into(),
        ..Source::default()
    };
    let categories = json!([
        {"category_id": "1", "category_name": "TV & Radio"},
        {"category_id": "2", "category_name": "NL - Radio"},
    ]);
    let rows = json!([
        {"stream_id": 7, "name": "Studio TV", "category_id": "1"},
        {"stream_id": 8, "name": "Zender FM", "category_id": "2", "tv_archive": 1},
        {"stream_id": 9, "name": "Station", "category_id": "1", "is_radio": true, "container_extension": "mp3"},
    ]);
    let items = xtream_catalog(&source, Kind::Live, &categories, &rows);
    assert_eq!(items[0].kind, Kind::Live);
    let station = &items[1];
    assert_eq!(station.kind, Kind::Radio);
    assert!(station.archive.is_none());
    let url = station.playback_url(&source).unwrap();
    assert_eq!(url.path(), "/provider/live/fixture/fixture/8.m3u8");
    assert!(
        station
            .alternate_live_url(&source, &url)
            .unwrap()
            .path()
            .ends_with("/8.ts")
    );
    assert_eq!(items[2].kind, Kind::Radio);
    assert!(items[2].stream.ends_with("/live/fixture/fixture/9.mp3"));
}

#[test]
fn m3u_replay_respects_channel_overrides_and_resolves_the_programme_template() {
    let base = Url::parse("https://example.test/playlist.m3u").unwrap();
    let stations = playlist::parse(
        concat!(
            "#EXTM3U catchup=\"default\" catchup-source=\"/archive/{catchup-id}?from=${utc}&to={utcend}\" catchup-days=\"2\"\n",
            "#EXTINF:-1,Replay\n/live/1.m3u8\n",
            "#EXTINF:-1 catchup-enable=\"0\",Live only\n/live/2.m3u8\n"
        ),
        &base,
    );
    let programmes = guide::parse(
        r#"<tv><programme channel="test" catchup-id="one/two" start="20261126100000 +0000" stop="20261126103000 +0000"><title>News</title></programme></tv>"#,
        0, i64::MAX,
    ).unwrap();
    let programme = &programmes[0];
    let rules = stations[0].archive.as_ref().unwrap();
    assert!(rules.available(
        programme.start_unix,
        programme.stop_unix,
        programme.stop_unix
    ));
    assert!(!rules.available(
        programme.start_unix,
        programme.stop_unix,
        programme.start_unix + 3 * 86_400
    ));
    assert!(stations[1].archive.is_none());
    assert_eq!(
        rules
            .template_url(&stations[0], programme, programme.stop_unix)
            .unwrap()
            .path(),
        "/archive/one%2Ftwo"
    );
    let mut no_id = programme.clone();
    no_id.catchup_id.clear();
    assert!(
        rules
            .template_url(&stations[0], &no_id, programme.stop_unix)
            .is_none()
    );
    let appended = playlist::parse(
        concat!(
            "#EXTM3U catchup=\"append\" catchup-source=\"?utc={utc}\" catchup-days=\"2\"\n",
            "#EXTINF:-1 catchup-days=\"1\",Replay\n/live/1.m3u8?token=demo#details\n",
            "#EXTINF:-1 catchup-days=\"0\",Disabled\n/live/2.m3u8\n"
        ),
        &base,
    );
    let archive = appended[0].archive.as_ref().unwrap();
    let target = archive
        .template_url(&appended[0], programme, programme.stop_unix)
        .unwrap();
    assert_eq!(
        target.query(),
        Some(format!("token=demo&utc={}", programme.start_unix).as_str())
    );
    assert_eq!(target.fragment(), Some("details"));
    assert!(!archive.available(
        programme.start_unix,
        programme.stop_unix,
        programme.start_unix + 86_401
    ));
    assert!(appended[1].archive.is_none());
}

#[test]
fn live_group_policy_applies_before_search_and_navigation_limits() {
    let item = |group: &str, id: &str| Item {
        kind: Kind::Live,
        category: group.into(),
        id: id.into(),
        title: format!("NL - Zender {id}"),
        stream: format!("https://example.test/{id}.m3u8"),
        ..Item::default()
    };
    let mut catalog = Catalog::new(vec![
        item("NL - A", "1"),
        item("NL - B", "2"),
        item("NL - C", "3"),
    ]);
    catalog.apply_live_groups(&LiveGroups {
        order: vec!["NL - C".to_owned()],
        hidden: ["NL - B".to_owned()].into_iter().collect(),
    });
    assert_eq!(catalog.search("zender", "", true, 1), (vec![2], 2));
    assert_eq!(
        catalog
            .visible_items(true)
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["3", "1"]
    );
    assert_eq!(catalog.categories(true), ["NL - A", "NL - B", "NL - C"]);
}

#[test]
fn xtream_zapping_uses_the_group_id_when_group_names_are_missing() {
    let source = Source {
        server: "https://example.test".into(),
        ..Source::default()
    };
    let stations = xtream_catalog(
        &source,
        Kind::Live,
        &json!(null),
        &json!([
            {"stream_id":1,"category_id":"7","name":"NL| NPO1 4K"},
            {"stream_id":2,"category_id":"7","name":"NL| NPO2 4K"},
            {"stream_id":3,"category_id":"8","name":"FR| Cinéma"},
        ]),
    );
    let catalog = Catalog::new(stations);
    let (previous, next) = catalog.live_neighbors(&catalog.items[0], false).unwrap();
    assert_eq!((previous.id.as_str(), next.id.as_str()), ("2", "2"));
    assert!(!preferred_content(&catalog.items[2]));
}

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
    assert!(items[0].stream.is_empty());
    assert_eq!(items[0].extension, "mkv");
    assert_eq!(
        items[0].playback_url(&source).unwrap().as_str(),
        "https://example.test/provider/movie/user%2Fname/p&ss%3F/77.mkv"
    );
}

#[test]
fn xtream_keeps_alternate_movie_artwork_and_resolves_relative_links() {
    let source = Source {
        server: "https://example.test/provider/".into(),
        playlist: "https://live.test/iptv/list.m3u".into(),
        ..Source::default()
    };
    let categories = json!([]);
    let rows = json!([
        {"stream_id":1,"name":"Invalid primary","stream_icon":0,"cover":"poster?id=1"},
        {"stream_id":2,"name":"Two candidates","stream_icon":"posters/two.jpg","cover":"//images.test/two.jpg"},
        {"stream_id":3,"name":"Duplicate","stream_icon":"/art/three.jpg","cover":"https://example.test/art/three.jpg"}
    ]);
    let items = xtream_catalog(&source, Kind::Movie, &categories, &rows);
    let urls = items
        .iter()
        .map(|item| {
            let (primary, fallback) = item.poster_urls(&source);
            (
                primary.map(|url| url.to_string()),
                fallback.map(|url| url.to_string()),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        urls,
        [
            (
                Some("https://example.test/provider/poster?id=1".into()),
                None
            ),
            (
                Some("https://example.test/provider/posters/two.jpg".into()),
                Some("https://images.test/two.jpg".into())
            ),
            (Some("https://example.test/art/three.jpg".into()), None),
        ]
    );

    let mut details = items[0].clone();
    webos_iptv_rs::catalog::merge_movie_details(
        &mut details,
        &json!({"info":{"movie_image":"null","cover":"/art/details.jpg"}}),
    );
    let (primary, fallback) = details.poster_urls(&source);
    assert_eq!(
        primary.unwrap().as_str(),
        "https://example.test/provider/poster?id=1"
    );
    assert_eq!(fallback.unwrap().path(), "/art/details.jpg");

    let live = Item {
        kind: Kind::Live,
        poster: "channel.webp".into(),
        ..Item::default()
    };
    assert_eq!(
        live.poster_urls(&source).0.unwrap().as_str(),
        "https://live.test/iptv/channel.webp"
    );
    let old: Item = serde_json::from_value(json!({"poster":"/old.jpg"})).unwrap();
    assert_eq!(old.poster_urls(&source).0.unwrap().path(), "/old.jpg");
}

#[test]
fn public_tmdb_movie_posters_keep_the_provider_image_as_a_fallback() {
    let provider = "http://192.0.2.20/images/movies//1E5baAaEse26fej7uHcjOgEE2t2.jpg";
    let mut item = Item {
        poster: provider.into(),
        ..Item::default()
    };
    let source = Source::default();
    let (primary, fallback) = item.poster_urls(&source);
    assert_eq!(
        primary.unwrap().as_str(),
        "https://image.tmdb.org/t/p/w342/1E5baAaEse26fej7uHcjOgEE2t2.jpg"
    );
    assert_eq!(fallback.unwrap().as_str(), provider);

    item.poster = format!("{provider}?user=fixture");
    let (primary, fallback) = item.poster_urls(&source);
    assert_eq!(primary.unwrap().as_str(), item.poster);
    assert!(fallback.is_none());

    item.kind = Kind::Live;
    item.poster = provider.into();
    assert_eq!(item.poster_urls(&source).0.unwrap().as_str(), provider);
}

#[test]
fn xtream_live_can_retry_the_same_channel_using_the_other_output_format() {
    let mut source = Source {
        server: "https://example.test/provider/".into(),
        username: "user/name".into(),
        password: "p&ss?".into(),
        ..Source::default()
    };
    let items = xtream_catalog(
        &source,
        Kind::Live,
        &json!([]),
        &json!([{ "stream_id": 77, "name": "NL| NPO1 4K" }]),
    );
    let item = &items[0];
    let primary = item.playback_url(&source).unwrap();
    assert!(primary.path().ends_with("/77.m3u8"));
    let alternate = item.alternate_live_url(&source, &primary).unwrap();
    assert_eq!(
        alternate.as_str(),
        "https://example.test/provider/live/user%2Fname/p&ss%3F/77.ts"
    );
    assert_eq!(
        item.alternate_live_url(&source, &alternate),
        Some(primary.clone())
    );
    let unrelated_url = Url::parse("https://example.test/playlist/channel.m3u8").unwrap();
    assert!(item.alternate_live_url(&source, &unrelated_url).is_none());
    source.playlist = "https://example.test/playlist.m3u".into();
    let playlist_stream =
        Url::parse("https://example.test/provider/live/user/pass/77.m3u8?token=demo").unwrap();
    let from_playlist = Item {
        id: "m3u-fixture".into(),
        ..item.clone()
    };
    let alternate = from_playlist
        .alternate_live_url(&source, &playlist_stream)
        .unwrap();
    assert_eq!(
        alternate.as_str(),
        "https://example.test/provider/live/user/pass/77.ts?token=demo"
    );
    assert!(
        from_playlist
            .alternate_live_url(&source, &unrelated_url)
            .is_none()
    );
}

#[test]
fn live_neighbors_wrap_within_the_category_and_skip_duplicate_or_hidden_channels() {
    let live = |id: &str, title: &str, category: &str| Item {
        id: id.into(),
        title: title.into(),
        category: category.into(),
        kind: Kind::Live,
        stream: format!("https://example.test/{id}.m3u8"),
        ..Item::default()
    };
    let first = live("1", "NL| NPO1", "NL - Nederland");
    let catalog = Catalog::new(vec![
        first.clone(),
        first.clone(),
        live("2", "DE - Hidden", "NL - Nederland"),
        live("3", "NL| NPO2", "NL - Nederland"),
        live("4", "NL| NPO3", "NL - Nederland"),
        live("3", "NL| NPO2", "NL - Nederland"),
        live("5", "UK - News", "UK - News"),
    ]);
    let (previous, next) = catalog.live_neighbors(&first, true).unwrap();
    assert_eq!((previous.id.as_str(), next.id.as_str()), ("4", "3"));
    let (previous, next) = catalog.live_neighbors(previous, true).unwrap();
    assert_eq!((previous.id.as_str(), next.id.as_str()), ("3", "1"));
    let (previous, next) = catalog.live_neighbors(&catalog.items[6], true).unwrap();
    assert_eq!((previous.id.as_str(), next.id.as_str()), ("4", "1"));
    let (previous, next) = catalog.live_neighbors(&catalog.items[2], true).unwrap();
    assert_eq!((previous.id.as_str(), next.id.as_str()), ("4", "1"));
    let mut without_news = catalog;
    let hidden = LiveGroups {
        hidden: ["UK - News".into()].into_iter().collect(),
        ..LiveGroups::default()
    };
    without_news.apply_live_groups(&hidden);
    let (previous, next) = without_news
        .live_neighbors(&without_news.items[6], true)
        .unwrap();
    assert_eq!((previous.id.as_str(), next.id.as_str()), ("4", "1"));
    assert!(
        Catalog::new(vec![first.clone()])
            .live_neighbors(&first, true)
            .is_none()
    );
}

#[test]
fn xtream_episode_prefix_and_season_order() {
    let source = Source {
        server: "https://example.test".into(),
        ..Source::default()
    };
    let series = Item {
        id: "42".into(),
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
    assert_eq!(episodes[0].series_id, "42");
    assert!(episodes[0].stream.contains("/series/"));
}

#[test]
fn series_progress_resumes_or_moves_to_the_next_season() {
    let episodes = [
        Item {
            id: "end-of-season".into(),
            season: 1,
            episode: 8,
            ..Item::default()
        },
        Item {
            id: "new-season".into(),
            season: 2,
            episode: 1,
            ..Item::default()
        },
    ];
    let mut saved = SeriesProgress {
        series: Item::default(),
        episode_id: "end-of-season".into(),
        season: 1,
        episode: 8,
        finished: false,
    };
    assert_eq!(saved.target(&episodes).unwrap().id, "end-of-season");
    saved.finished = true;
    assert_eq!(saved.target(&episodes).unwrap().id, "new-season");
    saved.episode_id = "new-season".into();
    saved.season = 2;
    saved.episode = 1;
    assert!(saved.target(&episodes).is_none());
}

#[test]
fn series_progress_handles_changed_episode_ids_without_guessing() {
    let episode = Item {
        id: "new-id".into(),
        season: 2,
        episode: 3,
        ..Item::default()
    };
    let saved = SeriesProgress {
        series: Item::default(),
        episode_id: "old-id".into(),
        season: 2,
        episode: 3,
        finished: false,
    };
    assert_eq!(
        saved.target(std::slice::from_ref(&episode)).unwrap().id,
        "new-id"
    );
    let completed = SeriesProgress {
        finished: true,
        ..saved
    };
    assert_eq!(
        completed
            .matched_episode(std::slice::from_ref(&episode))
            .unwrap()
            .id,
        "new-id"
    );
    assert!(completed.target(std::slice::from_ref(&episode)).is_none());
    assert!(
        completed
            .matched_episode(&[
                episode.clone(),
                Item {
                    id: "duplicate".into(),
                    ..episode
                }
            ])
            .is_none()
    );
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
    let plain = read_opensubtitles(
        &json!({
            "data": [
                {"attributes": {"language":"nl","feature_details":{"season_number":1,"episode_number":1},"files":[{"file_id":2}]}},
                {"attributes": {"language":"en","release":"SD","hearing_impaired":false,"files":[{"file_id":3}]}}
            ]
        }),
        &item,
    );
    assert_eq!(plain.len(), 1);
    assert!(needs_fallback(&item, &plain));
    let fallback = read_subdl(
        &json!({ "subtitles": [{"unpack_files": [
            {"name":"Mis", "season":1,"episode":1,"format":"srt","language":"NL","url":"/subtitle/123/mis"},
            {"name":"Juist","season":1,"episode":2,"format":"srt","language":"NL","url":"/subtitle/123/juist"},
            {"name":"Anders","season":1,"episode":2,"format":"srt","language":"FR","url":"/subtitle/123/anders"}
        ]}]}),
        &item,
    );
    assert_eq!(fallback.len(), 1);
    assert!(!needs_fallback(&item, &fallback));
    let mut combined = plain;
    combined.extend(fallback);
    rank(&item, &mut combined);
    assert!(combined[0].label().contains("Nederlands · SubDL"));
    let query = search_url(&item, Some("example"), "");
    assert!(
        query
            .query_pairs()
            .any(|(key, value)| key == "episode_number" && value == "2")
    );
}

#[test]
fn subtitle_ranking_prefers_release_fit_then_language_and_download_count() {
    let movie = Item {
        kind: Kind::Movie,
        title: "Voorbeeld Extended".into(),
        stream: "https://example.test/Voorbeeld.1080p.WEB-DL.mkv".into(),
        ..Item::default()
    };
    let mut results = read_opensubtitles(
        &json!({"data": [
            {"attributes":{"language":"nl","release":"Voorbeeld.BluRay","download_count":9999,"files":[{"file_id":1}]}},
            {"attributes":{"language":"nl","release":"Voorbeeld.WEB-DL-v1","download_count":"7","files":[{"file_id":2}]}},
            {"attributes":{"language":"nl","release":"Voorbeeld.WEB-DL-v2","download_count":21,"files":[{"file_id":3}]}},
            {"attributes":{"language":"en","hearing_impaired":true,"release":"Voorbeeld.WEB-DL","download_count":23,"files":[{"file_id":4}]}},
            {"attributes":{"language":"nl","release":"Voorbeeld.Extended.WEB-DL","download_count":1,"files":[{"file_id":5}]}}
        ]}),
        &movie,
    );
    assert!(needs_fallback(&movie, &results[..1]));
    assert!(!needs_fallback(&movie, &results));
    rank(&movie, &mut results);
    let ids: Vec<_> = results
        .iter()
        .map(|subtitle| match &subtitle.download {
            Download::OpenSubtitles(id) => id.as_str(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(ids, ["5", "3", "2", "4", "1"]);
}

#[test]
fn subtitle_duration_check_rejects_files_outside_the_video_timeline() {
    let long = "1\n00:05:01,000 --> 00:05:03,000\nHallo.\n";
    let short = "1\n00:00:01,000 --> 00:00:03,000\nHallo.\n";
    assert!(!fits_duration(long, 100.));
    assert!(fits_duration(short, 100.));
    assert!(fits_duration(short, f64::NAN));
    assert!(!fits_duration("geen ondertitels", 100.));
}

#[test]
fn srt_is_converted_to_valid_webvtt_and_offset_is_applied() {
    let track = webvtt("1\r\n00:00:01,100 --> 00:00:02,450\r\nHallo!\r\n", -0.5).unwrap();
    assert_eq!(track, "WEBVTT\n\n00:00:00.600 --> 00:00:01.950\nHallo!\n\n");
}

#[test]
fn movie_preview_uses_allowed_categories_and_keeps_already_visible_titles_first() {
    use webos_iptv_rs::catalog::{lead_with_preview, preview_categories, xtream_movie_preview};
    let categories = json!([
        {"category_id": "7", "category_name": "DE - German"},
        {"category_id": "2", "category_name": "NL - Films"}
    ]);
    let (id, name) = preview_categories(&categories, true).next().unwrap();
    assert_eq!(id, "2");

    let provider_rows = json!([
        {"stream_id": 1, "name": "EN - Outside category", "category_id": 7},
        {"stream_id": 2, "name": "DE - Outside filter", "category_id": 2},
        {"stream_id": 3, "name": "Een film zonder categorie-ID"},
        {"stream_id": 4, "name": "EN - Another film", "category_id": 2},
    ]);
    let source = Source::default();
    let first = xtream_movie_preview(&source, &provider_rows, (&id, &name), true, 1);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, "3");
    assert_eq!(first[0].category, name);
    let full = xtream_catalog(&source, Kind::Movie, &categories, &provider_rows);
    let full = lead_with_preview(full, &first);
    assert_eq!(
        full.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
        ["3", "1", "2", "4"]
    );
}

#[test]
fn movie_links_from_previous_caches_are_still_playable() {
    let movie: Item = serde_json::from_value(json!({
        "kind": "movie", "stream": "https://example.test/movie/fixture/fixture/1.mkv"
    }))
    .unwrap();
    assert_eq!(
        movie.playback_url(&Source::default()).unwrap().path(),
        "/movie/fixture/fixture/1.mkv"
    );
}

#[test]
fn cached_movie_omits_empty_fields_and_keeps_playback_metadata() {
    let item = Item {
        id: "2".into(),
        title: "Film".into(),
        extension: "mp4".into(),
        ..Item::default()
    };
    let value = serde_json::to_value(item).unwrap();
    assert_eq!(
        value,
        json!({"id": "2", "kind": "movie", "title": "Film", "extension": "mp4"})
    );
    let restored: Item = serde_json::from_value(value).unwrap();
    assert_eq!(restored.extension, "mp4");
    assert!(restored.sidecars.is_empty());
}

#[test]
fn guide_tiles_distinguish_a_gap_and_clip_to_the_visible_time_window() {
    fn programme(start_unix: i64, stop_unix: i64) -> guide::Programme {
        guide::Programme {
            channel: "fixture".into(),
            catchup_id: String::new(),
            start: String::new(),
            stop: String::new(),
            title: vec![format!("Programma {start_unix}")],
            description: Vec::new(),
            start_unix,
            stop_unix,
        }
    }
    let entries = [programme(10, 50), programme(60, 90)];
    let (current, next) = guide::now_and_next(&entries, 25);
    assert_eq!(current.unwrap().start_unix, 10);
    assert_eq!(next.unwrap().start_unix, 60);
    let (current, next) = guide::now_and_next(&entries, 50);
    assert!(current.is_none());
    assert_eq!(next.unwrap().start_unix, 60);
    assert_eq!(guide::overlap(&entries[0], 30..70), Some((0., 0.5)));
    assert_eq!(guide::overlap(&entries[1], 30..70), Some((0.75, 0.25)));
    assert_eq!(guide::overlap(&entries[0], 50..60), None);

    let cached = serde_json::to_string(&entries[0]).unwrap();
    let restored: guide::Programme = serde_json::from_str(&cached).unwrap();
    assert_eq!((restored.start_unix, restored.stop_unix), (10, 50));
}
