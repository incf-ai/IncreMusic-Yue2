//! Decodes songs with `symphonia` into a seekable sample source (design §2.2). The GUI owns
//! the output device and pulls samples with [`Playback::fill`].

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::error::{Error, IoContext, Result};
use crate::library::SongId;

#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    /// Interleaved f32 samples.
    pub samples: Vec<f32>,
    pub channels: usize,
    pub sample_rate: u32,
}

impl Decoded {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.frames() as f64 / self.sample_rate.max(1) as f64)
    }
}

pub fn decode_file(path: &Path) -> Result<Decoded> {
    let file = std::fs::File::open(path).at(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = path.extension() {
        hint.with_extension(&e.to_string_lossy());
    }
    let dec_err = |e: SymError| Error::Decode(format!("{}: {e}", path.display()));
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(dec_err)?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| Error::Decode("no audio track".into()))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|c| c.audio())
        .ok_or_else(|| Error::Decode("no audio codec parameters".into()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(dec_err)?;
    let track_id = track.id;
    let mut samples: Vec<f32> = Vec::new();
    let mut chunk: Vec<f32> = Vec::new();
    let mut channels = 0usize;
    let mut rate = 0u32;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(dec_err(e)),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                channels = buf.spec().channels().count();
                rate = buf.spec().rate();
                chunk.resize(buf.samples_interleaved(), 0.0);
                buf.copy_to_slice_interleaved(&mut chunk);
                samples.extend_from_slice(&chunk);
            }
            Err(SymError::DecodeError(e)) => tracing::debug!("decode error (skipped): {e}"),
            Err(e) => return Err(dec_err(e)),
        }
    }
    if channels == 0 {
        return Err(Error::Decode(format!(
            "{}: no audio decoded",
            path.display()
        )));
    }
    Ok(Decoded {
        samples,
        channels,
        sample_rate: rate,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayState {
    Stopped,
    Loading,
    Playing,
    Paused,
}

struct Inner {
    song: Option<SongId>,
    audio: Option<Arc<Decoded>>,
    /// Position in source frames (fractional for resampling).
    pos: f64,
    state: PlayState,
    volume: f32,
}

/// Shared between the core (commands) and the GUI's audio callback.
pub struct Playback {
    inner: Mutex<Inner>,
}

impl Default for Playback {
    fn default() -> Self {
        Playback {
            inner: Mutex::new(Inner {
                song: None,
                audio: None,
                pos: 0.0,
                state: PlayState::Stopped,
                volume: 1.0,
            }),
        }
    }
}

impl Playback {
    pub fn new() -> Arc<Playback> {
        Arc::new(Playback::default())
    }

    pub fn set_loading(&self, song: SongId) {
        let mut i = self.inner.lock();
        i.song = Some(song);
        i.audio = None;
        i.pos = 0.0;
        i.state = PlayState::Loading;
    }

    /// Installs decoded audio and starts playing (unless another song was requested since).
    pub fn load(&self, song: SongId, audio: Arc<Decoded>, play: bool) -> bool {
        let mut i = self.inner.lock();
        if i.song.as_ref().is_some_and(|s| s != &song) {
            return false;
        }
        i.song = Some(song);
        i.audio = Some(audio);
        i.pos = 0.0;
        i.state = if play {
            PlayState::Playing
        } else {
            PlayState::Paused
        };
        true
    }

    pub fn song(&self) -> Option<SongId> {
        self.inner.lock().song.clone()
    }

    pub fn state(&self) -> PlayState {
        self.inner.lock().state
    }

    pub fn play(&self) {
        let mut i = self.inner.lock();
        if let Some(a) = &i.audio {
            if i.pos as usize >= a.frames() {
                i.pos = 0.0;
            }
            i.state = PlayState::Playing;
        }
    }

    pub fn pause(&self) {
        let mut i = self.inner.lock();
        if i.state == PlayState::Playing {
            i.state = PlayState::Paused;
        }
    }

    pub fn toggle(&self) {
        if self.state() == PlayState::Playing {
            self.pause()
        } else {
            self.play()
        }
    }

    pub fn stop(&self) {
        let mut i = self.inner.lock();
        i.state = PlayState::Stopped;
        i.pos = 0.0;
        i.audio = None;
        i.song = None;
    }

    pub fn seek(&self, to: Duration) {
        let mut i = self.inner.lock();
        if let Some(a) = &i.audio {
            let f = to.as_secs_f64() * a.sample_rate as f64;
            i.pos = f.clamp(0.0, a.frames() as f64);
        }
    }

    /// Seek relative to the current position (±5 s / ±30 s in review mode).
    pub fn seek_by(&self, delta_secs: f64) {
        let p = self.position().as_secs_f64();
        self.seek(Duration::from_secs_f64((p + delta_secs).max(0.0)));
    }

    pub fn position(&self) -> Duration {
        let i = self.inner.lock();
        match &i.audio {
            Some(a) => Duration::from_secs_f64(i.pos / a.sample_rate.max(1) as f64),
            None => Duration::ZERO,
        }
    }

    pub fn duration(&self) -> Duration {
        self.inner
            .lock()
            .audio
            .as_ref()
            .map(|a| a.duration())
            .unwrap_or_default()
    }

    pub fn set_volume(&self, v: f32) {
        self.inner.lock().volume = v.clamp(0.0, 2.0);
    }

    /// Fills an interleaved output buffer, resampling linearly to `out_rate` and mapping
    /// channels. Writes silence when not playing.
    pub fn fill(&self, out: &mut [f32], out_channels: usize, out_rate: u32) {
        let mut i = self.inner.lock();
        let (Some(a), PlayState::Playing) = (i.audio.clone(), i.state) else {
            out.fill(0.0);
            return;
        };
        let oc = out_channels.max(1);
        let sc = a.channels.max(1);
        let step = a.sample_rate as f64 / out_rate.max(1) as f64;
        let frames = a.frames();
        let mut pos = i.pos;
        for frame in out.chunks_mut(oc) {
            let f0 = pos as usize;
            if f0 >= frames {
                frame.fill(0.0);
                continue;
            }
            let f1 = (f0 + 1).min(frames - 1);
            let t = (pos - f0 as f64) as f32;
            for (c, o) in frame.iter_mut().enumerate() {
                let sch = if sc == 1 { 0 } else { c % sc };
                let s0 = a.samples[f0 * sc + sch];
                let s1 = a.samples[f1 * sc + sch];
                *o = (s0 + (s1 - s0) * t) * i.volume;
            }
            pos += step;
        }
        i.pos = pos.min(frames as f64);
        if i.pos as usize >= frames {
            i.state = PlayState::Paused;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAV: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/one_second.wav"
    );
    const MP4: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/tiny.mp4");

    #[test]
    fn decodes_wav_and_mp4() {
        let w = decode_file(Path::new(WAV)).unwrap();
        assert_eq!((w.channels, w.sample_rate, w.frames()), (2, 48000, 48000));
        let m = decode_file(Path::new(MP4)).unwrap();
        assert_eq!((m.channels, m.sample_rate), (2, 48000));
        assert!(m.frames() > 10_000, "{}", m.frames());
    }

    fn pb_with(frames: usize, rate: u32) -> Playback {
        let p = Playback::default();
        let samples = (0..frames).flat_map(|f| [f as f32, -(f as f32)]).collect();
        p.load(
            SongId("s".into()),
            Arc::new(Decoded {
                samples,
                channels: 2,
                sample_rate: rate,
            }),
            true,
        );
        p
    }

    #[test]
    fn fill_advances_and_seeks() {
        let p = pb_with(100, 10);
        let mut out = vec![0.0; 8];
        p.fill(&mut out, 2, 10);
        assert_eq!(out, vec![0.0, -0.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
        assert_eq!(p.position(), Duration::from_millis(400));
        p.seek(Duration::from_secs(5));
        p.fill(&mut out[..2], 2, 10);
        assert_eq!(out[0], 50.0);
        p.seek_by(-100.0);
        assert_eq!(p.position(), Duration::ZERO);
    }

    #[test]
    fn resamples_and_maps_channels() {
        let p = pb_with(100, 10);
        let mut out = vec![0.0; 3];
        p.fill(&mut out, 1, 20); // half-speed steps, mono out takes channel 0
        assert_eq!(out, vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn pauses_at_end_and_silence_when_paused() {
        let p = pb_with(2, 10);
        let mut out = vec![1.0; 8];
        p.fill(&mut out, 2, 10);
        assert_eq!(&out[4..], &[0.0; 4]);
        assert_eq!(p.state(), PlayState::Paused);
        out.fill(1.0);
        p.fill(&mut out, 2, 10);
        assert!(out.iter().all(|s| *s == 0.0));
        p.play();
        assert_eq!(p.position(), Duration::ZERO, "play at end restarts");
    }

    #[test]
    fn stale_load_is_ignored() {
        let p = Playback::default();
        p.set_loading(SongId("b".into()));
        let a = Arc::new(Decoded {
            samples: vec![0.0; 4],
            channels: 2,
            sample_rate: 10,
        });
        assert!(!p.load(SongId("a".into()), a.clone(), true));
        assert!(p.load(SongId("b".into()), a, true));
    }
}
