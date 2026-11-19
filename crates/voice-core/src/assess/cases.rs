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
//!       { "kind": "substitution", "reference": "third", "said": "straight", "hint": "unclear" }
//!     ]
//!   }
//! }
//! ```

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

#[derive(Debug, Deserialize, Clone)]
pub struct ExpectIssue {
    pub kind: String,
    pub reference: Option<String>,
    /// 参考文本文件（长文本用这个；相对路径同样支持 ${VOICE_CASES_DIR}）
    #[serde(default)]
    pub reference_file: Option<String>,
    pub said: Option<String>,
    pub hint: Option<String>,
    pub severity: Option<String>,
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
        let raw = std::fs::read_to_string(&f)
            .with_context(|| format!("读用例失败: {}", f.display()))?;
        let c: Case = serde_json::from_str(&raw)
            .with_context(|| format!("解析用例失败: {}", f.display()))?;
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
            failures.push(format!("articulation_rate: 期望 {} 实际 {v:.1}", r.describe()));
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
    if let Some(want) = e.band_limited {
        if a.audio.band_limited != want {
            failures.push(format!(
                "band_limited: 期望 {want} 实际 {}（源采样率 {} Hz）",
                a.audio.band_limited, a.audio.source_sr
            ));
        }
    }
    if let Some(want) = e.degraded {
        if a.degraded != want {
            failures.push(format!("degraded: 期望 {want} 实际 {}", a.degraded));
        }
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
        td.map(|d| format!("{:.2}", d.accuracy)).unwrap_or_else(|| "-".into()),
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
    if let Some(r) = &want.reference {
        if got.reference.as_deref() != Some(r.as_str())
            && got.reference_surface.as_deref() != Some(r.as_str())
        {
            return false;
        }
    }
    if let Some(s) = &want.said {
        if got.said.as_deref() != Some(s.as_str()) {
            return false;
        }
    }
    if let Some(h) = &want.hint {
        let got_h = got.hint.map(|x| snake(&format!("{x:?}"))).unwrap_or_default();
        if got_h != h.to_lowercase() {
            return false;
        }
    }
    if let Some(sv) = &want.severity {
        if snake(&format!("{:?}", got.severity)) != sv.to_lowercase() {
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
        i.said.as_deref().unwrap_or("-"),
        i.hint.as_deref().unwrap_or("-"),
        i.severity.as_deref().unwrap_or("-"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_converts_enum_names() {
        assert_eq!(snake("LikelyAsrMiss"), "likely_asr_miss");
        assert_eq!(snake("Omitted"), "omitted");
        assert_eq!(snake("LikelyPronunciation"), "likely_pronunciation");
        assert_eq!(snake("Estimated"), "estimated");
    }
}
