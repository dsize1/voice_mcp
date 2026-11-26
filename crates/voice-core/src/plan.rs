//! 定向训练计划（原定路线 P5）：把 `learner.json` 的 `drill_queue` / `error_stats` / `focus`
//! 映射成一次可执行的训练清单。
//!
//! 设计约束（与 ADR-0010 对齐，便于 Route A 以后直接挂上）：
//! - 每条计划都携带 `text`、`tags`、`level`、`ref_audio`，词级时间来自素材库；
//! - 自由说是计划里的固定槽位，明确标注“伪参考 GOP 未启用（ADR-0010）”；
//! - 只做**选材与预算**，不在这里生成教学话术（那属于 Skill）。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::bank::{self, BankEntry, PickQuery};

pub const PLAN_VERSION: &str = "plan-0.1";

/// `learner.json` 中训练计划需要的字段（其余字段忽略，避免和 D27 的 schema 耦合）。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Learner {
    #[serde(default)]
    pub level: Level,
    #[serde(default)]
    pub focus: Vec<String>,
    #[serde(default)]
    pub error_stats: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub drill_queue: Vec<DrillItem>,
    #[serde(default)]
    pub history: Vec<HistoryItem>,
    #[serde(default)]
    pub readiness: Readiness,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Level {
    pub band_estimate: Option<f32>,
    pub confidence: Option<String>,
}

/// 自由说就绪门槛（见 docs/levels.md）。`free_speech_enabled` 由 Skill/用户确认后写入。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Readiness {
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub free_speech_enabled: bool,
    #[serde(default)]
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DrillItem {
    pub bank_id: Option<String>,
    pub text: Option<String>,
    pub reason: Option<String>,
    pub level: Option<u8>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HistoryItem {
    pub bank_id: Option<String>,
    #[serde(default)]
    pub mastered: bool,
}

#[derive(Debug, Clone)]
pub struct PlanRequest {
    pub minutes: f32,
    pub limit: usize,
    pub level: Option<u8>,
    pub seed: Option<u64>,
    pub corpus: Option<String>,
    /// 排除这些语料（`bank plan` 默认排除 `peppa` 这类非训练素材）
    pub exclude_corpora: Vec<String>,
    pub part: Option<u8>,
    /// 是否保留 60 s 无稿自由说槽位（默认 true；用户要求不做时置 false）
    /// None → 按 learner.readiness.free_speech_enabled 自动决定
    pub include_free_speech: Option<bool>,
}

impl Default for PlanRequest {
    fn default() -> Self {
        Self {
            minutes: 15.0,
            limit: 5,
            level: None,
            seed: None,
            corpus: None,
            exclude_corpora: Vec::new(),
            part: None,
            include_free_speech: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanItem {
    pub n: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank_id: Option<String>,
    pub text: String,
    pub zh: String,
    pub level: u8,
    pub wpm: f32,
    pub dur: f32,
    pub tags: Vec<String>,
    pub reason: String,
    /// drill_queue | error_stats | fill
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_audio: Option<String>,
    pub est_min: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FreeSpeechSlot {
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<u8>,
    pub dur_sec: u32,
    /// 自由说没有参考文本；Route A 启用后这里才会出现 pseudo reference
    pub reference: Option<String>,
    /// 固定为 disabled；见 ADR-0010
    pub pseudo_gop: String,
    pub metrics: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrainingPlan {
    pub version: &'static str,
    pub generated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub band_estimate: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    pub target_tags: Vec<String>,
    pub minutes_budget: f32,
    pub minutes_planned: f32,
    pub items: Vec<PlanItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_speech: Option<FreeSpeechSlot>,
    pub notes: Vec<String>,
}

/// 生成计划。纯函数：不读文件、不合成音频、不写证据。
pub fn build(entries: &[BankEntry], learner: &Learner, req: &PlanRequest) -> TrainingPlan {
    let level = req
        .level
        .unwrap_or_else(|| level_from_band(learner.level.band_estimate));
    let mastered: BTreeSet<String> = learner
        .history
        .iter()
        .filter(|h| h.mastered)
        .filter_map(|h| h.bank_id.clone())
        .collect();
    let target_tags = target_tags(learner);
    let mut notes: Vec<String> = Vec::new();

    let mut selected: BTreeSet<String> = BTreeSet::new();
    let mut items: Vec<PlanItem> = Vec::new();
    let mut planned = 0.0f32;
    // 预留 3 min 给自由说槽位（60 s + 反馈），避免素材把预算吃光
    let include_free_speech = req
        .include_free_speech
        .unwrap_or(learner.readiness.free_speech_enabled);
    let reserve = if include_free_speech { 3.0 } else { 0.0 };
    let drill_budget = (req.minutes - reserve).max(1.0);

    // 1) drill_queue 优先
    for d in &learner.drill_queue {
        if items.len() >= req.limit {
            break;
        }
        match resolve(entries, d, level) {
            Some((e, reason)) => {
                if mastered.contains(&e.id) || selected.contains(&e.id) {
                    continue;
                }
                let est = estimate(&e);
                if !items.is_empty() && planned + est > drill_budget {
                    continue;
                }
                selected.insert(e.id.clone());
                planned += est;
                items.push(item_from(&e, items.len() + 1, reason, "drill_queue", est));
            }
            None => {
                if let Some(text) = d.text.as_deref().filter(|t| !t.trim().is_empty()) {
                    let est = estimate_text(text);
                    if !items.is_empty() && planned + est > drill_budget {
                        continue;
                    }
                    planned += est;
                    items.push(PlanItem {
                        n: items.len() + 1,
                        bank_id: None,
                        text: text.trim().to_string(),
                        zh: String::new(),
                        level: d.level.unwrap_or(level),
                        wpm: 0.0,
                        dur: 0.0,
                        tags: bank::tags_for(text),
                        reason: d.reason.clone().unwrap_or_else(|| "drill_queue".into()),
                        source: "drill_queue",
                        ref_audio: None,
                        est_min: est,
                    });
                } else {
                    notes.push(format!(
                        "drill_queue 条目未找到素材：{}",
                        d.bank_id.clone().unwrap_or_else(|| "(无 id)".into())
                    ));
                }
            }
        }
    }

    // 2) 按 error_stats / focus 的 tag 定向选材（每个 tag 至多 1 条，保证多样性）
    for tag in &target_tags {
        if items.len() >= req.limit || planned >= drill_budget {
            break;
        }
        let q = PickQuery {
            level: Some(level),
            tags: vec![tag.clone()],
            corpus: req.corpus.clone(),
            exclude_corpora: req.exclude_corpora.clone(),
            kind: Some("read_aloud".into()),
            part: req.part,
            limit: 12,
            seed: req.seed,
            ..Default::default()
        };
        if let Some(e) = first_available(&q, entries, &selected, &mastered) {
            let est = estimate(&e);
            if !items.is_empty() && planned + est > drill_budget {
                continue;
            }
            selected.insert(e.id.clone());
            planned += est;
            items.push(item_from(
                &e,
                items.len() + 1,
                format!("{} · 权重 {:.4}", tag, bank::tag_weight(tag)),
                "error_stats",
                est,
            ));
        } else {
            notes.push(format!("tag `{tag}` 在 level {level} 没有可用素材"));
        }
    }

    // 3) 补足：先按目标 tags 加权，再按同难度兜底
    if items.len() < req.limit {
        for weighted in [true, false] {
            let q = PickQuery {
                level: Some(level),
                tags: if weighted {
                    target_tags.clone()
                } else {
                    Vec::new()
                },
                corpus: req.corpus.clone(),
                exclude_corpora: req.exclude_corpora.clone(),
                kind: Some("read_aloud".into()),
                part: req.part,
                weighted,
                limit: req.limit + selected.len() + 8,
                seed: req.seed,
                ..Default::default()
            };
            for e in bank::pick_from(entries, &q) {
                if items.len() >= req.limit || planned >= drill_budget {
                    break;
                }
                if mastered.contains(&e.id) || selected.contains(&e.id) {
                    continue;
                }
                let est = estimate(&e);
                if !items.is_empty() && planned + est > drill_budget {
                    continue;
                }
                selected.insert(e.id.clone());
                planned += est;
                items.push(item_from(
                    &e,
                    items.len() + 1,
                    format!("补足 level {level}"),
                    "fill",
                    est,
                ));
            }
            if items.len() >= req.limit || planned >= drill_budget {
                break;
            }
        }
    }

    if items.is_empty() {
        notes.push("没有匹配素材：检查 level / corpus / part 或先跑 bank import".into());
    }
    if target_tags.is_empty() {
        notes.push("error_stats / focus 里没有可映射的 tag，按难度兜底".into());
    }
    if learner.error_stats.contains_key("word_substitution") {
        notes.push(
            "word_substitution 没有对应素材 tag：靠词级 diff/定向重录复核，不参与选材".into(),
        );
    }
    let free_speech = if include_free_speech {
        let slot = free_speech_slot(entries, req, &mut notes);
        // 自由说槽位（60 s + 反馈）也计入预算
        planned += 3.0;
        notes.push("自由说发音筛查（伪参考 GOP）未启用，见 ADR-0010；当前只测语流 + 韵律".into());
        Some(slot)
    } else {
        let stage = learner.readiness.stage.as_deref().unwrap_or("L2+");
        if learner.readiness.missing.is_empty() {
            notes.push(format!(
                "无稿自由说未开启（readiness {stage}），用 --free-speech 可强制试一次"
            ));
        } else {
            notes.push(format!(
                "无稿自由说未开启（readiness {stage}）；未达标：{}",
                learner.readiness.missing.join("；")
            ));
        }
        None
    };

    TrainingPlan {
        version: PLAN_VERSION,
        generated_at: chrono::Local::now().to_rfc3339(),
        band_estimate: learner.level.band_estimate,
        confidence: learner.level.confidence.clone(),
        target_tags,
        minutes_budget: req.minutes,
        minutes_planned: crate::round_to(planned, 2),
        items,
        free_speech,
        notes,
    }
}

impl TrainingPlan {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# 定向训练计划\n\n");
        out.push_str(&format!(
            "- 生成：{}\n- 难度：level {} · band {}（{}）\n- 预算：{} min，预计 {:.1} min\n",
            self.generated_at,
            self.items.first().map(|i| i.level).unwrap_or(0),
            self.band_estimate
                .map(|b| format!("{b:.1}"))
                .unwrap_or_else(|| "?".into()),
            self.confidence.clone().unwrap_or_else(|| "?".into()),
            self.minutes_budget,
            self.minutes_planned
        ));
        if !self.target_tags.is_empty() {
            out.push_str(&format!("- 目标 tags：{}\n", self.target_tags.join(", ")));
        }
        out.push('\n');
        for it in &self.items {
            out.push_str(&format!(
                "## {}. {}（{} / level {}）\n",
                it.n, it.text, it.source, it.level
            ));
            if !it.zh.is_empty() {
                out.push_str(&format!("- 中文：{}\n", it.zh));
            }
            out.push_str(&format!(
                "- {:.0} wpm · {:.1} s · tags: {}\n",
                it.wpm,
                it.dur,
                if it.tags.is_empty() {
                    "—".into()
                } else {
                    it.tags.join(", ")
                }
            ));
            out.push_str(&format!("- 为什么：{}\n", it.reason));
            if let Some(a) = &it.ref_audio {
                out.push_str(&format!("- 原声：{a}\n"));
            }
            out.push_str(&format!("- 预算：{:.1} min\n\n", it.est_min));
        }
        if let Some(fs) = &self.free_speech {
            out.push_str(
                "## 自由说

",
            );
            out.push_str(&format!(
                "- 题目：{}
- 时长：{} s
- 指标：{}
- 伪参考 GOP：{}

",
                fs.prompt,
                fs.dur_sec,
                fs.metrics.join(" / "),
                fs.pseudo_gop
            ));
        }
        if !self.notes.is_empty() {
            out.push_str("## 备注\n\n");
            for n in &self.notes {
                out.push_str(&format!("- {n}\n"));
            }
        }
        out
    }
}

fn item_from(e: &BankEntry, n: usize, reason: String, source: &'static str, est: f32) -> PlanItem {
    PlanItem {
        n,
        bank_id: Some(e.id.clone()),
        text: e.text.clone(),
        zh: e.zh.clone(),
        level: e.level,
        wpm: e.wpm,
        dur: e.dur,
        tags: e.tags.clone(),
        reason,
        source,
        ref_audio: if !e.src.is_empty() {
            Some(e.src.clone())
        } else if e.ref_audio == "tts" {
            Some("tts".into())
        } else {
            None
        },
        est_min: est,
    }
}

fn resolve(entries: &[BankEntry], d: &DrillItem, level: u8) -> Option<(BankEntry, String)> {
    if let Some(id) = &d.bank_id
        && let Some(e) = entries.iter().find(|e| &e.id == id)
    {
        return Some((
            e.clone(),
            d.reason.clone().unwrap_or_else(|| "drill_queue".into()),
        ));
    }
    let text = d.text.as_deref()?.trim().to_lowercase();
    entries
        .iter()
        .find(|e| e.text.trim().to_lowercase() == text)
        .map(|e| {
            (
                e.clone(),
                d.reason
                    .clone()
                    .unwrap_or_else(|| format!("drill_queue（level {level}）")),
            )
        })
}

fn first_available(
    q: &PickQuery,
    entries: &[BankEntry],
    selected: &BTreeSet<String>,
    mastered: &BTreeSet<String>,
) -> Option<BankEntry> {
    bank::pick_from(entries, q)
        .into_iter()
        .find(|e| !selected.contains(&e.id) && !mastered.contains(&e.id))
}

fn estimate(e: &BankEntry) -> f32 {
    // 播示范 + 录音 + 评测，大约是素材时长的 2.5 倍，再留 0.8 min 反馈
    (e.dur.max(1.0) / 60.0) * 2.5 + 1.5
}

fn estimate_text(text: &str) -> f32 {
    let words = text.split_whitespace().count().max(3) as f32;
    (words / 120.0) * 2.5 + 1.5
}

fn level_from_band(band: Option<f32>) -> u8 {
    match band.unwrap_or(6.0) {
        b if b < 5.5 => 1,
        b if b < 6.5 => 2,
        b if b < 7.0 => 3,
        _ => 4,
    }
}

/// `error_stats` 的 key + `focus` 文本里出现的已知 tag，保序去重。
fn target_tags(learner: &Learner) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |t: &str| {
        if known_tag(t) && !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    };
    for k in learner.error_stats.keys() {
        push(k);
    }
    for f in &learner.focus {
        for tok in f.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if tok.len() >= 3 {
                push(&tok.to_ascii_lowercase());
            }
        }
    }
    out
}

fn known_tag(t: &str) -> bool {
    bank::tag_weight(t) > 0.0
        || matches!(
            t,
            "linking_missing" | "question_rise" | "tone_flat" | "l_dark" | "n_to_ng"
        )
}

fn free_speech_slot(
    entries: &[BankEntry],
    req: &PlanRequest,
    notes: &mut Vec<String>,
) -> FreeSpeechSlot {
    let part = req.part.or(Some(1));
    let mut prompts: Vec<&BankEntry> = entries
        .iter()
        .filter(|e| e.kind == "prompt" && e.corpus == "ielts")
        .collect();
    prompts.sort_by(|a, b| a.id.cmp(&b.id));
    let chosen = prompts
        .iter()
        .find(|e| e.exam.as_ref().and_then(|x| x.part) == part)
        .or_else(|| prompts.first())
        .copied();
    let (prompt, p) = match chosen {
        Some(e) => (e.text.clone(), e.exam.as_ref().and_then(|x| x.part)),
        None => {
            notes.push("素材库没有 IELTS prompt，自由说用默认题".into());
            (
                "Describe your hometown briefly. Where is it, what is it like, and what do you like about it?"
                    .to_string(),
                part,
            )
        }
    };
    FreeSpeechSlot {
        prompt,
        part: p,
        dur_sec: 60,
        reference: None,
        pseudo_gop: "disabled (ADR-0010)".into(),
        metrics: vec![
            "speech_rate".into(),
            "pause_count/pause_longest".into(),
            "longest_run_words".into(),
            "prosody".into(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, text: &str, level: u8, tags: &[&str]) -> BankEntry {
        BankEntry {
            id: id.into(),
            corpus: "nce".into(),
            kind: "read_aloud".into(),
            ref_audio: "native".into(),
            exam: None,
            book: "NCE2".into(),
            lesson: "01".into(),
            seq: 1,
            text: text.into(),
            zh: "测试".into(),
            t0: 0.0,
            t1: 2.0,
            dur: 2.0,
            words: text.split_whitespace().count(),
            wpm: 120.0,
            level,
            src: format!("D:/voice_data/bank/{id}.wav"),
            ref_slow: None,
            quality: bank::Quality {
                line_sim: 1.0,
                asr: text.into(),
                band_limited: false,
                lrc_fit_scale: 1.0,
                speech_ratio: 0.9,
            },
            tags: tags.iter().map(|s| s.to_string()).collect(),
            text_source: "lrc".into(),
        }
    }

    fn learner() -> Learner {
        Learner {
            level: Level {
                band_estimate: Some(6.0),
                confidence: Some("medium".into()),
            },
            focus: vec!["独白连续性：v_to_w 与词尾 /z/".into()],
            error_stats: [
                ("theta_to_s".to_string(), serde_json::json!({"count": 1})),
                ("ae_to_e".to_string(), serde_json::json!({"count": 1})),
                (
                    "word_substitution".to_string(),
                    serde_json::json!({"count": 1}),
                ),
            ]
            .into_iter()
            .collect(),
            drill_queue: vec![DrillItem {
                bank_id: Some("nce2/01/001".into()),
                reason: Some("队列优先".into()),
                ..Default::default()
            }],
            history: vec![HistoryItem {
                bank_id: Some("nce2/01/002".into()),
                mastered: true,
            }],
            readiness: Readiness {
                stage: Some("L2+".into()),
                free_speech_enabled: true,
                missing: Vec::new(),
            },
        }
    }

    fn entries() -> Vec<BankEntry> {
        vec![
            entry("nce2/01/001", "Our village is in a valley.", 2, &["v_to_w"]),
            entry(
                "nce2/01/002",
                "I think this is the third time.",
                2,
                &["theta_to_s"],
            ),
            entry(
                "nce2/01/003",
                "He lives in a very small village.",
                2,
                &["v_to_w"],
            ),
            entry(
                "nce2/01/004",
                "Please sit on the seat.",
                2,
                &["ae_to_e", "short_long_vowel"],
            ),
            entry("nce2/01/005", "Thank you very much.", 2, &["theta_to_s"]),
            BankEntry {
                kind: "prompt".into(),
                corpus: "ielts".into(),
                id: "ielts/custom/p1".into(),
                exam: Some(bank::ExamAlignment {
                    part: Some(1),
                    ..Default::default()
                }),
                text: "Where is your hometown?".into(),
                ..entry("ielts/custom/p1", "Where is your hometown?", 2, &[])
            },
        ]
    }

    #[test]
    fn queue_first_and_mastered_excluded() {
        let p = build(
            &entries(),
            &learner(),
            &PlanRequest {
                limit: 4,
                ..Default::default()
            },
        );
        assert_eq!(p.items[0].bank_id.as_deref(), Some("nce2/01/001"));
        assert_eq!(p.items[0].source, "drill_queue");
        assert!(
            !p.items
                .iter()
                .any(|i| i.bank_id.as_deref() == Some("nce2/01/002")),
            "mastered 必须排除"
        );
    }

    #[test]
    fn targets_follow_error_stats_and_focus() {
        let p = build(&entries(), &learner(), &PlanRequest::default());
        assert!(p.target_tags.contains(&"theta_to_s".into()));
        assert!(p.target_tags.contains(&"ae_to_e".into()));
        assert!(p.target_tags.contains(&"v_to_w".into()));
        assert!(!p.target_tags.contains(&"word_substitution".into()));
    }

    #[test]
    fn budget_and_limit_are_respected() {
        let p = build(
            &entries(),
            &learner(),
            &PlanRequest {
                minutes: 3.0,
                limit: 2,
                ..Default::default()
            },
        );
        assert!(p.items.len() <= 2);
        assert!(p.minutes_planned <= 3.0 + 0.01 || p.items.len() == 1);
    }

    #[test]
    fn free_speech_uses_ielts_prompt_and_marks_route_a_disabled() {
        let p = build(&entries(), &learner(), &PlanRequest::default());
        let fs = p.free_speech.as_ref().expect("默认包含自由说槽位");
        assert_eq!(fs.prompt, "Where is your hometown?");
        assert_eq!(fs.pseudo_gop, "disabled (ADR-0010)");
        assert!(fs.reference.is_none());
    }

    #[test]
    fn inline_queue_text_without_bank_id_still_planned() {
        let mut l = learner();
        l.drill_queue = vec![DrillItem {
            text: Some("A custom sentence for practice.".into()),
            reason: Some("inline".into()),
            ..Default::default()
        }];
        let p = build(&entries(), &l, &PlanRequest::default());
        assert_eq!(p.items[0].text, "A custom sentence for practice.");
        assert!(p.items[0].bank_id.is_none());
    }

    #[test]
    fn markdown_contains_free_speech_and_items() {
        let md = build(&entries(), &learner(), &PlanRequest::default()).to_markdown();
        assert!(md.contains("定向训练计划"));
        assert!(md.contains("Our village is in a valley."));
        assert!(md.contains("disabled (ADR-0010)"));
    }

    #[test]
    fn no_free_speech_omits_slot() {
        let p = build(
            &entries(),
            &learner(),
            &PlanRequest {
                include_free_speech: Some(false),
                minutes: 10.0,
                ..Default::default()
            },
        );
        assert!(p.free_speech.is_none());
        assert!(p.notes.iter().any(|n| n.contains("无稿自由说未开启")));
        assert!(p.minutes_planned <= 10.0 + 0.01);
    }

    #[test]
    fn readiness_gate_controls_free_speech_by_default() {
        let mut l = learner();
        l.readiness.free_speech_enabled = false;
        l.readiness.missing = vec!["连续 3 条 acc≥0.90".into()];
        let p = build(&entries(), &l, &PlanRequest::default());
        assert!(p.free_speech.is_none());
        assert!(p.notes.iter().any(|n| n.contains("连续 3 条 acc≥0.90")));

        l.readiness.free_speech_enabled = true;
        let p = build(&entries(), &l, &PlanRequest::default());
        assert!(p.free_speech.is_some());
    }

    #[test]
    fn empty_bank_is_safe() {
        let p = build(&[], &learner(), &PlanRequest::default());
        assert!(p.items.is_empty());
        assert!(!p.notes.is_empty());
        assert!(!p.free_speech.as_ref().unwrap().prompt.is_empty());
    }
}
