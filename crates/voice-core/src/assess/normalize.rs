//! 文本规范化：把参考文本与识别文本拉到同一个比较平面。
//!
//! 目标：**不因为书写差异误判发音问题**。
//! - 缩写展开（I'm → i am、don't → do not、gonna → going to）
//! - 数字统一成词（3 → three）
//! - 去标点、统一小写
//! - 中文按字符切分（diff 变成字级）
//!
//! **关键设计（D03 修复）**：规范化会改变词数（展开缩写），所以每个规范化词都带
//! 一个 `token` 字段指回**原始 token 序号**。上层用这个序号去取 ASR 词时间戳，
//! 而不是按"比例"把 N 个词映射到 M 个时间戳——后者在带缩写的句子上会静默错位，
//! 导致教练指错词。

/// 一个规范化后的比较单元。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// 规范化后的词形（小写、去标点、缩写已展开）
    pub text: String,
    /// 回到原始 token 的序号（`tokens()` 的下标）
    pub token: usize,
}

/// 原始 token 序列：英文/数字按非字母数字切分，CJK 逐字切分。
pub fn tokens(text: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if is_cjk_char(ch) {
            if !cur.is_empty() {
                v.push(std::mem::take(&mut cur));
            }
            v.push(ch.to_string());
        } else if ch.is_alphanumeric() || ch == '\'' || ch == '\u{2019}' {
            cur.push(ch);
        } else if !cur.is_empty() {
            v.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        v.push(cur);
    }
    v
}

/// 规范化单元序列（带来源 token 序号）。
pub fn units(text: &str) -> Vec<Unit> {
    let mut out = Vec::new();
    for (idx, raw) in tokens(text).iter().enumerate() {
        if raw.chars().all(is_cjk_char) {
            // CJK 已在 tokens() 里逐字切分，这里直接透传
            for ch in raw.chars() {
                out.push(Unit { text: ch.to_string(), token: idx });
            }
        } else {
            for w in expand_contraction(raw) {
                out.push(Unit { text: w, token: idx });
            }
        }
    }
    out
}

/// 规范化后的词形序列（不含来源信息，`flow` 等只要词数的地方用）。
pub fn words(text: &str) -> Vec<String> {
    units(text).into_iter().map(|u| u.text).collect()
}

/// 原始 token 的表面形式（用于反馈里显示 "I'm" 而不是 "i" + "am"）。
pub fn surfaces(text: &str) -> Vec<String> {
    tokens(text)
}

fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   // CJK 统一汉字
        | '\u{3400}'..='\u{4DBF}' // 扩展 A
        | '\u{3040}'..='\u{30FF}' // 日文假名
    )
}

/// 展开缩写与口语缩略，纯数字转英文词。
fn expand_contraction(w: &str) -> Vec<String> {
    let lower = w.replace('\u{2019}', "'").to_ascii_lowercase();
    let lower = lower.trim_matches('\'').to_string();
    if lower.is_empty() {
        return Vec::new();
    }
    if lower.chars().all(|c| c.is_ascii_digit()) {
        return number_words(&lower);
    }
    if let Some(v) = CONTRACTIONS.iter().find(|(k, _)| *k == lower) {
        return v.1.iter().map(|s| s.to_string()).collect();
    }
    vec![lower]
}

fn number_words(digits: &str) -> Vec<String> {
    const SMALL: [&str; 20] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen",
        "eighteen", "nineteen",
    ];
    const TENS: [&str; 8] = [
        "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    let n: u64 = match digits.parse() {
        Ok(v) if v < 1000 => v,
        _ => return digits.chars().map(|c| c.to_string()).collect(),
    };
    let mut out = Vec::new();
    let h = n / 100;
    let r = n % 100;
    if h > 0 {
        out.push(SMALL[h as usize].to_string());
        out.push("hundred".into());
    }
    if r > 0 {
        if r < 20 {
            out.push(SMALL[r as usize].to_string());
        } else {
            out.push(TENS[(r / 10 - 2) as usize].to_string());
            if r % 10 > 0 {
                out.push(SMALL[(r % 10) as usize].to_string());
            }
        }
    }
    if out.is_empty() {
        out.push("zero".into());
    }
    out
}

/// 常见缩写 → 展开形式（左右都必须是规范化后的词）
static CONTRACTIONS: &[(&str, &[&str])] = &[
    ("i'm", &["i", "am"]),
    ("i've", &["i", "have"]),
    ("i'll", &["i", "will"]),
    ("i'd", &["i", "would"]),
    ("you're", &["you", "are"]),
    ("you've", &["you", "have"]),
    ("you'll", &["you", "will"]),
    ("he's", &["he", "is"]),
    ("she's", &["she", "is"]),
    ("it's", &["it", "is"]),
    ("that's", &["that", "is"]),
    ("there's", &["there", "is"]),
    ("what's", &["what", "is"]),
    ("let's", &["let", "us"]),
    ("we're", &["we", "are"]),
    ("we've", &["we", "have"]),
    ("they're", &["they", "are"]),
    ("they've", &["they", "have"]),
    ("don't", &["do", "not"]),
    ("doesn't", &["does", "not"]),
    ("didn't", &["did", "not"]),
    ("isn't", &["is", "not"]),
    ("aren't", &["are", "not"]),
    ("wasn't", &["was", "not"]),
    ("weren't", &["were", "not"]),
    ("haven't", &["have", "not"]),
    ("hasn't", &["has", "not"]),
    ("hadn't", &["had", "not"]),
    ("can't", &["can", "not"]),
    ("cannot", &["can", "not"]),
    ("won't", &["will", "not"]),
    ("wouldn't", &["would", "not"]),
    ("couldn't", &["could", "not"]),
    ("shouldn't", &["should", "not"]),
    ("gonna", &["going", "to"]),
    ("wanna", &["want", "to"]),
    ("gotta", &["got", "to"]),
    ("kinda", &["kind", "of"]),
    ("sorta", &["sort", "of"]),
    ("ok", &["okay"]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_contractions() {
        assert_eq!(words("I'm gonna do it"), vec!["i", "am", "going", "to", "do", "it"]);
        assert_eq!(words("Don't stop"), vec!["do", "not", "stop"]);
    }

    #[test]
    fn strips_punctuation_and_case() {
        assert_eq!(words("Hello, world!  Really?"), vec!["hello", "world", "really"]);
    }

    #[test]
    fn converts_numbers() {
        assert_eq!(words("I have 3 cats"), vec!["i", "have", "three", "cats"]);
        assert_eq!(words("21"), vec!["twenty", "one"]);
    }

    #[test]
    fn splits_chinese_by_char() {
        assert_eq!(words("我觉得 third 很好"), vec!["我", "觉", "得", "third", "很", "好"]);
    }

    #[test]
    fn keeps_internal_apostrophe() {
        assert_eq!(words("rock'n'roll"), vec!["rock'n'roll"]);
    }

    #[test]
    fn handles_curly_apostrophe() {
        assert_eq!(words("I\u{2019}m here"), vec!["i", "am", "here"]);
    }

    // ---- D03：来源 token 序号 ----

    #[test]
    fn units_track_source_token() {
        let u = units("I'm fine");
        // "I'm" 展开成两个词，但都指向 token 0
        assert_eq!(u.len(), 3);
        assert_eq!((u[0].text.as_str(), u[0].token), ("i", 0));
        assert_eq!((u[1].text.as_str(), u[1].token), ("am", 0));
        assert_eq!((u[2].text.as_str(), u[2].token), ("fine", 1));
        assert_eq!(surfaces("I'm fine"), vec!["I'm", "fine"]);
    }

    #[test]
    fn units_are_monotonic_in_token() {
        let u = units("Don't 3 it, I'm sure.");
        for w in u.windows(2) {
            assert!(w[1].token >= w[0].token, "token 序号必须单调不减");
        }
    }

    #[test]
    fn chinese_units_are_one_char_each_with_own_token() {
        let u = units("我觉得");
        assert_eq!(u.len(), 3);
        assert_eq!(u.iter().map(|x| x.token).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(surfaces("我觉得"), vec!["我", "觉", "得"]);
    }

    #[test]
    fn number_expansion_shares_one_token() {
        let u = units("21 cats");
        assert_eq!(u.len(), 3);
        assert_eq!(u[0].token, 0);
        assert_eq!(u[1].token, 0, "twenty/one 都来自同一个数字 token");
        assert_eq!(u[2].token, 1);
    }
}
