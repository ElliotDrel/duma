//! Microphone capture via `handy-recorder` (WASAPI / CoreAudio), delivered as mono f32 at the requested rate.

use std::time::Instant;

use anyhow::Result;
use handy_recorder::{AudioChunk, Channels, Recorder, RecorderConfig, Sink};
use serde::Serialize;

/// 50 ms chunks: small enough that little audio is pending when the user releases the key.
const CHUNK_MS: u32 = 50;

pub type ChunkHandler = Box<dyn FnMut(&[f32]) + Send>;

pub struct StreamSink {
    on_chunk: ChunkHandler,
    samples: Vec<f32>,
    first_audio: Option<Instant>,
}

impl Sink for StreamSink {
    fn process_chunk(&mut self, chunk: AudioChunk<'_>) {
        let real = &chunk.samples[..chunk.valid_frames * chunk.channels as usize];
        if self.first_audio.is_none() {
            self.first_audio = Some(Instant::now());
        }
        self.samples.extend_from_slice(real);
        (self.on_chunk)(real);
    }
}

pub struct Recording {
    recorder: Recorder<StreamSink>,
    started: Instant,
    pub device_name: String,
    pub open_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct RecordingStats {
    pub device_name: String,
    pub sample_rate: u32,
    pub open_ms: u64,
    /// From start to the first audio chunk.
    pub first_audio_ms: Option<u64>,
    pub duration_ms: u64,
    pub dropped_frames: u64,
    pub end_reason: String,
}

/// Opens the microphone and streams mono audio at `sample_rate` into `on_chunk`.
pub fn start(
    device: Option<String>,
    sample_rate: u32,
    on_chunk: ChunkHandler,
) -> Result<Recording> {
    let started = Instant::now();
    let config = RecorderConfig {
        device,
        sample_rate: Some(sample_rate),
        channels: Channels::MixToMono,
        frames_per_chunk: Some((sample_rate * CHUNK_MS / 1000) as usize),
        take_headset: false,
    };
    let recorder = Recorder::open_with_failure_handler(config, |error| {
        log::error!("microphone failed: {error}");
    })?;
    let device_name = recorder.info().device.name.clone();
    let open_ms = started.elapsed().as_millis() as u64;
    recorder
        .start(StreamSink {
            on_chunk,
            samples: Vec::new(),
            first_audio: None,
        })
        .map_err(|e| anyhow::anyhow!("starting the microphone: {}", e.error))?;
    Ok(Recording {
        recorder,
        started,
        device_name,
        open_ms,
    })
}

impl Recording {
    /// Stops and returns every sample recorded.
    pub fn stop(self) -> Result<(Vec<f32>, RecordingStats)> {
        let sample_rate = self.recorder.info().format.sample_rate;
        let stopped = self.recorder.stop()?;
        let stats = RecordingStats {
            device_name: self.device_name,
            sample_rate,
            open_ms: self.open_ms,
            first_audio_ms: stopped
                .sink
                .first_audio
                .map(|t| t.duration_since(self.started).as_millis() as u64),
            duration_ms: self.started.elapsed().as_millis() as u64,
            dropped_frames: stopped.dropped_frames,
            end_reason: format!("{:?}", stopped.end_reason),
        };
        Ok((stopped.sink.samples, stats))
    }
}

pub fn input_devices() -> Vec<String> {
    handy_recorder::list_input_devices()
        .map(|devices| devices.into_iter().map(|d| d.name).collect())
        .unwrap_or_default()
}
