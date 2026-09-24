//! Minimal RIFF/WAVE header parsing (the server's `audio` field is a full WAV file).

use std::io::Read;
use std::path::Path;

use crate::error::{Error, IoContext, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavInfo {
    /// 1 = PCM, 3 = IEEE float, 0xFFFE = extensible.
    pub format_tag: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    /// Byte length of the `data` chunk as declared.
    pub data_len: u32,
    pub data_offset: u64,
}

impl WavInfo {
    pub fn is_pcm_s16(&self) -> bool {
        (self.format_tag == 1 || self.format_tag == 0xFFFE) && self.bits_per_sample == 16
    }

    pub fn duration_ms(&self) -> u64 {
        let frame = self.channels as u64 * (self.bits_per_sample as u64 / 8);
        if frame == 0 || self.sample_rate == 0 {
            return 0;
        }
        self.data_len as u64 / frame * 1000 / self.sample_rate as u64
    }
}

pub fn has_wav_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

/// Parses the header from the start of a WAV file (needs the bytes up to the `data` chunk).
pub fn parse_header(bytes: &[u8]) -> Result<WavInfo> {
    let bad = |m: &str| Error::Decode(format!("WAV: {m}"));
    if !has_wav_magic(bytes) {
        return Err(bad("missing RIFF/WAVE magic"));
    }
    let mut pos = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap());
        let body = pos + 8;
        if id == b"fmt " {
            if body + 16 > bytes.len() {
                return Err(bad("truncated fmt chunk"));
            }
            let b = &bytes[body..];
            fmt = Some((
                u16::from_le_bytes([b[0], b[1]]),
                u16::from_le_bytes([b[2], b[3]]),
                u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
                u16::from_le_bytes([b[14], b[15]]),
            ));
        } else if id == b"data" {
            let (format_tag, channels, sample_rate, bits_per_sample) =
                fmt.ok_or_else(|| bad("data chunk before fmt chunk"))?;
            return Ok(WavInfo {
                format_tag,
                channels,
                sample_rate,
                bits_per_sample,
                data_len: len,
                data_offset: body as u64,
            });
        }
        pos = body + len as usize + (len as usize & 1);
    }
    Err(bad("no data chunk in header"))
}

pub fn read_header(path: &Path) -> Result<WavInfo> {
    let mut f = std::fs::File::open(path).at(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut n = 0;
    while n < buf.len() {
        let r = f.read(&mut buf[n..]).at(path)?;
        if r == 0 {
            break;
        }
        n += r;
    }
    parse_header(&buf[..n])
}

/// Writes a PCM s16 WAV (used by tests and the mock server).
pub fn encode_pcm16(samples: &[i16], channels: u16, sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&sample_rate.to_le_bytes());
    v.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
    v.extend_from_slice(&(channels * 2).to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture() {
        let bytes = include_bytes!("../../../tests/fixtures/one_second.wav");
        let info = parse_header(bytes).unwrap();
        assert_eq!(info.channels, 2);
        assert_eq!(info.sample_rate, 48000);
        assert_eq!(info.bits_per_sample, 16);
        assert!(info.is_pcm_s16());
        assert_eq!(info.duration_ms(), 1000);
    }

    #[test]
    fn skips_unknown_chunks() {
        let mut w = encode_pcm16(&[1, 2, 3, 4], 1, 8000);
        // insert a LIST chunk (odd length → padded) between fmt and data
        let list = [b"LIST".as_slice(), &3u32.to_le_bytes(), b"abc\0"].concat();
        w.splice(36..36, list);
        let info = parse_header(&w).unwrap();
        assert_eq!(info.data_len, 8);
        assert_eq!(info.sample_rate, 8000);
    }

    #[test]
    fn rejects_non_wav() {
        assert!(parse_header(b"ID3\x04 not a wav").is_err());
    }
}
