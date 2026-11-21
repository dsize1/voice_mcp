//! 语流分析：语速、停顿、词界、填充词、自我修正。
//!
//! 这些都是**可测量**的量，也是雅思口语 Fluency 维度的物理对应物：
//!
//! | 指标 | 雅思含义 |
//! |---|---|
//! | `speech_rate_wpm` / `articulation_rate_wpm` | 语速（含/不含停顿） |
//! | `pause_count` / `longest_pause` | 卡顿频率与严重度 |
//! | `word_boundary_breaks` | 词与词之间插入停顿 → 连读/词界问题 |
//! | `fillers` | um/uh/嗯 这类填充词 |
//! | `repetitions` | 自我修正/卡壳 |

use serde::Serialize;

use crate::engine::vad::Segment;
use crate::engine::Transcript;

use super::normalize;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Span {
    pub t0: f32,
    pub t1: f32,
    pub dur: f32,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct WordGap {
    /// 前一个词
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<u32>,
    pub gap: f32,
    pub at: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowMetrics {
    pub duration: f32,
    /// 实际参与语速计算的窗口 = [首个语音段起点, 末个语音段终点]
    ///
    /// 为什么需要：录音文件的 duration 里包含"等用户开口"的前导静音和
    /// 自动停止的尾巴（实测 5.74 s 录音里真实语音只占 2.8 s），
    /// 用文件时长算语速会把 150 wpm 算成 73 wpm。
    pub analysis_window: f32,
    /// 有效发声时长（VAD 判定的语音段合计）
    pub speech_duration: f32,
    pub speech_ratio: f32,
    pub leading_silence: f32,
    pub trailing_silence: f32,
    pub word_count: usize,
    /// 每分钟词数（含停顿）——中文为每分钟字数
    pub speech_rate: f32,
    /// 每分钟词数（只算发声时间）
    pub articulation_rate: f32,
    pub rate_unit: &'static str,
    pub pause_count: usize,
    pub pause_total: f32,
    pub pause_mean: f32,
    pub pause_longest: f32,
    pub pauses: Vec<Span>,
    /// 词与词之间 >200 ms 的停顿（连读失败/词界切分过重）
    pub word_boundary_breaks: usize,
    pub word_gaps: Vec<WordGap>,
    pub fillers: Vec<Span>,
    /// 每 100 词填充词数
    pub filler_rate: f32,
    pub repetitions: usize,
    /// 一句话里最长连续不停顿的词数（越长越流利）
    pub longest_run_words: usize,
}

const PAUSE_MIN: f32 = 0.25;
const WORD_GAP_MIN: f32 = 0.20;
const FILLERS_EN: [&str; 10] = ["um", "uh", "er", "ah", "erm", "hmm", "eh", "mm", "uhh", "umm"];
const FILLERS_ZH: [&str; 4] = ["嗯", "呃", "啊", "哦"];

pub fn analyze(
    samples: &[f32],
    sample_rate: u32,
    transcript: &Transcript,
    segments: &[Segment],
) -> FlowMetrics {
    let duration = if sample_rate > 0 {
        samples.len() as f32 / sample_rate as f32
    } else {
        transcript.duration
    };
    let words = normalize::words(&transcript.text);
    let word_count = if !transcript.words.is_empty() {
        transcript.words.len()
    } else {
        words.len()
    };
    let is_zh = transcript.lang.starts_with("zh")
        || (transcript.lang == "mixed" && words.iter().any(|w| w.chars().all(is_cjk)));

    // 防御：丢弃完全落在音频之外的段，并裁剪到 [0, duration]
    // （引擎状态异常时曾出现 t0=9.0s 而音频只有 6.08s 的情况，见 ADR-0006 D4）
    let segments: Vec<Segment> = segments
        .iter()
        .filter(|s| s.t0 < duration.max(0.01))
        .map(|s| Segment {
            t0: s.t0.clamp(0.0, duration),
            t1: s.t1.clamp(0.0, duration),
        })
        .filter(|s| s.t1 > s.t0)
        .collect();

    let speech_duration: f32 = segments.iter().map(|s| s.dur()).sum();
    let leading_silence = segments.first().map(|s| s.t0.max(0.0)).unwrap_or(duration);
    let trailing_silence = segments
        .last()
        .map(|s| (duration - s.t1).max(0.0))
        .unwrap_or(0.0);

    // 停顿：VAD 段之间的空隙
    let mut pauses = Vec::new();
    for w in segments.windows(2) {
        let gap = w[1].t0 - w[0].t1;
        if gap >= PAUSE_MIN {
            pauses.push(Span { t0: w[0].t1, t1: w[1].t0, dur: gap });
        }
    }
    let pause_total: f32 = pauses.iter().map(|p| p.dur).sum();
    let pause_longest = pauses.iter().map(|p| p.dur).fold(0.0f32, f32::max);
    let pause_mean = if pauses.is_empty() { 0.0 } else { pause_total / pauses.len() as f32 };

    // 词界停顿
    let mut word_gaps = Vec::new();
    let mut longest_run = 0usize;
    let mut run = 0usize;
    for (i, w) in transcript.words.iter().enumerate() {
        if i == 0 {
            run = 1;
        } else {
            let prev = &transcript.words[i - 1];
            let gap = w.t0 - prev.t1;
            if gap > WORD_GAP_MIN {
                word_gaps.push(WordGap { before: Some((i - 1) as u32), gap, at: prev.t1 });
                longest_run = longest_run.max(run);
                run = 1;
            } else {
                run += 1;
            }
        }
    }
    longest_run = longest_run.max(run);

    // 填充词
    let mut fillers = Vec::new();
    for w in &transcript.words {
        let lw = w.w.to_lowercase();
        let lw = lw.trim_matches(|c: char| !c.is_alphanumeric() && !is_cjk(c));
        if FILLERS_EN.contains(&lw) || FILLERS_ZH.contains(&lw) {
            fillers.push(Span { t0: w.t0, t1: w.t1, dur: (w.t1 - w.t0).max(0.0) });
        }
    }

    // 相邻重复（the the / I I）→ 自我修正
    let mut repetitions = 0usize;
    for i in 1..words.len() {
        if words[i] == words[i - 1] {
            repetitions += 1;
        }
    }

    let window_t0 = segments.first().map(|s| s.t0).unwrap_or(0.0);
    let window_t1 = segments.last().map(|s| s.t1).unwrap_or(duration);
    let analysis_window = (window_t1 - window_t0).max(0.05);

    let speech_rate = if analysis_window > 0.0 {
        word_count as f32 / analysis_window * 60.0
    } else {
        0.0
    };
    let articulation_rate = if speech_duration > 0.05 {
        word_count as f32 / speech_duration * 60.0
    } else {
        0.0
    };
    let filler_rate = if word_count > 0 {
        fillers.len() as f32 / word_count as f32 * 100.0
    } else {
        0.0
    };

    FlowMetrics {
        duration,
        analysis_window: round2(analysis_window),
        speech_duration,
        speech_ratio: (speech_duration / analysis_window).clamp(0.0, 1.0),
        leading_silence,
        trailing_silence,
        word_count,
        speech_rate: round1(speech_rate),
        articulation_rate: round1(articulation_rate),
        rate_unit: if is_zh { "cpm" } else { "wpm" },
        pause_count: pauses.len(),
        pause_total: round2(pause_total),
        pause_mean: round2(pause_mean),
        pause_longest: round2(pause_longest),
        pauses: pauses.iter().map(|p| Span { t0: round2(p.t0), t1: round2(p.t1), dur: round2(p.dur) }).collect(),
        word_boundary_breaks: word_gaps.len(),
        word_gaps: word_gaps
            .iter()
            .map(|g| WordGap { before: g.before, gap: round2(g.gap), at: round2(g.at) })
            .collect(),
        fillers: fillers.iter().map(|f| Span { t0: round2(f.t0), t1: round2(f.t1), dur: round2(f.dur) }).collect(),
        filler_rate: round1(filler_rate),
        repetitions,
        longest_run_words: longest_run,
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}')
}

fn round1(v: f32) -> f32 {
    crate::round_to(v, 1)
}
fn round2(v: f32) -> f32 {
    crate::round_to(v, 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Word;

    fn tr(lang: &str, text: &str, words: &[(&str, f32, f32)]) -> Transcript {
        Transcript {
            text: text.into(),
            lang: lang.into(),
            engine: "test".into(),
            duration: 10.0,
            words: words.iter().map(|(w, a, b)| Word { w: w.to_string(), t0: *a, t1: *b }).collect(),
            tokens: Vec::new(),
            degraded: false,
            infer_ms: 0.0,
        }
    }

    #[test]
    fn rounding_never_yields_negative_zero() {
        // JSON 里 -0.0 会让 progress 汇总带符号
        assert!(round2(-0.0001).is_sign_positive(), "round2 产生了 -0.0");
        assert!(round1(-0.01).is_sign_positive(), "round1 产生了 -0.0");
        assert_eq!(round2(-0.0001), 0.0);
    }

    #[test]
    fn fluent_sentence_has_no_pauses() {
        let t = tr(
            "en",
            "I think this is the third time",
            &[
                ("I", 0.2, 0.4),
                ("think", 0.4, 0.8),
                ("this", 0.8, 1.0),
                ("is", 1.0, 1.2),
                ("the", 1.2, 1.4),
                ("third", 1.4, 1.8),
                ("time", 1.8, 2.1),
            ],
        );
        let segs = vec![Segment { t0: 0.2, t1: 2.1 }];
        let f = analyze(&vec![0.0; 32_000], 16_000, &t, &segs);
        assert_eq!(f.pause_count, 0);
        assert_eq!(f.word_boundary_breaks, 0);
        assert_eq!(f.word_count, 7);
        assert_eq!(f.longest_run_words, 7);
        assert_eq!(f.rate_unit, "wpm");
    }

    #[test]
    fn detects_pause_and_word_gap() {
        let t = tr(
            "en",
            "I think ... this is",
            &[("I", 0.2, 0.4), ("think", 0.4, 0.8), ("this", 1.6, 1.8), ("is", 1.8, 2.0)],
        );
        let segs = vec![Segment { t0: 0.2, t1: 0.8 }, Segment { t0: 1.6, t1: 2.0 }];
        let f = analyze(&vec![0.0; 32_000], 16_000, &t, &segs);
        assert_eq!(f.pause_count, 1);
        assert!((f.pause_longest - 0.8).abs() < 1e-3);
        assert_eq!(f.word_boundary_breaks, 1);
        assert!((f.word_gaps[0].gap - 0.8).abs() < 1e-3);
        assert_eq!(f.longest_run_words, 2);
    }

    #[test]
    fn detects_fillers_and_repetitions() {
        let t = tr(
            "en",
            "I I think um this is",
            &[
                ("I", 0.0, 0.2),
                ("I", 0.2, 0.4),
                ("think", 0.4, 0.8),
                ("um", 0.8, 1.0),
                ("this", 1.0, 1.2),
                ("is", 1.2, 1.4),
            ],
        );
        let segs = vec![Segment { t0: 0.0, t1: 1.4 }];
        let f = analyze(&vec![0.0; 32_000], 16_000, &t, &segs);
        assert_eq!(f.fillers.len(), 1);
        assert!(f.filler_rate > 0.0);
        assert_eq!(f.repetitions, 1);
    }

    #[test]
    fn analysis_window_ignores_leading_and_trailing_silence() {
        // 模拟真实麦克风录音：5.74 s 文件，语音只在 1.6–4.6 s
        let t = tr(
            "en",
            "I think this is the third time",
            &[
                ("I", 1.86, 2.10),
                ("think", 2.10, 2.50),
                ("this", 2.50, 2.80),
                ("is", 2.80, 3.00),
                ("the", 3.00, 3.20),
                ("third", 3.20, 3.80),
                ("time", 3.80, 4.60),
            ],
        );
        let segs = vec![Segment { t0: 1.86, t1: 4.60 }];
        let samples = vec![0.0f32; (5.74 * 16_000.0) as usize];
        let f = analyze(&samples, 16_000, &t, &segs);
        assert!((f.duration - 5.74).abs() < 0.02, "duration 仍是文件时长");
        assert!((f.analysis_window - 2.74).abs() < 0.02, "窗口 = 2.74 s");
        // 7 词 / 2.74 s ≈ 153 wpm（而不是按 5.74 s 算出的 73 wpm）
        assert!(f.speech_rate > 140.0 && f.speech_rate < 165.0, "speech_rate={}", f.speech_rate);
        assert!(f.speech_ratio > 0.95, "窗口内几乎全是语音");
    }

    #[test]
    fn chinese_uses_cpm() {
        let t = tr("zh", "我 觉得 很 好", &[("我", 0.0, 0.2), ("觉得", 0.2, 0.5), ("很", 0.5, 0.7), ("好", 0.7, 1.0)]);
        let segs = vec![Segment { t0: 0.0, t1: 1.0 }];
        let f = analyze(&vec![0.0; 16_000], 16_000, &t, &segs);
        assert_eq!(f.rate_unit, "cpm");
        assert!((f.speech_rate - 240.0).abs() < 1.0, "4 字/秒 = 240 cpm, got {}", f.speech_rate);
    }
}
