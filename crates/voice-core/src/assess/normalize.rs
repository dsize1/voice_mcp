//! 文本规范化：把参考文本与识别文本拉到同一个比较平面。
//!
//! 目标：**不因为书写差异误判发音问题**。
//! - 缩写展开（I'm → i am、don't → do not、gonna → going to）
//! - 数字统一成词（3 → three）
//! - 去标点、统一小写
//! - 中文按字符切分（diff 变成字级）；**中文场景下阿拉伯数字归一成汉字数字**
//!   （ASR 把「二十六」写成「26」曾被当成 3 个字全错，见 D13）
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

/// 规范化单元序列（带来源 token 序号）。英文口径：数字 → 英文词。
pub fn units(text: &str) -> Vec<Unit> {
    units_with(text, Script::English)
}

/// 规范化单元序列（中文口径）：数字 → **汉字数字**（`26` → `二十六`）。
///
/// 为什么要单独一条：SenseVoice 中文会把「二十六」写成 `26`、把「十四」写成 `14`，
/// 而参考文本是汉字。按英文口径展开会得到 `twenty six` 这种英文词，跟中文逐字比较
/// 完全对不上（实测「他把空调开到二十六度」的字级准确率被这 1 个数字拖到 0.70）。
pub fn units_zh(text: &str) -> Vec<Unit> {
    units_with(text, Script::Chinese)
}

/// 比较平面（决定数字怎么读）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    /// 英文：`3` → `three`
    English,
    /// 中文：`26` → `二十六`（5 位以上的年份/编号逐位读，见 `number_to_chinese`）
    Chinese,
}

fn units_with(text: &str, script: Script) -> Vec<Unit> {
    let mut out = Vec::new();
    for (idx, raw) in tokens(text).iter().enumerate() {
        if raw.chars().all(is_cjk_char) {
            // CJK 已在 tokens() 里逐字切分，这里直接透传
            for ch in raw.chars() {
                out.push(Unit {
                    text: ch.to_string(),
                    token: idx,
                });
            }
        } else {
            for w in expand_contraction_with(raw, script) {
                out.push(Unit {
                    text: w,
                    token: idx,
                });
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

/// CJK 判定（汉字基本区/扩展 A + 日文假名）。`assess::pinyin` 复用它。
pub fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   // CJK 统一汉字
        | '\u{3400}'..='\u{4DBF}' // 扩展 A
        | '\u{3040}'..='\u{30FF}' // 日文假名
    )
}

/// 展开缩写与口语缩略，纯数字按 `script` 读（英文词 / 汉字数字）。
fn expand_contraction_with(w: &str, script: Script) -> Vec<String> {
    let lower = w.replace('\u{2019}', "'").to_ascii_lowercase();
    let lower = lower.trim_matches('\'').to_string();
    if lower.is_empty() {
        return Vec::new();
    }
    if lower.chars().all(|c| c.is_ascii_digit()) {
        return match script {
            Script::English => number_words(&lower),
            // 汉字数字必须**逐字**成为比较单元（参考文本里「二十六」就是 3 个字）
            Script::Chinese => number_to_chinese(&lower)
                .chars()
                .map(|c| c.to_string())
                .collect(),
        };
    }
    if let Some(v) = CONTRACTIONS.iter().find(|(k, _)| *k == lower) {
        return v.1.iter().map(|s| s.to_string()).collect();
    }
    // ASR 伪词修复：`did't` / `could't` 这类不规范缩写（正确形是 didn't/couldn't），
    // 是识别引擎把 `did not` 糊成一个 token 的产物。若对应的规范形不在表里，
    // 按 `{base} + not` 展开，避免被当成“漏读 + 替换”两条错误。
    if let Some(base) = lower.strip_suffix("'t") {
        let is_word = !base.is_empty() && base.chars().all(|c| c.is_ascii_alphabetic());
        if is_word {
            return vec![base.to_string(), "not".to_string()];
        }
    }
    vec![canonical_variant(&lower)]
}

/// 英/美拼写变体 → 统一形（否则《新概念》的英式文本配上美语音轨时，
/// `theatre` 会被当成把 `theater` 读错 —— 学员根本没读）。
fn canonical_variant(w: &str) -> String {
    US_UK_VARIANTS
        .iter()
        .find(|(a, b)| *a == w || *b == w)
        .map(|(a, _)| a.to_string())
        .unwrap_or_else(|| w.to_string())
}

/// 英美拼写变体对：(统一形, 变体)。两边都映射到第一个。
///
/// 故意用**显式表**而不是 `-re→-er` 之类规则：规则会误伤 more/before/nature 这类两边相同的词。
/// 新增词时注意：两个词必须在**口语上同音**，否则不能归并。
static US_UK_VARIANTS: &[(&str, &str)] = &[
    // -re / -er
    ("theater", "theatre"),
    ("center", "centre"),
    ("meter", "metre"),
    ("liter", "litre"),
    ("fiber", "fibre"),
    ("somber", "sombre"),
    ("maneuver", "manoeuvre"),
    // -or / -our
    ("color", "colour"),
    ("favor", "favour"),
    ("honor", "honour"),
    ("labor", "labour"),
    ("neighbor", "neighbour"),
    ("behavior", "behaviour"),
    ("humor", "humour"),
    ("rumor", "rumour"),
    ("vapor", "vapour"),
    ("endeavor", "endeavour"),
    ("flavor", "flavour"),
    ("harbor", "harbour"),
    ("armor", "armour"),
    // -ize / -ise
    ("realize", "realise"),
    ("organize", "organise"),
    ("recognize", "recognise"),
    ("apologize", "apologise"),
    ("criticize", "criticise"),
    ("memorize", "memorise"),
    ("summarize", "summarise"),
    ("analyze", "analyse"),
    ("paralyze", "paralyse"),
    ("specialize", "specialise"),
    // -se / -ce
    ("defense", "defence"),
    ("offense", "offence"),
    ("license", "licence"),
    ("practice", "practise"),
    // 双写 / 其他
    ("traveling", "travelling"),
    ("traveled", "travelled"),
    ("canceled", "cancelled"),
    ("labeled", "labelled"),
    ("modeling", "modelling"),
    ("jewelry", "jewellery"),
    ("program", "programme"),
    ("catalog", "catalogue"),
    ("dialog", "dialogue"),
    ("gray", "grey"),
    ("plow", "plough"),
    ("tire", "tyre"),
    ("curb", "kerb"),
    ("check", "cheque"),
    ("aluminum", "aluminium"),
    ("mom", "mum"),
    ("math", "maths"),
    ("airplane", "aeroplane"),
];

/// 阿拉伯数字 → 汉字数字（`26` → `二十六`，`105` → `一百零五`）。
///
/// 口径（**必须承认的局限**）：
/// - 5 位及以上、以 `0` 开头、或 1–2 开头的 4 位数（年份 1000–2999）按**逐位**读
///   （`1998` → `一九九八`），因为这类串多半是年份/编号，中文口语就是逐位读；
/// - 因此若参考文本把年份写成「一千九百九十八」，会与本函数产出对不上 ——
///   这属于写法差异，拼音层不据此判发音（见 docs/pinyin.md）。
/// - 小数点在 `tokens()` 里已经切开：`3.5` → `3` + `5`（读作「三、五」）。
pub fn number_to_chinese(digits: &str) -> String {
    const D: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    let digitwise = || -> String {
        digits
            .chars()
            .filter_map(|c| c.to_digit(10))
            .map(|d| D[d as usize])
            .collect()
    };
    // 前导零 / 5 位以上 / 1–2 开头的 4 位（年份 1000–2999）→ 逐位读
    let year_like = digits.len() == 4 && matches!(digits.as_bytes()[0], b'1' | b'2');
    if digits.len() >= 5 || digits.starts_with('0') || year_like {
        return digitwise();
    }
    let Ok(n) = digits.parse::<u32>() else {
        return digitwise();
    };
    if n == 0 {
        return "零".into();
    }
    let mut out = String::new();
    let mut rest = n;
    let mut zero_pending = false;
    for (v, unit) in [(1000u32, "千"), (100, "百"), (10, "十"), (1, "")] {
        let d = rest / v;
        rest %= v;
        if d == 0 {
            if !out.is_empty() && rest > 0 {
                zero_pending = true;
            }
            continue;
        }
        if zero_pending {
            out.push('零');
            zero_pending = false;
        }
        // 10–19 读「十」而不是「一十」
        if v == 10 && d == 1 && out.is_empty() {
            out.push('十');
            continue;
        }
        out.push_str(D[d as usize]);
        out.push_str(unit);
    }
    out
}

fn number_words(digits: &str) -> Vec<String> {
    const SMALL: [&str; 20] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
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
            if !r.is_multiple_of(10) {
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
    ("we'll", &["we", "will"]),
    ("they'll", &["they", "will"]),
    ("he'll", &["he", "will"]),
    ("she'll", &["she", "will"]),
    ("it'll", &["it", "will"]),
    ("that'll", &["that", "will"]),
    ("there'll", &["there", "will"]),
    ("you'd", &["you", "would"]),
    ("we'd", &["we", "would"]),
    ("they'd", &["they", "would"]),
    ("he'd", &["he", "would"]),
    ("she'd", &["she", "would"]),
    ("it'd", &["it", "would"]),
    ("who's", &["who", "is"]),
    ("where's", &["where", "is"]),
    ("here's", &["here", "is"]),
    ("how's", &["how", "is"]),
    ("when's", &["when", "is"]),
    ("why's", &["why", "is"]),
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
    ("ain't", &["is", "not"]),
    ("shan't", &["shall", "not"]),
    ("daren't", &["dare", "not"]),
    ("oughtn't", &["ought", "not"]),
    ("mustn't", &["must", "not"]),
    ("needn't", &["need", "not"]),
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
        assert_eq!(
            words("I'm gonna do it"),
            vec!["i", "am", "going", "to", "do", "it"]
        );
        assert_eq!(words("Don't stop"), vec!["do", "not", "stop"]);
        // 2026-11-28 补：`we'll`/`they'll`/`he'd`/`where's` 之前漏了，
        // 导致 "we'll" 与 ASR 的 "we will" 对不上（D37 交叉复核也因此误判）
        assert_eq!(words("we'll start"), vec!["we", "will", "start"]);
        assert_eq!(words("they'll go"), vec!["they", "will", "go"]);
        assert_eq!(words("he'd know"), vec!["he", "would", "know"]);
        assert_eq!(words("where's the log"), vec!["where", "is", "the", "log"]);
    }

    #[test]
    fn repairs_pseudo_contractions() {
        // ASR 把 `did not` 糊成不存在的 `did't`，不能变成两条错误
        assert_eq!(
            words("I did't enjoy it"),
            vec!["i", "did", "not", "enjoy", "it"]
        );
        assert_eq!(words("I could't hear"), vec!["i", "could", "not", "hear"]);
        // 合法缩写仍然走表
        assert_eq!(words("I didn't go"), vec!["i", "did", "not", "go"]);
        // ain't 显式收录，不会落到伪词规则
        assert_eq!(words("I ain't sure"), vec!["i", "is", "not", "sure"]);
    }

    #[test]
    fn normalizes_us_uk_spellings() {
        assert_eq!(words("theatre"), words("theater"));
        assert_eq!(words("colour"), words("color"));
        assert_eq!(words("realise"), words("realize"));
        assert_eq!(words("programme"), words("program"));
        // 两边写法相同的词不能被误伤
        assert_eq!(
            words("more before nature"),
            vec!["more", "before", "nature"]
        );
    }

    #[test]
    fn strips_punctuation_and_case() {
        assert_eq!(
            words("Hello, world!  Really?"),
            vec!["hello", "world", "really"]
        );
    }

    #[test]
    fn converts_numbers() {
        assert_eq!(words("I have 3 cats"), vec!["i", "have", "three", "cats"]);
        assert_eq!(words("21"), vec!["twenty", "one"]);
    }

    // ---------------- D13：中文数字归一（ASR 写阿拉伯数字） ----------------

    #[test]
    fn chinese_numbers_use_hanzi_digits() {
        assert_eq!(
            normalize_zh_words("十六"),
            vec!["十", "六"],
            "汉字数字逐字透传"
        );
        assert_eq!(
            normalize_zh_words("我把空调开到26度"),
            vec!["我", "把", "空", "调", "开", "到", "二", "十", "六", "度"],
            "26 必须读成「二十六」，不能是 twenty six"
        );
        // 同一个数字串在英文平面下仍然走英文词
        assert_eq!(words("have 26 cats"), vec!["have", "twenty", "six", "cats"]);
    }

    #[test]
    fn number_to_chinese_covers_common_readings() {
        for (digits, want) in [
            ("0", "零"),
            ("7", "七"),
            ("10", "十"),
            ("12", "十二"),
            ("20", "二十"),
            ("26", "二十六"),
            ("105", "一百零五"),
            ("110", "一百一十"),
            ("1001", "一零零一"), // 1 开头的 4 位数按年份口径逐位读
            ("3001", "三千零一"),
            ("9999", "九千九百九十九"),
        ] {
            assert_eq!(number_to_chinese(digits), want, "digits={digits}");
        }
        // 5 位以上 / 前导零 → 逐位读（年份、编号）
        assert_eq!(number_to_chinese("1998"), "一九九八");
        assert_eq!(number_to_chinese("007"), "零零七");
    }

    fn normalize_zh_words(t: &str) -> Vec<String> {
        units_zh(t).into_iter().map(|u| u.text).collect()
    }

    #[test]
    fn splits_chinese_by_char() {
        assert_eq!(
            words("我觉得 third 很好"),
            vec!["我", "觉", "得", "third", "很", "好"]
        );
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
