//! LRC 歌词/字幕解析与逐行对齐。
//!
//! 这类文件（新概念音频配套）是天然的**真值**：每行都有起始时间 + 双语原文。
//! 用途：
//! 1. 作为跟读素材：逐行给出参考句与时间窗
//! 2. 作为评测基准：把 ASR 结果按行切分，比对每行文本与起始时间
//!
//! 支持格式：
//! ```text
//! [al:新概念英语第二册]
//! [00:10.47]Why was the writer's aunt surprised? | 作家的阿姨为什么感到惊讶？
//! ```

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Line {
    /// 行起始时间（秒）
    pub t0: f32,
    /// 行结束时间（下一行起点；最后一行用音频时长兜底）
    pub t1: f32,
    /// 英文原文
    pub en: String,
    /// 中文翻译（可能为空）
    pub zh: String,
}

/// 解析 LRC 文本。`duration` 用于补齐最后一行的结束时间。
pub fn parse(text: &str, duration: f32) -> Vec<Line> {
    let mut raw: Vec<(f32, String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('[') {
            continue;
        }
        // [mm:ss.xx] 或 [mm:ss.xxx]
        let Some(close) = line.find(']') else { continue };
        let tag = &line[1..close];
        let Some((mm, rest)) = tag.split_once(':') else { continue };
        let (Ok(min), Ok(sec)) = (mm.parse::<f32>(), rest.parse::<f32>()) else {
            continue; // [al:...] / [ar:...] 之类的元信息
        };
        let t = min * 60.0 + sec;
        let body = line[close + 1..].trim();
        if body.is_empty() {
            continue;
        }
        let (en, zh) = match body.split_once('|') {
            Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
            None => (body.to_string(), String::new()),
        };
        if en.is_empty() {
            continue;
        }
        raw.push((t, en, zh));
    }
    raw.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = Vec::with_capacity(raw.len());
    for i in 0..raw.len() {
        let t0 = raw[i].0;
        let t1 = raw
            .get(i + 1)
            .map(|n| n.0)
            .unwrap_or_else(|| duration.max(t0 + 1.0));
        out.push(Line {
            t0,
            t1: t1.max(t0),
            en: raw[i].1.clone(),
            zh: raw[i].2.clone(),
        });
    }
    out
}

/// 把所有行的英文拼成一段（用于整体 accuracy 评测）
pub fn full_reference(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|l| l.en.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[al:album]\n[ar:artist]\n[00:10.47]It was Sunday. | 那是星期天。\n[00:17.99]I never get up early. | 我从不起早。\n[00:22.03]Last Sunday I got up very late.\n";

    #[test]
    fn parses_timestamps_and_bilingual() {
        let lines = parse(SAMPLE, 30.0);
        assert_eq!(lines.len(), 3, "元信息行应被忽略");
        assert!((lines[0].t0 - 10.47).abs() < 1e-3);
        assert_eq!(lines[0].en, "It was Sunday.");
        assert_eq!(lines[0].zh, "那是星期天。");
        assert!((lines[0].t1 - 17.99).abs() < 1e-3, "t1 应等于下一行起点");
        assert_eq!(lines[2].en, "Last Sunday I got up very late.");
        assert!((lines[2].t1 - 30.0).abs() < 1e-3, "最后一行用音频时长兜底");
    }

    #[test]
    fn full_reference_joins_english_only() {
        let lines = parse(SAMPLE, 30.0);
        let full = full_reference(&lines);
        assert!(full.starts_with("It was Sunday. I never get up early."));
        assert!(!full.contains('星'), "中文不应混进参考文本");
    }

    #[test]
    fn unsorted_input_is_sorted() {
        let raw = "[00:20.00]b\n[00:10.00]a\n";
        let lines = parse(raw, 30.0);
        assert_eq!(lines[0].en, "a");
        assert_eq!(lines[1].en, "b");
    }
}
