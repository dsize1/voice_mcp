//! 评测层（P2）：词级差异 + 语流指标 + 音频体检。
//!
//! 设计原则（docs/adr/0001）：这一层只产出**可测量的数字与可解释的问题条目**，
//! 不产出教学话术；话术由 Skill + LLM 根据这些数字生成。

pub mod cases;
pub mod diff;
pub mod flow;
pub mod lrc;
pub mod normalize;
pub mod phonetic;

use anyhow::Result;
use serde::Serialize;

use crate::audio::AudioInfo;
use crate::engine::{Engines, Transcript};

pub use diff::{Hint, Issue, IssueKind, Op, Severity, TextDiff};

/// 评测算法版本：写入 metrics，便于算法升级后回溯与 A/B
pub const SCORER_VERSION: &str = "p2.0-wordlevel";

/// 可信诊断所需的最低信噪比（dB，口径 = 20 ms 帧 RMS 的 p90−p10）。
///
/// 13 dB 来自实测分界：可用录音 14.3–19.5 dB，ASR 开始出碎片的录音 11.0–11.5 dB。
pub const SNR_MIN_DB: f32 = 13.0;

#[derive(Debug, Clone, Serialize)]
pub struct Assessment {
    pub transcript: Transcript,
    pub audio: AudioInfo,
    pub flow: flow::FlowMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_diff: Option<TextDiff>,
    /// 稳健信噪比（20 ms 帧 RMS 的 p90−p10，dB）。低于 `SNR_MIN_DB` 时 ASR/VAD 都不可信。
    pub snr_db: f32,
    /// 音素级 GOP（`with_gop=true` 才有）。只能当排序/趋势，不能当判决。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gop: Option<crate::gop::GopResult>,
    /// 是否为降级结果（无词级时间戳 / 无音素级对齐）
    pub degraded: bool,
    /// 还缺哪些能力（给 Agent 解释为什么结论有限）
    pub limitations: Vec<String>,
    pub scorer_version: String,
}

/// 用原始采样率修正体检结果：`band_limited` 必须按**源**采样率判断，
/// 否则 16 kHz 的分析域会让所有输入都被标成窄带。
pub fn apply_source(mut info: AudioInfo, source_sr: Option<u32>) -> AudioInfo {
    if let Some(sr) = source_sr {
        info.source_sr = sr;
        info.band_limited = sr <= 16_000;
    }
    info
}

/// VAD 静默失败判定：转写出了多个词，却几乎没检测到语音段。
///
/// 这是 D02 的判定谓词，独立成函数以便单测（历史 bug：`articulation_rate=1891 wpm`）。
pub fn vad_suspect(word_count: usize, speech_duration: f32) -> bool {
    word_count >= 2 && speech_duration < 0.2
}

pub struct AssessRequest<'a> {
    /// 16 kHz mono 采样
    pub samples: &'a [f32],
    pub sample_rate: u32,
    pub lang: &'a str,
    pub engine: Option<&'a str>,
    /// 参考文本（跟读/朗读场景）；为空则只做语流分析
    pub reference: Option<&'a str>,
    /// 原始文件/设备的采样率。用于正确判断 `band_limited`
    /// （分析用的采样率恒为 16 kHz，不能拿它判断"源音频是否窄带"）。
    pub source_sr: Option<u32>,
    /// 是否跑音素级 GOP（需要参考文本；首次调用会加载 1.2 GB 模型）
    pub with_gop: bool,
}

/// 一次完成：ASR → 语流指标 → 词级差异。
pub fn assess(engines: &Engines, req: AssessRequest<'_>) -> Result<Assessment> {
    // 必须走 transcribe_long：长音频直接喂离线模型会静默截断
    let transcript = engines.transcribe_long(req.samples, req.sample_rate, req.lang, req.engine)?;
    let segments = engines.vad.segments(req.samples);
    let flow = flow::analyze(req.samples, req.sample_rate, &transcript, &segments);
    let audio = apply_source(crate::audio::info(req.samples, req.sample_rate), req.source_sr);
    let text_diff = req
        .reference
        .map(|r| diff::compare(r, &transcript))
        .filter(|_| !transcript.words.is_empty() || !transcript.text.is_empty());

    let band_limited = audio.band_limited;
    let snr_db = crate::round_to(crate::audio::snr_db(req.samples, req.sample_rate), 1);
    let mut limitations: Vec<String> = Vec::new();
    let mut gop = None;
    if crate::gop::should_run(req.with_gop, req.reference.is_some(), band_limited) && snr_db >= SNR_MIN_DB {
        let text = req.reference.unwrap_or_default();
        match crate::gop::with_engine(|e| crate::gop::score_text(&mut e.model, req.samples, text, &e.g2p)) {
            Ok(g) => gop = Some(g),
            Err(e) => limitations.push(format!("音素级 GOP 不可用：{e}")),
        }
    } else if req.with_gop && req.reference.is_none() {
        limitations.push("未提供参考文本，跳过音素级 GOP".into());
    } else if req.with_gop && band_limited {
        limitations.push("源音频带宽不足（<24 kHz），跳过音素级 GOP".into());
    } else if req.with_gop && snr_db < SNR_MIN_DB {
        limitations.push(format!("信噪比过低（{snr_db} dB < {SNR_MIN_DB} dB），跳过音素级 GOP"));
    }
    let mut degraded = transcript.degraded || transcript.words.is_empty();
    if snr_db < SNR_MIN_DB {
        degraded = true;
        limitations.push(format!(
            "信噪比过低（{snr_db} dB < {SNR_MIN_DB} dB）：VAD 与 ASR 都可能把噪声当语音，             本条的漏读/停顿/音素指标不能作为诊断依据。请靠近麦克风或降低环境噪声后重录"
        ));
    }

    // D02：VAD 静默失败检测——转写出了词，却几乎没有检测到语音段。
    // 此时 speech_duration / articulation_rate 不可信（历史上曾算出 1891 wpm）。
    if vad_suspect(transcript.words.len(), flow.speech_duration) {
        degraded = true;
        limitations.push(
            "VAD 未检测到有效语音段（speech_duration≈0），本次语速/停顿指标不可信，             请检查录音音量或重录"
                .to_string(),
        );
    }

    if transcript.words.is_empty() {
        limitations.push("无词级时间戳：词级定位依赖比例估算，音素级分析不可用".to_string());
    }
    if audio.band_limited {
        limitations.push(
            "源音频采样率 ≤16 kHz（如 Win11 录音机）：6–8 kHz 摩擦音细节有限，/θ/ vs /s/ 一类判断置信度降低"
                .to_string(),
        );
    }
    if let Some(d) = &text_diff {
        if d.timestamp_alignment == diff::TimestampAlignment::Estimated {
            limitations.push(
                "本次识别分词与规范化分词不一致，问题位置是按比例估算的（可能错位 ±1 个词），                 定位时请以词为准、不要死抠秒数"
                    .to_string(),
            );
        }
    }

    if transcript.engine == "sense-voice" {
        limitations.push(
            "SenseVoice 给出的是 CTC token 时间戳（±0.5 s 量级），词界停顿时序只能作参考；             逐词精确定位需要音素级对齐".to_string(),
        );
    }
    match &gop {
        Some(_) => limitations.push(crate::gop::GOP_LIMITATION.to_string()),
        None => limitations
            .push("尚无音素级强制对齐（GOP），因此不能给出音素级发音分数".to_string()),
    }

    Ok(Assessment {
        degraded,
        transcript,
        audio,
        flow,
        text_diff,
        snr_db,
        gop,
        limitations,
        scorer_version: SCORER_VERSION.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_limited_follows_source_rate() {
        let a = crate::audio::info(&vec![0.01f32; 16_000], 16_000);
        assert!(a.band_limited, "16k 分析域默认是窄带");
        // 源是 48k → 不是窄带
        assert!(!apply_source(a.clone(), Some(48_000)).band_limited);
        assert_eq!(apply_source(a.clone(), Some(48_000)).source_sr, 48_000);
        // 源是 16k → 窄带
        assert!(apply_source(a.clone(), Some(16_000)).band_limited);
        // 未提供 → 保持分析域判定
        assert!(apply_source(a, None).band_limited);
    }

    #[test]
    fn vad_suspect_flags_silent_failure() {
        // 7 个词却只有 0.05 s 语音 → 不可信
        assert!(vad_suspect(7, 0.05));
    }

    #[test]
    fn vad_suspect_accepts_normal_and_empty() {
        // 正常：7 个词 2.6 s 语音
        assert!(!vad_suspect(7, 2.6));
        // 空录音（0 个词）：不是 VAD 失败，而是没说话
        assert!(!vad_suspect(0, 0.0));
        // 单个词（可能是 "yes"/"no"）不判失败，避免误报
        assert!(!vad_suspect(1, 0.05));
    }
}
