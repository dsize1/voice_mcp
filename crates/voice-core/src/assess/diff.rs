//! 词级/字级差异对齐：参考文本 vs 识别结果。
//!
//! 用 Levenshtein DP 带回溯，得到 Equal/Substitute/Delete/Insert 四类操作，
//! 再把每个操作翻译成「教学上可解释的问题」：
//!
//! | 现象 | 含义 | 典型原因 |
//! |---|---|---|
//! | Substitute + 词形相似 | 读错 / ASR 听成近音词 | 发音问题（/θ/→/s/ 一类） |
//! | Substitute + 词形差异大 | 说了别的词 | 词汇/记忆问题，不是发音 |
//! | Delete（参考有、识别无） | 漏读 / 吞音 | 词尾辅音、弱读过度、语速过快 |
//! | Insert（识别有、参考无） | 多读 / 插入音 | 拼读习惯、犹豫 |

use serde::Serialize;

use super::{normalize, phonetic};
use crate::engine::{Transcript, Word};

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
    /// 音形接近 → 大概率是发音偏差（如 think→thing, thin→sin）
    LikelyPronunciation,
    /// 音形与词形都差得远 → 说成了别的词
    LikelyDifferentWord,
    /// 从文本无法判断（如 third→straight：可能是发音问题，也可能是 ASR 误听）
    Unclear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pair {
    pub op: Op,
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
    pub reference: Option<String>,
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
    /// 完整对齐序列（含 Equal），长度受参考文本限制，供上层需要时展开
    pub alignment: Vec<Pair>,
    /// 无词级时间戳时为 true，此时 t0/t1 是估算值
    pub timestamps_estimated: bool,
}

/// 比较参考文本与转写结果。
pub fn compare(reference: &str, transcript: &Transcript) -> TextDiff {
    let ref_words = normalize::words(reference);
    let hyp_words = normalize::words(&transcript.text);

    // 识别词的时间戳（与 hyp_words 一一对应，长度可能因规范化而不同 → 用比例映射兜底）
    let hyp_times = hyp_times(&transcript.words, &transcript.tokens, hyp_words.len());

    let dp = align(&ref_words, &hyp_words);
    let mut alignment = Vec::with_capacity(dp.len());
    let mut issues = Vec::new();
    let (mut s, mut d, mut i) = (0usize, 0usize, 0usize);

    let mut hyp_cursor = 0usize;
    let mut last_t1 = 0.0f32;
    for step in &dp {
        let (t0, t1) = match (&step.reference, &step.said) {
            // 说出了词：取该词时间戳并推进游标
            (_, Some(_)) => {
                let t = hyp_times
                    .get(hyp_cursor)
                    .copied()
                    .unwrap_or((last_t1, last_t1 + 0.2));
                hyp_cursor += 1;
                // 时间戳缺失时退回估算
                if t.0.is_finite() { t } else { (last_t1, last_t1 + 0.2) }
            }
            // 漏读：夹在前一个词结束与下一个词开始之间
            (Some(_), None) => {
                let prev = hyp_cursor
                    .checked_sub(1)
                    .and_then(|i| hyp_times.get(i))
                    .map(|t| t.1)
                    .unwrap_or(0.0);
                let next = hyp_times
                    .get(hyp_cursor)
                    .map(|t| t.0)
                    .unwrap_or(prev + 0.2);
                (prev, next.max(prev))
            }
            (None, None) => (last_t1, last_t1),
        };
        last_t1 = t1.max(last_t1);
        let pair = match (&step.reference, &step.said) {
            (Some(r), Some(h)) if r == h => Pair {
                op: Op::Equal,
                reference: Some(r.clone()),
                said: Some(h.clone()),
                similarity: 1.0,
                t0,
                t1,
            },
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
                let severity = Severity::High;
                let note = match hint {
                    Hint::LikelyPronunciation => format!(
                        "参考词 \"{r}\" → 识别为 \"{h}\"（音形相似 {:.0}%）：两个词听起来接近，                         大概率是发音偏差；具体是哪个音素错，需要音素级对齐确认",
                        psim * 100.0
                    ),
                    Hint::LikelyDifferentWord => format!(
                        "参考词 \"{r}\" → 识别为 \"{h}\"（音形相似 {:.0}%）：听起来差得远，                         更可能是说成了别的词（词汇/记忆），不一定是发音问题",
                        psim * 100.0
                    ),
                    Hint::Unclear => format!(
                        "参考词 \"{r}\" → 识别为 \"{h}\"（音形相似 {:.0}%、拼写相似 {:.0}%）：                         从文本无法判断是发音偏差还是 ASR 误听。可让用户重录该词，或对照示范音确认",
                        psim * 100.0,
                        sim * 100.0
                    ),
                };
                issues.push(Issue {
                    kind: IssueKind::Substitution,
                    reference: Some(r.clone()),
                    said: Some(h.clone()),
                    t0,
                    t1,
                    severity,
                    note,
                    similarity: sim,
                    phonetic_similarity: psim,
                    hint: Some(hint),
                });
                Pair {
                    op: Op::Substitute,
                    reference: Some(r.clone()),
                    said: Some(h.clone()),
                    similarity: sim,
                    t0,
                    t1,
                }
            }

            (Some(r), None) => {
                d += 1;
                issues.push(Issue {
                    kind: IssueKind::Omitted,
                    reference: Some(r.clone()),
                    said: None,
                    t0,
                    t1,
                    severity: Severity::High,
                    note: format!(
                        "参考词 \"{r}\" 没有被识别到：可能是漏读、吞音（词尾辅音/弱读过度），\
                         也可能被说得太快连在一起"
                    ),
                    similarity: 0.0,
                    phonetic_similarity: 0.0,
                    hint: None,
                });
                Pair {
                    op: Op::Delete,
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
                    said: Some(h.clone()),
                    t0,
                    t1,
                    severity: Severity::Medium,
                    note: format!("识别到多余内容 \"{h}\"：可能是插入音、口头禅，或参考文本里没有的词"),
                    similarity: 0.0,
                    phonetic_similarity: 0.0,
                    hint: None,
                });
                Pair {
                    op: Op::Insert,
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
    let accuracy = if errors >= n {
        0.0
    } else {
        1.0 - errors as f32 / n as f32
    };

    TextDiff {
        reference: reference.to_string(),
        reference_words: ref_words.len(),
        substitutions: s,
        deletions: d,
        insertions: i,
        accuracy,
        issues,
        alignment,
        timestamps_estimated: transcript.words.is_empty(),
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
    // dp[i][j] = a[..i] 与 b[..j] 的编辑距离
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

    // 回溯
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

/// 归一化 Levenshtein 相似度
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

/// 把识别词的时间戳对齐到规范化后的词序列上。
///
/// 规范化可能改变词数（缩写展开、数字转词、中文按字切分），
/// 用「按比例映射 + 尾部沿用」的方式近似，避免越界。
fn hyp_times(words: &[Word], tokens: &[Word], norm_len: usize) -> Vec<(f32, f32)> {
    let src: Vec<(f32, f32)> = if !words.is_empty() {
        words.iter().map(|w| (w.t0, w.t1)).collect()
    } else {
        tokens
            .iter()
            .filter(|t| t.t0.is_finite())
            .map(|w| (w.t0, w.t1))
            .collect()
    };
    if src.is_empty() || norm_len == 0 {
        return Vec::new();
    }
    (0..norm_len)
        .map(|i| {
            let pos = (i as f32 * (src.len() as f32 - 1.0) / (norm_len as f32 - 1.0).max(1.0))
                .round() as usize;
            src[pos.min(src.len() - 1)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Transcript;

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
        assert_eq!(is.said.as_deref(), Some("straight"));
        // third vs straight 音形也不接近 → 诚实标注为 unclear
        assert_eq!(is.kind, IssueKind::Substitution);
        assert_eq!(is.hint, Some(Hint::Unclear));
        assert!(d.accuracy > 0.8 && d.accuracy < 1.0);
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
        let t = tr("I really think so", &[("I", 0.0, 0.2), ("really", 0.2, 0.5), ("think", 0.5, 0.8), ("so", 0.8, 1.0)]);
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

    #[test]
    fn identical_text_scores_full_accuracy() {
        let t = tr("I think this is the third time", &[]);
        let d = compare("I think this is the third time", &t);
        assert_eq!(d.issues.len(), 0);
        assert!((d.accuracy - 1.0).abs() < 1e-6);
        assert!(d.timestamps_estimated);
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
        // 中文按字比较：参考 6 字 vs 识别 5 字，编辑距离最优解总错误数为 2
        assert_eq!(d.substitutions + d.deletions + d.insertions, 2);
        assert!(d.issues.iter().any(|i| i.reference.as_deref() == Some("非")));
        assert!(d.accuracy < 1.0);
    }
}
