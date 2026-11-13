//! Alarm and timer sounds through `cpal` (PipeWire via the ALSA plugin on Fedora).
//!
//! Tones are synthesized, so no sound files ship yet. The output stream is opened
//! when a sound starts and closed when it stops, so the app holds no audio device
//! while it's quiet.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::f32::consts::TAU;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// Two bell notes every 2 s.
    Alarm,
    /// Three short beeps every second.
    Timer,
}

struct Shared {
    pattern: Pattern,
    volume: f32,
    sample: u64,
}

pub struct Sound {
    shared: Arc<Mutex<Shared>>,
    stream: Option<cpal::Stream>,
    playing: Option<Pattern>,
    warned: bool,
}

impl Sound {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared { pattern: Pattern::Alarm, volume: 0.0, sample: 0 })),
            stream: None,
            playing: None,
            warned: false,
        }
    }

    /// Start (or keep playing) `pattern` at `volume` (0..1). Cheap to call every tick.
    pub fn play(&mut self, pattern: Pattern, volume: f32) {
        if let Ok(mut s) = self.shared.lock() {
            if self.playing != Some(pattern) {
                s.pattern = pattern;
                s.sample = 0;
            }
            s.volume = volume.clamp(0.0, 1.0);
        }
        if self.stream.is_none() {
            match self.open() {
                Ok(stream) => self.stream = Some(stream),
                Err(e) => {
                    if !self.warned {
                        tracing::warn!("audio unavailable: {e:#}");
                        self.warned = true;
                    }
                    return;
                }
            }
        }
        self.playing = Some(pattern);
    }

    pub fn stop(&mut self) {
        self.stream = None; // dropping the stream stops it
        self.playing = None;
    }

    fn open(&self) -> anyhow::Result<cpal::Stream> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no default output device"))?;
        let supported = device.default_output_config()?;
        let config = supported.config();
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => self.build::<f32>(&device, config)?,
            cpal::SampleFormat::I16 => self.build::<i16>(&device, config)?,
            cpal::SampleFormat::U16 => self.build::<u16>(&device, config)?,
            cpal::SampleFormat::I32 => self.build::<i32>(&device, config)?,
            other => anyhow::bail!("unsupported sample format {other:?}"),
        };
        stream.play()?;
        Ok(stream)
    }

    fn build<T>(&self, device: &cpal::Device, config: cpal::StreamConfig) -> anyhow::Result<cpal::Stream>
    where
        T: SizedSample + FromSample<f32>,
    {
        let channels = usize::from(config.channels).max(1);
        let rate = config.sample_rate as f32;
        let shared = self.shared.clone();
        let stream = device.build_output_stream(
            config,
            move |data: &mut [T], _| {
                // Never block the audio thread; output silence if the UI holds the lock.
                let Ok(mut s) = shared.try_lock() else {
                    data.iter_mut().for_each(|x| *x = T::from_sample(0.0));
                    return;
                };
                for frame in data.chunks_mut(channels) {
                    let t = s.sample as f32 / rate;
                    let v = synth(s.pattern, t) * s.volume * s.volume; // rough loudness curve
                    s.sample += 1;
                    frame.iter_mut().for_each(|x| *x = T::from_sample(v));
                }
            },
            |e| tracing::warn!("audio stream error: {e}"),
            None,
        )?;
        Ok(stream)
    }
}

/// One sample of the pattern at time `t` seconds.
fn synth(pattern: Pattern, t: f32) -> f32 {
    match pattern {
        Pattern::Alarm => {
            let p = t % 2.0;
            bell(p, 880.0) + if p >= 0.35 { bell(p - 0.35, 660.0) } else { 0.0 }
        }
        Pattern::Timer => {
            let p = t % 1.0;
            [0.0, 0.15, 0.30].iter().map(|start| beep(p - start, 1046.5)).sum()
        }
    }
}

fn bell(dt: f32, f: f32) -> f32 {
    if !(0.0..1.6).contains(&dt) {
        return 0.0;
    }
    let attack = (dt / 0.005).min(1.0);
    let env = attack * (-4.0 * dt).exp();
    0.28 * env * ((TAU * f * dt).sin() + 0.35 * (TAU * 2.0 * f * dt).sin() + 0.12 * (TAU * 3.0 * f * dt).sin())
}

fn beep(dt: f32, f: f32) -> f32 {
    const LEN: f32 = 0.09;
    if !(0.0..LEN).contains(&dt) {
        return 0.0;
    }
    let env = (dt / 0.004).min(1.0) * ((LEN - dt) / 0.01).min(1.0);
    0.25 * env * (TAU * f * dt).sin()
}
