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
pub mod prosody;

use anyhow::Result;
use serde::Serialize;

use crate::audio::AudioInfo;
use crate::engine::{Engines, Transcript};

pub use diff::{Hint, Issue, IssueKind, Op, Severity, TextDiff};

/// 评测算法版本：写入 metrics，便于算法升级后回溯与 A/B
pub const SCORER_VERSION: &str = "p2.1-wordlevel+prosody";

/// 削波采样占比上限：超过即判"输入过载"，ASR/GOP 都会失真。
pub const CLIP_RATIO_MAX: f32 = 0.001; // 0.1%

/// 绝对电平下限（dBFS，采样峰值）。低于此值 ASR 会开始整句丢失。
///
/// 实测：peak −24.6 dBFS → ASR acc 0.83（可用）；peak −33.5 dBFS → ASR 只输出 "The."（acc 0.00）。
/// SNR 闸门抓不到这种情况（房间安静时 SNR 仍然够），所以必须单独看绝对电平。
pub const LEVEL_MIN_PEAK_DBFS: f32 = -30.0;

/// 可信诊断所需的最低信噪比（dB，口径 = 20 ms 帧 RMS 的 p90−p10）。
///
/// 13 dB 来自实测分界：可用录音 14.3–19.5 dB，ASR 开始出碎片的录音 11.0–11.5 dB。
pub const SNR_MIN_DB: f32 = 13.0;

/// 短语音阀值（秒，口径 = VAD 语音跨度，不是文件时长）。
///
/// D47：单词 drill（1.5–2.5 s）里静音帧太少，p10 会落在“轻声语音”而不是底噪上，
/// SNR 被系统性低估（实测 `word` drill 12.6 dB 直接整条判不可诊断），
/// 但只要峰值够、听感清楚，ASR/GOP 仍然可用。这个区间不再用 SNR 降级。
pub const SHORT_SPEECH_SEC: f32 = 3.0;
/// 短语音的补偿条件：峰值不低于此值时，允许绕过 SNR 闸门（仍需峰值/削波闸门）。
pub const SHORT_SPEECH_PEAK_DBFS: f32 = -25.0;

/// SNR 闸门的三种结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnrVerdict {
    /// 可信：正常判定
    Ok,
    /// SNR 偏低，但是短语音 + 电平够 → 只记 limitation，不降级（D47）
    LowButShort,
    /// SNR 偏低 → 降级（不出发音结论、不跑 GOP）
    Low,
}

/// 纯函数：SNR 是否可以作为降级依据。
pub fn snr_verdict(snr_db: f32, speech_duration: f32, peak_dbfs: f32) -> SnrVerdict {
    if snr_db >= SNR_MIN_DB {
        return SnrVerdict::Ok;
    }
    if speech_duration > 0.0
        && speech_duration < SHORT_SPEECH_SEC
        && peak_dbfs >= SHORT_SPEECH_PEAK_DBFS
    {
        SnrVerdict::LowButShort
    } else {
        SnrVerdict::Low
    }
}

/// 双引擎文本闸门阈值（D49）：一致率低于此值说明 **ASR 自己没听清**，
/// 此时词级 diff / GOP 都不可信，只能出语流与韵律。
///
/// 0.7 的口径是词级 LCS 一致率 `2·|LCS| / (|a| + |b|)`：
/// 干净朗读（真机 TTS 与录音回归）实测 ≥0.9；噪声下两引擎各写一半（≈0.4）才会触发。
pub const TEXT_AGREE_MIN: f32 = 0.7;

/// 词级一致率：`2·|LCS| / (|a|+|b|)`。两边都空 → 1.0（无可比信息，不据此判坏）。
pub fn text_agreement(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    for x in a {
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y {
                prev[j] + 1
            } else {
                prev[j + 1].max(cur[j])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let lcs = prev[b.len()] as f32;
    2.0 * lcs / (a.len() + b.len()) as f32
}

#[derive(Debug, Clone, Serialize)]
pub struct Assessment {
    pub transcript: Transcript,
    pub audio: AudioInfo,
    pub flow: flow::FlowMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_diff: Option<TextDiff>,
    /// 稳健信噪比（20 ms 帧 RMS 的 p90−p10，dB）。低于 `SNR_MIN_DB` 时 ASR/VAD 都不可信。
    pub snr_db: f32,
    /// TTS 标准发音的 GOP 基线（`baseline_tts=true` 才有），用于和用户分数对比。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gop_baseline: Option<crate::gop::GopResult>,
    /// TTS 基线自检：ASR 回读是否与参考文本一致。
    /// `false` 时**禁止**使用 `gop_baseline` 做 Δ 对比。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gop_baseline_valid: Option<bool>,
    /// 音素级 GOP（`with_gop=true` 才有）。只能当排序/趋势，不能当判决。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gop: Option<crate::gop::GopResult>,
    /// 韵律（`with_prosody=true` 才有）：只给数值与位置，不解释好坏。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prosody: Option<prosody::Prosody>,
    /// 双引擎文本一致率（0–1，D49）。低于 `TEXT_AGREE_MIN` 时会 `degraded=true`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_agreement: Option<f32>,
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
    /// 同时用 TTS 合成"标准发音"跑一遍同一套 GOP，作为**对照基线**。
    ///
    /// 为什么要基线：GOP 的绝对值受模型偏好影响（实测 TTS 自己读的 `their` 也只有 −0.22，
    /// 因为该模型对 r 化元音偏保守）。只有"用户分数 − 基线分数"才能回答
    /// "是他读错了，还是模型分辨不出来"。
    pub baseline_tts: bool,
    /// 是否跑韵律分析（F0/句末升降/平调/重音候选）
    pub with_prosody: bool,
    /// 参考音路径（TTS 示范/真人示范），用于 F0 形状 DTW 对比。
    /// 提供时自动开启韵律分析（不必再传 `with_prosody`）。
    pub reference_audio: Option<&'a str>,
}

/// 一次完成：ASR → 语流指标 → 词级差异。
pub fn assess(engines: &Engines, req: AssessRequest<'_>) -> Result<Assessment> {
    // 必须走 transcribe_long：长音频直接喂离线模型会静默截断
    let transcript = engines.transcribe_long(req.samples, req.sample_rate, req.lang, req.engine)?;
    let segments = engines.vad.segments(req.samples);
    let flow = flow::analyze(req.samples, req.sample_rate, &transcript, &segments);
    let audio = apply_source(
        crate::audio::info(req.samples, req.sample_rate),
        req.source_sr,
    );
    let text_diff = req
        .reference
        .map(|r| diff::compare(r, &transcript))
        .filter(|_| !transcript.words.is_empty() || !transcript.text.is_empty());

    let band_limited = audio.band_limited;
    let snr_db = crate::round_to(crate::audio::snr_db(req.samples, req.sample_rate), 1);
    // D47：SNR 闸门只在“可信”时降级；短语音单独走 limitation。
    let snr_state = snr_verdict(snr_db, flow.speech_duration, audio.peak_dbfs);
    let mut limitations: Vec<String> = Vec::new();
    let mut gop = None;
    let clipped_too_much = audio.clipped > 0
        && audio.source_sr > 0
        && audio.duration > 0.0
        && (audio.clipped as f32 / (audio.duration * audio.source_sr as f32)) > CLIP_RATIO_MAX;
    if crate::gop::should_run(req.with_gop, req.reference.is_some(), band_limited)
        && snr_state != SnrVerdict::Low
        && !clipped_too_much
    {
        let text = req.reference.unwrap_or_default();
        match crate::gop::with_engine(|e| {
            crate::gop::score_text(&mut e.model, req.samples, text, &e.g2p)
        }) {
            Ok(g) => gop = Some(g),
            Err(e) => limitations.push(format!("音素级 GOP 不可用：{e}")),
        }
    } else if req.with_gop && req.reference.is_none() {
        limitations.push("未提供参考文本，跳过音素级 GOP".into());
    } else if req.with_gop && band_limited {
        limitations.push("源音频带宽不足（<24 kHz），跳过音素级 GOP".into());
    } else if req.with_gop && snr_state == SnrVerdict::Low {
        limitations.push(format!(
            "信噪比过低（{snr_db} dB < {SNR_MIN_DB} dB），跳过音素级 GOP"
        ));
    }
    // TTS 对照基线：同一段参考文本用标准发音再跑一遍 GOP（失败不影响主流程）
    let mut gop_baseline = None;
    let mut gop_baseline_valid = None;
    if req.baseline_tts
        && gop.is_some()
        && let Some(text) = req.reference
    {
        let sid = engines.tts.resolve_sid(
            &std::env::var("VOICE_GOP_BASELINE_VOICE").unwrap_or_else(|_| "af_heart".into()),
        );
        match engines
            .tts
            .synthesize_at_wpm(text, sid, engines.cfg.tts_wpm)
        {
            Ok(syn) => {
                let pcm = crate::audio::resample::resample(
                    &syn.samples,
                    syn.sample_rate,
                    crate::INTERNAL_SR,
                );
                // 先自检：TTS 自己读出来的词必须与参考文本一致，否则基线不可信
                let want = normalize::words(text);
                match engines
                    .asr
                    .transcribe(&pcm, crate::INTERNAL_SR, "en", Some("sense-voice"))
                {
                    Ok(t) if normalize::words(&t.text) == want => {
                        gop_baseline_valid = Some(true);
                    }
                    Ok(t) => {
                        gop_baseline_valid = Some(false);
                        limitations.push(format!(
                            "TTS 基线自检失败：回读为「{}」，与参考文本不一致；该基线不能用于 Δ 对比（D51）",
                            t.text
                        ));
                    }
                    Err(e) => {
                        gop_baseline_valid = Some(false);
                        limitations.push(format!("TTS 基线自检失败（ASR）：{e}"));
                    }
                }
                if gop_baseline_valid != Some(false) {
                    match crate::gop::with_engine(|e| {
                        crate::gop::score_text(&mut e.model, &pcm, text, &e.g2p)
                    }) {
                        Ok(b) => gop_baseline = Some(b),
                        Err(e) => limitations.push(format!("TTS 基线 GOP 失败：{e}")),
                    }
                }
            }
            Err(e) => {
                gop_baseline_valid = Some(false);
                limitations.push(format!("TTS 基线合成失败：{e}"));
            }
        }
    }

    let mut degraded = transcript.degraded || transcript.words.is_empty();
    // 削波：AGC 把增益顶到满刻度时谐波失真严重，音素后验与 ASR 都不可信
    let clip_ratio = if audio.source_sr > 0 && audio.duration > 0.0 {
        audio.clipped as f32 / (audio.duration * audio.source_sr as f32)
    } else {
        0.0
    };
    if clip_ratio > CLIP_RATIO_MAX {
        degraded = true;
        limitations.push(format!(
            "输入过载（削波）：{:.2}% 的采样被削平（阈值 {:.1}%）。             语音会失真，音素与漏读指标不可信；请把系统输入音量调低后用正常音量重读",
            clip_ratio * 100.0,
            CLIP_RATIO_MAX * 100.0
        ));
    }

    if audio.peak_dbfs < LEVEL_MIN_PEAK_DBFS {
        degraded = true;
        limitations.push(format!(
            "电平过低（峰值 {:.1} dBFS < {:.0} dBFS）：ASR 会整句丢失，本条不能作为诊断依据。             请把系统输入音量调高或靠近麦克风",
            audio.peak_dbfs, LEVEL_MIN_PEAK_DBFS
        ));
    }

    if snr_state == SnrVerdict::Low {
        degraded = true;
        limitations.push(format!(
            "信噪比过低（{snr_db} dB < {SNR_MIN_DB} dB）：VAD 与 ASR 都可能把噪声当语音，             本条的漏读/停顿/音素指标不能作为诊断依据。请靠近麦克风或降低环境噪声后重录"
        ));
    } else if snr_state == SnrVerdict::LowButShort {
        // 不降级，但必须把证据强度写清（SKILL 只读 degraded/limitations）
        limitations.push(format!(
            "短语音（语音段 {:.1} s < {:.0} s）：SNR {snr_db} dB 会被低估，仅供参考；             音素级结论置信度降低（D47）",
            flow.speech_duration, SHORT_SPEECH_SEC
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
    if let Some(d) = &text_diff
        && d.timestamp_alignment == diff::TimestampAlignment::Estimated
    {
        limitations.push(
                "本次识别分词与规范化分词不一致，问题位置是按比例估算的（可能错位 ±1 个词），                 定位时请以词为准、不要死抠秒数"
                    .to_string(),
            );
    }

    if transcript.engine == "sense-voice" {
        limitations.push(
            "SenseVoice 给出的是 CTC token 时间戳（±0.5 s 量级），词界停顿时序只能作参考；             逐词精确定位需要音素级对齐".to_string(),
        );
    }
    match &gop {
        Some(_) => limitations.push(crate::gop::GOP_LIMITATION.to_string()),
        None => {
            limitations.push("尚无音素级强制对齐（GOP），因此不能给出音素级发音分数".to_string())
        }
    }

    // D49：文本可用性闸门。声学过闸（电平/SNR/削波都 OK）不代表 ASR 听清了；
    // 用另一个引擎再转一遍，一致率过低时不允许出词级/音素级结论。
    let mut agree_value = None;
    if req.reference.is_some() && !transcript.words.is_empty() && req.lang != "zh" {
        let alt = if transcript.engine.starts_with("sense") {
            "whisper"
        } else {
            "sense-voice"
        };
        match engines.transcribe_long(req.samples, req.sample_rate, req.lang, Some(alt)) {
            Ok(t2) => {
                let a = normalize::words(&transcript.text);
                let b = normalize::words(&t2.text);
                if !a.is_empty() || !b.is_empty() {
                    let ag = text_agreement(&a, &b);
                    agree_value = Some(crate::round_to(ag, 2));
                    if ag < TEXT_AGREE_MIN {
                        degraded = true;
                        limitations.push(format!(
                            "双引擎文本一致率 {:.0}% < {:.0}%：ASR 自身没听清，词级/音素级指标不可用（D49）。             请重录；本条只保留语流与韵律",
                            ag * 100.0,
                            TEXT_AGREE_MIN * 100.0
                        ));
                    }
                }
            }
            Err(e) => limitations.push(format!("文本闸门：第二引擎不可用（{e}）")),
        }
    }

    // 韵律：与 GOP 相互独立（不需要参考文本），但低 SNR/削波会让 F0 不可信
    let mut prosody = None;
    if req.with_prosody || req.reference_audio.is_some() {
        if degraded {
            limitations.push("音频质量未过闸门（degraded），跳过韵律分析".into());
        } else {
            match prosody::analyze(req.samples, req.sample_rate, &transcript.words) {
                Some(mut p) => {
                    if let Some(path) = req.reference_audio {
                        match crate::audio::load_16k(path) {
                            Ok(d) => match prosody::analyze(&d.samples, d.sample_rate, &[]) {
                                Some(r) => p.reference = Some(prosody::compare(&p, &r)),
                                None => limitations
                                    .push("参考音中找不到稳定浊音帧，无法做 F0 形状对比".into()),
                            },
                            Err(e) => limitations.push(format!("参考音解码失败：{e}")),
                        }
                    }
                    if p.unreliable {
                        limitations.push(format!(
                            "F0 跟踪不稳定（起伏 {:.1} st / 音域 {:.1} st，疑似八度跳变或噪声）：升降方向只能作参考",
                            p.f0_std_st, p.range_st
                        ));
                    }
                    if p.word_count == 0 {
                        limitations.push("无词级时间戳，韵律只给句级指标（无重音候选）".into());
                    }
                    prosody = Some(p);
                }
                None => limitations.push("浊音帧不足（F0 不稳定或录音过短），跳过韵律分析".into()),
            }
        }
    }

    Ok(Assessment {
        degraded,
        text_agreement: agree_value,
        transcript,
        audio,
        flow,
        text_diff,
        snr_db,
        gop_baseline,
        gop_baseline_valid,
        gop,
        prosody,
        limitations,
        scorer_version: SCORER_VERSION.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------- D49：双引擎文本闸门 ----------------

    fn ws(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn agreement_is_one_for_identical_text() {
        let a = ws("i brought it here three days ago");
        assert!((text_agreement(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn agreement_penalizes_split_and_dropped_words() {
        // 单引擎把 thanks 听成 "thank you"、丢一个词：一致率明显下降但没到 0
        let a = ws("thanks for the help");
        let b = ws("thank for the help");
        let ag = text_agreement(&a, &b);
        assert!((0.6..0.9).contains(&ag), "ag={ag}");
    }

    #[test]
    fn agreement_is_low_when_engines_disagree() {
        // 两个引擎各听出完全不同的东西（噪声录音的真实失败模式）
        let a = ws("the man get his umbrella back");
        let b = ws("哦 那个 什么 东西");
        assert!(text_agreement(&a, &b) < TEXT_AGREE_MIN);
        // 一边全空也算不可用
        assert_eq!(text_agreement(&a, &[]), 0.0);
        assert_eq!(text_agreement(&[], &[]), 1.0);
    }

    // ---------------- D47：短语音 SNR 不下降 ----------------

    #[test]
    fn snr_gate_trusts_normal_clips() {
        assert_eq!(snr_verdict(20.0, 1.8, -18.0), SnrVerdict::Ok);
        assert_eq!(snr_verdict(13.0, 0.9, -12.0), SnrVerdict::Ok);
    }

    #[test]
    fn snr_gate_exempts_short_loud_drill() {
        // 实测失败样本：1.8 s 的 word drill、峰值 −18 dBFS、SNR 12.6 dB
        assert_eq!(snr_verdict(12.6, 1.8, -18.0), SnrVerdict::LowButShort);
        // 边界：2.5 s / 峰值 −25 dBFS 也算过
        assert_eq!(snr_verdict(9.0, 2.5, -25.0), SnrVerdict::LowButShort);
    }

    #[test]
    fn snr_gate_still_blocks_long_or_quiet_audio() {
        // 长录音：SNR 低是真问题（实测 11.0–11.5 dB 的 ASR 碎片录音，10–13 s）
        assert_eq!(snr_verdict(11.5, 10.5, -18.0), SnrVerdict::Low);
        // 短但太轻：峰值不够，仍然降级
        assert_eq!(snr_verdict(11.0, 1.8, -33.5), SnrVerdict::Low);
        // 语音跨度为 0（VAD 失败）：不能拿“短”当豁免
        assert_eq!(snr_verdict(9.0, 0.0, -18.0), SnrVerdict::Low);
    }

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
