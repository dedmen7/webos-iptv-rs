#[derive(Clone, Copy, Debug)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct AudioSample {
    pub time: f64,
    pub energy: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Alignment {
    pub offset: f64,
    pub confidence: f64,
}

const SEARCH_STEP: f64 = 0.1;
const SEARCH_RANGE: f64 = 12.;
const MIN_WINDOW: f64 = 36.;

pub fn align(cues: &[Cue], samples: &[AudioSample], current_offset: f64) -> Option<Alignment> {
    let (first, last) = (samples.first()?.time, samples.last()?.time);
    let duration = last - first;
    if duration < MIN_WINDOW || samples.len() as f64 / duration < 6. {
        return None;
    }
    let mut envelope: Vec<_> = samples
        .iter()
        .map(|sample| sample.energy)
        .filter(|energy| energy.is_finite())
        .collect();
    envelope.sort_unstable_by(f64::total_cmp);
    if envelope.len() != samples.len() {
        return None;
    }
    let low = envelope[envelope.len() / 5];
    let high = envelope[envelope.len() * 4 / 5];
    if high - low < 0.006 {
        return None;
    }
    let activity: Vec<_> = samples
        .iter()
        .map(|sample| ((sample.energy - low) / (high - low)).clamp(0., 1.))
        .collect();
    let mid = samples.partition_point(|sample| sample.time < first + duration / 2.);
    let first_part = Window {
        samples: &samples[..mid],
        activity: &activity[..mid],
    };
    let second_part = Window {
        samples: &samples[mid..],
        activity: &activity[mid..],
    };
    let cue_window: Vec<_> = cues
        .iter()
        .copied()
        .filter(|cue| {
            cue.end - cue.start <= 9.
                && cue.end + current_offset + SEARCH_RANGE > first
                && cue.start + current_offset - SEARCH_RANGE < last
        })
        .collect();
    if cue_window.len() < 8 {
        return None;
    }
    let lower = current_offset - SEARCH_RANGE;
    let steps = (SEARCH_RANGE * 2. / SEARCH_STEP).round() as usize;
    let mut evaluated = Vec::with_capacity(steps + 1);
    for step in 0..=steps {
        let offset = lower + step as f64 * SEARCH_STEP;
        let first = score(&cue_window, &first_part, offset);
        let second = score(&cue_window, &second_part, offset);
        if let (Some(first), Some(second)) = (first, second) {
            evaluated.push((offset, first, second));
        }
    }
    let joint = |(_, first, second): &(f64, f64, f64)| (*first + *second) / 2.;
    let best = evaluated
        .iter()
        .max_by(|left, right| joint(left).total_cmp(&joint(right)))?;
    if best.1.min(best.2) < 0.25 || joint(best) < 0.37 {
        return None;
    }
    let runner_up = evaluated
        .iter()
        .filter(|row| (row.0 - best.0).abs() >= 0.95)
        .map(joint)
        .fold(0., f64::max);
    if joint(best) - runner_up < 0.085 {
        return None;
    }
    let half_agrees = |idx: usize| {
        let value = |row: &(f64, f64, f64)| if idx == 0 { row.1 } else { row.2 };
        let other = evaluated
            .iter()
            .filter(|row| (row.0 - best.0).abs() >= 1.25)
            .map(value)
            .fold(0., f64::max);
        value(best) > other + 0.015
    };
    if !half_agrees(0) || !half_agrees(1) {
        return None;
    }
    Some(Alignment {
        offset: (best.0 * 10.).round() / 10.,
        confidence: joint(best),
    })
}

struct Window<'a> {
    samples: &'a [AudioSample],
    activity: &'a [f64],
}

fn score(cues: &[Cue], window: &Window<'_>, offset: f64) -> Option<f64> {
    let mut next = 0;
    let mut count = 0usize;
    let mut inside_sum = 0.;
    let mut total_sum = 0.;
    let mut squared_sum = 0.;
    for (sample, energy) in window.samples.iter().zip(window.activity.iter().copied()) {
        let time = sample.time - offset;
        while next < cues.len() && cues[next].end <= time {
            next += 1;
        }
        let inside = cues.get(next).is_some_and(|cue| cue.start <= time);
        if inside {
            count += 1;
            inside_sum += energy;
        }
        total_sum += energy;
        squared_sum += energy * energy;
    }
    let n = window.samples.len() as f64;
    let fraction = count as f64 / n;
    if !(0.12..=0.83).contains(&fraction) {
        return None;
    }
    let avg = total_sum / n;
    let variance = squared_sum / n - avg * avg;
    if variance < 0.012 {
        return None;
    }
    Some((inside_sum / n - fraction * avg) / (fraction * (1. - fraction) * variance).sqrt())
}
