//! 引擎集合：启动时一次性加载并常驻（避免"首次调用极慢"）。

pub mod asr;
pub mod tts;
pub mod vad;

pub use asr::{Asr, AsrEngine, AsrKind, Transcript, Word};
pub use tts::{demo_speeds, KOKORO_VOICES, SLOW_SPEED, Synthesized, TtsEngine, VoiceInfo};

use anyhow::Result;

use crate::config::Config;
use crate::engine::vad::Segment;

pub struct Engines {
    pub cfg: Config,
    pub asr: Asr,
    pub tts: TtsEngine,
    pub vad: vad::Vad,
}

impl Engines {
    /// 加载全部引擎。ASR 与 TTS 并行加载，把启动时间从 ~3s 压到 ~2s。
    pub fn load(cfg: Config) -> Result<Self> {
        let cfg_for_tts = cfg.clone();
        let tts_handle = std::thread::spawn(move || TtsEngine::load(&cfg_for_tts));
        let asr = Asr::load(&cfg)?;
        let tts = tts_handle
            .join()
            .map_err(|_| anyhow::anyhow!("TTS 加载线程崩溃"))??;
        let vad = vad::Vad::load(&cfg, crate::INTERNAL_SR)?;
        Ok(Self { cfg, asr, tts, vad })
    }
}

/// 单块最长时长：SenseVoice/whisper 都按 ~30 s 训练，
/// 超过后会**静默截断**（实测 81 s 音频只转出前 57 s 的内容，后半段消失）。
const MAX_CHUNK_SEC: f32 = 20.0;

/// 把 VAD 语音段规划成分块（每块 ≤ `max_sec`，块间不重叠）。
///
/// 单个语音段本身就超长（连续朗读没有 VAD 断点）时按 `max_sec` 硬切。
pub fn plan_chunks(segments: &[Segment], duration: f32, max_sec: f32) -> Vec<(f32, f32)> {
    let mut chunks: Vec<(f32, f32)> = Vec::new();
    for seg in segments {
        let mut start = seg.t0;
        while start < seg.t1 {
            let end = (start + max_sec).min(seg.t1);
            match chunks.last_mut() {
                // 与上一块间隔很小且合并后不超长 → 合并，避免把一句话切成两半
                Some(last) if start - last.1 < 0.35 && (end - last.0) <= max_sec => {
                    last.1 = end;
                }
                _ => chunks.push((start, end)),
            }
            start = end;
        }
    }
    if chunks.is_empty() {
        // 没有 VAD 段（全静音或 VAD 失败）→ 按固定窗口切，保证不丢内容
        let mut t = 0.0;
        while t < duration {
            chunks.push((t, (t + max_sec).min(duration)));
            t += max_sec;
        }
    }
    chunks
}

impl Engines {
    /// 长音频安全转写：按 VAD 分段 → 分块解码 → 时间戳加偏移后拼接。
    ///
    /// 这是**唯一**应该在业务层调用的转写入口；直接调 `asr.transcribe`
    /// 在 >30 s 音频上会静默丢内容。
    pub fn transcribe_long(
        &self,
        samples: &[f32],
        sr: u32,
        lang: &str,
        engine: Option<&str>,
    ) -> Result<crate::engine::Transcript> {
        let duration = samples.len() as f32 / sr as f32;
        if duration <= MAX_CHUNK_SEC {
            return self.asr.transcribe(samples, sr, lang, engine);
        }
        let segs = self.vad.segments(samples);
        let chunks = plan_chunks(&segs, duration, MAX_CHUNK_SEC);

        let mut text = String::new();
        let mut words: Vec<crate::engine::Word> = Vec::new();
        let mut tokens: Vec<crate::engine::Word> = Vec::new();
        let mut degraded = false;
        let mut infer_ms = 0.0f64;
        let mut engine_name = String::new();
        let mut lang_out = String::new();

        for (t0, t1) in chunks {
            let a = (t0 * sr as f32) as usize;
            let b = ((t1 * sr as f32) as usize).min(samples.len());
            if b <= a {
                continue;
            }
            let t = self.asr.transcribe(&samples[a..b], sr, lang, engine)?;
            if engine_name.is_empty() {
                engine_name = t.engine.clone();
                lang_out = t.lang.clone();
            }
            degraded |= t.degraded;
            infer_ms += t.infer_ms;
            if !t.text.trim().is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(t.text.trim());
            }
            for w in t.words {
                words.push(crate::engine::Word { t0: w.t0 + t0, t1: w.t1 + t0, ..w });
            }
            for w in t.tokens {
                tokens.push(crate::engine::Word { t0: w.t0 + t0, t1: w.t1 + t0, ..w });
            }
        }

        Ok(crate::engine::Transcript {
            text,
            lang: if lang_out.is_empty() { lang.to_string() } else { lang_out },
            engine: engine_name,
            duration,
            words,
            tokens,
            degraded,
            infer_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(t0: f32, t1: f32) -> Segment {
        Segment { t0, t1 }
    }

    #[test]
    fn merges_short_gaps_and_splits_long_segments() {
        // 两段 8 s，间隔 0.2 s → 合并成一块
        let c = plan_chunks(&[seg(0.0, 8.0), seg(8.2, 16.0)], 20.0, 20.0);
        assert_eq!(c.len(), 1);
        assert!((c[0].1 - 16.0).abs() < 1e-3);

        // 一段 55 s 连续朗读 → 切成 20/20/15
        let c = plan_chunks(&[seg(0.0, 55.0)], 60.0, 20.0);
        assert_eq!(c.len(), 3);
        assert!((c[0].1 - 20.0).abs() < 1e-3);
        assert!((c[2].1 - 55.0).abs() < 1e-3);
    }

    #[test]
    fn no_segments_falls_back_to_fixed_windows() {
        let c = plan_chunks(&[], 45.0, 20.0);
        assert_eq!(c.len(), 3, "全静音也要覆盖整段，避免静默丢弃");
        assert!((c[0].0 - 0.0).abs() < 1e-6);
        assert!((c[2].1 - 45.0).abs() < 1e-3);
    }

    #[test]
    fn chunks_never_overlap() {
        let segs = vec![seg(0.0, 30.0), seg(30.5, 61.0), seg(61.2, 80.0)];
        let c = plan_chunks(&segs, 81.0, 20.0);
        for w in c.windows(2) {
            assert!(w[1].0 >= w[0].1 - 1e-3, "块之间不能重叠");
        }
        assert!((c.last().unwrap().1 - 80.0).abs() < 1e-3, "末块必须覆盖到最后一个语音段");
    }
}
