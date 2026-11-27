use webos_iptv_rs::{
    subtitles::retime,
    sync::{AudioSample, Cue, Timing, TimingPoint, align, fit_rate, measure_point},
};

const LINES: [(f64, f64); 19] = [
    (3.5, 5.1),
    (6.0, 7.0),
    (9.3, 11.7),
    (14.2, 15.9),
    (20.6, 22.5),
    (25.9, 27.2),
    (29.0, 30.7),
    (32.1, 34.8),
    (38.0, 40.2),
    (44.5, 47.1),
    (49.3, 50.4),
    (52.4, 54.9),
    (57.4, 58.9),
    (61.3, 64.2),
    (66.0, 67.8),
    (70.4, 71.9),
    (74.2, 76.2),
    (78.0, 80.8),
    (85.1, 87.2),
];

fn cues() -> Vec<Cue> {
    LINES
        .into_iter()
        .map(|(start, end)| Cue { start, end })
        .collect()
}

fn audio(offset: f64) -> Vec<AudioSample> {
    (0..960)
        .map(|tick| {
            let time = tick as f64 / 10.;
            let has_dialogue = LINES
                .iter()
                .any(|&(start, end)| (start + offset..end + offset).contains(&time));
            let noise = (tick as f64 * 2.375).sin().abs() * 0.015;
            AudioSample {
                time,
                energy: 0.003 + noise + if has_dialogue { 0.12 } else { 0. },
            }
        })
        .collect()
}

#[test]
fn corrects_subtitles_that_are_early_or_late() {
    for offset in [-3.7, 2.4] {
        let alignment = align(&cues(), &audio(offset)[..720], 0.).unwrap();
        assert!((alignment.offset - offset).abs() <= 0.2, "{alignment:?}");
        assert!(alignment.confidence > 0.8);
    }
}

#[test]
fn rechecks_timing_when_dialogue_has_shifted_further() {
    let alignment = align(&cues(), &audio(5.1)[160..], 2.4).unwrap();
    assert!((alignment.offset - 5.1).abs() <= 0.2);
}

#[test]
fn refuses_to_guess_from_silence_short_samples_or_unrelated_audio() {
    let mut samples = audio(2.4);
    assert!(align(&cues(), &samples[..200], 0.).is_none());
    samples.iter_mut().for_each(|sample| sample.energy = 0.);
    assert!(align(&cues(), &samples, 0.).is_none());
    samples
        .iter_mut()
        .for_each(|sample| sample.energy = (sample.time * 10.).sin().abs() * 0.4);
    assert!(align(&cues(), &samples, 0.).is_none());
}

#[test]
fn repeated_rhythm_is_ambiguous() {
    let patterned: Vec<_> = (0..45)
        .map(|index| Cue {
            start: index as f64 * 2.,
            end: index as f64 * 2. + 1.,
        })
        .collect();
    let samples: Vec<_> = (0..900)
        .map(|index| AudioSample {
            time: index as f64 / 10.,
            energy: if index % 20 < 10 { 0.15 } else { 0. },
        })
        .collect();
    assert!(align(&patterned, &samples, 0.).is_none());
}

#[test]
fn corrects_subtitles_that_get_progressively_earlier() {
    let expected = Timing {
        offset: 0.5,
        rate: 1.04,
    };
    let points = [100., 200., 300.].map(|subtitle| TimingPoint {
        subtitle,
        video: expected.video_time(subtitle),
    });
    let timing = fit_rate(points, Timing::default()).unwrap();
    let track = retime(
        "1\n00:00:01,000 --> 00:00:02,000\nEarly\n\n2\n00:50:00,000 --> 00:50:02,000\nLate\n",
        timing,
    )
    .unwrap();
    assert!(track.contains("00:00:01.540 --> 00:00:02.580"), "{track}");
    assert!(track.contains("00:52:00.500 --> 00:52:02.580"), "{track}");
}

#[test]
fn measures_drift_for_a_24_to_25_frame_rate_mismatch_from_audio() {
    let current = Timing::default();
    let mut start = 0.;
    let mut random = 1977_u32;
    let mut sample = || {
        random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(random) / f64::from(u32::MAX)
    };
    let cues: Vec<_> = (0..170)
        .map(|_| {
            start += 2.4 + sample() * 2.;
            Cue {
                start,
                end: start + 0.65 + sample() * 1.5,
            }
        })
        .collect();
    for rate in [25. / 24., 24. / 25.] {
        let actual = Timing { rate, offset: 0.5 };
        let points = [1200, 2200, 3200].map(|first| {
            let samples: Vec<_> = (first..first + 640)
                .map(|tick| {
                    let time = f64::from(tick) / 10.;
                    let dialogue = cues.iter().any(|cue| {
                        (actual.video_time(cue.start)..actual.video_time(cue.end)).contains(&time)
                    });
                    let noise = (f64::from(tick) * 2.375).sin().abs() * 0.015;
                    AudioSample {
                        time,
                        energy: noise + if dialogue { 0.12 } else { 0.003 },
                    }
                })
                .collect();
            let measurement = align(&cues, &samples, current.offset);
            measure_point(&cues, &samples, current, measurement).unwrap()
        });
        let estimated = fit_rate(points, current).unwrap_or_else(|| panic!("{rate}: {points:?}"));
        for at in [120., 420.] {
            assert!(
                (estimated.video_time(at) - actual.video_time(at)).abs() < 0.35,
                "{estimated:?} at {at} s"
            );
        }
    }
}

#[test]
fn rejects_a_single_cut_or_nearby_measurements_as_a_rate_change() {
    let cut = [(100., 100.), (200., 204.), (300., 304.)];
    let nearby = [(100., 104.), (130., 135.2), (160., 166.4)];
    for samples in [cut, nearby] {
        let points = samples.map(|(subtitle, video)| TimingPoint { subtitle, video });
        assert!(fit_rate(points, Timing::default()).is_none());
    }
}

#[test]
fn saved_shifts_from_older_versions_play_at_normal_subtitle_speed() {
    let timing: Timing = serde_json::from_str(r#"{"offset":1.5,"auto":false}"#).unwrap();
    assert_eq!(timing.video_time(500.), 501.5);
    assert_eq!(timing.subtitle_time(501.5), 500.);
}
