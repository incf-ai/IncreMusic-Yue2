//! The audio output device (the GUI owns it, design §2.2). The callback only copies
//! already-decoded samples out of `Playback`; decoding happens in the core beforehand.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{Context as _, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, SampleFormat, SizedSample, StreamConfig};
use incremusic_core::playback::Playback;

pub struct AudioOut {
    _stream: cpal::Stream,
    health: Arc<Health>,
}

/// Shared with the stream's error callback, which runs on cpal's audio thread.
#[derive(Default)]
struct Health {
    /// Set once the stream stops producing sound for good (e.g. the sound server went away);
    /// the owner then drops it and builds a new one.
    failed: AtomicBool,
    /// Repaints the UI so it notices `failed` even when nothing else is happening.
    wake: OnceLock<egui::Context>,
}

impl AudioOut {
    /// Whether the stream is dead and should be replaced.
    pub fn failed(&self) -> bool {
        self.health.failed.load(Ordering::Acquire)
    }

    /// Lets the audio thread wake the UI when the stream fails.
    pub fn set_wake(&self, ctx: &egui::Context) {
        let _ = self.health.wake.set(ctx.clone());
    }

    pub fn start(playback: Arc<Playback>) -> Result<AudioOut> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("no default output device"))?;
        let supported = device.default_output_config().context("output config")?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let health = Arc::new(Health::default());
        let h = health.clone();
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, playback, h)?,
            SampleFormat::I16 => build::<i16>(&device, config, playback, h)?,
            SampleFormat::I32 => build::<i32>(&device, config, playback, h)?,
            SampleFormat::U16 => build::<u16>(&device, config, playback, h)?,
            other => return Err(anyhow!("unsupported sample format {other}")),
        };
        stream.play().context("starting audio stream")?;
        Ok(AudioOut {
            _stream: stream,
            health,
        })
    }
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    playback: Arc<Playback>,
    health: Arc<Health>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    let mut buf: Vec<f32> = Vec::new();
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            buf.resize(data.len(), 0.0);
            playback.fill(&mut buf, channels, rate);
            for (o, s) in data.iter_mut().zip(&buf) {
                *o = T::from_sample(*s);
            }
        },
        move |e: cpal::Error| on_error(&health, e),
        None,
    )?;
    Ok(stream)
}

fn on_error(health: &Health, e: cpal::Error) {
    match e.kind() {
        // cpal recovers from these itself; at worst a short glitch
        ErrorKind::Xrun | ErrorKind::RealtimeDenied | ErrorKind::DeviceChanged => {
            if !health.failed.load(Ordering::Acquire) {
                tracing::warn!("audio stream: {e}");
            }
        }
        // anything else (e.g. ALSA's EIO once the sound server is gone) repeats on every
        // period without ever recovering, so report it once and let the owner rebuild
        _ => {
            if !health.failed.swap(true, Ordering::AcqRel) {
                tracing::warn!("audio stream failed, reconnecting: {e}");
                if let Some(ctx) = health.wake.get() {
                    ctx.request_repaint();
                }
            }
        }
    }
}
