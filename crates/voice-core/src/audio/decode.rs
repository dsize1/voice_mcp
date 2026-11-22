//! 容器/编解码解码：把任意受支持的音频文件解成 `mono f32`。
//!
//! 覆盖矩阵（ADR-0004）：wav / m4a(AAC-LC in MP4) / aac / mp3 / flac / ogg-vorbis。
//! Win11 录音机实测输出：AAC-LC, 16 kHz, mono, ~39 kbps。

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::conv::IntoSample;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// 基础声学统计，用于「输入质量体检」与评测降级判断。
#[derive(Debug, Clone, Serialize)]
pub struct AudioInfo {
    /// 源文件采样率（Hz）
    pub source_sr: u32,
    /// 源文件声道数
    pub source_channels: u32,
    /// 时长（秒）
    pub duration: f32,
    /// 峰值（dBFS）
    pub peak_dbfs: f32,
    /// 有效值（dBFS）
    pub rms_dbfs: f32,
    /// 削波采样点数
    pub clipped: usize,
    /// 低于 -50 dBFS 的采样占比（近似静音率）
    pub silent_ratio: f32,
    /// 码率（kbps，未知时为 None）
    pub bitrate_kbps: Option<f32>,
    /// 源采样率带宽受限标记：16 kHz 源意味着 8 kHz 以上信息全失，
    /// 摩擦音（/θ/ /s/ /f/）与部分音素对比精度下降 → 评测需降级。
    pub band_limited: bool,
}

#[derive(Debug, Clone)]
pub struct DecodedAudio {
    /// mono f32，已按 `target_sr` 重采样（spike 阶段仅支持原采样率或 16k 直通）
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub info: AudioInfo,
}

/// 解码文件为 mono f32，并按 `target_sr` 重采样。
///
/// 目前只做「原生 16 kHz 直通 + 线性重采样」；P1 会换成 `rubato` 带抗混叠的多相重采样。
pub fn decode_to_mono(path: impl AsRef<Path>, target_sr: u32) -> Result<DecodedAudio> {
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("打不开音频文件: {}", path.display()))?;

    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions {
            enable_gapless: true,
            ..Default::default()
        },
        &MetadataOptions::default(),
    )?;

    let mut format = probed.format;

    let (track_id, source_sr, source_channels) = {
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .context("文件里没有可解码的音轨")?;
        let sr = track.codec_params.sample_rate.unwrap_or(0);
        let ch = track
            .codec_params
            .channels
            .map(|c| c.count() as u32)
            .unwrap_or(1);
        (track.id, sr, ch)
    };

    let track = format
        .tracks()
        .iter()
        .find(|t| t.id == track_id)
        .context("音轨丢失")?;
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut samples: Vec<f32> = Vec::with_capacity(16_000 * 4);
    let mut detected_ch = 0u32;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // 正常结束
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                append_mono(&buf, &mut samples, &mut detected_ch);
            }
            // 单个包解码失败不致命（AAC 常见）
            Err(SymError::DecodeError(_)) => continue,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
    }

    if samples.is_empty() {
        bail!("解码结果为空（文件损坏或编码不受支持）: {}", path.display());
    }

    let ch = if detected_ch > 0 {
        detected_ch
    } else {
        source_channels.max(1)
    };
    let sr = if source_sr > 0 { source_sr } else { target_sr };

    let info = analyze(&samples, sr, ch);

    let (samples, sample_rate) = if sr == target_sr {
        (samples, sr)
    } else {
        (resample_linear(&samples, sr, target_sr), target_sr)
    };

    Ok(DecodedAudio {
        samples,
        sample_rate,
        info,
    })
}

fn append_mono(buf: &AudioBufferRef<'_>, out: &mut Vec<f32>, ch_out: &mut u32) {
    macro_rules! push {
        ($b:expr) => {{
            let b = $b;
            let frames = b.frames();
            let ch = b.spec().channels.count();
            let inv = 1.0f32 / ch as f32;
            if ch > 0 {
                *ch_out = ch as u32;
            }
            for i in 0..frames {
                let mut acc = 0.0f32;
                for c in 0..ch {
                    let v: f32 = b.chan(c)[i].into_sample();
                    acc += v;
                }
                out.push(acc * inv);
            }
        }};
    }

    match buf {
        AudioBufferRef::U8(b) => push!(b),
        AudioBufferRef::U16(b) => push!(b),
        AudioBufferRef::U24(b) => push!(b),
        AudioBufferRef::U32(b) => push!(b),
        AudioBufferRef::S8(b) => push!(b),
        AudioBufferRef::S16(b) => push!(b),
        AudioBufferRef::S24(b) => push!(b),
        AudioBufferRef::S32(b) => push!(b),
        AudioBufferRef::F32(b) => push!(b),
        AudioBufferRef::F64(b) => push!(b),
    }
}

/// 线性插值重采样（占位实现，P1 换 rubato）。
/// 注意：升采样无抗镜像、降采样无抗混叠，仅用于 spike 验证。
pub fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let i0 = src_pos.floor() as usize;
        let frac = (src_pos - i0 as f64) as f32;
        let s0 = input.get(i0).copied().unwrap_or(0.0);
        let s1 = input.get(i0 + 1).copied().unwrap_or(s0);
        out.push(s0 + (s1 - s0) * frac);
    }
    out
}

/// 对外暴露的基础体检（由 audio::info 调用）
pub fn analyze_pub(samples: &[f32], sr: u32, ch: u32) -> AudioInfo {
    analyze(samples, sr, ch)
}

fn analyze(samples: &[f32], sr: u32, ch: u32) -> AudioInfo {
    let mut peak = 0f32;
    let mut sq_sum = 0f64;
    let mut clipped = 0usize;
    let mut silent = 0usize;
    let silence_thresh = 10f32.powf(-50.0 / 20.0);

    for &s in samples {
        let a = s.abs();
        if a > peak {
            peak = a;
        }
        if a >= 0.999 {
            clipped += 1;
        }
        if a < silence_thresh {
            silent += 1;
        }
        sq_sum += (s as f64) * (s as f64);
    }

    let rms = (sq_sum / samples.len() as f64).sqrt() as f32;
    let db = |x: f32| -> f32 { if x <= 0.0 { -120.0 } else { 20.0 * x.log10() } };

    AudioInfo {
        source_sr: sr,
        source_channels: ch,
        duration: samples.len() as f32 / sr as f32,
        peak_dbfs: db(peak),
        rms_dbfs: db(rms),
        clipped,
        silent_ratio: silent as f32 / samples.len() as f32,
        bitrate_kbps: None,
        band_limited: sr <= 16_000,
    }
}
