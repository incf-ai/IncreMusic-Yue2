//! The audio output device (the GUI owns it, design §2.2). The callback only copies
//! already-decoded samples out of `Playback`; decoding happens in the core beforehand.

use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use audiocpp_core::playback::Playback;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

pub struct AudioOut {
    _stream: cpal::Stream,
}

impl AudioOut {
    pub fn start(playback: Arc<Playback>) -> Result<AudioOut> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("no default output device"))?;
        let supported = device.default_output_config().context("output config")?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, playback)?,
            SampleFormat::I16 => build::<i16>(&device, config, playback)?,
            SampleFormat::I32 => build::<i32>(&device, config, playback)?,
            SampleFormat::U16 => build::<u16>(&device, config, playback)?,
            other => return Err(anyhow!("unsupported sample format {other}")),
        };
        stream.play().context("starting audio stream")?;
        Ok(AudioOut { _stream: stream })
    }
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    playback: Arc<Playback>,
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
        |e| tracing::warn!("audio stream: {e}"),
        None,
    )?;
    Ok(stream)
}
