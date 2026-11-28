//! Readiness 自动评估（D48 / N6）：从 `learner.json.history` 算出 `readiness`。
//!
//! ## 为什么需要
//!
//! `docs/levels.md` 的 L2/L3/L4 判据早就写在文档里，但代码只**消费** `readiness.free_speech_enabled`
//! 这个布尔值，`stage` / `missing` 全靠 Skill 手写。结果是：判据漂移没人发现，
//! `missing` 与真实历史不一致，`bank plan` 的提示话术跟着失真。
//!
//! 这里把判据**编码**成可复算的函数，Skill 只负责"确认/解释"，不负责算。
//!
//! ## 口径（与 levels.md §2/§5 一一对应）
//!
//! | 判据 | 取值 |
//! |---|---|
//! | 连续 N 条 | `gate.consecutive_n`（默认 3） |
//! | acc 下限 | `gate.min_consecutive_acc`（默认 0.90） |
//! | 语速带 | `gate.rate_wpm`（默认 120–160） |
//! | 停顿上限 | `gate.pause_per_30s_max`（默认 6 / 30 s） |
//! | 自由说 | `gate.free_speech_pause_per_45s_max` / `repetitions_max`（默认 8 / 1） |
//!
//! ## 三条诚实原则
//!
//! 1. **只用"句子级跟读/朗读"**：`scene` 为 `word`（单词 drill）和 `free`（无稿）
//!    不参与 L3 的连续 acc 统计——单词 drill 的 acc 与句子跟读不可比，无稿没有 accuracy。
//! 2. **缺数据 = `unknown`，不当成通过**：例如没记 `pause_count` 的条目既不能证"停顿达标"，
//!    也不能证"没达标"。报告里 `unknown` 单独列，`missing` 只收**明确未达标**的项。
//! 3. **不自动开自由说**：`free_speech_enabled` 由用户确认（`--confirm-free-speech`），
//!    L4 的"用户同意"这一条机器无法判断。
//!
//! 数据结构刻意用 `serde_json::Value` 读：`learner.json` 的 schema 还在演进（M8/D27），
//! 这里只认自己需要的字段，缺字段就 `unknown`，不跟着 schema 一起崩。

use serde::Serialize;
use serde_json::Value;

/// 就绪门槛（可被 `learner.json.readiness.gate` 覆盖）。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Gate {
    pub min_consecutive_acc: f32,
    pub consecutive_n: usize,
    pub rate_wpm: [f32; 2],
    pub pause_per_30s_max: f32,
    pub free_speech_pause_per_45s_max: f32,
    pub free_speech_repetitions_max: u32,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            min_consecutive_acc: 0.90,
            consecutive_n: 3,
            rate_wpm: [120.0, 160.0],
            pause_per_30s_max: 6.0,
            free_speech_pause_per_45s_max: 8.0,
            free_speech_repetitions_max: 1,
        }
    }
}

impl Gate {
    /// 从 `learner.json.readiness.gate` 读（缺字段用默认值）。
    pub fn from_json(v: Option<&Value>) -> Self {
        let mut g = Gate::default();
        let Some(v) = v else { return g };
        if let Some(x) = v.get("min_consecutive_acc").and_then(Value::as_f64) {
            g.min_consecutive_acc = x as f32;
        }
        if let Some(x) = v.get("consecutive_n").and_then(Value::as_u64) {
            g.consecutive_n = (x as usize).max(1);
        }
        if let Some(arr) = v.get("rate_wpm").and_then(Value::as_array)
            && arr.len() == 2
            && let (Some(a), Some(b)) = (arr[0].as_f64(), arr[1].as_f64())
        {
            g.rate_wpm = [a as f32, b as f32];
        }
        if let Some(x) = v.get("pause_per_30s_max").and_then(Value::as_f64) {
            g.pause_per_30s_max = x as f32;
        }
        if let Some(x) = v
            .get("free_speech_pause_per_45s_max")
            .and_then(Value::as_f64)
        {
            g.free_speech_pause_per_45s_max = x as f32;
        }
        if let Some(x) = v.get("free_speech_repetitions_max").and_then(Value::as_u64) {
            g.free_speech_repetitions_max = x as u32;
        }
        g
    }
}

/// 三态判据结果（`None` = 数据不足，既不算过也不算不过）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Met,
    NotMet,
    Unknown,
}

/// 一条判据的评估结果。
#[derive(Debug, Clone, Serialize)]
pub struct Criterion {
    pub id: String,
    pub label: String,
    pub verdict: Verdict,
    /// 为什么是这个结论（人读得懂的一句话）
    pub detail: String,
}

impl Criterion {
    fn new(id: &str, label: &str, verdict: Verdict, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            verdict,
            detail: detail.into(),
        }
    }
}

/// 从历史里摘出来的一条"句子级跟读/朗读"。
#[derive(Debug, Clone, Serialize)]
pub struct RecentItem {
    pub date: String,
    pub scene: String,
    pub accuracy: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_rate: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pause_per_30s: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// 评估结果（可直接写回 `learner.json.readiness`）。
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// `L0` / `L1` / `L2` / `L2+` / `L3` / `L4`
    pub stage: String,
    pub name: String,
    /// 门槛是否全部满足（含 unknown 也算通过；`free_speech_enabled` 仍需用户确认）
    pub l3_met: bool,
    /// 建议值：**不自动生效**，需用户确认后写 `true`
    pub free_speech_ready: bool,
    pub gate: Gate,
    /// 参与统计的最近 N 条（句子级跟读/朗读，按时间正序）
    pub recent: Vec<RecentItem>,
    /// 连续达标的条数（从最近一条往前数）
    pub consecutive_met: usize,
    pub criteria: Vec<Criterion>,
    /// 明确未达标项（给 `readiness.missing` 用）
    pub missing: Vec<String>,
    /// 数据不足、需要人工/Skill 判读的项
    pub unknown: Vec<String>,
    /// 参考数字（evidence）
    pub recent_acc: Vec<f32>,
    pub recent_rate: Vec<f32>,
    /// 版本标记（写进 JSON 便于回溯算法）
    pub version: String,
}

pub const READINESS_VERSION: &str = "readiness-0.1";

/// `scene` 是否算"句子级跟读/朗读"（`word` 单词 drill 与 `free` 无稿不算）。
fn is_sentence_scene(scene: &str) -> bool {
    let s = scene.to_ascii_lowercase();
    !(s == "word" || s == "free" || s.contains("word"))
}

fn num(v: &Value, key: &str) -> Option<f32> {
    v.get(key).and_then(Value::as_f64).map(|x| x as f32)
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// 停顿密度：`pause_count / (speech_duration / 30)`；两个字段缺一个就 `None`。
fn pause_per_30s(v: &Value) -> Option<f32> {
    let n = num(v, "pause_count")?;
    let dur = num(v, "speech_duration")?;
    (dur > 0.0).then(|| n / (dur / 30.0))
}

/// 评估 `learner.json`（整体 JSON）。
pub fn evaluate(learner: &Value) -> Report {
    let gate = Gate::from_json(learner.get("readiness").and_then(|r| r.get("gate")));
    let history = learner
        .get("history")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // 1) 句子级跟读/朗读（保持时间正序，history 是追加写的）
    let sentence_items: Vec<&Value> = history
        .iter()
        .filter(|h| s(h, "scene").is_some_and(|sc| is_sentence_scene(&sc)))
        .filter(|h| num(h, "accuracy").is_some())
        .collect();

    let n = gate.consecutive_n.min(sentence_items.len().max(1));
    let tail = &sentence_items[sentence_items.len().saturating_sub(n)..];
    let recent: Vec<RecentItem> = tail
        .iter()
        .map(|h| RecentItem {
            date: s(h, "date").unwrap_or_default(),
            scene: s(h, "scene").unwrap_or_default(),
            accuracy: num(h, "accuracy").unwrap_or(0.0),
            speech_rate: num(h, "speech_rate"),
            pause_per_30s: pause_per_30s(h).map(|x| crate::round_to(x, 1)),
            session: s(h, "session"),
        })
        .collect();

    // 2) 连续达标条数（从最近一条往前数；acc 达标 + 语速在带内）
    let mut consecutive = 0usize;
    for item in recent.iter().rev() {
        let acc_ok = item.accuracy >= gate.min_consecutive_acc;
        let rate_ok = item
            .speech_rate
            .is_some_and(|r| r >= gate.rate_wpm[0] && r <= gate.rate_wpm[1]);
        if acc_ok && rate_ok {
            consecutive += 1;
        } else {
            break;
        }
    }

    // 3) 判据
    let mut criteria = Vec::new();

    let acc_ok = recent.len() >= gate.consecutive_n
        && recent
            .iter()
            .all(|x| x.accuracy >= gate.min_consecutive_acc);
    let acc_detail = if recent.is_empty() {
        "history 里没有带 accuracy 的句子级记录".to_string()
    } else {
        format!(
            "最近 {} 条 acc = {}（需 ≥ {:.2}）",
            recent.len(),
            recent
                .iter()
                .map(|x| format!("{:.3}", x.accuracy))
                .collect::<Vec<_>>()
                .join(" / "),
            gate.min_consecutive_acc
        )
    };
    criteria.push(Criterion::new(
        "consecutive_acc",
        "连续 N 条跟读 acc 达标",
        if recent.len() < gate.consecutive_n {
            Verdict::NotMet
        } else if acc_ok {
            Verdict::Met
        } else {
            Verdict::NotMet
        },
        acc_detail,
    ));

    let missing_rate: Vec<&RecentItem> =
        recent.iter().filter(|x| x.speech_rate.is_none()).collect();
    let rate_bad: Vec<&RecentItem> = recent
        .iter()
        .filter(|x| {
            x.speech_rate
                .is_some_and(|r| r < gate.rate_wpm[0] || r > gate.rate_wpm[1])
        })
        .collect();
    let rate_verdict = if recent.is_empty() || missing_rate.len() == recent.len() {
        Verdict::Unknown
    } else if missing_rate.is_empty() && rate_bad.is_empty() {
        Verdict::Met
    } else {
        Verdict::NotMet
    };
    criteria.push(Criterion::new(
        "rate_band",
        "语速落在目标带",
        rate_verdict,
        if recent.is_empty() {
            "没有可用的语速记录".to_string()
        } else {
            format!(
                "最近 {} 条语速 = {}（目标 {:.0}–{:.0} wpm{}）",
                recent.len(),
                recent
                    .iter()
                    .map(|x| x
                        .speech_rate
                        .map(|r| format!("{r:.0}"))
                        .unwrap_or_else(|| "缺".into()))
                    .collect::<Vec<_>>()
                    .join(" / "),
                gate.rate_wpm[0],
                gate.rate_wpm[1],
                if missing_rate.is_empty() {
                    String::new()
                } else {
                    format!("，{} 条缺语速", missing_rate.len())
                }
            )
        },
    ));

    let pause_known: Vec<&RecentItem> = recent
        .iter()
        .filter(|x| x.pause_per_30s.is_some())
        .collect();
    let pause_bad: Vec<&RecentItem> = pause_known
        .iter()
        .copied()
        .filter(|x| x.pause_per_30s.is_some_and(|p| p > gate.pause_per_30s_max))
        .collect();
    let pause_verdict = if pause_known.is_empty() {
        Verdict::Unknown
    } else if pause_bad.is_empty() {
        Verdict::Met
    } else {
        Verdict::NotMet
    };
    criteria.push(Criterion::new(
        "pause_density",
        "停顿密度达标",
        pause_verdict,
        if pause_known.is_empty() {
            "history 里没有 pause_count + speech_duration，无法计算停顿/30 s".to_string()
        } else {
            format!(
                "可算停顿的 {} 条 = {} 次/30 s（上限 {:.0}）",
                pause_known.len(),
                pause_known
                    .iter()
                    .map(|x| format!("{:.1}", x.pause_per_30s.unwrap_or(0.0)))
                    .collect::<Vec<_>>()
                    .join(" / "),
                gate.pause_per_30s_max
            )
        },
    ));

    // 核心音素回归：需要 GOP 混淆对判读，机器不猜（D48 明确保留人工）
    criteria.push(Criterion::new(
        "core_phonemes",
        "核心音素（θ / v / 词尾）无回归",
        Verdict::Unknown,
        "需 Skill 判读最近 3 条的 `gop_words` / `note` / 混淆对；代码不做断言（避免把 ASR 误听当发音错）",
    ));

    // 自由说（L4）：需要自由说样本的停顿/重复数
    let free_items: Vec<&Value> = history
        .iter()
        .filter(|h| s(h, "scene").is_some_and(|sc| sc.eq_ignore_ascii_case("free")))
        .collect();
    let last_free = free_items.last().copied();
    let free_verdict = match last_free {
        None => Verdict::Unknown,
        Some(v) => {
            let reps = num(v, "repetitions");
            let pauses = pause_per_30s(v).map(|p| p * 1.5); // 30 s → 45 s 口径
            match (reps, pauses) {
                (Some(r), Some(p)) => {
                    if r <= gate.free_speech_repetitions_max as f32
                        && p <= gate.free_speech_pause_per_45s_max
                    {
                        Verdict::Met
                    } else {
                        Verdict::NotMet
                    }
                }
                _ => Verdict::Unknown,
            }
        }
    };
    criteria.push(Criterion::new(
        "free_speech",
        "45–60 s 无稿独白（停顿 / 重复）",
        free_verdict,
        match last_free {
            None => "history 里没有 scene=free 的自由说样本".to_string(),
            Some(v) => format!(
                "最近一条自由说 {}：pause_count={} repetitions={}（需 ≤ {:.0} 次/45 s、≤ {} 次重复；缺字段则 unknown）",
                s(v, "date").unwrap_or_default(),
                num(v, "pause_count")
                    .map(|x| format!("{x:.0}"))
                    .unwrap_or_else(|| "缺".into()),
                num(v, "repetitions")
                    .map(|x| format!("{x:.0}"))
                    .unwrap_or_else(|| "缺".into()),
                gate.free_speech_pause_per_45s_max,
                gate.free_speech_repetitions_max
            ),
        },
    ));

    // 4) 阶段判定
    let hard = |id: &str| -> Verdict {
        criteria
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.verdict)
            .unwrap_or(Verdict::Unknown)
    };
    let l3_met = hard("consecutive_acc") == Verdict::Met
        && hard("rate_band") != Verdict::NotMet
        && hard("pause_density") != Verdict::NotMet
        && hard("core_phonemes") != Verdict::NotMet;
    let l4_met = l3_met && hard("free_speech") == Verdict::Met;

    let best_acc = recent.iter().map(|x| x.accuracy).fold(0.0f32, f32::max);
    let (stage, name) = if l4_met {
        ("L4", "自由说就绪（需用户确认后开启）")
    } else if l3_met {
        ("L3", "半自由：可做 30–45 s 提纲独白")
    } else if best_acc >= 0.80 {
        ("L2+", "跟读稳定（半自由未满）")
    } else if best_acc >= 0.50 {
        ("L1", "基础：短句跟读")
    } else {
        ("L0", "起步：单词 / 短词组")
    };

    // 5) missing（只收明确未达标）/ unknown（数据不足）
    let mut missing = Vec::new();
    let mut unknown = Vec::new();
    for c in &criteria {
        match c.verdict {
            Verdict::NotMet => missing.push(format!("{}：{}", c.label, c.detail)),
            Verdict::Unknown => unknown.push(format!("{}：{}", c.label, c.detail)),
            Verdict::Met => {}
        }
    }

    Report {
        stage: stage.into(),
        name: name.into(),
        l3_met,
        free_speech_ready: l4_met,
        gate,
        recent: recent.clone(),
        consecutive_met: consecutive,
        criteria,
        missing,
        unknown,
        recent_acc: recent.iter().map(|x| x.accuracy).collect(),
        recent_rate: recent.iter().filter_map(|x| x.speech_rate).collect(),
        version: READINESS_VERSION.into(),
    }
}

/// 把评估结果写回 `learner.json`（只动 `readiness`，其他字段原样保留）。
///
/// `confirm_free_speech`：
/// - `false` → `free_speech_enabled` **保持原值**（开自由说必须用户确认，见 levels.md §5）；
/// - `true` 且 `l4_met` → 置 `true`；`true` 但未达标 → 拒绝（返回 `false`，调用方要提示）。
///
/// 返回是否真的开了自由说。
pub fn apply(learner: &mut Value, report: &Report, confirm_free_speech: bool) -> bool {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let readiness = learner
        .as_object_mut()
        .map(|o| o.entry("readiness").or_insert(Value::Null));
    let Some(readiness) = readiness else {
        return false;
    };
    if !readiness.is_object() {
        *readiness = serde_json::json!({});
    }
    let obj = readiness.as_object_mut().expect("刚保证是对象");
    let was_enabled = obj
        .get("free_speech_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let enable = was_enabled || (confirm_free_speech && report.free_speech_ready);

    obj.insert("stage".into(), Value::from(report.stage.clone()));
    obj.insert("name".into(), Value::from(report.name.clone()));
    obj.insert("free_speech_enabled".into(), Value::from(enable));
    obj.insert("updated".into(), Value::from(today));
    obj.insert("computed_by".into(), Value::from(report.version.clone()));
    obj.insert(
        "gate".into(),
        serde_json::to_value(report.gate).unwrap_or(Value::Null),
    );
    let mut evidence = serde_json::json!({
        "recent_acc": report.recent_acc,
        "recent_rate": report.recent_rate,
        "consecutive_met": report.consecutive_met,
        "criteria": report.criteria,
    });
    // 保留历史 evidence 里人工写的东西（如 theta/v/contractions 的文字结论）
    if let Some(old) = obj.get("evidence").and_then(Value::as_object) {
        for (k, v) in old.clone() {
            evidence
                .as_object_mut()
                .expect("json 宏给的是对象")
                .entry(k)
                .or_insert(v);
        }
    }
    obj.insert("evidence".into(), evidence);
    obj.insert(
        "missing".into(),
        serde_json::to_value(&report.missing).unwrap_or(Value::Null),
    );
    obj.insert(
        "unknown".into(),
        serde_json::to_value(&report.unknown).unwrap_or(Value::Null),
    );
    enable && (!was_enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 造一条句子级跟读记录
    fn item(acc: f32, rate: f32) -> Value {
        json!({"date":"2026-11-28","scene":"bank","accuracy":acc,"speech_rate":rate})
    }

    fn learner(history: Vec<Value>) -> Value {
        json!({"history": history, "readiness": {"stage":"L2+","free_speech_enabled":false}})
    }

    #[test]
    fn three_clean_reads_reach_l3() {
        let l = learner(vec![
            item(0.92, 130.0),
            item(0.95, 140.0),
            item(0.91, 150.0),
        ]);
        let r = evaluate(&l);
        assert_eq!(r.stage, "L3", "{:?}", r.missing);
        assert!(r.l3_met);
        assert_eq!(r.consecutive_met, 3);
        assert!(!r.free_speech_ready, "没有自由说样本 → 不能到 L4");
        // L3 达标但 L4 未达标时，missing 里只有自由说那条以外的明确项
        assert!(
            r.unknown.iter().any(|u| u.contains("核心音素")),
            "核心音素必须显式标为 unknown: {:?}",
            r.unknown
        );
    }

    #[test]
    fn one_low_accuracy_keeps_l2_plus() {
        let l = learner(vec![
            item(1.00, 140.0),
            item(0.80, 128.0),
            item(0.875, 99.9),
        ]);
        let r = evaluate(&l);
        assert_eq!(r.stage, "L2+");
        assert!(!r.l3_met);
        assert_eq!(r.consecutive_met, 0, "最近一条不达标 → 连续 0");
        assert!(
            r.missing.iter().any(|m| m.contains("连续")),
            "{:?}",
            r.missing
        );
    }

    #[test]
    fn rate_out_of_band_blocks_l3() {
        // acc 全达标，但语速 208 越界（实测踩过：208 wpm 时 /θ/ 失守）
        let l = learner(vec![
            item(0.95, 208.0),
            item(0.95, 140.0),
            item(0.95, 144.0),
        ]);
        let r = evaluate(&l);
        assert_eq!(r.stage, "L2+");
        assert!(!r.l3_met);
        assert!(
            r.missing.iter().any(|m| m.contains("语速")),
            "{:?}",
            r.missing
        );
    }

    #[test]
    fn word_and_free_scenes_are_ignored_for_streak() {
        // 单词 drill 的 acc=1.0 不能拿来凑 L3 连续；free 没有 accuracy 也不算
        let l = learner(vec![
            json!({"date":"2026-11-28","scene":"word","accuracy":1.0}),
            json!({"date":"2026-11-28","scene":"free","accuracy":null,"pause_count":8}),
            item(1.0, 144.0),
        ]);
        let r = evaluate(&l);
        assert_eq!(r.recent.len(), 1, "只应剩 1 条句子级记录");
        assert_eq!(r.stage, "L2+");
        assert!(!r.l3_met);
        assert!(
            r.criteria
                .iter()
                .find(|c| c.id == "consecutive_acc")
                .is_some_and(|c| c.verdict == Verdict::NotMet)
        );
    }

    #[test]
    fn missing_pause_data_is_unknown_not_failure() {
        let l = learner(vec![
            item(0.95, 130.0),
            item(0.95, 140.0),
            item(0.95, 150.0),
        ]);
        let r = evaluate(&l);
        let pause = r
            .criteria
            .iter()
            .find(|c| c.id == "pause_density")
            .expect("应有停顿判据");
        assert_eq!(pause.verdict, Verdict::Unknown);
        assert!(r.l3_met, "unknown 不能阻断 L3（也不能当成通过去开自由说）");
        assert!(!r.free_speech_ready);
        assert!(
            r.unknown.iter().any(|u| u.contains("停顿")),
            "{:?}",
            r.unknown
        );
    }

    #[test]
    fn free_speech_sample_can_unlock_l4() {
        let mut h = vec![item(0.95, 130.0), item(0.95, 140.0), item(0.95, 150.0)];
        h.push(json!({"date":"2026-11-28","scene":"free","pause_count":6,
                      "speech_duration":60.0,"repetitions":0}));
        let r = evaluate(&learner(h));
        assert!(r.free_speech_ready, "{:?}", r.criteria);
        assert_eq!(r.stage, "L4");
    }

    #[test]
    fn apply_writes_readiness_and_keeps_manual_evidence() {
        let mut l = json!({
            "history": [item(0.95, 130.0), item(0.95, 140.0), item(0.95, 150.0)],
            "readiness": {
                "stage": "L2+", "free_speech_enabled": false,
                "evidence": {"theta": "144 wpm 句中 theta"}
            },
            "focus": ["语速控制"]
        });
        let r = evaluate(&l);
        let changed = apply(&mut l, &r, false);
        assert!(!changed, "未确认时不得自动开自由说");
        let rd = &l["readiness"];
        assert_eq!(rd["stage"], "L3");
        assert_eq!(rd["free_speech_enabled"], false);
        assert_eq!(rd["computed_by"], READINESS_VERSION);
        assert_eq!(
            rd["evidence"]["theta"], "144 wpm 句中 theta",
            "人工写的 evidence 必须保留"
        );
        assert!(rd["unknown"].is_array());
        assert_eq!(l["focus"][0], "语速控制", "其他字段不能被改");
    }

    #[test]
    fn apply_blocks_free_speech_until_l4() {
        let mut l = learner(vec![
            item(0.95, 130.0),
            item(0.95, 140.0),
            item(0.95, 150.0),
        ]);
        let r = evaluate(&l);
        let changed = apply(&mut l, &r, true);
        assert!(!changed, "L3 未到 L4 时 confirm 也不能强开");
        assert_eq!(l["readiness"]["free_speech_enabled"], false);
    }
}
