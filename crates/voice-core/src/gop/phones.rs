//! 音素集映射与符号规范化。
//!
//! 参考音素来自 CMUdict（ARPAbet），模型输出是 espeak IPA。两者不是 1:1：
//! 同一个 ARPAbet 音素在不同词/语流里可能被模型记成不同 IPA 符号
//! （实测 `MARK` → `m ʌ k`，AA 被记成 ʌ；`TO` → `t ə`，UW 弱化成 ə）。
//!
//! 因此每个 ARPAbet 音素映射到一组**可接受符号**，GOP 取这组里的最大值；
//! 这等价于把"模型分不清的相邻音素"合成一个等价类，避免把方言/弱读当错读。

use serde::{Deserialize, Serialize};

/// 规范化 IPA 符号：去掉重音与长音标记，统一常见等价写法。
///
/// 目的：让 `ɔːɹ` / `ɔr` / `ɔː` 在比较时落到同一形，不因记法差异判错。
pub fn norm_symbol(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'ˈ' | 'ˌ' | 'ː' | 'ˑ' | '\u{0303}' | 'ʰ' | '̩' => continue, // 重音/长音/鼻化/送气/成音节
            'ɡ' => out.push('g'),  // IPA ɡ → ASCII g
            'ɹ' => out.push('r'),  // ɹ → r
            'ɫ' => out.push('l'),  // 暗 l
            'ɾ' | 'ʔ' => out.push('t'), // 闪音/喉塞：当作 /t/ 的弱化实现
            'ɐ' => out.push('ə'),  // 近开元音，并入 schwa 类
            'ɚ' | 'ɝ' => out.push('ə'), // r 化元音
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

/// ARPAbet → 可接受的 espeak 符号（已规范化）。
///
/// 保守原则：**等价类宁宽勿窄**。多收一个符号只会漏报（FN），
/// 少收一个就会把标准发音判成错（FP）——FP 会直接毁掉用户信任。
static ARPABET_TO_ESPEAK: &[(&str, &[&str])] = &[
    // 元音（含实测到的弱读/方言变体）
    ("AA", &["ɑ", "ʌ", "a", "ə"]),
    ("AE", &["æ", "a", "ɛ"]),
    ("AH", &["ʌ", "ə", "a"]),
    ("AO", &["ɔ", "o", "ɑ", "ʌ"]),
    ("AW", &["au", "aʊ"]),
    ("AY", &["ai", "aɪ"]),
    ("EH", &["ɛ", "e", "æ"]),
    ("ER", &["ə", "ɜ", "ə r", "ər"]),
    ("EY", &["ei", "eɪ", "e"]),
    ("IH", &["ɪ", "i", "ə"]),
    ("IY", &["i", "ɪ"]),
    ("OW", &["o", "ou", "oʊ", "əu"]),
    ("OY", &["ɔi", "ɔɪ"]),
    ("UH", &["ʊ", "u", "ə"]),
    ("UW", &["u", "ʊ", "ə"]),
    // 辅音
    ("B", &["b"]),
    ("CH", &["tʃ", "ʃ"]),
    ("D", &["d", "t"]),
    ("DH", &["ð", "d", "z"]),
    ("F", &["f"]),
    ("G", &["g", "k"]),
    ("HH", &["h", ""]),
    ("JH", &["dʒ", "ʒ", "tʃ"]),
    ("K", &["k", "g"]),
    ("L", &["l", "ə l"]),
    ("M", &["m"]),
    ("N", &["n", "ŋ"]),
    ("NG", &["ŋ", "n"]),
    ("P", &["p", "b"]),
    ("R", &["r", "ə", "ɜ"]),
    ("S", &["s", "z", "ʃ"]),
    ("SH", &["ʃ", "s"]),
    ("T", &["t", "d", "s"]),
    ("TH", &["θ", "f", "s", "t"]),
    ("V", &["v", "w", "f"]),
    ("W", &["w", "v"]),
    ("Y", &["j", "i"]),
    ("Z", &["z", "s", "ʒ"]),
    ("ZH", &["ʒ", "ʃ"]),
];

/// 参考音素在模型词表里的候选 id 集合。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhoneSet {
    /// 词表（index → 符号），由 `vocab.json` 读入
    pub vocab: Vec<String>,
    /// CTC blank 的 id（espeak 模型里是 `<pad>` = 0）
    pub blank: usize,
}

impl PhoneSet {
    pub fn new(vocab: Vec<String>) -> Self {
        let blank = vocab.iter().position(|s| s == "<pad>").unwrap_or(0);
        Self { vocab, blank }
    }

    /// 词表里某符号的 id（按规范化后比较）
    pub fn id_of(&self, sym: &str) -> Option<usize> {
        let want = norm_symbol(sym);
        self.vocab
            .iter()
            .position(|v| norm_symbol(v) == want)
    }

    /// ARPAbet 音素 → 词表候选 id（去重、去掉 blank 与特殊符号）
    pub fn candidates(&self, arpabet: &str) -> Vec<usize> {
        let key = arpabet.trim_end_matches(|c: char| c.is_ascii_digit()).to_uppercase();
        let mut out: Vec<usize> = Vec::new();
        for sym in espeak_candidates(&key) {
            if sym.is_empty() {
                continue;
            }
            if let Some(id) = self.id_of(sym) {
                if id != self.blank && !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        out
    }
}

/// 查 ARPAbet 音素的可接受符号（已规范化）。
pub fn espeak_candidates(arpabet: &str) -> Vec<&'static str> {
    let key = arpabet.trim_end_matches(|c: char| c.is_ascii_digit()).to_uppercase();
    ARPABET_TO_ESPEAK
        .iter()
        .find(|(a, _)| *a == key)
        .map(|(_, v)| v.to_vec())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_ipa_variants() {
        assert_eq!(norm_symbol("ɔːɹ"), "ɔr");
        assert_eq!(norm_symbol("ˈɡoʊ"), "goʊ"); // 长音/重音被去掉，ʊ 保留（候选表里 oʊ/ou 都收）
        assert_eq!(norm_symbol("tʃ"), "tʃ");
        assert_eq!(norm_symbol("ɐ"), "ə");
        assert_eq!(norm_symbol("ɹ"), "r");
    }

    #[test]
    fn arpabet_candidates_are_wide_enough() {
        // 实测：MARK 的 AA 被记成 ʌ，TO 的 UW 被记成 ə —— 都必须能命中，否则会造假错
        assert!(espeak_candidates("AA0").contains(&"ʌ"));
        assert!(espeak_candidates("UW").contains(&"ə"));
        assert!(espeak_candidates("TH").contains(&"θ"));
        assert!(espeak_candidates("NOPE").is_empty());
    }

    #[test]
    fn phone_set_resolves_ids() {
        let vocab = vec![
            "<pad>".to_string(),
            "<s>".to_string(),
            "</s>".to_string(),
            "<unk>".to_string(),
            "ʌ".to_string(),
            "t".to_string(),
            "ʃ".to_string(),
        ];
        let ps = PhoneSet::new(vocab);
        assert_eq!(ps.blank, 0);
        assert_eq!(ps.candidates("AH"), vec![4], "ʌ 是 AH 的唯一可用候选");
        assert!(ps.candidates("X").is_empty());
        assert!(!ps.candidates("HH").contains(&0), "blank 不能当候选");
    }
}
