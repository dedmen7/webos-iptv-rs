use crate::sync::{self, Alignment, AudioSample, Cue};
use js_sys::{Function, Reflect};
use std::collections::VecDeque;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    AnalyserNode, AudioContext, AudioContextState, HtmlVideoElement, MediaStream, MediaStreamTrack,
};

const WINDOW_SECONDS: f64 = 64.;
const RECHECK_SECONDS: f64 = 12.;

pub struct AudioSync {
    context: AudioContext,
    audio: MediaStream,
    analyser: AnalyserNode,
    cues: Vec<Cue>,
    samples: VecDeque<AudioSample>,
    last_check: f64,
    bins: [u8; 512],
}

impl AudioSync {
    pub fn start(video: &HtmlVideoElement, cues: Vec<Cue>) -> Result<Self, JsValue> {
        meaningful(&cues)?;
        let method: Function =
            Reflect::get(video, &JsValue::from_str("captureStream"))?.dyn_into()?;
        let stream: MediaStream = method.call0(video)?.dyn_into()?;
        for video_track in stream
            .get_video_tracks()
            .iter()
            .filter_map(|track| track.dyn_into::<MediaStreamTrack>().ok())
        {
            video_track.stop();
        }
        let tracks = stream.get_audio_tracks();
        if tracks.length() == 0 {
            return Err(JsValue::from_str("Geen audiospoor"));
        }
        let audio = MediaStream::new_with_tracks(&tracks)?;
        let context = AudioContext::new()?;
        let analyser = match connect(&context, &audio) {
            Ok(analyser) => analyser,
            Err(error) => {
                quiet(context.close());
                stop_tracks(&audio);
                return Err(error);
            }
        };
        quiet(context.resume());
        Ok(Self {
            context,
            audio,
            analyser,
            cues,
            samples: VecDeque::with_capacity(650),
            last_check: video.current_time(),
            bins: [0; 512],
        })
    }

    pub fn skip(&mut self, time: f64) {
        self.samples.clear();
        self.last_check = time;
    }

    pub fn sample(&mut self, time: f64, offset: f64) -> Option<Option<Alignment>> {
        if self.context.state() != AudioContextState::Running || !time.is_finite() {
            return None;
        }
        if let Some(previous) = self.samples.back() {
            if time < previous.time || time - previous.time > 1.25 {
                self.skip(time);
            } else if time - previous.time < 0.055 {
                return None;
            }
        }
        self.analyser.get_byte_frequency_data(&mut self.bins);
        let hz_per_bin = f64::from(self.context.sample_rate()) / 1024.;
        let first = (170. / hz_per_bin).ceil() as usize;
        let last = ((3800. / hz_per_bin).floor() as usize).min(self.bins.len() - 1);
        let band = self.bins.get(first..=last)?;
        let energy = f64::from(band.iter().copied().max()?) / 255.;
        self.samples.push_back(AudioSample { time, energy });
        while self
            .samples
            .front()
            .is_some_and(|sample| time - sample.time > WINDOW_SECONDS)
        {
            self.samples.pop_front();
        }
        if time - self.last_check < RECHECK_SECONDS || time - self.samples.front()?.time < 36. {
            return None;
        }
        self.last_check = time;
        Some(sync::align(
            &self.cues,
            self.samples.make_contiguous(),
            offset,
        ))
    }
}

impl Drop for AudioSync {
    fn drop(&mut self) {
        stop_tracks(&self.audio);
        quiet(self.context.close());
    }
}

fn meaningful(cues: &[Cue]) -> Result<(), JsValue> {
    if cues.len() < 8 {
        return Err(JsValue::from_str("Te weinig ondertitelregels"));
    }
    Ok(())
}

fn connect(context: &AudioContext, stream: &MediaStream) -> Result<AnalyserNode, JsValue> {
    let source = context.create_media_stream_source(stream)?;
    let analyser = context.create_analyser()?;
    analyser.set_fft_size(1024);
    analyser.set_smoothing_time_constant(0.2);
    analyser.set_min_decibels(-90.);
    analyser.set_max_decibels(-20.);
    let silent = context.create_gain()?;
    silent.gain().set_value(0.);
    source.connect_with_audio_node(&analyser)?;
    analyser.connect_with_audio_node(&silent)?;
    silent.connect_with_audio_node(&context.destination())?;
    Ok(analyser)
}

fn stop_tracks(stream: &MediaStream) {
    for track in stream
        .get_audio_tracks()
        .iter()
        .filter_map(|track| track.dyn_into::<MediaStreamTrack>().ok())
    {
        track.stop();
    }
}

fn quiet(promise: Result<js_sys::Promise, JsValue>) {
    if let Ok(promise) = promise {
        spawn_local(async move {
            let _ = JsFuture::from(promise).await;
        });
    }
}
