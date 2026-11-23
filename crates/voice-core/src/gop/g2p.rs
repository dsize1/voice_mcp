//! 文本 → 音素（G2P）：CMUdict 查表 + OOV 报告。
//!
//! 为什么不用神经网络 G2P：教材/雅思素材的 OOV 率很低，查表已经够用，
//! 而且**可解释**——查不到的词直接报出来，而不是让模型猜一个音素串（猜错会把
//! 标准发音判成错读）。
//!
//! 音素集是 ARPAbet（与 speechocean762 的人工标注同一套），重音数字在入队时去掉。

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::assess::normalize;

/// 一个词的发音（可能有多个变体，取第一个用于对齐）。
#[derive(Debug, Clone)]
pub struct WordPhones {
    pub word: String,
    pub phones: Vec<String>,
}

#[derive(Debug, Default)]
pub struct G2p {
    dict: HashMap<String, Vec<Vec<String>>>,
}

/// G2P 结果：可用于对齐的音素串 + 未命中的词。
#[derive(Debug, Clone, Default)]
pub struct TextPhones {
    pub words: Vec<WordPhones>,
    /// 展平后的音素（ARPAbet，无重音数字）
    pub phones: Vec<String>,
    /// 词典里查不到的词（这些词的音素**不在** phones 里，不要当成"漏读"）
    pub oov: Vec<String>,
}

impl G2p {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取 CMUdict 失败: {}", path.display()))?;
        let mut dict: HashMap<String, Vec<Vec<String>>> = HashMap::new();
        for line in text.lines() {
            let line = line.trim_start_matches('\'');
            let mut it = line.split_whitespace();
            let Some(word) = it.next() else { continue };
            let base = word.split('(').next().unwrap_or(word).to_lowercase();
            let phones: Vec<String> = it
                .map(|p| {
                    p.trim_end_matches(|c: char| c.is_ascii_digit())
                        .to_uppercase()
                })
                .filter(|p| !p.is_empty())
                .collect();
            if phones.is_empty() {
                continue;
            }
            dict.entry(base).or_default().push(phones);
        }
        anyhow::ensure!(!dict.is_empty(), "CMUdict 为空: {}", path.display());
        Ok(Self { dict })
    }

    pub fn entries(&self) -> usize {
        self.dict.len()
    }

    /// 文本 → 音素串。未命中的词进 `oov`，不猜、不丢进 phones。
    pub fn text_to_phones(&self, text: &str) -> TextPhones {
        let mut out = TextPhones::default();
        for tok in normalize::tokens(text) {
            let w = tok.replace('\u{2019}', "'").to_lowercase();
            let w = w.trim_matches('\'').to_string();
            if w.is_empty() {
                continue;
            }
            // 纯数字：CMUdict 没有，直接算 OOV（数字读音交给上层决定）
            match self.dict.get(&w).and_then(|v| v.first()) {
                Some(phones) => {
                    out.phones.extend(phones.iter().cloned());
                    out.words.push(WordPhones {
                        word: w,
                        phones: phones.clone(),
                    });
                }
                None => {
                    if !out.oov.contains(&w) {
                        out.oov.push(w);
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 每个测试用独立文件：多个测试并行时共用一个路径会互相截断（曾导致 flaky）。
    static DICT_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn tmp_dict() -> std::path::PathBuf {
        let n = DICT_SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("cmudict_test_{}_{n}.dict", std::process::id()));
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "mark M AA1 R K").unwrap();
        writeln!(f, "is IH0 Z").unwrap();
        writeln!(f, "going G OW1 IH0 NG").unwrap();
        writeln!(f, "to T UW1").unwrap();
        writeln!(f, "see S IY1").unwrap();
        writeln!(f, "elephant EH1 L IH0 F AH0 N T").unwrap();
        writeln!(f, "don't D OW1 N T").unwrap();
        p
    }

    #[test]
    fn strips_stress_and_splits_words() {
        let g = G2p::load(&tmp_dict()).unwrap();
        let r = g.text_to_phones("Mark is going to see elephant.");
        assert_eq!(r.oov, Vec::<String>::new());
        assert_eq!(r.phones[0], "M");
        assert_eq!(r.phones[1], "AA", "重音数字应被去掉");
        assert!(r.phones.contains(&"OW".to_string()));
    }

    #[test]
    fn oov_is_reported_not_guessed() {
        let g = G2p::load(&tmp_dict()).unwrap();
        let r = g.text_to_phones("Mark zzzqx is");
        assert_eq!(r.oov, vec!["zzzqx".to_string()]);
        assert!(!r.phones.is_empty());
        assert_eq!(r.words.len(), 2, "OOV 词不进 words");
    }

    #[test]
    fn keeps_apostrophes_for_contractions() {
        let g = G2p::load(&tmp_dict()).unwrap();
        let r = g.text_to_phones("I don't know");
        // don't 命中词典；I / know 不在测试词典里 → OOV
        assert!(r.oov.contains(&"i".to_string()));
        assert!(r.oov.contains(&"know".to_string()));
        assert!(r.phones.contains(&"OW".to_string()));
    }
}
