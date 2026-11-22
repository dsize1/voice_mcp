//! WAV 写出（16 kHz mono pcm_s16le 为内部标准形态，另有 f32 变体供调试）。

use std::path::Path;

use anyhow::Result;
use hound::{SampleFormat, WavSpec, WavWriter};

pub fn write_wav_i16(path: impl AsRef<Path>, samples: &[f32], sample_rate: u32) -> Result<()> {
    let path = path.as_ref();
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut w = WavWriter::create(path, spec)?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        w.write_sample(v)?;
    }
    w.finalize()?;
    Ok(())
}

pub fn write_wav_f32(path: impl AsRef<Path>, samples: &[f32], sample_rate: u32) -> Result<()> {
    let path = path.as_ref();
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut w = WavWriter::create(path, spec)?;
    for &s in samples {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(())
}
