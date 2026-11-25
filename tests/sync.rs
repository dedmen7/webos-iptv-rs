use webos_iptv_rs::sync::{AudioSample, Cue, align};

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
