//! 录音回归用例（golden cases）—— 用真实录音锁住评测行为。
//!
//! 用例是 JSON，**只存断言不存音频**（音频留在本机，避免把个人声音提交进仓库）。
//! 音频路径支持 `${VOICE_CASES_DIR}` 前缀，便于换机器时只改一个环境变量。
//!
//! ```json
//! {
//!   "id": "r01_third_16k",
//!   "audio": "${VOICE_CASES_DIR}/test.m4a",
//!   "reference": "I think this is the third time.",
//!   "lang": "en",
//!   "expect": {
//!     "accuracy": { "gte": 0.80, "lte": 0.90 },
//!     "timestamp_alignment": "exact",
//!     "band_limited": true,
//!     "issue_count": { "gte": 1 },
//!     "issues": [
//!       { "kind": "substitution", "reference": "third", "said": ["straight", "three"], "hint": "unclear" }
//!     ]
//!   }
//! }
//! ```
//!
//! `said` / `hint` / `severity` 可以写字符串，也可以写**等价集合**（数组，命中任一即算过）。
//!
//! 为什么要集合（D56）：ASR 输出会随模型/语言锁定/归一化演进漂移——
//! `third` 从被听成 `straight` 变成 `three`、`Lesson2` 从粘连一个 token 变成 `lesson` + `two`，
//! 这些都是**同一个现象的两种写法**，不是行为退化。锁死单字符串会让门禁长期假红，
//! 最后没人看门禁。写集合的规则：集合里每一项都必须能用"同一个语义"解释（例如都是 /θ/ 丢失）。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::assess::Assessment;

#[derive(Debug, Deserialize)]
pub struct Case {
    pub id: String,
    pub audio: String,
    pub reference: Option<String>,
    /// 参考文本文件（长文本用这个；相对路径同样支持 ${VOICE_CASES_DIR}）
    #[serde(default)]
    pub reference_file: Option<String>,
    #[serde(default = "default_lang")]
    pub lang: String,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub expect: Expect,
    /// 备注：这条用例在验证什么
    #[serde(default)]
    pub note: Option<String>,
}

fn default_lang() -> String {
    "en".into()
}

#[derive(Debug, Deserialize, Default)]
pub struct Expect {
    pub accuracy: Option<Range>,
    pub speech_rate: Option<Range>,
    pub articulation_rate: Option<Range>,
    pub issue_count: Option<Range>,
    pub timestamp_alignment: Option<String>,
    pub band_limited: Option<bool>,
    pub degraded: Option<bool>,
    /// 必须存在的 issue（子集匹配，顺序无关）
    #[serde(default)]
    pub issues: Vec<ExpectIssue>,
    /// 必须**不存在**的 issue（防止误报）
    #[serde(default)]
    pub absent_issues: Vec<ExpectIssue>,
}

/// 断言值：单个字符串，或**等价集合**（命中任一即算过，见 D56）。
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn matches(&self, got: Option<&str>) -> bool {
        let Some(g) = got else {
            return false;
        };
        match self {
            OneOrMany::One(s) => s == g,
            OneOrMany::Many(v) => v.iter().any(|s| s == g),
        }
    }

    fn describe(&self) -> String {
        match self {
            OneOrMany::One(s) => s.clone(),
            OneOrMany::Many(v) => format!("[{}]", v.join(" | ")),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ExpectIssue {
    pub kind: String,
    pub reference: Option<String>,
    /// 参考文本文件（长文本用这个；相对路径同样支持 ${VOICE_CASES_DIR}）
    #[serde(default)]
    pub reference_file: Option<String>,
    /// 识别出的词 / `hint` / `severity`：可写字符串或等价集合
    pub said: Option<OneOrMany>,
    pub hint: Option<OneOrMany>,
    pub severity: Option<OneOrMany>,
}

#[derive(Debug, Deserialize)]
pub struct Range {
    pub gte: Option<f64>,
    pub lte: Option<f64>,
}

impl Range {
    fn check(&self, v: f64) -> bool {
        self.gte.map(|g| v >= g).unwrap_or(true) && self.lte.map(|l| v <= l).unwrap_or(true)
    }
    fn describe(&self) -> String {
        match (self.gte, self.lte) {
            (Some(g), Some(l)) => format!("[{g}, {l}]"),
            (Some(g), None) => format!(">= {g}"),
            (None, Some(l)) => format!("<= {l}"),
            _ => "(任意)".into(),
        }
    }
}

/// 解析音频路径：支持 `${VOICE_CASES_DIR}` 前缀
pub fn resolve_audio(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("${VOICE_CASES_DIR}") {
        let base = std::env::var("VOICE_CASES_DIR").unwrap_or_else(|_| ".".into());
        return PathBuf::from(base).join(rest.trim_start_matches(['/', '\\']));
    }
    PathBuf::from(path)
}

pub fn load_dir(dir: impl AsRef<Path>) -> Result<Vec<Case>> {
    let dir = dir.as_ref();
    let mut cases = Vec::new();
    if !dir.is_dir() {
        return Ok(cases);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    for f in files {
        let raw =
            std::fs::read_to_string(&f).with_context(|| format!("读用例失败: {}", f.display()))?;
        let c: Case =
            serde_json::from_str(&raw).with_context(|| format!("解析用例失败: {}", f.display()))?;
        cases.push(c);
    }
    Ok(cases)
}

pub struct CaseResult {
    pub id: String,
    pub passed: bool,
    pub failures: Vec<String>,
    pub summary: String,
}

/// 把一次评测结果与用例断言比对
pub fn check(case: &Case, a: &Assessment) -> CaseResult {
    let mut failures = Vec::new();
    let e = &case.expect;
    let td = a.text_diff.as_ref();

    if let Some(r) = &e.accuracy {
        let v = td.map(|d| d.accuracy as f64).unwrap_or(f64::NAN);
        if !r.check(v) {
            failures.push(format!(
                "accuracy: 期望 {} 实际 {:.3}（需提供 reference）",
                r.describe(),
                v
            ));
        }
    }
    if let Some(r) = &e.speech_rate {
        let v = a.flow.speech_rate as f64;
        if !r.check(v) {
            failures.push(format!("speech_rate: 期望 {} 实际 {v:.1}", r.describe()));
        }
    }
    if let Some(r) = &e.articulation_rate {
        let v = a.flow.articulation_rate as f64;
        if !r.check(v) {
            failures.push(format!(
                "articulation_rate: 期望 {} 实际 {v:.1}",
                r.describe()
            ));
        }
    }
    if let Some(r) = &e.issue_count {
        let v = td.map(|d| d.issues.len()).unwrap_or(0) as f64;
        if !r.check(v) {
            failures.push(format!("issue_count: 期望 {} 实际 {v:.0}", r.describe()));
        }
    }
    if let Some(want) = &e.timestamp_alignment {
        let got = td
            .map(|d| snake(&format!("{:?}", d.timestamp_alignment)))
            .unwrap_or_else(|| "missing".into());
        if got != want.to_lowercase() {
            failures.push(format!("timestamp_alignment: 期望 {want} 实际 {got}"));
        }
    }
    if let Some(want) = e.band_limited
        && a.audio.band_limited != want
    {
        failures.push(format!(
            "band_limited: 期望 {want} 实际 {}（源采样率 {} Hz）",
            a.audio.band_limited, a.audio.source_sr
        ));
    }
    if let Some(want) = e.degraded
        && a.degraded != want
    {
        failures.push(format!("degraded: 期望 {want} 实际 {}", a.degraded));
    }

    let issues = td.map(|d| d.issues.clone()).unwrap_or_default();
    for want in &e.issues {
        let hit = issues.iter().any(|i| matches_issue(want, i));
        if !hit {
            failures.push(format!("缺少期望问题: {}", describe_issue(want)));
        }
    }
    for want in &e.absent_issues {
        if let Some(i) = issues.iter().find(|i| matches_issue(want, i)) {
            failures.push(format!(
                "出现了不该有的问题: {}（实际 note: {}）",
                describe_issue(want),
                i.note.chars().take(40).collect::<String>()
            ));
        }
    }

    let summary = format!(
        "{} | accurs={} | issues={} | rate={} | align={}",
        a.transcript.text.chars().take(48).collect::<String>(),
        td.map(|d| format!("{:.2}", d.accuracy))
            .unwrap_or_else(|| "-".into()),
        issues.len(),
        a.flow.speech_rate,
        td.map(|d| snake(&format!("{:?}", d.timestamp_alignment)))
            .unwrap_or_else(|| "-".into()),
    );

    CaseResult {
        id: case.id.clone(),
        passed: failures.is_empty(),
        failures,
        summary,
    }
}

/// CamelCase → snake_case（枚举用 `{:?}` 拿到的是 `LikelyAsrMiss`，断言里写的是 `likely_asr_miss`）
pub fn snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn matches_issue(want: &ExpectIssue, got: &crate::assess::Issue) -> bool {
    if snake(&format!("{:?}", got.kind)) != want.kind.to_lowercase() {
        return false;
    }
    if let Some(r) = &want.reference
        && got.reference.as_deref() != Some(r.as_str())
        && got.reference_surface.as_deref() != Some(r.as_str())
    {
        return false;
    }
    if let Some(s) = &want.said
        && !s.matches(got.said.as_deref())
    {
        return false;
    }
    if let Some(h) = &want.hint {
        let got_h = got
            .hint
            .map(|x| snake(&format!("{x:?}")))
            .unwrap_or_default();
        if !h.matches(Some(got_h.as_str())) {
            return false;
        }
    }
    if let Some(sv) = &want.severity {
        let got_sv = snake(&format!("{:?}", got.severity));
        if !sv.matches(Some(got_sv.as_str())) {
            return false;
        }
    }
    true
}

fn describe_issue(i: &ExpectIssue) -> String {
    format!(
        "kind={} reference={} said={} hint={} severity={}",
        i.kind,
        i.reference.as_deref().unwrap_or("-"),
        i.said
            .as_ref()
            .map(OneOrMany::describe)
            .unwrap_or_else(|| "-".into()),
        i.hint
            .as_ref()
            .map(OneOrMany::describe)
            .unwrap_or_else(|| "-".into()),
        i.severity
            .as_ref()
            .map(OneOrMany::describe)
            .unwrap_or_else(|| "-".into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(said: &str, hint: Option<crate::assess::Hint>) -> crate::assess::Issue {
        crate::assess::Issue {
            kind: crate::assess::IssueKind::Substitution,
            reference: Some("third".into()),
            reference_surface: Some("third".into()),
            said: Some(said.into()),
            t0: 0.0,
            t1: 0.1,
            severity: crate::assess::Severity::High,
            note: String::new(),
            similarity: 0.0,
            phonetic_similarity: 0.0,
            hint,
            cross_checked: None,
            other_engine_heard: None,
            pinyin: None,
        }
    }

    /// D56：`said` 支持等价集合，命中任一即算过
    #[test]
    fn said_accepts_equivalence_set() {
        let set: ExpectIssue =
            serde_json::from_str(r#"{"kind":"substitution","said":["straight","three"]}"#).unwrap();
        assert!(matches_issue(
            &set,
            &issue("three", Some(crate::assess::Hint::Unclear))
        ));
        assert!(matches_issue(&set, &issue("straight", None)));
        assert!(!matches_issue(&set, &issue("tree", None)));

        // 单字符串写法必须继续有效（向后兼容旧用例）
        let one: ExpectIssue =
            serde_json::from_str(r#"{"kind":"substitution","said":"three"}"#).unwrap();
        assert!(matches_issue(&one, &issue("three", None)));
        assert!(!matches_issue(&one, &issue("straight", None)));

        // hint 也能写集合
        let both: ExpectIssue = serde_json::from_str(
            r#"{"kind":"substitution","hint":["unclear","likely_pronunciation"]}"#,
        )
        .unwrap();
        assert!(matches_issue(
            &both,
            &issue("x", Some(crate::assess::Hint::Unclear))
        ));
        assert!(matches_issue(
            &both,
            &issue("x", Some(crate::assess::Hint::LikelyPronunciation))
        ));
        assert!(!matches_issue(
            &both,
            &issue("x", Some(crate::assess::Hint::LikelyHomophone))
        ));
    }

    #[test]
    fn snake_converts_enum_names() {
        assert_eq!(snake("LikelyAsrMiss"), "likely_asr_miss");
        assert_eq!(snake("Omitted"), "omitted");
        assert_eq!(snake("LikelyPronunciation"), "likely_pronunciation");
        assert_eq!(snake("Estimated"), "estimated");
    }
}
