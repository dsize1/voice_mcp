//! 音形编码（phonetic key）：把英文词压成粗略的音形骨架，用来判断
//! 「两个词听起来像不像」，从而区分**发音偏差**与**说成了别的词**。
//!
//! 编码规则（够用就好，不追求精确音素）：
//! - 二合字母先行：`tch/ch→C`、`th→0`、`sh→S`、`ph→F`、`wh→W`、`ng→N`、`ck→K`、`gh→∅`、`qu→KW`
//! - 同音字母归并：`b→P d→T g→K v→F z→S c→K q→K x→KS j→J y→I`
//! - 元音 a/e/i/o/u 一律记为 `A` 并压缩连续
//! - `h` 单独出现时丢弃
//!
//! 已知局限（必须承认，写进 metrics 的 limitations）：
//! **不区分元音长短**（bit/beat、bad/bed 会编码成同一个 key），
//! 因此元音长短、双元音滑动必须靠音素级对齐（P2 第 4 步 GOP）才能判断。

/// 计算音形 key（小写输出）。
pub fn encode(word: &str) -> String {
    let chars: Vec<char> = word
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    let mut out = String::new();
    let mut i = 0usize;

    while i < chars.len() {
        // 1) 二合/三合
        let rest = chars.len() - i;
        let take = |n: usize, s: &mut String| {
            for k in 0..n {
                s.push(chars[i + k]);
            }
        };
        let _ = take;

        if rest >= 3 && chars[i] == 't' && chars[i + 1] == 'c' && chars[i + 2] == 'h' {
            out.push('C');
            i += 3;
            continue;
        }
        if rest >= 2 {
            let dg = (chars[i], chars[i + 1]);
            let mapped = match dg {
                ('t', 'h') => Some('0'),
                ('s', 'h') => Some('S'),
                ('c', 'h') => Some('C'),
                ('p', 'h') => Some('F'),
                ('w', 'h') => Some('W'),
                ('n', 'g') => Some('N'),
                ('c', 'k') => Some('K'),
                ('g', 'h') => Some('\u{0}'), // 静音
                ('q', 'u') => Some('K'),
                _ => None,
            };
            if let Some(m) = mapped {
                if m != '\u{0}' {
                    out.push(m);
                }
                i += 2;
                continue;
            }
        }
        // 2) 单字母
        let c = chars[i];
        if c == 'h' {
            i += 1;
            continue;
        }
        let m = match c {
            'a' | 'e' | 'i' | 'o' | 'u' | 'y' => 'A',
            'b' => 'P',
            'd' => 'T',
            'g' => 'K',
            'v' => 'F',
            'z' => 'S',
            'c' | 'k' | 'q' => 'K',
            'x' => {
                out.push('K');
                'S'
            }
            'j' => 'J',
            other => other.to_ascii_uppercase(),
        };
        // 元音压缩：连续 A 只留一个
        if !(m == 'A' && out.ends_with('A')) {
            out.push(m);
        }
        i += 1;
    }
    out
}

/// 音形相似度（0..1）
pub fn similarity(a: &str, b: &str) -> f32 {
    let (ka, kb) = (encode(a), encode(b));
    if ka.is_empty() || kb.is_empty() {
        return 0.0;
    }
    levenshtein_ratio(&ka, &kb)
}

fn levenshtein_ratio(a: &str, b: &str) -> f32 {
    let x: Vec<char> = a.chars().collect();
    let y: Vec<char> = b.chars().collect();
    let (n, m) = (x.len(), y.len());
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = if x[i - 1] == y[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j - 1] + cost).min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    1.0 - prev[m] as f32 / n.max(m) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_confusions_are_close() {
        // 中国学习者最典型的一批混淆
        for (a, b) in [
            ("think", "thing"),
            ("thin", "sin"),
            ("three", "tree"),
            ("light", "right"),
            ("world", "word"),
            ("work", "walk"),
            ("ship", "sheep"),
            ("west", "vest"),
        ] {
            let s = similarity(a, b);
            assert!(s >= 0.55, "{a} vs {b} 音形相似度仅 {s:.2}（keys: {} / {}）", encode(a), encode(b));
        }
    }

    #[test]
    fn unrelated_words_are_far() {
        assert!(similarity("cat", "elephant") < 0.35);
        assert!(similarity("third", "straight") < 0.55, "third/straight 不应被判成高相似");
    }

    #[test]
    fn same_word_is_one() {
        assert!((similarity("third", "third") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn documents_vowel_length_limitation() {
        // 已知局限：本编码不区分元音长短
        assert_eq!(encode("bit"), encode("beat"));
        assert_eq!(encode("bad"), encode("bed"));
    }
}
