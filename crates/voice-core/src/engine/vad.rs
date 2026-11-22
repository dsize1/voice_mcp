//! 语音活动检测（Silero VAD v5）：切句、去首尾静音、停顿统计。

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};

use crate::config::Config;

#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    pub t0: f32,
    pub t1: f32,
}

impl Segment {
    pub fn dur(&self) -> f32 {
        (self.t1 - self.t0).max(0.0)
    }
}

pub struct Vad {
    vd: VoiceActivityDetector,
    sample_rate: u32,
}

impl Vad {
    pub fn load(cfg: &Config, sample_rate: u32) -> Result<Self> {
        let model = cfg.silero_vad();
        if !model.is_file() {
            bail!("缺少 Silero VAD 模型: {}", model.display());
        }
        let mc = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(model.to_string_lossy().to_string()),
                threshold: 0.4,
                min_silence_duration: 0.25,
                min_speech_duration: 0.15,
                window_size: 512,
                max_speech_duration: 30.0,
            },
            sample_rate: sample_rate as i32,
            num_threads: 1,
            provider: Some(cfg.provider.clone()),
            ..Default::default()
        };

        let vd = VoiceActivityDetector::create(&mc, 30.0).ok_or_else(|| anyhow!("VAD 创建失败"))?;
        Ok(Self { vd, sample_rate })
    }

    /// 返回语音段列表（秒）。
    ///
    /// 两个必须遵守的约束（均由 `crates/spike/src/bin/vad_probe.rs` 实测得出）：
    ///
    /// 1. **必须按 `window_size`(512) 分块喂入**：一次性喂整段音频时只剩最后一段
    ///    （6.08 s 音频只返回 0.22 s），分块后恢复正常（2.85 s）。
    /// 2. **每次调用前必须 `reset()` 而不是 `clear()`**：`clear()` 只清空已排队的段，
    ///    不清已处理采样数，导致同一 detector 连续调用时时间戳累积漂移
    ///    （第 1 次 8.10–8.96 s、第 2 次 14.18–15.04 s，而音频只有 6.08 s）。
    ///    `reset()` 每次都从 0 开始，连续调用结果稳定为 1.99–2.88 / 3.46–5.38。
    pub fn segments(&self, samples: &[f32]) -> Vec<Segment> {
        self.vd.reset();
        for chunk in samples.chunks(512) {
            self.vd.accept_waveform(chunk);
        }
        self.vd.flush();
        let mut out = Vec::new();
        while !self.vd.is_empty() {
            if let Some(seg) = self.vd.front() {
                let start = seg.start();
                let n = seg.samples().len();
                out.push(Segment {
                    t0: start as f32 / self.sample_rate as f32,
                    t1: (start + n as i64) as f32 / self.sample_rate as f32,
                });
            }
            self.vd.pop();
        }
        out
    }

    /// 去掉首尾静音（保留 `pad` 秒余量），返回裁剪后的采样与偏移。
    pub fn trim_silence(&self, samples: &[f32], pad: f32) -> (Vec<f32>, f32) {
        let segs = self.segments(samples);
        let sr = self.sample_rate as f32;
        match (segs.first(), segs.last()) {
            (Some(a), Some(b)) => {
                let start = ((a.t0 - pad).max(0.0) * sr) as usize;
                let end = (((b.t1 + pad) * sr) as usize).min(samples.len());
                if start >= end {
                    (samples.to_vec(), 0.0)
                } else {
                    (samples[start..end].to_vec(), start as f32 / sr)
                }
            }
            _ => (samples.to_vec(), 0.0),
        }
    }

    /// 停顿统计：返回 (停顿次数, 最长停顿, 停顿总时长, 语音总时长)
    pub fn pause_stats(&self, samples: &[f32]) -> (usize, f32, f32, f32) {
        let segs = self.segments(samples);
        if segs.len() < 2 {
            let speech: f32 = segs.iter().map(|s| s.dur()).sum();
            return (0, 0.0, 0.0, speech);
        }
        let mut gaps = Vec::new();
        for w in segs.windows(2) {
            let gap = w[1].t0 - w[0].t1;
            if gap > 0.15 {
                gaps.push(gap);
            }
        }
        let speech: f32 = segs.iter().map(|s| s.dur()).sum();
        (
            gaps.len(),
            gaps.iter().copied().fold(0.0f32, f32::max),
            gaps.iter().sum(),
            speech,
        )
    }
}
