//! 字幕解析（SRT / WebVTT）——给 `bank ingest-audio --srt` 用。
//!
//! 为什么需要它：真人音频能做素材，但没有**权威文本**时，答案键只能靠 ASR，
//! 实测佩奇音频会出 `Pepper` / `Mdy puddles` 这类错字（双引擎一起错），不能当听写答案键。
//! 有字幕就不同了：文本来自字幕（`text_source=srt`），时间也来自字幕，
//! ASR 只用来**交叉验证时间轴对不对**。

/// 一条字幕（时间单位秒）。
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub t0: f32,
    pub t1: f32,
    pub text: String,
}

/// 解析时间戳：`00:01:02,500` / `00:01:02.500` / `01:02.5`（SRT、VTT、短格式都吃）。
pub fn parse_timestamp(s: &str) -> Option<f32> {
    let s = s.trim();
    let (h, rest) = match s.rsplit_once(':') {
        Some((left, rest)) => match left.rsplit_once(':') {
            Some((h, m)) => (Some(h), format!("{m}:{rest}")),
            None => (None, format!("{left}:{rest}")),
        },
        None => (None, s.to_string()),
    };
    let h: f32 = match h {
        Some(h) => h.trim().parse().ok()?,
        None => 0.0,
    };
    let (m, sec) = rest.split_once(':')?;
    let m: f32 = m.trim().parse().ok()?;
    let sec = sec.trim().replace(',', ".");
    let sec: f32 = sec.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// 清掉字幕里的非语音内容：`<i>` 标签、`[MUSIC]`、`♪`、说话人前缀 `- PEPPA:`。
///
/// 只保留字母/撇号/空格与基本标点；清理后没有字母的 cue 返回空串（调用方应丢弃）。
pub fn clean_text(raw: &str) -> String {
    let mut s = raw.to_string();
    // HTML/VTT 标签
    while let (Some(a), Some(b)) = (s.find('<'), s.find('>')) {
        if a < b {
            s.replace_range(a..=b, " ");
        } else {
            break;
        }
    }
    // 方括号/圆括号里的音效说明、花括号里的卡拉OK 时间
    for (open, close) in [('[', ']'), ('{', '}')] {
        while let (Some(a), Some(b)) = (s.find(open), s.find(close)) {
            if a < b {
                s.replace_range(a..=b, " ");
            } else {
                break;
            }
        }
    }
    // 说话人前缀：`- PEPPA:` / `NARRATOR:` / `-`
    s = s.replace('♪', " ");
    let mut out = String::new();
    for line in s.lines() {
        let mut line = line.trim();
        if let Some(rest) = line.strip_prefix('-').or_else(|| line.strip_prefix('–')) {
            line = rest.trim();
        }
        // 开头 `NAME:`（全大写或首字母大写，长度 ≤ 12）
        if let Some((head, rest)) = line.split_once(':')
            && head.len() <= 12
            && !head.trim().is_empty()
            && !head.contains(' ')
            && head
                .chars()
                .all(|c| c.is_alphabetic() || c == '-' || c == '.')
        {
            line = rest.trim();
        }
        if !line.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(line);
        }
    }
    // 只保留字母/数字/撇号/连字符/基本标点
    let cleaned: String = out
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '\'' | '’' | '-' | ',' | '.' | '?' | '!' | ';')
            {
                c
            } else {
                ' '
            }
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 解析 SRT / WebVTT 文本。无法识别的块直接跳过（不报错）。
pub fn parse(text: &str) -> Vec<Cue> {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut times: Option<(f32, f32)> = None;
        let mut lines: Vec<&str> = Vec::new();
        for line in block.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line.contains("-->") {
                let parts: Vec<&str> = line.split("-->").collect();
                // VTT 允许在时间戳后加参数：`00:00:02.000 align:start line:0`
                let first = parts.first().copied().unwrap_or("");
                let second = parts
                    .get(1)
                    .and_then(|p| p.split_whitespace().next())
                    .unwrap_or("");
                if parts.len() == 2
                    && let (Some(a), Some(b)) = (parse_timestamp(first), parse_timestamp(second))
                {
                    // VTT 的 cue 可能带位置参数：`00:00:01.000 --> 00:00:03.000 align:start`
                    times = Some((a, b));
                }
                continue;
            }
            if times.is_none() {
                continue; // 序号行 / WEBVTT 头
            }
            lines.push(line);
        }
        if let Some((t0, t1)) = times {
            let text = clean_text(&lines.join(" "));
            if text.chars().any(|c| c.is_alphabetic()) && t1 > t0 {
                out.push(Cue { t0, t1, text });
            }
        }
    }
    out.sort_by(|a, b| a.t0.partial_cmp(&b.t0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// 把碎字幕合并成“句子级”素材：遇到句末标点或达到 `max_sec` 就收一句。
///
/// 为什么要合并：字幕常按 2–4 个词切（为了屏幕可读性），直接当素材会出现
/// `It is raining today, so` 这种半句。合并门槛：
/// - 当前句最后一词以 `.` `?` `!` 结尾 → 收；
/// - 或时长 ≥ `max_sec`；
/// - 或两 cue 之间间隔 > 1.2 s（说话人换了 / 场景切换）。
pub fn merge_cues(cues: &[Cue], max_sec: f32) -> Vec<Cue> {
    let mut out: Vec<Cue> = Vec::new();
    let mut cur: Option<Cue> = None;
    for c in cues {
        match cur.as_mut() {
            None => cur = Some(c.clone()),
            Some(acc) => {
                let gap = c.t0 - acc.t1;
                let ends_sentence = acc.text.trim_end().ends_with(['.', '?', '!'])
                    || acc.text.trim_end().ends_with("\".");
                let too_long = c.t1 - acc.t0 >= max_sec;
                if ends_sentence || too_long || gap > 1.2 {
                    out.push(acc.clone());
                    *acc = c.clone();
                } else {
                    acc.t1 = c.t1;
                    acc.text = format!("{} {}", acc.text, c.text);
                }
            }
        }
    }
    if let Some(acc) = cur {
        out.push(acc);
    }
    out.retain(|c| c.t1 > c.t0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRT: &str = "\u{feff}1\r\n00:00:01,000 --> 00:00:03,500\r\n<i>It is raining today,</i> so\r\nPeppa cannot play outside.\r\n\r\n2\r\n00:00:04,000 --> 00:00:05,000\r\n- NARRATOR: [MUSIC] ♪\r\n\r\n3\r\n00:00:05,500 --> 00:00:07,000\r\n- PEPPA: Can we go out to play?\r\n";

    #[test]
    fn parses_srt_with_tags_speakers_and_music() {
        let cues = parse(SRT);
        assert_eq!(cues.len(), 2, "纯音效/音乐 cue 应被丢掉: {cues:?}");
        assert!((cues[0].t0 - 1.0).abs() < 1e-3);
        assert!((cues[0].t1 - 3.5).abs() < 1e-3);
        assert_eq!(
            cues[0].text,
            "It is raining today, so Peppa cannot play outside."
        );
        // 说话人前缀与音效被清掉，只留台词
        assert_eq!(cues[1].text, "Can we go out to play?");
    }

    #[test]
    fn parses_webvtt_and_short_timestamps() {
        let vtt = "WEBVTT\n\n00:01.000 --> 00:02.000 align:start\nHello there.\n\n01:00:03.500 --> 01:00:05.000\nGeneral Kenobi.\n";
        let cues = parse(vtt);
        assert_eq!(cues.len(), 2);
        assert!((cues[0].t0 - 1.0).abs() < 1e-3);
        assert!((cues[1].t0 - 3603.5).abs() < 1e-3);
        assert_eq!(cues[1].text, "General Kenobi.");
    }

    #[test]
    fn merges_fragmented_cues_into_sentences() {
        let cues = vec![
            Cue {
                t0: 0.0,
                t1: 1.0,
                text: "It is raining today,".into(),
            },
            Cue {
                t0: 1.0,
                t1: 2.0,
                text: "so Peppa cannot".into(),
            },
            Cue {
                t0: 2.0,
                t1: 3.0,
                text: "play outside.".into(),
            },
            Cue {
                t0: 3.2,
                t1: 4.0,
                text: "Can we go out".into(),
            },
            Cue {
                t0: 4.0,
                t1: 5.0,
                text: "to play?".into(),
            },
        ];
        let merged = merge_cues(&cues, 12.0);
        assert_eq!(merged.len(), 2, "{merged:?}");
        assert_eq!(
            merged[0].text,
            "It is raining today, so Peppa cannot play outside."
        );
        assert!((merged[0].t0 - 0.0).abs() < 1e-6 && (merged[0].t1 - 3.0).abs() < 1e-6);
        assert_eq!(merged[1].text, "Can we go out to play?");
    }

    #[test]
    fn merge_respects_max_sec_and_long_gaps() {
        let cues = vec![
            Cue {
                t0: 0.0,
                t1: 3.0,
                text: "one".into(),
            },
            Cue {
                t0: 3.0,
                t1: 6.0,
                text: "two".into(),
            },
            Cue {
                t0: 8.0,
                t1: 10.0,
                text: "three".into(),
            },
        ];
        let merged = merge_cues(&cues, 5.0);
        assert_eq!(
            merged.len(),
            3,
            "达到 max_sec / 间隔 >1.2s 都要收句: {merged:?}"
        );
    }

    #[test]
    fn timestamp_parsing_is_lenient() {
        assert!((parse_timestamp("00:00:02,500").unwrap() - 2.5).abs() < 1e-3);
        assert!((parse_timestamp("01:00:00.000").unwrap() - 3600.0).abs() < 1e-3);
        assert!(parse_timestamp("garbage").is_none());
    }
}
