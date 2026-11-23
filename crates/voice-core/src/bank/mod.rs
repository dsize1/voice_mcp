//! 跟读素材库（ADR-0009）：把 LRC 教材切成"一行一句"的跟读素材。
//!
//! 设计要点（与 ADR 对齐）：
//! - 入库单位是**一行字幕**，不是整课；过短的相邻行自动合并到 ≥3 词；
//! - 质量门槛不达标的行进 `quarantine.jsonl`，**带具体原因**，不做"宁可错收"；
//! - 素材**只读**：生成后不再修改（重复 ingest 默认跳过，`--overwrite` 才覆盖）；
//! - 索引是唯一真相：`bank/index.jsonl`，一行一个素材。

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::assess::{diff, lrc, normalize};
use crate::audio;
use crate::engine::Engines;

/// 逐行切片的门限（ADR-0009 D5）
pub const LINE_SIM_MIN: f32 = 0.90;
pub const DUR_MIN: f32 = 0.8;
pub const DUR_MAX: f32 = 15.0;
pub const SPEECH_RATIO_MIN: f32 = 0.50;
pub const WORDS_MIN: usize = 3;
pub const SLICE_PAD: f32 = 0.15;
pub const FIT_SCALE_TOL: f32 = 0.02;
pub const FIT_OFFSET_MAX: f32 = 0.6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quality {
    pub line_sim: f32,
    pub asr: String,
    pub band_limited: bool,
    pub lrc_fit_scale: f32,
    pub speech_ratio: f32,
}

/// 雅思对齐信息（ADR-0009 S0c）。NCE 教材素材没有这一层。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExamAlignment {
    #[serde(default)]
    pub framework: String,
    /// 1 | 2 | 3（Part 2 是长独白，不做逐句 GOP）
    #[serde(default)]
    pub part: Option<u8>,
    #[serde(default)]
    pub topics: Vec<String>,
    /// short_answer | cue_card | discussion
    #[serde(default)]
    pub task: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BankEntry {
    pub id: String,
    /// 语料集合：nce | ielts | custom（旧数据缺省时从 id 推断）
    #[serde(default)]
    pub corpus: String,
    /// read_aloud（有原生音频，可切片）| prompt（只有题面，示范音靠 TTS）
    #[serde(default)]
    pub kind: String,
    /// native | tts —— 决定 GOP 参考音素序列的可信度
    #[serde(default)]
    pub ref_audio: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exam: Option<ExamAlignment>,
    pub book: String,
    pub lesson: String,
    pub seq: u32,
    pub text: String,
    pub zh: String,
    pub t0: f32,
    pub t1: f32,
    pub dur: f32,
    pub words: usize,
    pub wpm: f32,
    pub level: u8,
    pub src: String,
    pub ref_slow: Option<String>,
    pub quality: Quality,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rejection {
    pub seq: u32,
    pub text: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fit {
    pub scale: f32,
    pub offset: f32,
    pub consistent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonMeta {
    pub book: String,
    pub lesson: String,
    pub source: String,
    pub ingested: String,
    pub lrc_fit: Fit,
    pub lines_total: usize,
    pub lines_kept: usize,
    pub rejected: Vec<Rejection>,
}

#[derive(Debug, Clone, Default)]
pub struct IngestOptions {
    pub book: String,
    pub overwrite: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct LessonIngest {
    pub lesson_slug: String,
    pub kept: usize,
    pub rejected: usize,
    pub entries: Vec<String>,
}

// ---------------------------------------------------------------- 纯函数

/// 难度分级（ADR-0009 D6）：只看词数与语速，不引词表。
pub fn level_of(words: usize, wpm: f32) -> u8 {
    if words <= 6 && wpm <= 120.0 {
        1
    } else if words <= 10 && wpm <= 150.0 {
        2
    } else if words <= 16 && wpm <= 180.0 {
        3
    } else {
        4
    }
}

/// 音素标签：只做**关键词命中**，不做词形还原（宁缺勿错）。
pub fn tags_for(text: &str) -> Vec<String> {
    let low = format!(" {} ", text.to_lowercase());
    let has = |w: &str| low.contains(&format!(" {w} ")) || low.contains(&format!(" {w},"));
    let mut out: Vec<String> = Vec::new();
    let mut push = |t: &str| {
        if !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    };
    // /θ/
    for w in [
        "think",
        "third",
        "three",
        "thank",
        "month",
        "thought",
        "nothing",
        "something",
        "through",
        "thousand",
        "health",
        "both",
        "math",
    ] {
        if has(w) {
            push("theta_to_s");
            break;
        }
    }
    // /ð/
    for w in [
        "this", "that", "these", "those", "they", "them", "their", "there", "then", "brother",
        "mother", "father", "other", "another", "the",
    ] {
        if has(w) {
            push("eth_to_z");
            break;
        }
    }
    // /v/ vs /w/
    for w in [
        "very", "village", "love", "live", "lives", "leave", "have", "give", "seven", "five",
        "over", "never", "every",
    ] {
        if has(w) {
            push("v_to_w");
            break;
        }
    }
    for w in [
        "would", "want", "world", "work", "word", "winter", "woman", "women", "week", "with",
    ] {
        if has(w) {
            push("w_to_v");
            break;
        }
    }
    // /r/ /l/
    for w in [
        "right", "road", "rice", "read", "really", "very", "around", "already",
    ] {
        if has(w) {
            push("r_to_l");
            break;
        }
    }
    // 词尾辅音：以爆破音/摩擦音结尾的实词
    for w in [
        "like", "time", "work", "talked", "walked", "asked", "small", "village", "seat", "good",
        "week",
    ] {
        if has(w) {
            push("final_consonant_drop");
            break;
        }
    }
    // -ed / -s
    if text
        .split_whitespace()
        .any(|w| w.to_lowercase().ends_with("ed"))
    {
        push("ed_ending");
    }
    if text
        .split_whitespace()
        .any(|w| w.to_lowercase().ends_with('s') && w.len() > 3)
    {
        push("s_ending");
    }
    // 辅音丛
    for w in [
        "street", "straight", "asked", "texts", "twelfth", "splendid", "scream", "strong",
    ] {
        if has(w) {
            push("cluster_simplify");
            break;
        }
    }
    // 连读/弱读素材
    if text.contains(" of ")
        || text.contains(" a ")
        || text.contains(" at ")
        || text.contains(" it ")
    {
        push("linking_missing");
    }
    // 一般疑问句
    if text.trim_end().ends_with('?') {
        push("question_rise");
    }
    // 课头/导语：不是跟读素材，Skill 默认跳过
    let tl = text.trim().to_lowercase();
    if tl.starts_with("lesson ") || tl.starts_with("new concept english") {
        push("lesson_header");
    }
    // 合并后的行可能形如 "Our village Listen to the tape then answer this question." → 用 contains 判定
    if tl.contains("listen to the tape")
        || tl.contains("answer this question")
        || tl.contains("first listen")
        || tl.contains("now listen")
    {
        push("instruction");
    }
    // 片段：LRC 把长段落切成了半句（小写开头 / 没有句末标点）→ Skill 优先选整句
    let t = text.trim();
    let starts_ok = t.chars().next().map(|c| !c.is_lowercase()).unwrap_or(false);
    let ends_ok = matches!(
        t.chars().last().unwrap_or(' '),
        '.' | '?' | '!' | '"' | '\''
    );
    if !(starts_ok && ends_ok) {
        push("fragment");
    }
    out
}

/// tag 权重 = **实测错读率** × **可理解度影响**。
///
/// 来源（由 `tools/derive_tag_weights.py` 生成，`docs/tag-weights.json` 有完整溯源）：
/// - rate：speechocean762（5 000 句 / 250 名普通话母语者）音素级 mispronunciations 统计，
///   tag 的成员音素按出现次数加权（基线错读率 3.60%，94 445 个 token）
/// - impact：`references/phoneme-map.md` 的可理解度分级（high=1.0 / 0.8 / 0.6 / 0.45）
///
/// 为什么不只用 rate：so762 量的是"错了多少次"，不是"错了有多严重"。
/// `/v/→/w/` 在母语者里只 2.47%，但一旦错 `very→wery` 直接变词，影响是 high。
/// 为什么不只用 impact：那又会把"罕见但严重"的缺点放大成噪声。
///
/// 第三列是样本量（音素 token 数），用于判断权重是否可信；<1000 的 tag 建议保持默认。
pub const TAG_WEIGHTS: &[(&str, f32, u32)] = &[
    ("theta_to_s", 0.0770, 649), // rate 7.70% × impact 1.0（lift 2.14x）
    ("r_to_l", 0.0411, 6087),    // rate 4.11% × impact 1.0（lift 1.14x）
    ("ae_to_e", 0.0294, 5393),   // rate 4.90% × impact 0.6（lift 1.36x）
    ("final_consonant_drop", 0.0259, 20426), // rate 2.59% × impact 1.0（lift 0.72x）
    ("short_long_vowel", 0.0257, 19895), // rate 4.28% × impact 0.6（lift 1.19x）
    ("w_to_v", 0.0254, 3148),    // rate 2.54% × impact 1.0（lift 0.71x）
    ("v_to_w", 0.0247, 1617),    // rate 2.47% × impact 1.0（lift 0.69x）
    ("l_dark", 0.0216, 3642),    // rate 3.60% × impact 0.6（lift 1.00x）
    ("s_ending", 0.0194, 6759),  // rate 3.23% × impact 0.6（lift 0.90x）
    ("ed_ending", 0.0192, 11649), // rate 3.19% × impact 0.6（lift 0.89x）
    ("eth_to_z", 0.0190, 3027),  // rate 2.38% × impact 0.8（lift 0.66x）
    ("cluster_simplify", 0.0175, 4222), // rate 2.91% × impact 0.6（lift 0.81x）
    ("n_to_ng", 0.0161, 7113),   // rate 3.57% × impact 0.45（lift 0.99x）
];

/// 查 tag 权重；没有实测证据的 tag（`linking_missing` / `question_rise` / `tone_flat` …）
/// 返回 0，**不参与加权排序**——宁可不排，也不编造依据。
pub fn tag_weight(tag: &str) -> f32 {
    TAG_WEIGHTS
        .iter()
        .find(|(t, _, _)| *t == tag)
        .map(|(_, w, _)| *w)
        .unwrap_or(0.0)
}

/// 文件名/标题 → 稳定 slug（小写、非字母数字折成 '-'）。
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// `<book>/<lesson编号>/<行号>`，教材里编号是文件名前两位（如 `02.Breakfast or Lunch`）。
pub fn lesson_no(name: &str) -> String {
    let head: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
    if head.is_empty() { slug(name) } else { head }
}

pub fn id_for(book: &str, lesson: &str, seq: u32) -> String {
    format!("{}/{}/{:03}", slug(book), lesson_no(lesson), seq)
}

/// 合并过短的行：ADR-0009 D5「必要时合并相邻行直到 ≥3 词」。
pub fn merge_short_lines(lines: &[lrc::Line]) -> Vec<lrc::Line> {
    let mut out: Vec<lrc::Line> = Vec::new();
    let mut pending: Option<lrc::Line> = None;
    for l in lines {
        let cur = match pending.take() {
            Some(mut p) => {
                p.t1 = l.t1;
                p.en = format!("{} {}", p.en.trim(), l.en.trim());
                if !l.zh.is_empty() {
                    p.zh = format!("{} {}", p.zh.trim(), l.zh.trim())
                        .trim()
                        .to_string();
                }
                p
            }
            None => l.clone(),
        };
        if normalize::words(&cur.en).len() >= WORDS_MIN {
            out.push(cur);
        } else {
            pending = Some(cur);
        }
    }
    if let Some(p) = pending {
        // 末尾仍不足 3 词：并入上一行，避免丢弃内容
        if let Some(last) = out.last_mut() {
            last.t1 = p.t1;
            last.en = format!("{} {}", last.en.trim(), p.en.trim());
        } else {
            out.push(p);
        }
    }
    out
}

/// 最小二乘拟合 `t_hyp = a * t_lrc + b`。
pub fn linear_fit(pairs: &[(f32, f32)]) -> Option<Fit> {
    if pairs.len() < 2 {
        return None;
    }
    let n = pairs.len() as f64;
    let sx: f64 = pairs.iter().map(|p| p.0 as f64).sum();
    let sy: f64 = pairs.iter().map(|p| p.1 as f64).sum();
    let sxx: f64 = pairs.iter().map(|p| (p.0 as f64) * (p.0 as f64)).sum();
    let sxy: f64 = pairs.iter().map(|p| (p.0 as f64) * (p.1 as f64)).sum();
    let den = n * sxx - sx * sx;
    if den.abs() < 1e-9 {
        return None;
    }
    let a = (n * sxy - sx * sy) / den;
    let b = (sy - a * sx) / n;
    let consistent = (a as f32 - 1.0).abs() < FIT_SCALE_TOL && (b as f32).abs() < FIT_OFFSET_MAX;
    Some(Fit {
        scale: crate::round_to(a as f32, 3),
        offset: crate::round_to(b as f32, 2),
        consistent,
    })
}

// ---------------------------------------------------------------- 索引读写

pub fn bank_dir(home: &Path) -> PathBuf {
    home.join("bank")
}

pub fn index_path(home: &Path) -> PathBuf {
    bank_dir(home).join("index.jsonl")
}

pub fn quarantine_path(home: &Path) -> PathBuf {
    bank_dir(home).join("quarantine.jsonl")
}

/// 给旧索引补齐 S0c 新字段（不落盘；要落盘就跑 `bank migrate`）。
pub fn normalize_entry(e: &mut BankEntry) {
    if e.corpus.is_empty() {
        let head = e.id.split('/').next().unwrap_or_default().to_lowercase();
        e.corpus = if head.starts_with("nce") {
            "nce".to_string()
        } else if head == "ielts" {
            "ielts".to_string()
        } else if head.is_empty() {
            "custom".to_string()
        } else {
            head
        };
    }
    if e.kind.is_empty() {
        e.kind = if e.src.is_empty() {
            "prompt".into()
        } else {
            "read_aloud".into()
        };
    }
    if e.ref_audio.is_empty() {
        e.ref_audio = if e.src.is_empty() {
            "tts".into()
        } else {
            "native".into()
        };
    }
    if e.words == 0 {
        e.words = crate::assess::normalize::words(&e.text).len();
    }
}

pub fn read_index(home: &Path) -> Result<Vec<BankEntry>> {
    let p = index_path(home);
    if !p.is_file() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&p).with_context(|| format!("读取索引失败: {}", p.display()))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<BankEntry>(line) {
            Ok(mut e) => {
                normalize_entry(&mut e);
                out.push(e)
            }
            Err(e) => eprintln!("[bank] 索引第 {} 行无法解析，已跳过: {e}", i + 1),
        }
    }
    Ok(out)
}

/// 重写整个索引（按 id 排序）；用临时文件 + rename，避免写一半损坏。
fn write_index(home: &Path, entries: &[BankEntry]) -> Result<()> {
    let p = index_path(home);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut body = String::new();
    for e in entries {
        body.push_str(&serde_json::to_string(e)?);
        body.push('\n');
    }
    let tmp = p.with_extension("jsonl.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// quarantine 也按 id upsert（重复 ingest 不堆重复行）。
fn upsert_quarantine(home: &Path, rows: &[serde_json::Value]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let p = quarantine_path(home);
    let mut all: Vec<serde_json::Value> = Vec::new();
    if p.is_file() {
        for line in std::fs::read_to_string(&p)?.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                all.push(v);
            }
        }
    }
    for r in rows {
        let id = r
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        match all
            .iter()
            .position(|x| x.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
        {
            Some(i) => all[i] = r.clone(),
            None => all.push(r.clone()),
        }
    }
    let mut body = String::new();
    for v in &all {
        body.push_str(&serde_json::to_string(v)?);
        body.push('\n');
    }
    std::fs::write(&p, body)?;
    Ok(())
}

// ---------------------------------------------------------------- ingest

/// ingest 一个 `<NN>.<Title>.mp3` + 同名 `.lrc`。
pub fn ingest_file(
    home: &Path,
    audio_path: &Path,
    lrc_path: &Path,
    opts: &IngestOptions,
    engines: &Engines,
) -> Result<LessonIngest> {
    let started = std::time::Instant::now();
    let lrc_text = std::fs::read_to_string(lrc_path)
        .with_context(|| format!("读取 LRC 失败: {}", lrc_path.display()))?;
    let decoded = audio::load_16k(audio_path)?;
    let duration = decoded.samples.len() as f32 / decoded.sample_rate as f32;
    let lines = lrc::parse(&lrc_text, duration);
    if lines.is_empty() {
        bail!("LRC 没有可用的行: {}", lrc_path.display());
    }
    let lesson_name = lesson_title(audio_path);
    let book = if opts.book.is_empty() {
        guess_book(audio_path)
    } else {
        opts.book.clone()
    };
    let slug_dir = format!("{}/{}", slug(&book), slug(&lesson_name));

    // 整课 ASR 一次，按行窗口分配（比逐行推理快一个量级）
    let transcript = engines.transcribe_long(&decoded.samples, decoded.sample_rate, "en", None)?;

    // 逐行：参考词 / 命中词 / 行首时间
    let mut sims: Vec<f32> = Vec::new();
    let mut pairs: Vec<(f32, f32)> = Vec::new();
    for l in &lines {
        let ref_words = normalize::words(&l.en);
        let said: Vec<String> = transcript
            .words
            .iter()
            .filter(|w| w.t0 >= l.t0 - 0.25 && w.t0 < l.t1)
            .map(|w| w.w.clone())
            .collect();
        if said.is_empty() {
            continue;
        }
        let sim = diff::similarity(
            &ref_words.join(" "),
            &normalize::words(&said.join(" ")).join(" "),
        );
        sims.push(sim);
        if let Some(first) = transcript
            .words
            .iter()
            .find(|w| w.t0 >= l.t0 - 0.25 && w.t0 < l.t1)
        {
            pairs.push((l.t0, first.t0));
        }
    }
    let fit = linear_fit(&pairs).unwrap_or(Fit {
        scale: 1.0,
        offset: 0.0,
        consistent: false,
    });
    let mean_sim = if sims.is_empty() {
        0.0
    } else {
        sims.iter().sum::<f32>() / sims.len() as f32
    };
    let mean_sim = crate::round_to(mean_sim, 3);

    let mut meta = LessonMeta {
        book: book.clone(),
        lesson: lesson_name.clone(),
        source: audio_path.to_string_lossy().to_string(),
        ingested: chrono::Local::now().to_rfc3339(),
        lrc_fit: fit.clone(),
        lines_total: lines.len(),
        lines_kept: 0,
        rejected: Vec::new(),
    };

    // 整课不合格：全部行进 quarantine，原因写清楚
    if !fit.consistent {
        for (i, l) in lines.iter().enumerate() {
            meta.rejected.push(Rejection {
                seq: i as u32 + 1,
                text: l.en.clone(),
                reasons: vec![format!(
                    "lrc_fit_mismatch: scale={:.3} offset={:+.2}s（阈值 ±{FIT_SCALE_TOL} / ±{FIT_OFFSET_MAX}s）→ 先跑 voice-cli lesson --write-lrc 重定时",
                    fit.scale, fit.offset
                )],
            });
        }
        if !opts.dry_run {
            write_lesson_meta(home, &slug_dir, &meta)?;
            let rows: Vec<serde_json::Value> = meta
                .rejected
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "id": id_for(&book, &lesson_name, r.seq),
                        "book": book, "lesson": lesson_name, "seq": r.seq,
                        "text": r.text, "reasons": r.reasons,
                    })
                })
                .collect();
            upsert_quarantine(home, &rows)?;
        }
        return Ok(LessonIngest {
            lesson_slug: slug_dir,
            kept: 0,
            rejected: meta.rejected.len(),
            entries: Vec::new(),
        });
    }

    let merged = merge_short_lines(&lines);
    let mut kept: Vec<BankEntry> = Vec::new();
    for (i, l) in merged.iter().enumerate() {
        let seq = i as u32 + 1;
        let id = id_for(&book, &lesson_name, seq);
        let start = ((l.t0 - SLICE_PAD).max(0.0) * 16_000.0) as usize;
        let end = (((l.t1 + SLICE_PAD) * 16_000.0) as usize).min(decoded.samples.len());
        if start >= end {
            meta.rejected.push(Rejection {
                seq,
                text: l.en.clone(),
                reasons: vec!["slice_out_of_range: 行时间超出音频长度".into()],
            });
            continue;
        }
        // 先按行窗口 + pad 粗切，再用 VAD 把首尾静音收回到 pad：
        // 行 t1 到下一行 t0 之间常有 1–3 s 句间停顿，直接保留会变成"句子后面拖一段静音"。
        let raw = &decoded.samples[start..end];
        let segs = engines.vad.segments(raw);
        let (slice, offset) = if let (Some(a), Some(b)) = (segs.first(), segs.last()) {
            let s0 = ((a.t0 - SLICE_PAD).max(0.0) * 16_000.0) as usize;
            let s1 = (((b.t1 + SLICE_PAD) * 16_000.0) as usize).min(raw.len());
            if s0 < s1 {
                (&raw[s0..s1], a.t0 - s0 as f32 / 16_000.0)
            } else {
                (raw, 0.0)
            }
        } else {
            (raw, 0.0)
        };
        let dur = slice.len() as f32 / 16_000.0;
        let words = normalize::words(&l.en).len();
        let wpm = if dur > 0.0 {
            words as f32 / dur * 60.0
        } else {
            0.0
        };
        let _ = offset;

        // 收紧后的切片内 VAD 语音占比（用于质量门槛）
        let speech: f32 = segs.iter().map(|s| s.dur()).sum();
        let speech_ratio = if dur > 0.0 {
            (speech / dur).min(1.0)
        } else {
            0.0
        };

        let line_sim = {
            let ref_words = normalize::words(&l.en);
            let said: Vec<String> = transcript
                .words
                .iter()
                .filter(|w| w.t0 >= l.t0 - 0.25 && w.t0 < l.t1)
                .map(|w| w.w.clone())
                .collect();
            if said.is_empty() {
                0.0
            } else {
                diff::similarity(
                    &ref_words.join(" "),
                    &normalize::words(&said.join(" ")).join(" "),
                )
            }
        };

        let mut reasons: Vec<String> = Vec::new();
        if line_sim < LINE_SIM_MIN {
            reasons.push(format!(
                "line_sim {:.2} < {LINE_SIM_MIN}（字幕与音频不符或缺句）",
                line_sim
            ));
        }
        if !(DUR_MIN..=DUR_MAX).contains(&dur) {
            reasons.push(format!("dur {dur:.2}s 不在 [{DUR_MIN}, {DUR_MAX}]"));
        }
        if decoded.info.band_limited {
            reasons.push(format!(
                "band_limited（源采样率 {} Hz < 24 kHz）",
                decoded.info.source_sr
            ));
        }
        if speech_ratio < SPEECH_RATIO_MIN {
            reasons.push(format!(
                "speech_ratio {:.2} < {SPEECH_RATIO_MIN}（切到静音或行边界错位）",
                speech_ratio
            ));
        }
        if words < WORDS_MIN {
            reasons.push(format!("words {words} < {WORDS_MIN}"));
        }
        if !reasons.is_empty() {
            meta.rejected.push(Rejection {
                seq,
                text: l.en.clone(),
                reasons,
            });
            continue;
        }

        let entry = BankEntry {
            id: id.clone(),
            corpus: if book.to_lowercase().starts_with("nce") {
                "nce".into()
            } else {
                slug(&book)
            },
            kind: "read_aloud".into(),
            ref_audio: "native".into(),
            exam: None,
            book: book.clone(),
            lesson: lesson_name.clone(),
            seq,
            text: l.en.clone(),
            zh: l.zh.clone(),
            t0: crate::round_to(l.t0, 2),
            t1: crate::round_to(l.t1, 2),
            dur: crate::round_to(dur, 2),
            words,
            wpm: crate::round_to(wpm, 1),
            level: level_of(words, wpm),
            src: format!("bank/{slug_dir}/{seq:03}.ref.wav"),
            ref_slow: None,
            quality: Quality {
                line_sim: crate::round_to(line_sim, 3),
                asr: transcript.engine.clone(),
                band_limited: decoded.info.band_limited,
                lrc_fit_scale: fit.scale,
                speech_ratio: crate::round_to(speech_ratio, 3),
            },
            tags: tags_for(&l.en),
        };
        kept.push(entry);

        if !opts.dry_run {
            let dir = bank_dir(home).join(&slug_dir);
            std::fs::create_dir_all(&dir)?;
            audio::wav::write_wav_i16(dir.join(format!("{seq:03}.ref.wav")), slice, 16_000)?;
        }
    }

    meta.lines_kept = kept.len();
    if !opts.dry_run {
        write_lesson_meta(home, &slug_dir, &meta)?;
        let mut all = read_index(home)?;
        let mut changed = false;
        for e in &kept {
            match all.iter().position(|x| x.id == e.id) {
                Some(i) => {
                    if opts.overwrite {
                        all[i] = e.clone();
                        changed = true;
                    }
                }
                None => {
                    all.push(e.clone());
                    changed = true;
                }
            }
        }
        if changed {
            all.sort_by(|a, b| a.id.cmp(&b.id));
            write_index(home, &all)?;
        }
        let rows: Vec<serde_json::Value> = meta
            .rejected
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": id_for(&book, &lesson_name, r.seq),
                    "book": book, "lesson": lesson_name, "seq": r.seq,
                    "text": r.text, "reasons": r.reasons,
                })
            })
            .collect();
        upsert_quarantine(home, &rows)?;
        append_attribution(
            home,
            &book,
            &lesson_name,
            &audio_path.to_string_lossy(),
            mean_sim,
        )?;
    }

    eprintln!(
        "[bank] {} · {} → 入库 {} 行 / 拒绝 {} 行（均 sim {:.2}，fit scale {:.3} offset {:+.2}s，{:.1}s）",
        book,
        lesson_name,
        kept.len(),
        meta.rejected.len(),
        mean_sim,
        fit.scale,
        fit.offset,
        started.elapsed().as_secs_f32()
    );

    Ok(LessonIngest {
        lesson_slug: slug_dir,
        kept: kept.len(),
        rejected: meta.rejected.len(),
        entries: kept.into_iter().map(|e| e.id).collect(),
    })
}

fn write_lesson_meta(home: &Path, slug_dir: &str, meta: &LessonMeta) -> Result<()> {
    let dir = bank_dir(home).join(slug_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("lesson.json"), serde_json::to_string_pretty(meta)?)?;
    Ok(())
}

fn append_attribution(
    home: &Path,
    book: &str,
    lesson: &str,
    src: &str,
    mean_sim: f32,
) -> Result<()> {
    use std::io::Write;
    let p = bank_dir(home).join("ATTRIBUTION.md");
    if !p.is_file() {
        std::fs::write(
            &p,
            "# 素材来源\n\n音频与字幕来自公开教材站点，仅本地个人学习使用；不入 git。\n\n| 日期 | 书 | 课 | 来源 | 行相似度 |\n|---|---|---|---|---|\n",
        )?;
    }
    let mut f = std::fs::OpenOptions::new().append(true).open(&p)?;
    writeln!(
        f,
        "| {} | {} | {} | `{}` | {:.2} |",
        chrono::Local::now().format("%Y-%m-%d"),
        book,
        lesson,
        src,
        mean_sim
    )?;
    Ok(())
}

/// `02.Breakfast or Lunch.mp3` → `02.Breakfast or Lunch`
pub fn lesson_title(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// 没有 `--book` 时猜：文件名/目录里出现 nce1..4 就用它，否则 `NCE?`。
pub fn guess_book(path: &Path) -> String {
    let s = path.to_string_lossy().to_lowercase();
    for n in ["nce1", "nce2", "nce3", "nce4"] {
        if s.contains(n) {
            return n.to_uppercase();
        }
    }
    let s2 = path.to_string_lossy().to_lowercase();
    for b in ["book1", "book2", "book3", "book4"] {
        if s2.contains(b) {
            return b.replace("book", "NCE").to_uppercase();
        }
    }
    "NCE?".into()
}

/// 把目录里所有成对的 `<name>.mp3` / `<name>.lrc` 逐个 ingest。
pub fn ingest_dir(
    home: &Path,
    dir: &Path,
    opts: &IngestOptions,
    engines: &Engines,
    limit: Option<usize>,
) -> Result<Vec<LessonIngest>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("列目录失败: {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "mp3").unwrap_or(false))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for mp3 in files {
        if let Some(n) = limit
            && out.len() >= n
        {
            break;
        }
        let lrc = mp3.with_extension("lrc");
        if !lrc.is_file() {
            eprintln!("[bank] 跳过（无同名 .lrc）: {}", mp3.display());
            continue;
        }
        match ingest_file(home, &mp3, &lrc, opts, engines) {
            Ok(r) => out.push(r),
            Err(e) => eprintln!("[bank] 失败 {}: {e:#}", mp3.display()),
        }
    }
    Ok(out)
}

/// 把索引里的旧条目升级成当前 schema（补 corpus/kind/ref_audio），并重写文件。
pub fn migrate(home: &Path) -> Result<usize> {
    let mut idx = read_index(home)?;
    if idx.is_empty() {
        return Ok(0);
    }
    for e in idx.iter_mut() {
        normalize_entry(e);
    }
    idx.sort_by(|a, b| a.id.cmp(&b.id));
    write_index(home, &idx)?;
    Ok(idx.len())
}

/// 导入"只有题面"的素材（雅思口语题库）：`kind=prompt`，示范音由 TTS 生成。
///
/// 为什么单独一个入口：这类素材**没有原生音频**，走不了 `ingest`（那边要 mp3+lrc），
/// 但 schema 与选材逻辑必须和教材素材一致，否则 Skill 要写两套分支。
pub fn import_prompts(home: &Path, docs: &[serde_json::Value]) -> Result<(usize, usize)> {
    let mut idx = read_index(home)?;
    let mut added = 0;
    let mut skipped = 0;
    for d in docs {
        let text = d
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if text.is_empty() {
            bail!("素材缺 text 字段: {d}");
        }
        let id = d
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("ielts/custom/{:04}", idx.len() + 1));
        if idx.iter().any(|e| e.id == id) {
            skipped += 1;
            continue;
        }
        let part = d.get("part").and_then(|v| v.as_u64()).map(|p| p as u8);
        if let Some(p) = part
            && !(1..=3).contains(&p)
        {
            bail!("{id}: part={p} 不在 1..=3");
        }
        let topics: Vec<String> = d
            .get("topics")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let words = crate::assess::normalize::words(&text).len();
        idx.push(BankEntry {
            id: id.clone(),
            corpus: d
                .get("corpus")
                .and_then(|v| v.as_str())
                .unwrap_or("ielts")
                .to_string(),
            kind: d
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("prompt")
                .to_string(),
            ref_audio: d
                .get("ref_audio")
                .and_then(|v| v.as_str())
                .unwrap_or("tts")
                .to_string(),
            exam: Some(ExamAlignment {
                framework: "IELTS".into(),
                part,
                topics,
                task: d
                    .get("task")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            }),
            book: d
                .get("book")
                .and_then(|v| v.as_str())
                .unwrap_or("IELTS prompts")
                .to_string(),
            lesson: d
                .get("lesson")
                .and_then(|v| v.as_str())
                .unwrap_or("custom")
                .to_string(),
            seq: idx.len() as u32 + 1,
            text: text.clone(),
            zh: d
                .get("zh")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            t0: 0.0,
            t1: 0.0,
            dur: 0.0,
            words,
            wpm: 0.0,
            level: d
                .get("level")
                .and_then(|v| v.as_u64())
                .map(|l| l.clamp(1, 4) as u8)
                .unwrap_or_else(|| level_of(words, 0.0)),
            src: String::new(),
            ref_slow: None,
            quality: Quality {
                line_sim: 0.0,
                asr: "none".into(),
                band_limited: false,
                lrc_fit_scale: 1.0,
                speech_ratio: 0.0,
            },
            tags: tags_for(&text),
        });
        added += 1;
    }
    idx.sort_by(|a, b| a.id.cmp(&b.id));
    write_index(home, &idx)?;
    Ok((added, skipped))
}

/// 重算全部素材的 tags（tags 只依赖文本，不需要音频）。
pub fn retag(home: &Path) -> Result<usize> {
    let mut idx = read_index(home)?;
    let mut n = 0;
    for e in idx.iter_mut() {
        let t = tags_for(&e.text);
        if t != e.tags {
            e.tags = t;
            n += 1;
        }
    }
    if n > 0 {
        write_index(home, &idx)?;
    }
    Ok(n)
}

// ---------------------------------------------------------------- 取材（P4.5）

/// 取材查询：Skill 用 focus 的 tags + 难度从 3 000+ 条里挑 1–3 条练。
#[derive(Debug, Clone)]
pub struct PickQuery {
    /// nce | ielts | custom
    pub corpus: Option<String>,
    /// 雅思 Part（只有 kind=prompt/read_aloud 且 exam.part 有值时才命中）
    pub part: Option<u8>,
    pub topic: Option<String>,
    pub kind: Option<String>,
    pub level: Option<u8>,
    pub tags: Vec<String>,
    pub book: Option<String>,
    /// 默认排除这些标签（导语/片段不该拿来练）
    pub exclude_tags: Vec<String>,
    pub limit: usize,
    pub offset: usize,
    /// true（默认）→ 按 tag 权重之和排序；false → 退回"命中几个 tag"
    pub weighted: bool,
    /// Some(seed) → 可复现的洗牌；None → 按 (权重↓, level↑, id↑) 稳定排序
    pub seed: Option<u64>,
}

impl Default for PickQuery {
    fn default() -> Self {
        Self {
            corpus: None,
            part: None,
            topic: None,
            kind: None,
            level: None,
            tags: Vec::new(),
            book: None,
            exclude_tags: vec![
                "lesson_header".into(),
                "instruction".into(),
                "fragment".into(),
            ],
            limit: 5,
            offset: 0,
            weighted: true,
            seed: None,
        }
    }
}

fn shuffle_key(id: &str, seed: u64) -> u64 {
    // FNV-1a + 简单 scrambler：同一 seed 下顺序稳定
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ seed;
    for b in id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 29;
    h
}

/// 按 focus 取材（索引内数据版）。
///
/// 供训练计划等上层复用，避免每选一条就重新读 2 MB 索引。
pub fn pick_from(entries: &[BankEntry], q: &PickQuery) -> Vec<BankEntry> {
    let mut scored: Vec<(f32, usize, &BankEntry, u64)> = Vec::new();
    for e in entries {
        if let Some(lv) = q.level
            && e.level != lv
        {
            continue;
        }
        if let Some(b) = &q.book
            && !e.book.eq_ignore_ascii_case(b)
        {
            continue;
        }
        if let Some(c) = &q.corpus
            && !e.corpus.eq_ignore_ascii_case(c)
        {
            continue;
        }
        if let Some(k) = &q.kind
            && !e.kind.eq_ignore_ascii_case(k)
        {
            continue;
        }
        if let Some(pt) = q.part {
            match e.exam.as_ref().and_then(|x| x.part) {
                Some(p) if p == pt => {}
                _ => continue,
            }
        }
        if let Some(t) = &q.topic {
            let hit = e
                .exam
                .as_ref()
                .map(|x| x.topics.iter().any(|y| y.eq_ignore_ascii_case(t)))
                .unwrap_or(false);
            if !hit {
                continue;
            }
        }
        if q.exclude_tags.iter().any(|t| e.tags.iter().any(|x| x == t)) {
            continue;
        }
        let mut score = 0.0f32;
        let mut hits = 0usize;
        for t in &q.tags {
            if e.tags.iter().any(|x| x == t) {
                hits += 1;
                score += if q.weighted { tag_weight(t) } else { 1.0 };
            }
        }
        if !q.tags.is_empty() && hits == 0 {
            continue;
        }
        scored.push((score, hits, e, shuffle_key(&e.id, q.seed.unwrap_or(0))));
    }
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.1.cmp(&a.1))
            .then(a.2.level.cmp(&b.2.level))
            .then_with(|| {
                if q.seed.is_some() {
                    a.3.cmp(&b.3)
                } else {
                    a.2.id.cmp(&b.2.id)
                }
            })
    });
    scored
        .into_iter()
        .skip(q.offset)
        .take(q.limit)
        .map(|(_, _, e, _)| e.clone())
        .collect()
}

/// 按 focus 取材。命中 tags 越多越靠前；同分按 level 升序、再按 id（或 seed 洗牌）。
pub fn pick(home: &Path, q: &PickQuery) -> Result<Vec<BankEntry>> {
    let idx = read_index(home)?;
    Ok(pick_from(&idx, q))
}

// ---------------------------------------------------------------- 统计

#[derive(Debug, Serialize)]
pub struct BankStats {
    pub entries: usize,
    pub quarantine: usize,
    pub total_dur: f32,
    pub mean_wpm: f32,
    pub by_level: std::collections::BTreeMap<u8, usize>,
    pub by_book: std::collections::BTreeMap<String, usize>,
    pub top_tags: Vec<(String, usize)>,
}

pub fn stats(home: &Path) -> Result<BankStats> {
    let idx = read_index(home)?;
    let mut by_level = std::collections::BTreeMap::new();
    let mut by_book = std::collections::BTreeMap::new();
    let mut tags: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut total_dur = 0.0;
    let mut wpm_sum = 0.0;
    for e in &idx {
        *by_level.entry(e.level).or_insert(0) += 1;
        *by_book.entry(e.book.clone()).or_insert(0) += 1;
        total_dur += e.dur;
        wpm_sum += e.wpm;
        for t in &e.tags {
            *tags.entry(t.clone()).or_insert(0) += 1;
        }
    }
    let mut top_tags: Vec<(String, usize)> = tags.into_iter().collect();
    top_tags.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    top_tags.truncate(10);
    let quarantine = std::fs::read_to_string(quarantine_path(home))
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    Ok(BankStats {
        entries: idx.len(),
        quarantine,
        total_dur: crate::round_to(total_dur, 1),
        mean_wpm: if idx.is_empty() {
            0.0
        } else {
            crate::round_to(wpm_sum / idx.len() as f32, 1)
        },
        by_level,
        by_book,
        top_tags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_thresholds() {
        assert_eq!(level_of(3, 100.0), 1);
        assert_eq!(level_of(6, 120.0), 1);
        assert_eq!(level_of(7, 120.0), 2);
        assert_eq!(level_of(10, 149.0), 2);
        assert_eq!(level_of(10, 160.0), 3);
        assert_eq!(level_of(15, 180.0), 3);
        assert_eq!(level_of(17, 180.0), 4);
        assert_eq!(level_of(5, 200.0), 4, "词少但读得飞快也算难");
    }

    #[test]
    fn slugs_and_ids_are_stable() {
        assert_eq!(slug("02.Breakfast or Lunch"), "02-breakfast-or-lunch");
        assert_eq!(lesson_no("02.Breakfast or Lunch"), "02");
        assert_eq!(id_for("NCE2", "02.Breakfast or Lunch", 3), "nce2/02/003");
        assert_eq!(slug("A  B--c"), "a-b-c");
    }

    #[test]
    fn tags_hit_expected_phonemes() {
        let t = tags_for("I think this is the third time.");
        assert!(t.contains(&"theta_to_s".to_string()));
        assert!(t.contains(&"eth_to_z".to_string()));
        assert!(!t.contains(&"v_to_w".to_string()));
        let v = tags_for("He lives in a very small village.");
        assert!(v.contains(&"v_to_w".to_string()));
        assert!(v.contains(&"final_consonant_drop".to_string()));
        assert!(tags_for("Would you like a cup of tea?").contains(&"question_rise".to_string()));
        // 课头/导语要能识别出来
        assert!(tags_for("Lesson 33").contains(&"lesson_header".to_string()));
        assert!(
            tags_for("Listen to the tape then answer this question.")
                .contains(&"instruction".to_string())
        );
        // 标题与导语被合并成一行时也要能识别
        assert!(
            tags_for("Our village Listen to the tape then answer this question.")
                .contains(&"instruction".to_string())
        );
    }

    fn mk_entry(id: &str, level: u8, tags: &[&str]) -> BankEntry {
        BankEntry {
            id: id.into(),
            corpus: "nce".into(),
            kind: "read_aloud".into(),
            ref_audio: "native".into(),
            exam: None,
            book: "NCE2".into(),
            lesson: "x".into(),
            seq: 1,
            text: "x".into(),
            zh: String::new(),
            t0: 0.0,
            t1: 1.0,
            dur: 1.0,
            words: 5,
            wpm: 100.0,
            level,
            src: "x".into(),
            ref_slow: None,
            quality: Quality {
                line_sim: 1.0,
                asr: "t".into(),
                band_limited: false,
                lrc_fit_scale: 1.0,
                speech_ratio: 1.0,
            },
            tags: tags.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn legacy_entries_get_new_fields() {
        // 旧索引入口（S0c 之前写的行）只有 id/src，其余字段缺省
        let mut e: BankEntry = serde_json::from_str(
            r#"{"id":"nce2/02/003","book":"NCE2","lesson":"02","seq":3,"text":"I never get up early on Sundays.",
                "zh":"","t0":1.0,"t1":2.0,"dur":1.0,"words":0,"wpm":0.0,"level":2,
                "src":"bank/nce2/02/003.ref.wav","ref_slow":null,
                "quality":{"line_sim":1.0,"asr":"sense-voice","band_limited":false,"lrc_fit_scale":1.0,"speech_ratio":0.9},
                "tags":["v_to_w"]}"#,
        )
        .unwrap();
        normalize_entry(&mut e);
        assert_eq!(e.corpus, "nce");
        assert_eq!(e.kind, "read_aloud");
        assert_eq!(e.ref_audio, "native");
        assert_eq!(e.words, 7, "words=0 时应按文本回填");
    }

    #[test]
    fn import_prompts_validates_and_dedupes() {
        let tmp = std::env::temp_dir().join(format!("voice_bank_import_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("bank")).unwrap();

        let docs = vec![
            serde_json::json!({"id":"ielts/custom/x","part":1,"topics":["hometown"],
                "text":"Where is your hometown?","level":2}),
            serde_json::json!({"id":"ielts/custom/x","part":1,"text":"重复 id 应被跳过"}),
        ];
        let (added, skipped) = import_prompts(&tmp, &docs).unwrap();
        assert_eq!((added, skipped), (1, 1));

        let idx = read_index(&tmp).unwrap();
        assert_eq!(idx[0].corpus, "ielts");
        assert_eq!(idx[0].kind, "prompt");
        assert_eq!(idx[0].ref_audio, "tts");
        assert_eq!(idx[0].exam.as_ref().unwrap().part, Some(1));
        assert!(idx[0].tags.contains(&"question_rise".to_string()));

        // 空 text / 非法 part 必须报错，而不是静默入库
        assert!(import_prompts(&tmp, &[serde_json::json!({"text":"   "})]).is_err());
        assert!(
            import_prompts(
                &tmp,
                &[serde_json::json!({"id":"ielts/custom/y","part":9,"text":"bad part"})]
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn pick_filters_excludes_and_ranks() {
        let tmp = std::env::temp_dir().join(format!("voice_bank_pick_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("bank")).unwrap();
        let entries = vec![
            mk_entry("nce2/01/001", 2, &["theta_to_s"]),
            mk_entry("nce2/01/002", 1, &["theta_to_s", "final_consonant_drop"]),
            mk_entry("nce2/01/003", 1, &["v_to_w"]),
            mk_entry("nce2/01/004", 1, &["theta_to_s", "fragment"]),
            mk_entry("nce2/01/005", 3, &["lesson_header"]),
        ];
        let mut body = String::new();
        for e in &entries {
            body.push_str(&serde_json::to_string(e).unwrap());
            body.push('\n');
        }
        std::fs::write(index_path(&tmp), body).unwrap();

        // 全库取材：默认排除 fragment / lesson_header
        let all = pick(&tmp, &PickQuery::default()).unwrap();
        assert_eq!(all.len(), 3, "fragment 与 lesson_header 默认被排除");

        // 按 focus 两个 tag 取材：命中两个的排第一
        let q = PickQuery {
            tags: vec!["theta_to_s".into(), "final_consonant_drop".into()],
            ..Default::default()
        };
        let got = pick(&tmp, &q).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "nce2/01/002", "命中两个 tag 的应排第一");

        // 难度过滤
        let q1 = PickQuery {
            level: Some(1),
            ..Default::default()
        };
        assert_eq!(pick(&tmp, &q1).unwrap().len(), 2);

        // 语料过滤：全是 NCE2，没有 ielts
        let qc = PickQuery {
            corpus: Some("ielts".into()),
            ..Default::default()
        };
        assert_eq!(pick(&tmp, &qc).unwrap().len(), 0);
        let qc2 = PickQuery {
            corpus: Some("nce".into()),
            ..Default::default()
        };
        assert_eq!(pick(&tmp, &qc2).unwrap().len(), 3);

        // seed 可复现
        let qs = PickQuery {
            seed: Some(42),
            ..Default::default()
        };
        let a: Vec<String> = pick(&tmp, &qs).unwrap().into_iter().map(|e| e.id).collect();
        let b: Vec<String> = pick(&tmp, &qs).unwrap().into_iter().map(|e| e.id).collect();
        assert_eq!(a, b);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn weighted_pick_prefers_high_rate_tags() {
        // theta_to_s 权重 0.0770 > s_ending+ed_ending+n_to_ng (0.0194+0.0192+0.0161)
        let q = PickQuery {
            tags: vec![
                "theta_to_s".into(),
                "s_ending".into(),
                "ed_ending".into(),
                "n_to_ng".into(),
            ],
            weighted: true,
            ..Default::default()
        };
        assert!(
            tag_weight("theta_to_s")
                > tag_weight("s_ending") + tag_weight("ed_ending") + tag_weight("n_to_ng")
        );
        // 没证据的 tag 权重必须是 0（否则等于编造依据）
        assert_eq!(tag_weight("question_rise"), 0.0);
        assert_eq!(tag_weight("linking_missing"), 0.0);
        assert!(q.weighted);
    }

    #[test]
    fn tag_weights_match_derived_artifact() {
        // docs/tag-weights.json 由 tools/derive_tag_weights.py 从 so762 生成；
        // 这里有漂移就说明"代码里的权重"和"数据推导出的权重"不一致。
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/tag-weights.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读不到 {}: {e}", path.display()));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let tags = v["tags"].as_object().expect("tags 字段");
        for (tag, w, _n) in TAG_WEIGHTS {
            let got = tags
                .get(*tag)
                .and_then(|x| x["weight"].as_f64())
                .unwrap_or_else(|| panic!("artifact 里缺 tag {tag}"));
            assert!(
                (got - *w as f64).abs() < 1e-4,
                "{tag}: 代码 {w} vs artifact {got}"
            );
        }
    }

    #[test]
    fn short_lines_get_merged() {
        let lines = vec![
            lrc::Line {
                t0: 0.0,
                t1: 1.0,
                en: "Yes.".into(),
                zh: "是的。".into(),
            },
            lrc::Line {
                t0: 1.0,
                t1: 2.0,
                en: "No.".into(),
                zh: "不。".into(),
            },
            lrc::Line {
                t0: 2.0,
                t1: 4.0,
                en: "I never get up early.".into(),
                zh: "我从不早起。".into(),
            },
            lrc::Line {
                t0: 4.0,
                t1: 6.0,
                en: "It was Sunday.".into(),
                zh: "".into(),
            },
        ];
        let m = merge_short_lines(&lines);
        // "Yes." + "No." 只有 2 词 → 继续吞并到 "I never get up early."；"It was Sunday." 自身够 3 词
        assert_eq!(m.len(), 2);
        assert!(m[0].en.starts_with("Yes. No. I never get up early."));
        assert!((m[0].t1 - 4.0).abs() < 1e-3);
        assert_eq!(m[1].en, "It was Sunday.");
    }

    #[test]
    fn fit_detects_mismatch() {
        // 一致
        let good: Vec<(f32, f32)> = (0..8)
            .map(|i| (i as f32 * 10.0, i as f32 * 10.0 + 0.1))
            .collect();
        let f = linear_fit(&good).unwrap();
        assert!(f.consistent);
        // 整条音轨时间轴差 28%
        let bad: Vec<(f32, f32)> = (0..8).map(|i| (i as f32 * 10.0, i as f32 * 12.8)).collect();
        let f = linear_fit(&bad).unwrap();
        assert!(!f.consistent);
        assert!((f.scale - 1.28).abs() < 0.01);
    }
}
