//! 词级/字级差异对齐：参考文本 vs 识别结果。
//!
//! Levenshtein DP 带回溯，得到 Equal/Substitute/Delete/Insert，
//! 再翻译成「教学上可解释的问题」：
//!
//! | 现象 | 含义 |
//! |---|---|
//! | Substitute + 音形接近 | 疑似发音偏差（如 think→thing, thin→sin） |
//! | Substitute + 音形差远 | 说成了别的词（词汇/记忆），不是发音 |
//! | Delete（参考有、识别无） | 漏读 / 吞音（词尾辅音、弱读过度、语速过快） |
//! | Insert（识别有、参考无） | 多读 / 插入音 / 口头禅 |
//!
//! ## D03：时间戳对齐
//!
//! 规范化会**改变词数**（`I'm` → `i am`、`3` → `three`），所以不能把 N 个规范化词按比例
//! 映射到 M 个 ASR 词时间戳——那会在带缩写的句子上静默错位，让教练指错词。
//!
//! 现在每个规范化词都带"来源 token 序号"（`normalize::Unit.token`）：
//! - 原始 token 数与 ASR 词数一致 → **按 token 序号精确取时间**（同一 token 展开出的多个词，
//!   在它自己的时间窗内均分）→ `timestamp_alignment = "exact"`
//! - 不一致 → 才退回比例映射 → `"estimated"`
//! - 完全没有时间戳 → `"missing"`

use serde::Serialize;

use super::normalize::{self, Unit};
use crate::engine::Transcript;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Equal,
    Substitute,
    Delete,
    Insert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    /// 替换：参考词位置被识别成另一个词
    Substitution,
    /// 漏读 / 吞音
    Omitted,
    /// 多读 / 插入
    Extra,
}

/// 替换项的原因推断（三态，承认不确定）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Hint {
    /// 音形接近 → 大概率是发音偏差
    LikelyPronunciation,
    /// 音形与词形都差得远 → 说成了别的词
    LikelyDifferentWord,
    /// 从文本无法判断（如 third→straight）
    Unclear,
    /// 疑似 ASR 漏听：短功能词（is / a / the / of…）被 CTC 解码吞掉是常见现象，
    /// 实测同一条录音 whisper 能听到 "is" 而 SenseVoice 漏掉 → 不能直接判用户漏读
    LikelyAsrMiss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    High,
    Medium,
    Low,
}

/// 时间戳对齐质量（D03）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestampAlignment {
    /// 按 token 序号精确对齐
    Exact,
    /// 词数不匹配，退回比例估算（可能错位）
    Estimated,
    /// 完全没有时间戳
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pair {
    pub op: Op,
    /// 该参考词在规范化参考序列中的下标（用于按行/按段聚合）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_index: Option<usize>,
    pub reference: Option<String>,
    pub said: Option<String>,
    /// 归一化后词形相似度 0..1（仅 Substitute 有意义）
    pub similarity: f32,
    /// 在录音中的时间（Delete 用相邻词插值）
    pub t0: f32,
    pub t1: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    pub kind: IssueKind,
    /// 规范化后的参考词（`I'm` → `i` / `am`）
    pub reference: Option<String>,
    /// 参考文本里的**原始写法**（`I'm`），反馈里优先用它
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_surface: Option<String>,
    pub said: Option<String>,
    pub t0: f32,
    pub t1: f32,
    pub severity: Severity,
    /// 给人/LLM 看的解释
    pub note: String,
    /// 词形（拼写）相似度 0..1
    pub similarity: f32,
    /// 音形相似度 0..1（比拼写更能反映"听起来像不像"）
    pub phonetic_similarity: f32,
    /// 原因推断：三态，`unclear` 时必须由上层追问或用音素级对齐确认
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<Hint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TextDiff {
    pub reference: String,
    pub reference_words: usize,
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
    /// (参考词数 - 错误数) / 参考词数，裁剪到 0..1
    pub accuracy: f32,
    pub issues: Vec<Issue>,
    /// 完整对齐序列（含 Equal）
    pub alignment: Vec<Pair>,
    /// 时间戳对齐质量
    pub timestamp_alignment: TimestampAlignment,
    /// 兼容字段：只要不是 `exact` 就为 true
    pub timestamps_estimated: bool,
}

/// 比较参考文本与转写结果。
pub fn compare(reference: &str, transcript: &Transcript) -> TextDiff {
    let ref_surfaces = normalize::surfaces(reference);
    let ref_units = normalize::units(reference);
    let hyp_units = normalize::units(&transcript.text);
    let hyp_token_count = normalize::surfaces(&transcript.text).len();

    let ref_words: Vec<String> = ref_units.iter().map(|u| u.text.clone()).collect();
    // ASR 词时间戳（优先 words，其次 tokens）
    let src: Vec<(f32, f32)> = if !transcript.words.is_empty() {
        transcript.words.iter().map(|w| (w.t0, w.t1)).collect()
    } else {
        transcript
            .tokens
            .iter()
            .filter(|t| t.t0.is_finite())
            .map(|w| (w.t0, w.t1))
            .collect()
    };
    let (mut hyp_times, alignment_quality) = times_for(&hyp_units, &src, hyp_token_count);

    // 合成词修复：ASR 常把 "lunchtime" 拆成 "lunch time"，
    // 不合并会产出「sub 取代 + ins 多读」两条假问题。
    let ref_set: std::collections::HashSet<String> =
        ref_units.iter().map(|u| u.text.clone()).collect();
    let mut hyp_units = hyp_units;
    merge_split_compounds(&mut hyp_units, &mut hyp_times, &ref_set);
    let hyp_words: Vec<String> = hyp_units.iter().map(|u| u.text.clone()).collect();

    let dp = align(&ref_words, &hyp_words);
    let mut alignment = Vec::with_capacity(dp.len());
    let mut issues = Vec::new();
    let (mut s, mut d, mut i) = (0usize, 0usize, 0usize);

    let mut hyp_cursor = 0usize; // 指向 hyp_words / hyp_times
    let mut ref_cursor = 0usize; // 指向 ref_units
    let mut last_reported_ref_token: Option<usize> = None;
    let mut last_t1 = 0.0f32;

    for step in &dp {
        // 参考侧来源 token（用于显示 "I'm" 而不是 "i"+"am"）
        let ref_surface = ref_units
            .get(ref_cursor)
            .and_then(|u| ref_surfaces.get(u.token))
            .cloned();
        let ref_token = ref_units.get(ref_cursor).map(|u| u.token);

        let (t0, t1) = match (&step.reference, &step.said) {
            (_, Some(_)) => {
                let t = hyp_times
                    .get(hyp_cursor)
                    .copied()
                    .unwrap_or((last_t1, last_t1 + 0.2));
                hyp_cursor += 1;
                if t.0.is_finite() { t } else { (last_t1, last_t1 + 0.2) }
            }
            (Some(_), None) => {
                let prev = hyp_cursor
                    .checked_sub(1)
                    .and_then(|k| hyp_times.get(k))
                    .map(|t| t.1)
                    .unwrap_or(0.0);
                let next = hyp_times.get(hyp_cursor).map(|t| t.0).unwrap_or(prev + 0.2);
                (prev, next.max(prev))
            }
            (None, None) => (last_t1, last_t1),
        };
        last_t1 = t1.max(last_t1);

        let pair = match (&step.reference, &step.said) {
            (Some(r), Some(h)) if r == h => {
                let ref_idx = ref_cursor;
                ref_cursor += 1;
                Pair {
                op: Op::Equal,
                ref_index: Some(ref_idx),
                reference: Some(r.clone()),
                said: Some(h.clone()),
                similarity: 1.0,
                t0,
                t1,
            }
            }
            (Some(r), Some(h)) => {
                s += 1;
                let sim = similarity(r, h);
                let psim = phonetic::similarity(r, h);
                let hint = if psim >= 0.6 || sim >= 0.6 {
                    Hint::LikelyPronunciation
                } else if psim <= 0.35 && sim <= 0.45 {
                    Hint::LikelyDifferentWord
                } else {
                    Hint::Unclear
                };
                if !already_reported(ref_token, &mut last_reported_ref_token) {
                    let shown = ref_surface.clone().unwrap_or_else(|| r.clone());
                    let note = match hint {
                        Hint::LikelyPronunciation => format!(
                            "参考词 \"{shown}\" → 识别为 \"{h}\"（音形相似 {:.0}%）：两个词听起来接近，\
                             大概率是发音偏差；具体是哪个音素错，需要音素级对齐确认",
                            psim * 100.0
                        ),
                        Hint::LikelyDifferentWord => format!(
                            "参考词 \"{shown}\" → 识别为 \"{h}\"（音形相似 {:.0}%）：听起来差得远，\
                             更可能是说成了别的词（词汇/记忆），不一定是发音问题",
                            psim * 100.0
                        ),
                        Hint::Unclear => format!(
                            "参考词 \"{shown}\" → 识别为 \"{h}\"（音形相似 {:.0}%、拼写相似 {:.0}%）：\
                             从文本无法判断是发音偏差还是 ASR 误听。可让用户重录该词，或对照示范音确认",
                            psim * 100.0,
                            sim * 100.0
                        ),
                        // LikelyAsrMiss 只用于 omitted 分支；这里理论上不会出现
                        Hint::LikelyAsrMiss => format!(
                            "参考词 \"{shown}\" → 识别为 \"{h}\"：疑似 ASR 误听，请复核"
                        ),
                    };
                    issues.push(Issue {
                        kind: IssueKind::Substitution,
                        reference: Some(r.clone()),
                        reference_surface: ref_surface.clone(),
                        said: Some(h.clone()),
                        t0,
                        t1,
                        severity: Severity::High,
                        note,
                        similarity: sim,
                        phonetic_similarity: psim,
                        hint: Some(hint),
                    });
                }
                let ref_idx = ref_cursor;
                ref_cursor += 1;
                Pair {
                    op: Op::Substitute,
                    ref_index: Some(ref_idx),
                    reference: Some(r.clone()),
                    said: Some(h.clone()),
                    similarity: sim,
                    t0,
                    t1,
                }
            }
            (Some(r), None) => {
                d += 1;
                if !already_reported(ref_token, &mut last_reported_ref_token) {
                    let shown = ref_surface.clone().unwrap_or_else(|| r.clone());
                    // 短功能词漏读的可信度低：CTC 解码吞掉 1–4 字母的弱读词很常见
                    let (hint, severity, note) = if is_short_function_word(r) {
                        (
                            Some(Hint::LikelyAsrMiss),
                            Severity::Medium,
                            format!(
                                "参考词 \"{shown}\" 没有被识别到。它是短功能词（通常弱读），                                 ASR 漏听这种情况很常见——**不能直接判定为漏读**。                                 请让用户只重读该短语确认，或用 engine=whisper 复核一次"
                            ),
                        )
                    } else {
                        (
                            None,
                            Severity::High,
                            format!(
                                "参考词 \"{shown}\" 没有被识别到：可能是漏读、吞音（词尾辅音/弱读过度），                                 也可能被说得太快连在一起"
                            ),
                        )
                    };
                    issues.push(Issue {
                        kind: IssueKind::Omitted,
                        reference: Some(r.clone()),
                        reference_surface: ref_surface.clone(),
                        said: None,
                        t0,
                        t1,
                        severity,
                        note,
                        similarity: 0.0,
                        phonetic_similarity: 0.0,
                        hint,
                    });
                }
                let ref_idx = ref_cursor;
                ref_cursor += 1;
                Pair {
                    op: Op::Delete,
                    ref_index: Some(ref_idx),
                    reference: Some(r.clone()),
                    said: None,
                    similarity: 0.0,
                    t0,
                    t1,
                }
            }
            (None, Some(h)) => {
                i += 1;
                issues.push(Issue {
                    kind: IssueKind::Extra,
                    reference: None,
                    reference_surface: None,
                    said: Some(h.clone()),
                    t0,
                    t1,
                    severity: Severity::Medium,
                    note: format!(
                        "识别到多余内容 \"{h}\"：可能是插入音、口头禅，或参考文本里没有的词"
                    ),
                    similarity: 0.0,
                    phonetic_similarity: 0.0,
                    hint: None,
                });
                Pair {
                    op: Op::Insert,
                    ref_index: None,
                    reference: None,
                    said: Some(h.clone()),
                    similarity: 0.0,
                    t0,
                    t1,
                }
            }
            (None, None) => continue,
        };
        alignment.push(pair);
    }

    let n = ref_words.len().max(1);
    let errors = s + d + i;
    let accuracy = if errors >= n { 0.0 } else { 1.0 - errors as f32 / n as f32 };

    TextDiff {
        reference: reference.to_string(),
        reference_words: ref_words.len(),
        substitutions: s,
        deletions: d,
        insertions: i,
        accuracy,
        issues,
        alignment,
        timestamp_alignment: alignment_quality,
        timestamps_estimated: alignment_quality != TimestampAlignment::Exact,
    }
}

/// 合并被 ASR 拆开的合成词（`lunch time` → `lunchtime`），并同步时间与 token 序号。
///
/// 只在「拼接后是参考里出现过的词」且「两部分都不是独立参考词」时才合并，
/// 避免把 "in to" → "into"、"some thing" → "something" 这类正常分词误合并。
fn merge_split_compounds(
    units: &mut Vec<Unit>,
    times: &mut Vec<(f32, f32)>,
    ref_set: &std::collections::HashSet<String>,
) {
    let mut i = 0usize;
    while i + 1 < units.len() {
        let adjacent = units[i + 1].token == units[i].token + 1;
        let joined = format!("{}{}", units[i].text, units[i + 1].text);
        let should_merge = adjacent
            && units[i].text.chars().count() >= 2
            && units[i + 1].text.chars().count() >= 2
            && ref_set.contains(&joined)
            && !ref_set.contains(&units[i].text);
        if should_merge {
            units[i].text = joined;
            if let (Some(a), Some(b)) = (times.get(i).copied(), times.get(i + 1).copied()) {
                times[i] = (a.0, b.1.max(a.1));
            }
            units.remove(i + 1);
            if i + 1 < times.len() {
                times.remove(i + 1);
            }
        } else {
            i += 1;
        }
    }
}

/// 短功能词（弱读词）判定：这些词被 CTC 解码漏掉时，不能轻易归因于用户漏读。
///
/// 实测依据：真机 48 kHz 录音 "I think this is the third time"，
/// SenseVoice 输出 "I think this the third time"（漏 `is`），
/// 同一音频 whisper 输出 "I think it is the third time"（听到了 `is`）。
pub fn is_short_function_word(w: &str) -> bool {
    const WORDS: [&str; 40] = [
        "a", "an", "the", "is", "are", "was", "were", "am", "be", "been", "of", "to", "in",
        "on", "at", "it", "its", "this", "that", "and", "or", "but", "as", "so", "do", "does",
        "did", "have", "has", "had", "will", "would", "can", "could", "shall", "should", "may",
        "i", "you", "we",
    ];
    let lc = w.trim().to_lowercase();
    lc.chars().count() <= 4 && WORDS.contains(&lc.as_str())
}

/// 同一个来源 token（如 `I'm` → `i`+`am`）只报一条问题，避免重复刷屏。
fn already_reported(token: Option<usize>, last: &mut Option<usize>) -> bool {
    match (token, *last) {
        (Some(t), Some(l)) if t == l => true,
        (Some(t), _) => {
            *last = Some(t);
            false
        }
        _ => false,
    }
}

/// 规范化词 → 时间戳。
///
/// 原始 token 数与 ASR 词数一致时按 `Unit.token` 精确取；同一 token 展开出的多个词
/// 在它自己的时间窗内均分；否则退回比例映射并标记 `Estimated`。
fn times_for(
    units: &[Unit],
    src: &[(f32, f32)],
    token_count: usize,
) -> (Vec<(f32, f32)>, TimestampAlignment) {
    if units.is_empty() {
        return (Vec::new(), TimestampAlignment::Missing);
    }
    if src.is_empty() {
        return (
            vec![(f32::NAN, f32::NAN); units.len()],
            TimestampAlignment::Missing,
        );
    }
    if src.len() == token_count {
        let mut out = vec![(f32::NAN, f32::NAN); units.len()];
        let mut i = 0usize;
        while i < units.len() {
            let tok = units[i].token;
            let mut j = i;
            while j < units.len() && units[j].token == tok {
                j += 1;
            }
            let (t0, t1) = src[tok.min(src.len() - 1)];
            let k = (j - i) as f32;
            let step = ((t1 - t0) / k).max(0.0);
            for (n, idx) in (i..j).enumerate() {
                let a = t0 + step * n as f32;
                let b = if n + 1 == j - i { t1 } else { t0 + step * (n + 1) as f32 };
                out[idx] = (a, b.max(a));
            }
            i = j;
        }
        (out, TimestampAlignment::Exact)
    } else {
        // 词数不匹配（ASR 分词与规范化分词不一致）→ 比例映射 + 显式标记
        let n = units.len();
        let m = src.len();
        let out = (0..n)
            .map(|i| {
                let pos = if n == 1 {
                    0
                } else {
                    (i as f32 * (m as f32 - 1.0) / (n as f32 - 1.0)).round() as usize
                };
                src[pos.min(m - 1)]
            })
            .collect();
        (out, TimestampAlignment::Estimated)
    }
}

#[derive(Debug)]
struct Step {
    reference: Option<String>,
    said: Option<String>,
}

/// Levenshtein 对齐 + 回溯，返回操作序列
fn align(a: &[String], b: &[String]) -> Vec<Step> {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i as u32;
    }
    for j in 0..=m {
        dp[0][j] = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j - 1] + cost)
                .min(dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1);
        }
    }

    let mut steps = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            if dp[i][j] == dp[i - 1][j - 1] + cost {
                steps.push(Step {
                    reference: Some(a[i - 1].clone()),
                    said: Some(b[j - 1].clone()),
                });
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && dp[i][j] == dp[i - 1][j] + 1 {
            steps.push(Step { reference: Some(a[i - 1].clone()), said: None });
            i -= 1;
            continue;
        }
        if j > 0 {
            steps.push(Step { reference: None, said: Some(b[j - 1].clone()) });
            j -= 1;
            continue;
        }
        break;
    }
    steps.reverse();
    steps
}

/// 归一化 Levenshtein 相似度（拼写层面）
pub fn similarity(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    let (x, y): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (n, m) = (x.len(), y.len());
    if n == 0 || m == 0 {
        return 0.0;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = if x[i - 1] == y[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j - 1] + cost).min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let dist = prev[m] as f32;
    1.0 - dist / n.max(m) as f32
}

use super::phonetic;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Transcript, Word};

    fn tr(text: &str, words: &[(&str, f32, f32)]) -> Transcript {
        Transcript {
            text: text.into(),
            lang: "en".into(),
            engine: "test".into(),
            duration: 6.0,
            words: words
                .iter()
                .map(|(w, t0, t1)| Word { w: w.to_string(), t0: *t0, t1: *t1 })
                .collect(),
            tokens: Vec::new(),
            degraded: false,
            infer_ms: 0.0,
        }
    }

    #[test]
    fn detects_substitution_and_keeps_alignment() {
        let t = tr(
            "I think this is the straight time",
            &[
                ("I", 0.0, 0.2),
                ("think", 0.2, 0.6),
                ("this", 0.6, 0.8),
                ("is", 0.8, 1.0),
                ("the", 1.0, 1.2),
                ("straight", 1.2, 1.8),
                ("time", 1.8, 2.1),
            ],
        );
        let d = compare("I think this is the third time", &t);
        assert_eq!(d.substitutions, 1);
        assert_eq!(d.deletions, 0);
        assert_eq!(d.insertions, 0);
        assert_eq!(d.issues.len(), 1);
        let is = &d.issues[0];
        assert_eq!(is.reference.as_deref(), Some("third"));
        assert_eq!(
            is.reference_surface.as_deref(),
            Some("third"),
            "surface 必须来自被替换的那个词，而不是句首"
        );
        assert_eq!(is.said.as_deref(), Some("straight"));
        assert_eq!(is.kind, IssueKind::Substitution);
        assert_eq!(is.hint, Some(Hint::Unclear));
        assert_eq!(d.timestamp_alignment, TimestampAlignment::Exact);
        // 精确对齐：straight 的时间应等于它自己的 ASR 时间
        assert!((is.t0 - 1.2).abs() < 1e-6, "t0={}", is.t0);
        assert!((is.t1 - 1.8).abs() < 1e-6, "t1={}", is.t1);
        assert!(d.accuracy > 0.8 && d.accuracy < 1.0);
    }

    #[test]
    fn short_function_word_omission_is_flagged_as_asr_miss() {
        // 实测案例：SenseVoice 漏掉 "is"
        let t = tr("I think this the third time", &[]);
        let d = compare("I think this is the third time", &t);
        let is_issue = d
            .issues
            .iter()
            .find(|i| i.reference.as_deref() == Some("is"))
            .expect("应报 1 条 omitted");
        assert_eq!(is_issue.kind, IssueKind::Omitted);
        assert_eq!(is_issue.hint, Some(Hint::LikelyAsrMiss));
        assert_eq!(is_issue.severity, Severity::Medium, "不能当 high 直接指责");
        assert!(is_issue.note.contains("不能直接判定"));
    }

    #[test]
    fn content_word_omission_stays_high_severity() {
        let t = tr("The cat sat", &[]);
        let d = compare("The elephant cat sat", &t);
        let el = d
            .issues
            .iter()
            .find(|i| i.reference.as_deref() == Some("elephant"))
            .expect("应报 elephant 漏读");
        assert_eq!(el.severity, Severity::High);
        assert_eq!(el.hint, None, "实词漏读不应降级");
    }

    #[test]
    fn function_word_list_is_sane() {
        for w in ["is", "the", "of", "to", "a", "it"] {
            assert!(is_short_function_word(w), "{w} 应算短功能词");
        }
        for w in ["think", "elephant", "third", "straight"] {
            assert!(!is_short_function_word(w), "{w} 不应算短功能词");
        }
    }

    #[test]
    fn split_compound_is_not_a_problem() {
        // 实测：原生音频 "lunchtime" 被 ASR 拆成 "lunch time"
        let t = tr(
            "I stay in bed until lunch time",
            &[
                ("I", 0.0, 0.2),
                ("stay", 0.2, 0.5),
                ("in", 0.5, 0.7),
                ("bed", 0.7, 1.0),
                ("until", 1.0, 1.4),
                ("lunch", 1.4, 1.7),
                ("time", 1.7, 2.0),
            ],
        );
        let d = compare("I stay in bed until lunchtime", &t);
        assert_eq!(d.issues.len(), 0, "合成词被拆开不应算错: {:?}", d.issues);
        assert_eq!(d.insertions, 0);
        assert_eq!(d.substitutions, 0);
    }

    #[test]
    fn compound_merge_does_not_join_normal_words() {
        // "in to" 不应被当成 "into"（参考里没有 into）
        let t = tr("go in to the room", &[]);
        let d = compare("go in to the room", &t);
        assert_eq!(d.issues.len(), 0);
        // 参考里真的有 into 时才合并
        let t2 = tr("go in to the room", &[]);
        let d2 = compare("go into the room", &t2);
        assert_eq!(d2.issues.len(), 0, "参考 into / 识别 in to 应视为同一词: {:?}", d2.issues);
    }

    #[test]
    fn detects_omission() {
        let t = tr("Hello world", &[("Hello", 0.0, 0.4), ("world", 0.4, 0.9)]);
        let d = compare("Hello brave new world", &t);
        assert_eq!(d.deletions, 2);
        assert_eq!(d.issues.iter().filter(|i| i.kind == IssueKind::Omitted).count(), 2);
    }

    #[test]
    fn detects_insertion() {
        let t = tr(
            "I really think so",
            &[("I", 0.0, 0.2), ("really", 0.2, 0.5), ("think", 0.5, 0.8), ("so", 0.8, 1.0)],
        );
        let d = compare("I think so", &t);
        assert_eq!(d.insertions, 1);
        assert_eq!(d.issues[0].kind, IssueKind::Extra);
    }

    #[test]
    fn contraction_is_not_an_error() {
        let t = tr("I am fine", &[("I", 0.0, 0.1), ("am", 0.1, 0.3), ("fine", 0.3, 0.6)]);
        let d = compare("I'm fine", &t);
        assert_eq!(d.issues.len(), 0, "缩写展开后不应判定为错误");
    }

    // ---- D03 回归：缩写/数字不再让时间戳错位 ----

    #[test]
    fn contraction_expansion_keeps_timestamps_aligned() {
        // 参考是 "I'm fine."（2 个原始 token），识别是 3 个词 → 规范化后 3 个词
        let t = tr(
            "I am fine",
            &[("I", 0.0, 0.30), ("am", 0.30, 0.62), ("fine", 0.62, 1.10)],
        );
        let d = compare("I'm fine.", &t);
        assert_eq!(d.issues.len(), 0);
        assert_eq!(
            d.timestamp_alignment,
            TimestampAlignment::Exact,
            "token 数与 ASR 词数一致时必须精确对齐"
        );
        // 每个识别词的时间应严格等于它自己的 ASR 时间，而不是被比例拉伸
        let times: Vec<(f32, f32)> = d
            .alignment
            .iter()
            .filter(|p| p.op == Op::Equal)
            .map(|p| (p.t0, p.t1))
            .collect();
        assert_eq!(times.len(), 3);
        assert!((times[1].0 - 0.30).abs() < 1e-6, "am 的起点被拉偏: {}", times[1].0);
        assert!((times[2].0 - 0.62).abs() < 1e-6, "fine 的起点被拉偏: {}", times[2].0);
    }

    #[test]
    fn omitted_contraction_is_reported_once_with_surface() {
        // 用户漏了 "I'm"，只说了 "fine"
        let t = tr("fine", &[("fine", 0.5, 0.9)]);
        let d = compare("I'm fine.", &t);
        let omitted: Vec<&Issue> =
            d.issues.iter().filter(|i| i.kind == IssueKind::Omitted).collect();
        // i / am 来自同一个 token → 只报 1 条，且显示原始写法 "I'm"
        assert_eq!(omitted.len(), 1, "同一 token 展开的词不应重复上报: {:?}", d.issues);
        assert_eq!(omitted[0].reference_surface.as_deref(), Some("I'm"));
        assert!(omitted[0].note.contains("I'm"));
    }

    #[test]
    fn mismatched_tokenization_marks_estimated() {
        // "a-b" 规范化后 2 个 token，但 ASR 只给了 1 个词 → 只能估算
        let t = tr("a-b", &[("a-b", 0.0, 0.5)]);
        let d = compare("a b", &t);
        assert_eq!(d.timestamp_alignment, TimestampAlignment::Estimated);
        assert!(d.timestamps_estimated);
    }

    #[test]
    fn number_expansion_uses_single_word_window() {
        // 参考 "3"，识别 "three" → 不应算错；且不重启时间窗
        let t = tr("have three cats", &[
            ("have", 0.0, 0.2),
            ("three", 0.2, 0.5),
            ("cats", 0.5, 0.8),
        ]);
        let d = compare("have 3 cats", &t);
        assert_eq!(d.issues.len(), 0);
        assert_eq!(d.timestamp_alignment, TimestampAlignment::Exact);
    }

    #[test]
    fn missing_timestamps_marked_missing() {
        let t = tr("I think this is the third time", &[]);
        let d = compare("I think this is the third time", &t);
        assert_eq!(d.timestamp_alignment, TimestampAlignment::Missing);
        assert!(d.timestamps_estimated);
    }

    #[test]
    fn identical_text_scores_full_accuracy() {
        let t = tr("I think this is the third time", &[]);
        let d = compare("I think this is the third time", &t);
        assert_eq!(d.issues.len(), 0);
        assert!((d.accuracy - 1.0).abs() < 1e-6);
    }

    #[test]
    fn spelling_similarity_baseline() {
        assert!(similarity("think", "thing") > 0.75);
        assert!(similarity("cat", "elephant") < 0.3);
    }

    #[test]
    fn chinese_is_compared_per_char() {
        let t = tr("我觉得很好", &[]);
        let d = compare("我觉得非常好", &t);
        assert_eq!(d.substitutions + d.deletions + d.insertions, 2);
        assert!(d.issues.iter().any(|i| i.reference.as_deref() == Some("非")));
        assert!(d.accuracy < 1.0);
    }
}
