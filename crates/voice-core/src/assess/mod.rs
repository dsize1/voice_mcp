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
pub mod pinyin;
pub mod prosody;

use anyhow::Result;
use serde::Serialize;

use crate::audio::AudioInfo;
use crate::engine::{Engines, Transcript};

pub use diff::{Hint, Issue, IssueKind, Op, Severity, TextDiff};

/// 评测算法版本：写入 metrics，便于算法升级后回溯与 A/B
pub const SCORER_VERSION: &str = "p2.2-wordlevel+prosody+pinyin";

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

/// D13 守卫：参考文本是汉字、识别结果里却**一个汉字都没有**。
///
/// 触发场景（2026-11-28 真机踩到）：用户传 `lang="zh"` 但 `path="last"` 指向一条**英文**录音，
/// ASR 被强制按中文解码 → 输出 `X Ste par loved books to Shanghai last week.`。
/// 此时字级 diff 会把每个汉字都报成"被替换成了英文单词"（`likely_different_word`），
/// 拼音层又因为拿不到汉字而整体缺失 —— 看起来像"用户把中文读成了英文"，非常误导。
///
/// 判据刻意保守：只在「参考有汉字 + 转写非空 + 转写无汉字」时成立；
/// 空转写（没说话）不算，避免和"没检测到语音"混为一谈。
pub fn chinese_ref_without_hanzi(reference: Option<&str>, transcript: &str) -> bool {
    let Some(r) = reference else {
        return false;
    };
    let t = transcript.trim();
    pinyin::has_cjk(r) && !t.is_empty() && !pinyin::has_cjk(t)
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
    /// 基线自检是否可用：参考音（真人或 TTS）自己读出来的词必须与参考文本一致，
    /// 且音素层面不能明显读坏。`false` 时**禁止**使用 `gop_baseline` 做 Δ 对比。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gop_baseline_valid: Option<bool>,
    /// 基线来源：`native`（真人原声，优先）或 `tts`（合成，兜底）。
    /// **`tts` 基线不能用来判"语调/重音是否地道"**，只能说"与合成示范的差异"。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gop_baseline_source: Option<String>,
    /// **这个 Δ 能不能跨会话/跨时间比**（D52）。
    ///
    /// 只有「基线存在 + 来源是 `native` + 自检通过」才 `true`。
    /// `false` 时只能当"与合成示范的差异"，**不得**写进趋势/迁移结论；
    /// `None` 表示本次没有基线（没开 `baseline_tts`、也没有 `reference_audio`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_comparable: Option<bool>,
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
    /// D37：对「疑似 ASR 漏听」的 omitted 项跑 whisper 交叉复核（多一次 ASR，约 +0.3–1 s）
    pub cross_check: bool,
    /// **真人参考音**（原生素材切片、真人示范）。给了就不合成 TTS：
    /// 它同时充当 GOP 基线与韵律 F0 参考（`gop_baseline_source="native"`）。
    ///
    /// 为什么不默认用 TTS：TTS 自己会读错（audit：country/thank/tokyo 回读不一致）、
    /// 与 G2P 共用同一套音素变体（同形异音词无法自救，D46），而且合成音的语调不是人标准。
    pub baseline_audio: Option<&'a str>,
    /// 参考音路径（TTS 示范/真人示范），用于 F0 形状 DTW 对比。
    /// 提供时自动开启韵律分析（不必再传 `with_prosody`）。
    pub reference_audio: Option<&'a str>,
}

/// 参考音来源推断（启发式，仅用于标签）：
/// `synthesize` 产出的文件命名是 `*.ref_norm.wav` / `*.ref_slow.wav`；
/// 素材库原生切片是 `*.ref.wav`。推断不出来就写 `unknown`，不要冒充 native。
fn guess_ref_source(path: &str) -> &'static str {
    let p = path.replace('\\', "/");
    if p.contains(".ref_norm") || p.contains(".ref_slow") {
        "tts"
    } else {
        "unknown"
    }
}

/// 用一段“参考音”（真人原声优先，TTS 兜底）给同一段文本打 GOP 基线，并自检。
///
/// 自检两条（任何一条不过 → `valid=Some(false)`，禁止用于 Δ）：
/// 1. **内容**：回读词序列必须与参考文本一致；
/// 2. **音素**：词级 mean-GOP 最小值不能低于 −1.5（实测 TTS 把 `That's`/`export` 读坏时是 −3 ~ −6）。
///
/// 第 2 条是新加的：旧实现只看第 1 条，所以“读出来词对了但音读坏了”的基线会被当成标准。
fn baseline_gop(
    engines: &Engines,
    samples: &[f32],
    sr: u32,
    text: &str,
) -> (Option<crate::gop::GopResult>, Option<bool>, Vec<String>) {
    let mut notes = Vec::new();
    let pcm = if sr == crate::INTERNAL_SR {
        samples.to_vec()
    } else {
        crate::audio::resample::resample(samples, sr, crate::INTERNAL_SR)
    };
    // 1) 内容自检
    let want = normalize::words(text);
    match engines
        .asr
        .transcribe(&pcm, crate::INTERNAL_SR, "en", Some("sense-voice"))
    {
        Ok(t) if normalize::words(&t.text) == want => {}
        Ok(t) => {
            notes.push(format!(
                "基线自检失败：回读为「{}」，与参考文本不一致；该基线不能用于 Δ 对比（D51）",
                t.text
            ));
            return (None, Some(false), notes);
        }
        Err(e) => {
            notes.push(format!("基线自检失败（ASR 不可用）：{e}"));
            return (None, Some(false), notes);
        }
    }
    // 2) 音素自检 + 打分
    match crate::gop::with_engine(|e| crate::gop::score_text(&mut e.model, &pcm, text, &e.g2p)) {
        Ok(b) => {
            let min = b
                .words
                .iter()
                .map(|w| w.gop_mean)
                .fold(f32::INFINITY, f32::min);
            if min.is_finite() && min < -1.5 {
                notes.push(format!(
                    "基线音素自检失败：该参考音里有词被读坏（最差词级 GOP {min:.2} < −1.5）；             这条基线不能当标准，请换真人参考音或换音色"
                ));
                return (Some(b), Some(false), notes);
            }
            (Some(b), Some(true), notes)
        }
        Err(e) => {
            notes.push(format!("基线 GOP 失败：{e}"));
            (None, Some(false), notes)
        }
    }
}

/// D37：对「疑似 ASR 漏听」的 omitted 项跑第二引擎（whisper）**交叉复核**。
///
/// 为什么必须做：短功能词（`is`/`the`/`of`…）被 CTC 解码吞掉很常见，
/// 旧实现只按"它是不是短功能词"猜一个 `LikelyAsrMiss`，既不确认也不定罪。
/// 复核后两种结论都可落地：
/// - whisper 听到了 → 判为 **ASR 漏记**，降级为 low，明确不算漏读；
/// - whisper 也没听到 → 更可能是真漏读，升级为 high，提示人工复听。
///
/// D37 的判定部分（纯函数，便于单测）：第二引擎听到没有 → (severity, hint, 附注)
fn classify_cross_check(heard: bool) -> (Severity, Option<Hint>, &'static str) {
    if heard {
        (
            Severity::Low,
            Some(Hint::LikelyAsrMiss),
            "｜whisper 交叉复核：**听到了该词** → 判为本引擎漏记，不算漏读（D37）",
        )
    } else {
        (
            Severity::High,
            None,
            "｜whisper 交叉复核：**也没听到** → 更可能是真的漏读，请让用户重读该短语确认（D37）",
        )
    }
}

fn cross_check_omissions(
    engines: &Engines,
    samples: &[f32],
    sr: u32,
    lang: &str,
    reference: Option<&str>,
    issues: &mut [Issue],
) {
    let targets: Vec<usize> = issues
        .iter()
        .enumerate()
        .filter(|(_, i)| {
            matches!(i.kind, IssueKind::Omitted) && matches!(i.hint, Some(Hint::LikelyAsrMiss))
        })
        .map(|(k, _)| k)
        .collect();
    if targets.is_empty() {
        return;
    }
    let pcm = if sr == crate::INTERNAL_SR {
        samples.to_vec()
    } else {
        crate::audio::resample::resample(samples, sr, crate::INTERNAL_SR)
    };
    // `auto`/`mixed` 下 whisper 可能按错误语言解码，复核就失去意义：
    // 参考文本是纯 ASCII 时按 en 复核，否则沿用原 lang。
    let cc_lang = if matches!(lang, "auto" | "mixed") {
        if reference.is_some_and(|r| r.is_ascii()) {
            "en"
        } else {
            lang
        }
    } else {
        lang
    };
    let Ok(t) = engines
        .asr
        .transcribe(&pcm, crate::INTERNAL_SR, cc_lang, Some("whisper"))
    else {
        return;
    };
    let heard = normalize::words(&t.text);
    for k in targets {
        let Some(word) = issues[k].reference.clone() else {
            continue;
        };
        let got = heard.contains(&word);
        issues[k].cross_checked = Some(true);
        issues[k].other_engine_heard = Some(got);
        let (severity, hint, note) = classify_cross_check(got);
        issues[k].severity = severity;
        issues[k].hint = hint;
        issues[k].note.push_str(note);
        if std::env::var("VOICE_CROSSCHECK_DEBUG").is_ok() {
            eprintln!(
                "[cross-check] word={word:?} heard={got} cc_text={:?}",
                t.text
            );
        }
    }
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
    let mut text_diff = req
        .reference
        .map(|r| diff::compare(r, &transcript))
        .filter(|_| !transcript.words.is_empty() || !transcript.text.is_empty());
    // D37：功能词 omitted 用第二引擎复核（"ASR 漏记" vs "真漏读"）
    if req.cross_check
        && let Some(td) = text_diff.as_mut()
    {
        cross_check_omissions(
            engines,
            req.samples,
            req.sample_rate,
            req.lang,
            req.reference,
            &mut td.issues,
        );
    }

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
    // D13：中文（参考文本无拉丁字母）不走音素级 GOP。
    // 现有 CTC 模型是英/多语 espeak 音素，中文没标定；硬跑只会把每个汉字丢进 `oov`，
    // 再把空音素串当“没读对”。中文的发音诊断交给拼音层（`text_diff.pinyin`）。
    let chinese_reference = req
        .reference
        .is_some_and(|r| pinyin::has_cjk(r) && !r.chars().any(|c| c.is_ascii_alphabetic()));
    if req.with_gop && chinese_reference {
        limitations.push(
            "参考文本是中文：跳过音素级 GOP（当前 CTC 音素模型面向英文/espeak，中文未标定），发音诊断看 text_diff.pinyin（D13）"
                .into(),
        );
    }
    if crate::gop::should_run(req.with_gop, req.reference.is_some(), band_limited)
        && !chinese_reference
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
    // 对照基线（失败不影响主流程）：
    //   1) 有真人参考音（`baseline_audio`）→ 用它，**不合成 TTS**；
    //   2) 否则 `baseline_tts=true` → 用 TTS 合成，标 `source=tts`。
    let mut gop_baseline = None;
    let mut gop_baseline_valid = None;
    let mut gop_baseline_source: Option<String> = None;
    if gop.is_some()
        && let Some(text) = req.reference
    {
        let native = req
            .baseline_audio
            .map(|p| crate::audio::load_16k(p).map(|d| (d.samples, d.sample_rate, p.to_string())));
        match native {
            Some(Ok((samples, sr, path))) => {
                gop_baseline_source = Some("native".into());
                let (b, valid, mut notes) = baseline_gop(engines, &samples, sr, text);
                limitations.append(&mut notes);
                gop_baseline = b;
                gop_baseline_valid = valid;
                if valid == Some(false) {
                    limitations.push(format!(
                        "真人基线自检失败（{path}）：参考音与文本不一致或音素读坏，该基线不能用于 Δ 对比"
                    ));
                }
            }
            Some(Err(e)) => limitations.push(format!("真人参考音读取失败（回落到 TTS 基线）：{e}")),
            None => {}
        }
        if gop_baseline_valid.is_none() && req.baseline_tts {
            let sid = engines.tts.resolve_sid(
                &std::env::var("VOICE_GOP_BASELINE_VOICE")
                    .unwrap_or_else(|_| crate::engine::DEFAULT_VOICE.to_string()),
            );
            let target = engines.cfg.tts_wpm;
            match engines.tts.synthesize_at_wpm(text, sid, target) {
                Ok(syn) => {
                    gop_baseline_source = Some("tts".into());
                    let (b, valid, mut notes) =
                        baseline_gop(engines, &syn.samples, syn.sample_rate, text);
                    limitations.append(&mut notes);
                    gop_baseline = b;
                    gop_baseline_valid = valid;
                }
                Err(e) => {
                    gop_baseline_valid = Some(false);
                    limitations.push(format!("TTS 基线合成失败：{e}"));
                }
            }
        }
    }

    let mut degraded = transcript.degraded || transcript.words.is_empty();
    // D13：中文参考 + 转写里没有汉字 → 语言不匹配，字级差异不能当发音结论
    if chinese_ref_without_hanzi(req.reference, &transcript.text) {
        degraded = true;
        limitations.push(format!(
            "参考文本是汉字，识别结果却没有任何汉字（「{}」）：`lang` 与录音语言可能不一致（或这条音频本身是英文）。拼音层无法成立，本次的字级差异**不能当发音结论**，请用正确的 lang 重考",
            transcript.text.trim()
        ));
    }
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

    // D13：中文拼音层的口径必须写清（文本层，不是声学层）
    if let Some(p) = text_diff.as_ref().and_then(|d| d.pinyin.as_ref()) {
        limitations.push(format!(
            "中文拼音层（{}）是**文本层**结论：来自 ASR 输出的汉字，不是声学信号。同音不同字不算错（{} 处），声调结论最弱（{} 处声调差异可能只是 ASR 选字）；中文无声学级音素打分（D13）",
            p.table, p.homophones, p.tone_only
        ));
        if p.chars_lookup_missing > 0 {
            limitations.push(format!(
                "有 {} 个汉字不在拼音表里（生僻字/非汉字），未计入拼音层准确率",
                p.chars_lookup_missing
            ));
        }
    }

    if transcript.engine == "sense-voice" {
        limitations.push(
            "SenseVoice 给出的是 CTC token 时间戳（±0.5 s 量级），词界停顿时序只能作参考；             逐词精确定位需要音素级对齐".to_string(),
        );
    }
    match &gop {
        Some(_) => limitations.push(crate::gop::GOP_LIMITATION.to_string()),
        // 中文的 GOP 缺口在上面已经用 D13 那条说清了，不重复刷英文口径
        None if !chinese_reference => {
            limitations.push("尚无音素级强制对齐（GOP），因此不能给出音素级发音分数".to_string())
        }
        None => {}
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
    // 参考音优先用显式 `reference_audio`，否则回落到真人 `baseline_audio`（不合成 TTS）
    let prosody_ref = req.reference_audio.or(req.baseline_audio);
    let prosody_ref_native = match (prosody_ref, req.baseline_audio) {
        (Some(r), Some(b)) => r == b,
        _ => false,
    };
    if req.with_prosody || prosody_ref.is_some() {
        if degraded {
            limitations.push("音频质量未过闸门（degraded），跳过韵律分析".into());
        } else {
            match prosody::analyze(req.samples, req.sample_rate, &transcript.words) {
                Some(mut p) => {
                    if let Some(path) = prosody_ref {
                        match crate::audio::load_16k(path) {
                            Ok(d) => match prosody::analyze(&d.samples, d.sample_rate, &[]) {
                                Some(r) => {
                                    let mut cmp = prosody::compare(&p, &r);
                                    cmp.source = Some(
                                        if prosody_ref_native {
                                            "native"
                                        } else {
                                            guess_ref_source(path)
                                        }
                                        .into(),
                                    );
                                    p.reference = Some(cmp);
                                }
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

    // D52：Δ 是否可比 —— 只有真人基线且自检通过才行
    let delta_comparable = if gop_baseline.is_none() {
        None
    } else {
        Some(gop_baseline_source.as_deref() == Some("native") && gop_baseline_valid == Some(true))
    };
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
        gop_baseline_source,
        delta_comparable,
        gop,
        prosody,
        limitations,
        scorer_version: SCORER_VERSION.to_string(),
    })
}

#[cfg(test)]
mod tests {

    /// D52：Δ 可比性判定（只有真人基线 + 自检通过才算可比）
    #[test]
    fn delta_comparable_requires_native_and_valid() {
        let f = |has_baseline: bool, source: Option<&str>, valid: Option<bool>| -> Option<bool> {
            if !has_baseline {
                None
            } else {
                Some(source == Some("native") && valid == Some(true))
            }
        };
        assert_eq!(f(false, None, None), None);
        assert_eq!(f(true, Some("native"), Some(true)), Some(true));
        assert_eq!(f(true, Some("native"), Some(false)), Some(false));
        assert_eq!(f(true, Some("tts"), Some(true)), Some(false));
        assert_eq!(f(true, Some("tts"), None), Some(false));
        assert_eq!(f(true, None, Some(true)), Some(false));
    }

    /// D37：第二引擎听到 → 降级为 low 且保留 `LikelyAsrMiss`；也没听到 → 升为 high 且清空 hint
    #[test]
    fn cross_check_classifies_both_directions() {
        let (sev, hint, note) = classify_cross_check(true);
        assert_eq!(sev, Severity::Low);
        assert_eq!(hint, Some(Hint::LikelyAsrMiss));
        assert!(note.contains("不算漏读"), "{note}");

        let (sev, hint, note) = classify_cross_check(false);
        assert_eq!(sev, Severity::High);
        assert_eq!(hint, None);
        assert!(note.contains("真的漏读"), "{note}");
    }
    use super::*;

    // ---------------- D52：参考音来源标签 ----------------

    #[test]
    fn ref_source_labels_synthetic_refs() {
        assert_eq!(
            guess_ref_source(r"D:\voice_data\sessions\x\001_tts.ref_norm.wav"),
            "tts"
        );
        assert_eq!(guess_ref_source("sessions/x/001_a.ref_slow.wav"), "tts");
        // 素材库原生切片：*.ref.wav，不能误标成 tts
        assert_eq!(
            guess_ref_source("D:/voice_data/bank/nce2/02/003.ref.wav"),
            "unknown"
        );
        assert_eq!(guess_ref_source("/tmp/whatever.wav"), "unknown");
    }

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

    // ---------------- D13：中文参考 + 无汉字转写 ----------------

    #[test]
    fn chinese_reference_with_english_transcript_is_flagged() {
        // 真机样本：lang=zh 却传了一条英文录音
        assert!(chinese_ref_without_hanzi(
            Some("四是四，十是十"),
            "X Ste par loved books to Shanghai last week."
        ));
        // 正常中文转写 → 不触发
        assert!(!chinese_ref_without_hanzi(Some("四是四"), "四是四"));
        // 英文参考 → 不归这条管
        assert!(!chinese_ref_without_hanzi(Some("I think so"), "I think so"));
        // 没说话（空转写）→ 交给“没检测到语音”那条，不重复定罪
        assert!(!chinese_ref_without_hanzi(Some("四是四"), "   "));
        assert!(!chinese_ref_without_hanzi(None, "hello"));
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
