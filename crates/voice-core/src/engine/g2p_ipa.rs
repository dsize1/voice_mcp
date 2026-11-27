//! 开放词表 G2P：**TTS 的默认文本前端**。
//!
//! 为什么需要这一层（2026-11-27 事故结论）：
//! sherpa-onnx v2.0.0 起删除了 espeak-ng 兜底（二进制里写死
//! `espeak-ng is no longer used`），Kokoro 的文本前端只剩 `lexicon` 查表；
//! 词表里没有的词（`client's`、`Where's`、未见专名）被**整词静默跳过**，
//! 音频缺词而调用方毫无察觉。实验已证明声学模型没问题（同模型 + 本标准音素即修复），
//! 瓶颈在**前端是否闭集**。
//!
//! 因此：
//! - 前端抽象成 [`G2pBackend`]，可以有多个实现；
//! - 音素统一经 [`KokoroTokenMap`] 映射到 Kokoro `tokens.txt`，再走
//!   `GenerationConfig::phoneme_codepoints` 注入 —— **完全绕开引擎词表**，
//!   引擎想丢词也没有机会；
//! - 默认实现 [`CmuBackend`]（纯 Rust、零外部依赖）；
//! - [`SidecarBackend`] 用于接外部开放词表 G2P（misaki / espeak-ng /
//!   piper_phonemize），见文末「接 misaki 的两条路」；
//! - `tts-lexicon.txt`（项目 overlay）降级为**高优先级覆盖层**，不再是唯一路径。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

// ================================================================ 公共类型

/// 一个词的发音来源（证据用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PhoneSource {
    /// 项目 overlay（最高优先级覆盖）
    Overlay,
    /// CMUdict 直查
    Dict,
    /// 形态规则（所有格/缩写）
    Morphology,
    /// 数字展开
    Number,
    /// 外部 G2P（misaki / espeak / piper_phonemize 等 sidecar）
    External,
    /// Kokoro 内置 lexicon（高优先级覆盖层：发音惯例与声学模型训练分布一致）
    Lexicon,
}

/// 逐词结果（留证 + 回归断言用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct WordPhones {
    pub word: String,
    pub ipa: String,
    pub source: PhoneSource,
}

/// 一次音素化的结果。
#[derive(Debug, Clone, Default)]
pub struct Phonemized {
    /// 逐句音素字符串（IPA；词内不空格、词间一个空格）
    pub sentence_ipa: Vec<String>,
    /// 逐词发音（顺序与原文一致）
    pub words: Vec<WordPhones>,
    /// 查不到音素的词。**调用方必须报错**，不允许继续合成。
    pub oov: Vec<String>,
}

impl Phonemized {
    pub fn is_empty(&self) -> bool {
        self.sentence_ipa.iter().all(|s| s.trim().is_empty())
    }

    pub fn ipa(&self) -> String {
        self.sentence_ipa.join(" ")
    }
}

/// 开放词表 G2P 后端。任何实现都**只允许**“产出音素”或“报 OOV”，
/// 不允许静默跳过词（这是这一层的契约）。
pub trait G2pBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn phonemize(&self, text: &str) -> Phonemized;
}

// ================================================================ 音素 → Kokoro token

/// espeak / misaki / 本项目 IPA 之间的常见写法差异 → Kokoro tokens.txt 的符号。
///
/// Kokoro 的 token 集是 misaki 那一套；喂 espeak 风格的串时必须归一化，
/// 否则会出现“音素字符不在词表里 → 被 skip”（又一种静默丢音）。
const IPA_ALIASES: &[(&str, &str)] = &[
    // 双元音：misaki/Kokoro 用**单字母符号**（A=eɪ I=aɪ O=oʊ W=aʊ Y=ɔɪ）。
    // 拆成两个 IPA 字符会被当成两个音素读（Friday → "fra-ee-di" → "fried"）。
    ("eɪ", "A"),
    ("aɪ", "I"),
    ("oʊ", "O"),
    ("əʊ", "O"),
    ("aʊ", "W"),
    ("ɔɪ", "Y"),
    // 儿化/卷舌
    ("ɚ", "əɹ"),
    ("ɝ", "ɜɹ"),
    ("tʃ", "ʧ"),
    ("dʒ", "ʤ"),
    ("ʧ", "ʧ"),
    ("ʤ", "ʤ"),
    ("ɡ", "ɡ"),
    ("g", "ɡ"),
    ("r", "ɹ"),
    ("ɜː", "ɜ"),
    ("ɑː", "ɑ"),
    ("ɔː", "ɔ"),
    ("iː", "i"),
    ("uː", "u"),
    ("eɪ", "eɪ"),
    ("oʊ", "oʊ"),
    ("aɪ", "aɪ"),
    ("aʊ", "aʊ"),
    ("ɔɪ", "ɔɪ"),
    ("ʔ", ""),
    ("ˑ", ""),
];

/// 归一化：把常见 IPA 变体折叠到 Kokoro 的符号集（长度标记 `ː`/重音保留）。
pub fn canonicalize_ipa(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'outer: while !rest.is_empty() {
        for (from, to) in IPA_ALIASES {
            if rest.starts_with(from) {
                out.push_str(to);
                rest = &rest[from.len()..];
                continue 'outer;
            }
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// Kokoro `tokens.txt` 的**字符集校验器**。
///
/// 关键语义（别搞错）：`GenerationConfig::phoneme_codepoints` 要的是
/// **音素字符的 Unicode 码点**（`'k'` → 107、`'ˈ'` → 712），不是 tokens.txt 里的
/// token id。传 id 会被 C++ 侧当成字符去查表，日志里出现
/// `Skip unknown phonemes. Unicode codepoint: ...`，然后**静默丢音**。
///
/// 因此这里的职责只有两件：
/// 1. [`KokoroTokenMap::encode`]：IPA 串 → 字符码点（喂 `phoneme_codepoints`）；
/// 2. 校验：串里每个字符是否真的在这个模型的符号集里（**合成前**报错）。
pub struct KokoroTokenMap {
    /// 模型符号集里出现过的全部字符（多字符符号按字符拆开计）
    chars: HashMap<char, i32>,
    path: PathBuf,
}

impl KokoroTokenMap {
    pub fn load(tokens_txt: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(tokens_txt)
            .with_context(|| format!("读 tokens.txt 失败: {}", tokens_txt.display()))?;
        let mut chars: HashMap<char, i32> = HashMap::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mut it = trimmed.rsplitn(2, ' ');
            let Some(id) = it.next().and_then(|v| v.trim().parse::<i32>().ok()) else {
                continue;
            };
            let Some(sym) = it.next().map(str::trim) else {
                continue;
            };
            for ch in sym.chars() {
                chars.entry(ch).or_insert(id);
            }
        }
        anyhow::ensure!(
            !chars.is_empty(),
            "tokens.txt 为空: {}",
            tokens_txt.display()
        );
        Ok(Self {
            chars,
            path: tokens_txt.to_path_buf(),
        })
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn has(&self, ch: char) -> bool {
        self.chars.contains_key(&ch)
    }

    /// IPA → **字符码点**（不是 token id）。`missing` 非空表示模型读不了这些音素。
    pub fn encode(&self, ipa: &str) -> (Vec<i32>, Vec<char>) {
        let mut cps = Vec::with_capacity(ipa.chars().count());
        let mut missing: Vec<char> = Vec::new();
        for ch in ipa.chars() {
            // 词边界必须**保留**：Kokoro 的符号集里有空格 token（tokens.txt `  16`），
            // 丢掉它会把 "start on" 读成 "star ton"、"isn't ready" 读成 "tizen tready"
            // （实测：不传空格时 whisper 会把邻近词合并成新词）。
            // 空格**不能**当 token 发出去：Kokoro 的 tokens.txt 里 `  16` 是个符号，
            // 但它不是“静音”，发出去会把相邻词糊成一个（实测 "and they"→"then they"、
            // "isn't ready"→"tizen dready"）。词边界交给音素序列本身。
            if ch == ' ' {
                continue;
            }
            if self.chars.contains_key(&ch) {
                cps.push(ch as i32);
            } else if !missing.contains(&ch) {
                missing.push(ch);
            }
        }
        (cps, missing)
    }

    /// 逐句编码；任何一句出现模型符号集之外的音素都返回 Err（不静默降级）。
    pub fn encode_sentences(&self, ipas: &[String]) -> Result<Vec<Vec<i32>>> {
        let mut out = Vec::new();
        let mut all_missing: Vec<char> = Vec::new();
        for s in ipas {
            let (cps, missing) = self.encode(&canonicalize_ipa(s));
            for m in missing {
                if !all_missing.contains(&m) {
                    all_missing.push(m);
                }
            }
            out.push(cps);
        }
        if !all_missing.is_empty() {
            bail!(
                "音素字符不在 {} 里: {:?}（换模型/换词表后必须重新校验）",
                self.path.display(),
                all_missing
            );
        }
        Ok(out)
    }
}

// ================================================================ 文本切片

pub fn is_cjk_char(c: char) -> bool {
    is_cjk(c)
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{3040}'..='\u{30FF}')
}

/// 含 CJK 的文本不走英文 G2P（中文需要 jieba/pypinyin → Bopomofo，
/// 见 `SidecarBackend` 与文末说明；当前由引擎 `lexicon-zh` 兜着）。
pub fn contains_cjk(text: &str) -> bool {
    text.chars().any(is_cjk)
}

/// 是否中英混说（同时含 CJK 段与非 CJK 段）。
///
/// 产品契约是“一段文本只允许一种语言”（ADR-0002）。混说在单音色下必然降级
/// （`zf_xiaobei` 读英文、`am_michael` 读中文都跑调，拼接点还有 0.2–0.5 s 停顿），
/// 所以默认**拒答**；内部实验用 `VOICE_ALLOW_MIXED=1` / `voice-cli say --allow-mixed` 放行。
pub fn is_mixed(text: &str) -> bool {
    let mut has_cjk = false;
    let mut has_en = false;
    for (cjk, seg) in split_segments(text) {
        if seg.trim().is_empty() {
            continue;
        }
        if cjk {
            has_cjk = true;
        } else {
            has_en = true;
        }
    }
    has_cjk && has_en
}

/// 中英混说时拼接多段音素的标记（只用于留证，不参与合成）。
pub const SEGMENT_JOINER: &str = " | ";

/// 把文本切成「CJK 段 / 非 CJK 段」，保持原文顺序与标点不变。
///
/// 为什么必须分段：`phoneme_codepoints` 一旦给出，引擎就**完全**按我们的音素
/// 合成；而中文要走引擎的 `lexicon-zh`（Bopomofo 需要 jieba/pypinyin）。
/// 不分段就只有两个坏选项：整句走中文词表（英文词被静默丢掉）或整句走英文 G2P
/// （中文全部报 OOV）。分段后两套前端各自只处理自己那段。
pub fn split_segments(text: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut cur = String::new();
    let mut cur_cjk: Option<bool> = None;
    for ch in text.chars() {
        // 标点/空白不切换语言段，跟着当前段走
        let this = if is_cjk(ch) {
            Some(true)
        } else if ch.is_whitespace()
            || ch.is_ascii_punctuation()
            || "，。！？；：、（）《》“”‘’…".contains(ch)
        {
            None
        } else {
            Some(false)
        };
        match (cur_cjk, this) {
            (None, Some(k)) => {
                cur_cjk = Some(k);
                cur.push(ch);
            }
            (Some(k), Some(k2)) if k != k2 => {
                if !cur.trim().is_empty() {
                    out.push((k, std::mem::take(&mut cur)));
                } else {
                    cur.clear();
                }
                cur_cjk = Some(k2);
                cur.push(ch);
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty()
        && let Some(k) = cur_cjk
    {
        out.push((k, cur));
    }
    out
}

fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if matches!(ch, '.' | '!' | '?' | ';' | ':') {
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// 词切分：字母数字 + 撇号 + 连字符；标点丢弃。
pub fn words_of(sentence: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in sentence.chars() {
        if ch.is_alphanumeric() || ch == '\'' || ch == '\u{2019}' || ch == '-' {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.into_iter()
        .map(|w| w.trim_matches('-').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

// ================================================================ CMUdict 后端

const VOWELS: &[&str] = &[
    "AA", "AE", "AH", "AO", "AW", "AY", "EH", "ER", "EY", "IH", "IY", "OW", "OY", "UH", "UW",
];
/// 清辅音（`'s` → /s/）
const VOICELESS: &[&str] = &["P", "T", "K", "F", "TH", "S", "SH", "CH"];
/// 咝音（`'s` → /ɪz/）
const SIBILANT: &[&str] = &["S", "Z", "SH", "ZH", "CH", "JH"];

/// 默认后端：CMUdict（135k 词根，带重音）+ 形态规则 + 数字展开 + overlay 覆盖。
pub struct CmuBackend {
    dict: HashMap<String, Vec<String>>,
    /// Kokoro 内置 lexicon（us-en / gb-en）：**高优先级覆盖层**。
    /// 命中时用它的发音（与声学模型训练分布一致，音质最好）；
    /// 未命中才落到 CMUdict + 形态规则 —— 于是“闭集”只影响音质，不影响覆盖。
    lexicon: HashMap<String, String>,
    overlay: HashMap<String, String>,
    dict_path: PathBuf,
}

impl CmuBackend {
    pub fn load(
        cmu_path: &Path,
        overlay_path: Option<&Path>,
        lexicon_paths: &[PathBuf],
    ) -> Result<Self> {
        let text = std::fs::read_to_string(cmu_path)
            .with_context(|| format!("读 CMUdict 失败: {}", cmu_path.display()))?;
        let mut dict: HashMap<String, Vec<String>> = HashMap::new();
        for line in text.lines() {
            let line = line.trim_start_matches('\'');
            let mut it = line.split_whitespace();
            let Some(word) = it.next() else { continue };
            let base = word.split('(').next().unwrap_or(word).to_lowercase();
            let phones: Vec<String> = it.map(str::to_uppercase).collect();
            if !phones.is_empty() {
                dict.entry(base).or_insert(phones);
            }
        }
        anyhow::ensure!(!dict.is_empty(), "CMUdict 为空: {}", cmu_path.display());
        // 顺序：先 gb-en 再 us-en（美音优先覆盖），overlay 最后（最高优先级）
        let mut lexicon: HashMap<String, String> = HashMap::new();
        for p in lexicon_paths {
            load_lexicon_into(&mut lexicon, p);
        }
        Ok(Self {
            dict,
            lexicon,
            overlay: load_overlay(overlay_path),
            dict_path: cmu_path.to_path_buf(),
        })
    }

    pub fn entries(&self) -> usize {
        self.dict.len()
    }

    pub fn overlay_entries(&self) -> usize {
        self.overlay.len()
    }

    pub fn dict_path(&self) -> &Path {
        &self.dict_path
    }

    /// 单词 → IPA。`None` 表示确实没有依据（进 OOV，由调用方报错）。
    fn word_ipa(&self, raw: &str) -> Option<(String, PhoneSource)> {
        self.word_ipa_ctx(raw, None)
    }

    fn word_ipa_ctx(&self, raw: &str, next_word: Option<&str>) -> Option<(String, PhoneSource)> {
        let w = raw.replace('\u{2019}', "'").to_lowercase();
        if w.is_empty() {
            return None;
        }
        if let Some(ipa) = self.overlay.get(&w) {
            return Some((ipa.clone(), PhoneSource::Overlay));
        }
        if let Some(ipa) = self.lexicon.get(&w) {
            return Some((ipa.clone(), PhoneSource::Lexicon));
        }
        if w.chars().all(|c| c.is_ascii_digit()) {
            // 读法按上下文分三种（见 number_candidates）：
            //   404 error → four oh four（逐位）；2026 → twenty twenty six（年份）
            //   140 wpm  → one hundred forty（基数）
            let words = read_number(&w, next_word)?;
            let ipa = self.phrase_ipa(&words)?;
            return Some((ipa, PhoneSource::Number));
        }
        if let Some(p) = self.dict.get(&w) {
            return Some((arpa_to_ipa(p), PhoneSource::Dict));
        }
        if w.contains('-') {
            let mut ipa = String::new();
            for part in w.split('-').filter(|p| !p.is_empty()) {
                let (i, _) = self.word_ipa(part)?;
                ipa.push_str(&i);
            }
            return Some((ipa, PhoneSource::Morphology));
        }
        if let Some((base, suf)) = w.rsplit_once('\'') {
            // n't 必须先把词尾 n 去掉再查（isn't → is），否则 base="isn" 查不到
            if suf == "t" && base.ends_with('n') && base.len() > 1 {
                let stem = &base[..base.len() - 1];
                let stem_ipa = self.base_ipa(stem)?;
                return Some((format!("{stem_ipa}nt"), PhoneSource::Morphology));
            }
            let base_ipa = self.base_ipa(base)?;
            let last = self.base_last_phone(base);
            let tail = match suf {
                "s" => {
                    if last.as_deref().is_some_and(|l| SIBILANT.contains(&l)) {
                        "ɪz".to_string()
                    } else if last.as_deref().is_some_and(|l| VOICELESS.contains(&l)) {
                        "s".to_string()
                    } else {
                        "z".to_string()
                    }
                }
                "ll" => "l".into(),
                "re" => "ɹ".into(),
                "ve" => "v".into(),
                "d" => "d".into(),
                "m" => "m".into(),
                _ => return None,
            };
            return Some((format!("{base_ipa}{tail}"), PhoneSource::Morphology));
        }
        None
    }

    fn base_ipa(&self, base: &str) -> Option<String> {
        if let Some(ipa) = self.overlay.get(base) {
            return Some(ipa.clone());
        }
        if let Some(ipa) = self.lexicon.get(base) {
            return Some(ipa.clone());
        }
        self.dict.get(base).map(|p| arpa_to_ipa(p))
    }

    /// 词根最后一个音素（用来给 `'s` 选 /s//z//ɪz/）
    fn base_last_phone(&self, base: &str) -> Option<String> {
        if let Some(ipa) = self.lookup_ipa(base) {
            // 去掉重音符号后取最后一个音素字符
            let last = ipa.chars().rev().find(|c| !matches!(c, 'ˈ' | 'ˌ' | 'ː'))?;
            return Some(last.to_string());
        }
        self.dict
            .get(base)
            .and_then(|p| p.last())
            .map(|p| p.trim_end_matches(|c: char| c.is_ascii_digit()).to_string())
    }

    fn lookup_ipa(&self, word: &str) -> Option<String> {
        if let Some(ipa) = self.overlay.get(word) {
            return Some(ipa.clone());
        }
        if let Some(ipa) = self.lexicon.get(word) {
            return Some(ipa.clone());
        }
        None
    }

    fn phrase_ipa(&self, phrase: &str) -> Option<String> {
        let mut out = String::new();
        for w in phrase.split_whitespace() {
            let (ipa, _) = self.word_ipa(w)?;
            out.push_str(&ipa);
        }
        Some(out)
    }
}

impl G2pBackend for CmuBackend {
    fn name(&self) -> &'static str {
        "cmudict+overlay"
    }

    fn phonemize(&self, text: &str) -> Phonemized {
        let mut out = Phonemized::default();
        for sentence in split_sentences(text) {
            let mut ipa_sentence = String::new();
            let toks = words_of(&sentence);
            for (i, word) in toks.iter().enumerate() {
                let next = toks.get(i + 1).map(|w| w.to_lowercase());
                match self.word_ipa_ctx(word, next.as_deref()) {
                    Some((ipa, source)) => {
                        if !ipa_sentence.is_empty() {
                            ipa_sentence.push(' ');
                        }
                        ipa_sentence.push_str(&ipa);
                        out.words.push(WordPhones {
                            word: word.clone(),
                            ipa,
                            source,
                        });
                    }
                    None => {
                        if !out.oov.iter().any(|w| w.eq_ignore_ascii_case(word)) {
                            out.oov.push(word.clone());
                        }
                    }
                }
            }
            out.sentence_ipa.push(ipa_sentence);
        }
        out
    }
}

/// 读 Kokoro lexicon（`word phoneme phoneme ...`，音素之间空格仅作分隔，
/// 转成无空格 IPA 串，和 G2P 输出同一形态）。
fn load_lexicon_into(map: &mut HashMap<String, String>, path: &Path) {
    let Ok(txt) = std::fs::read_to_string(path) else {
        return;
    };
    for line in txt.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let Some(w) = it.next() else { continue };
        let ipa: String = it.collect::<Vec<_>>().join("");
        if !ipa.is_empty() {
            map.entry(w.to_lowercase()).or_insert(ipa);
        }
    }
}

fn load_overlay(path: Option<&Path>) -> HashMap<String, String> {
    let mut overlay = HashMap::new();
    let Some(p) = path else { return overlay };
    let Ok(txt) = std::fs::read_to_string(p) else {
        return overlay;
    };
    for line in txt.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let Some(w) = it.next() else { continue };
        let ipa: String = it.collect::<Vec<_>>().join("");
        if !ipa.is_empty() {
            overlay.insert(w.to_lowercase().replace('\u{2019}', "'"), ipa);
        }
    }
    overlay
}

// ================================================================ 外部 G2P sidecar

/// 把“外部开放词表 G2P”接进来的通用适配器：**每行一句 → 每行一个 IPA 串**。
///
/// 命令约定（stdin 写文本，stdout 逐行读音素）：
/// ```text
/// # misaki（Python 包，Kokoro 官方前端）
/// python -m misaki --ipa -
/// # espeak-ng
/// espeak-ng --ipa=3 -q -f -
/// # piper_phonemize（Piper 生态）
/// python -c "import sys,piper_phonemize as p; ..."
/// ```
///
/// 优点：不需要任何 Rust 依赖，也不需要联网拉 crate；语言覆盖由 sidecar 决定。
/// 缺点：多一个进程；启动/异常要显式报错（不允许安静退化）。
pub struct SidecarBackend {
    program: String,
    args: Vec<String>,
    name: &'static str,
    /// 单次调用超时（超时即杀进程并报错）
    timeout: std::time::Duration,
    /// 连续失败次数（≥3 熔断，避免每个请求都白等一次超时）
    fail_streak: std::sync::atomic::AtomicUsize,
}

impl SidecarBackend {
    pub fn new(program: impl Into<String>, args: Vec<String>, name: &'static str) -> Self {
        Self {
            program: program.into(),
            args,
            name,
            timeout: default_timeout(),
            fail_streak: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 从 `VOICE_G2P_CMD` 读环境变量形式的配置：
    /// `VOICE_G2P_CMD="python -m misaki --ipa -"`
    ///
    /// 启动时做一次**存在性检查**：程序找不到就立刻失败（而不是每个请求都超时一次）。
    pub fn from_env() -> Option<Self> {
        let cmd = std::env::var("VOICE_G2P_CMD").ok()?;
        let mut it = cmd.split_whitespace();
        let program = it.next()?.to_string();
        Some(Self::new(
            program,
            it.map(str::to_string).collect(),
            "external-sidecar",
        ))
    }

    /// 超时（单次调用）。默认 5 s，可用 `VOICE_G2P_TIMEOUT_MS` 覆盖。
    pub fn with_timeout(mut self, d: std::time::Duration) -> Self {
        self.timeout = d;
        self
    }

    /// 健康检查：程序能否解析到（PATH/绝对路径）。找不到 → 立即报错。
    pub fn check_program(&self) -> Result<()> {
        if program_exists(&self.program) {
            Ok(())
        } else {
            bail!(
                "外部 G2P 程序找不到: {}（VOICE_G2P_CMD 配置错误？）",
                self.program
            )
        }
    }

    /// 一次调用：写 stdin（独立线程，避免卡住调用方）＋ 读 stdout/stderr 带超时。
    ///
    /// 超时/崩溃都**杀进程并报错**，绝不退回闭集词表（那会重新引入静默丢词）。
    fn run_once(&self, sentences: &[String]) -> Result<Vec<String>> {
        self.check_program()?;
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("启动外部 G2P 失败: {}", self.program))?;

        // stdin 交给独立线程写：子进程不读 stdin 时不会把调用方卡死
        let payload = {
            let mut s = sentences.join("\n");
            s.push('\n');
            s
        };
        if let Some(mut stdin) = child.stdin.take() {
            std::thread::spawn(move || {
                let _ = stdin.write_all(payload.as_bytes());
                let _ = stdin.flush();
                // 关闭 stdin 让子进程看到 EOF
            });
        }

        // stdout / stderr 各一个读线程 + 超时等待
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = std::io::BufReader::new(stdout).read_to_end(&mut buf);
                let _ = tx.send(buf);
            });
        }
        let (etx, erx) = std::sync::mpsc::channel::<Vec<u8>>();
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = std::io::BufReader::new(stderr).read_to_end(&mut buf);
                let _ = etx.send(buf);
            });
        }

        let stdout_buf = match rx.recv_timeout(self.timeout) {
            Ok(b) => b,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "外部 G2P 超时（>{} ms，{} 句）：已杀进程。检查 sidecar 是否卡住/需要预热",
                    self.timeout.as_millis(),
                    sentences.len()
                );
            }
        };
        let status = child.wait()?;
        let stderr_buf = erx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap_or_default();
        if !status.success() {
            bail!(
                "外部 G2P 退出码 {:?}: {}",
                status.code(),
                String::from_utf8_lossy(&stderr_buf).trim()
            );
        }
        let text = String::from_utf8_lossy(&stdout_buf);
        let lines: Vec<String> = text.lines().map(|l| l.trim().to_string()).collect();
        if lines.len() != sentences.len() {
            bail!(
                "外部 G2P 行数不匹配：{} 句 → {} 行（约定：每行一句，逐行一个音素串）",
                sentences.len(),
                lines.len()
            );
        }
        Ok(lines)
    }

    /// 带**一次重启重试**的调用 + 熔断（连续失败 3 次后快速失败，避免每个请求都等超时）。
    fn run(&self, sentences: &[String]) -> Result<Vec<String>> {
        use std::sync::atomic::Ordering;
        if self.fail_streak.load(Ordering::Relaxed) >= 3 {
            bail!(
                "外部 G2P 已连续失败 {} 次，熔断中（重启 MCP server 或修正 VOICE_G2P_CMD 后恢复）",
                self.fail_streak.load(Ordering::Relaxed)
            );
        }
        let mut last = None;
        for attempt in 0..2 {
            match self.run_once(sentences) {
                Ok(v) => {
                    self.fail_streak.store(0, Ordering::Relaxed);
                    return Ok(v);
                }
                Err(e) => {
                    self.fail_streak.fetch_add(1, Ordering::Relaxed);
                    last = Some(e);
                    if attempt == 0 {
                        // 第一次失败：子进程可能已崩，重起一次（新进程 = 自愈）
                        continue;
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("外部 G2P 调用失败")))
    }
}

fn default_timeout() -> std::time::Duration {
    let ms = std::env::var("VOICE_G2P_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(5_000);
    std::time::Duration::from_millis(ms.clamp(200, 120_000))
}

/// 程序能否解析到（绝对/相对路径直接查文件；裸名字查 PATH + PATHEXT）。
fn program_exists(program: &str) -> bool {
    let p = std::path::Path::new(program);
    if program.contains('/') || program.contains('\\') {
        return p.is_file();
    }
    let exts: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".into());
        std::iter::once(String::new())
            .chain(pathext.split(';').map(|e| e.to_lowercase()))
            .collect()
    } else {
        vec![String::new()]
    };
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            if dir.join(format!("{program}{ext}")).is_file() {
                return true;
            }
        }
    }
    false
}

impl G2pBackend for SidecarBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    fn phonemize(&self, text: &str) -> Phonemized {
        let sentences = split_sentences(text);
        let mut out = Phonemized::default();
        let ipas = match self.run(&sentences) {
            Ok(v) => v,
            Err(e) => {
                // 外部后端失败 → 整句进 OOV，让调用方**报错**而不是给出错音频
                out.oov.push(format!("<sidecar: {e:#}>"));
                out.sentence_ipa = sentences;
                return out;
            }
        };
        for (sent, ipa) in sentences.iter().zip(ipas) {
            out.sentence_ipa.push(ipa.clone());
            let toks = words_of(sent);
            if toks.is_empty() {
                continue;
            }
            let mut chunks = ipa.split(' ');
            for w in toks {
                match chunks.next() {
                    Some(c) if !c.is_empty() => out.words.push(WordPhones {
                        word: w,
                        ipa: c.to_string(),
                        source: PhoneSource::External,
                    }),
                    _ => out.oov.push(w),
                }
            }
        }
        out
    }
}

// ================================================================ ARPAbet → IPA

fn arpa_to_ipa_one(p: &str) -> Option<&'static str> {
    let base = p.trim_end_matches(|c: char| c.is_ascii_digit());
    Some(match base {
        "AA" => "ɑ",
        "AE" => "æ",
        "AH" => "ʌ",
        "AO" => "ɔ",
        "AW" => "W",
        "AY" => "I",
        "B" => "b",
        "CH" => "ʧ",
        "D" => "d",
        "DH" => "ð",
        "EH" => "ɛ",
        "ER" => "ɜɹ",
        "EY" => "A",
        "F" => "f",
        "G" => "ɡ",
        "HH" => "h",
        "IH" => "ɪ",
        "IY" => "i",
        "JH" => "ʤ",
        "K" => "k",
        "L" => "l",
        "M" => "m",
        "N" => "n",
        "NG" => "ŋ",
        "OW" => "O",
        "OY" => "Y",
        "P" => "p",
        "R" => "ɹ",
        "S" => "s",
        "SH" => "ʃ",
        "T" => "t",
        "TH" => "θ",
        "UH" => "ʊ",
        "UW" => "u",
        "V" => "v",
        "W" => "w",
        "Y" => "j",
        "Z" => "z",
        "ZH" => "ʒ",
        _ => return None,
    })
}

fn arpa_to_ipa(phones: &[String]) -> String {
    let mut s = String::new();
    for p in phones {
        let base = p.trim_end_matches(|c: char| c.is_ascii_digit());
        let stress = p
            .chars()
            .last()
            .filter(|c| c.is_ascii_digit())
            .and_then(|c| c.to_digit(10));
        if VOWELS.contains(&base) {
            match stress {
                Some(1) => s.push('ˈ'),
                Some(2) => s.push('ˌ'),
                _ => {}
            }
        }
        // 央元音按重音分：AH0 → /ə/（schwa）、AH1/2 → /ʌ/；ER0 → /ɚ/、ER1/2 → /ɜɹ/
        // 只按 AH→ʌ 一处映射会让 "the"→ðʌ、"client's"→klˈaɪʌnts、"aren't"→ˈɑɹʌnt 全部读歪。
        let ipa = match (base, stress) {
            ("AH", Some(0)) => Some("ə"),
            ("AH", _) => Some("ʌ"),
            ("ER", Some(0)) => Some("əɹ"),
            ("ER", _) => Some("ɜɹ"),
            _ => arpa_to_ipa_one(p),
        };
        if let Some(ipa) = ipa {
            s.push_str(ipa);
        }
    }
    s
}

// ================================================================ 数字

const ONES: &[&str] = &[
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
const TENS: &[&str] = &[
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// 0..=999_999 基数词（小数/负数/序数暂不特殊处理）。
pub fn number_to_words(digits: &str) -> Option<String> {
    number_words(digits)
}

/// 状态码语境：后面跟这些词时按**逐位**读（`404 error` → four oh four）。
fn is_code_context(next: Option<&str>) -> bool {
    let Some(n) = next else { return false };
    matches!(
        n,
        "error" | "errors" | "code" | "codes" | "status" | "http" | "not" | "gateway"
    )
}

/// 逐位读：0 读 "oh"（`404` → four oh four）。
fn digit_by_digit(digits: &str) -> String {
    digits
        .chars()
        .filter_map(|c| c.to_digit(10))
        .map(|d| {
            if d == 0 {
                "oh".to_string()
            } else {
                ONES[d as usize].to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 年份成对读：`2026` → twenty twenty six、`1900` → nineteen hundred、`2005` → two thousand five。
fn year_words(digits: &str) -> Option<String> {
    if digits.len() != 4 {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    if !(1100..=2099).contains(&n) {
        return None;
    }
    if (2000..=2009).contains(&n) {
        return Some(format!("two thousand {}", ONES[(n - 2000) as usize]));
    }
    let hi: u32 = digits[..2].parse().ok()?;
    let lo: u32 = digits[2..].parse().ok()?;
    if lo == 0 {
        return Some(format!("{} hundred", number_words(&hi.to_string())?));
    }
    Some(format!(
        "{} {}",
        number_words(&hi.to_string())?,
        number_words(&lo.to_string())?
    ))
}

/// 一个数字串所有“合理读法”（供回归门禁做多候选匹配）。
pub fn number_candidates(digits: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if let Some(c) = number_words(digits) {
        v.push(c);
    }
    if digits.len() >= 2 {
        v.push(digit_by_digit(digits));
    }
    if let Some(y) = year_words(digits)
        && !v.contains(&y)
    {
        v.push(y);
    }
    v
}

/// 按上下文决定数字读法。
fn read_number(digits: &str, next: Option<&str>) -> Option<String> {
    if is_code_context(next) && digits.len() >= 3 {
        return Some(digit_by_digit(digits));
    }
    if let Some(y) = year_words(digits) {
        return Some(y);
    }
    number_words(digits)
}

fn number_words(digits: &str) -> Option<String> {
    let n: u64 = digits.parse().ok()?;
    if n > 999_999 {
        return None;
    }
    if n < 20 {
        return Some(ONES[n as usize].to_string());
    }
    if n < 100 {
        let t = TENS[(n / 10) as usize];
        return Some(match n % 10 {
            0 => t.to_string(),
            r => format!("{t} {}", ONES[r as usize]),
        });
    }
    let mut parts: Vec<String> = Vec::new();
    let thousands = n / 1000;
    let rest = n % 1000;
    if thousands > 0 {
        parts.push(format!(
            "{} thousand",
            number_words(&thousands.to_string())?
        ));
    }
    if rest > 0 {
        let h = rest / 100;
        let r = rest % 100;
        let mut s = String::new();
        if h > 0 {
            s.push_str(&format!("{} hundred", ONES[h as usize]));
        }
        if r > 0 {
            if !s.is_empty() {
                s.push(' ');
            }
            s.push_str(&number_words(&r.to_string())?);
        }
        parts.push(s);
    }
    Some(parts.join(" "))
}

// ================================================================ 测试

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn tmp_cmu() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("cmudict_g2p_{}_{n}.dict", std::process::id()));
        let mut f = std::fs::File::create(&p).unwrap();
        // 只放词根：所有格/缩写全部靠形态规则
        for line in [
            "client K L AY1 AH0 N T",
            "the DH AH0",
            "but B AH1 T",
            "time T AY1 M",
            "day D EY1",
            "no N OW1",
            "how HH AW1",
            "boy B OY1",
            "aren't AA1 R AH0 N T",
            "this DH IH1 S",
            "they DH EY1",
            "and AE1 N D",
            "teacher T IY1 CH ER0",
            "where W EH1 R",
            "what W AH1 T",
            "sarah S EH1 R AH0",
            "apple AE1 P AH0 L",
            "did D IH1 D",
            "is IH1 Z",
            "he HH IY1",
            "report R IH0 P AO1 R T",
            "ready R EH1 D IY0",
            "laptop L AE1 P T AA2 P",
            "working W ER1 K IH0 NG",
        ] {
            writeln!(f, "{line}").unwrap();
        }
        p
    }

    fn backend() -> CmuBackend {
        CmuBackend::load(&tmp_cmu(), None, &[]).unwrap()
    }

    /// 事故核心：`X's` / `Where's` 必须产出音素（旧前端在这里整词丢失）。
    #[test]
    fn possessive_and_wh_clitic_never_vanish() {
        let b = backend();
        for text in [
            "The client's report isn't ready.",
            "Where's the report?",
            "The teacher's laptop isn't working.",
            "What's this?",
            "Sarah's laptop.",
        ] {
            let p = b.phonemize(text);
            assert!(p.oov.is_empty(), "{text} 应有全部音素，却缺 {:?}", p.oov);
            assert!(!p.is_empty(), "{text} 音素为空");
            assert_eq!(p.words.len(), words_of(text).len(), "{text} 丢词");
            for w in &p.words {
                assert!(!w.ipa.is_empty(), "{} 没有音素", w.word);
            }
        }
    }

    /// `'s` 的清浊三选一按词尾推导（生产性规则，不是枚举）。
    #[test]
    fn possessive_sibilant_rule_is_phonological() {
        let b = backend();
        let ipa = |t: &str| b.phonemize(t).words[0].ipa.clone();
        assert!(ipa("client's").ends_with('s'), "t 后应读 /s/");
        assert!(ipa("teacher's").ends_with('z'), "ɹ 后应读 /z/");
        assert!(ipa("sarah's").ends_with('z'), "元音后应读 /z/");
    }

    /// 未登录词根必须报 OOV，不能静默产出半句。
    #[test]
    fn unknown_root_is_reported_not_dropped() {
        let b = backend();
        let p = b.phonemize("Zorbly's report.");
        assert_eq!(p.oov, vec!["Zorbly's".to_string()]);
        assert!(p.words.iter().any(|w| w.word == "report"));
    }

    /// overlay = 高优先级覆盖层。
    #[test]
    fn overlay_overrides_dict() {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let ov = std::env::temp_dir().join(format!("overlay_{}_{n}.txt", std::process::id()));
        std::fs::write(&ov, "# comment\nclient k l ˈ aɪ ə n t s\n").unwrap();
        let b = CmuBackend::load(&tmp_cmu(), Some(&ov), &[]).unwrap();
        let p = b.phonemize("client");
        assert_eq!(p.words[0].source, PhoneSource::Overlay);
        assert_eq!(p.words[0].ipa, "klˈaɪənts");
    }

    /// 词数守恒：每个词要么有音素，要么进 OOV，没有第三种状态。
    /// 内置 lexicon 是**高优先级覆盖层**：命中即用它的发音（音质优先），
    /// 未命中才走 CMUdict/形态规则（覆盖优先）。
    #[test]
    fn builtin_lexicon_overrides_dict_but_misses_fall_through() {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let lex = std::env::temp_dir().join(format!("lex_{}_{n}.txt", std::process::id()));
        std::fs::write(
            &lex,
            "isn't ˈ ɪ z ᵊ n t
",
        )
        .unwrap();
        let b = CmuBackend::load(&tmp_cmu(), None, &[lex]).unwrap();
        let p = b.phonemize("isn't");
        assert_eq!(p.words[0].source, PhoneSource::Lexicon);
        assert_eq!(p.words[0].ipa, "ˈɪzᵊnt");
        // lexicon 没有的词仍然能出音素（这就是“覆盖”与“闭集”的区别）
        let q = b.phonemize("teacher's");
        assert_eq!(q.oov.len(), 0);
        assert_eq!(q.words[0].source, PhoneSource::Morphology);
    }

    #[test]
    fn every_word_is_accounted_for() {
        let b = backend();
        let text = "They aren't ready, and it isn't done.";
        let p = b.phonemize(text);
        assert_eq!(
            p.words.len() + p.oov.len(),
            words_of(text).len(),
            "有词既没音素也没进 OOV → 静默丢词"
        );
    }

    #[test]
    fn schwa_and_stressed_central_vowel_are_distinguished() {
        let b = backend();
        let ipa = |t: &str| b.phonemize(t).ipa();
        assert_eq!(ipa("the"), "ðə", "AH0 必须是 schwa");
        assert!(ipa("client's").contains("ənts"), "client's = klˈaɪənts");
        assert_eq!(ipa("aren't"), "ˈɑɹənt", "AH0 在词中必须是 schwa");
        assert!(ipa("but").contains('ʌ'), "AH1 应读 ʌ");
    }

    #[test]
    fn numbers_expand() {
        assert_eq!(number_words("7").unwrap(), "seven");
        assert_eq!(number_words("42").unwrap(), "forty two");
        assert_eq!(number_words("140").unwrap(), "one hundred forty");
        assert_eq!(number_words("2026").unwrap(), "two thousand twenty six");
    }

    /// 双元音必须映射成 Kokoro 的单字母符号，否则会被读成两个音素。
    #[test]
    fn diphthongs_map_to_kokoro_single_letters() {
        let b = backend();
        let ipa = |t: &str| b.phonemize(t).ipa();
        assert!(ipa("time").contains('I'), "aɪ → I: {}", ipa("time"));
        assert!(ipa("day").contains('A'), "eɪ → A");
        assert!(ipa("no").contains('O'), "oʊ → O");
        assert!(ipa("how").contains('W'), "aʊ → W");
        assert!(ipa("boy").contains('Y'), "ɔɪ → Y");
        assert_eq!(canonicalize_ipa("fɹaɪdeɪ"), "fɹIdA");
        assert_eq!(canonicalize_ipa("tʃɚtʃ"), "ʧəɹʧ");
    }

    #[test]
    fn canonicalize_maps_espeak_style_to_kokoro_tokens() {
        assert_eq!(canonicalize_ipa("tʃɜːnz"), "ʧɜnz");
        assert_eq!(canonicalize_ipa("dʒʌst"), "ʤʌst");
        assert_eq!(canonicalize_ipa("wɜːr"), "wɜɹ");
        assert_eq!(canonicalize_ipa("ɡoʊ"), "ɡO");
    }

    #[test]
    fn token_map_encodes_and_reports_missing() {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("tokens_{}_{n}.txt", std::process::id()));
        std::fs::write(&p, "; comment\nk 1\nl 2\nˈ 3\naɪ 4\nə 5\nn 6\nt 7\ns 8\n").unwrap();
        let m = KokoroTokenMap::load(&p).unwrap();
        let (cps, missing) = m.encode("klˈaɪənts");
        assert!(missing.is_empty(), "missing: {missing:?}");
        // 必须是**字符码点**（'k' = 107），不是 tokens.txt 里的 token id
        assert_eq!(cps[0], 'k' as i32);
        assert_eq!(cps[2], 'ˈ' as i32);
        assert_eq!(m.encode("klʒ").1, vec!['ʒ']);
        assert!(
            m.encode_sentences(&["klʒ".to_string()]).is_err(),
            "未登录音素必须报错"
        );
    }

    /// 中英混说必须分段：英文段走 G2P、中文段走 lexicon-zh，谁都不许被整段吞掉。
    #[test]
    fn mixed_sentence_splits_into_language_segments() {
        let segs = split_segments("这个 API 返回 404 error，我们明天再查。");
        let kinds: Vec<bool> = segs.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, vec![true, false, true, false, true]);
        assert_eq!(segs[1].1.trim(), "API", "英文段: {:?}", segs[1].1);
        assert!(segs[3].1.contains("404"), "数字段归英文: {:?}", segs[3].1);
        assert_eq!(split_segments("hello world").len(), 1);
        assert!(!split_segments("hello world")[0].0);
        assert!(split_segments("你好")[0].0);
    }

    #[test]
    fn mixed_detection() {
        assert!(is_mixed("这个 API 返回 404 error。"));
        assert!(is_mixed("Sarah's laptop 上那个 build 失败了。"));
        assert!(!is_mixed("The client's report is ready."));
        assert!(!is_mixed("我们明天再查这个问题。"));
        assert!(!is_mixed("404 error")); // 纯英文/数字不算混说
    }

    /// 用本机 python 当假 sidecar（CI 上 windows-latest 自带 python）。
    fn python_backend(script: &str) -> Option<SidecarBackend> {
        if !program_exists("python") {
            eprintln!("跳过：本机没有 python");
            return None;
        }
        Some(SidecarBackend::new(
            "python",
            vec!["-c".into(), script.into()],
            "test-sidecar",
        ))
    }

    #[test]
    fn sidecar_returns_one_line_per_sentence() {
        let Some(b) = python_backend(
            "import sys
for line in sys.stdin:
    print(line.strip().upper())
",
        ) else {
            return;
        };
        let out = b.run(&["a b".into(), "c d".into()]).unwrap();
        assert_eq!(out, vec!["A B".to_string(), "C D".to_string()]);
    }

    #[test]
    fn sidecar_line_count_mismatch_is_hard_error() {
        let Some(b) = python_backend(
            "print('only one')
",
        ) else {
            return;
        };
        let e = b.run(&["a".into(), "b".into()]).unwrap_err();
        assert!(format!("{e:#}").contains("行数不匹配"), "{e:#}");
    }

    #[test]
    fn sidecar_nonzero_exit_is_hard_error() {
        let Some(b) = python_backend(
            "import sys
sys.stderr.write('boom')
sys.exit(3)
",
        ) else {
            return;
        };
        let e = b.run(&["a".into()]).unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("退出码"), "{msg}");
        assert!(msg.contains("boom"), "{msg}");
    }

    /// 卡住的 sidecar 必须被超时杀掉并报错，而不是把调用方挂住。
    #[test]
    fn sidecar_timeout_kills_process_and_errors() {
        let Some(b) = python_backend(
            "import time
time.sleep(30)
",
        ) else {
            return;
        };
        let b = b.with_timeout(std::time::Duration::from_millis(300));
        let t0 = std::time::Instant::now();
        let e = b.run(&["a".into()]).unwrap_err();
        let elapsed = t0.elapsed();
        assert!(format!("{e:#}").contains("超时"), "{e:#}");
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "超时后应立刻返回，实际 {elapsed:?}"
        );
    }

    /// 程序不存在：启动时（而不是每个请求）就失败。
    #[test]
    fn sidecar_missing_program_is_detected_up_front() {
        let b = SidecarBackend::new("definitely-not-a-real-g2p-xyz", vec![], "test");
        let e = b.check_program().unwrap_err();
        assert!(format!("{e:#}").contains("找不到"), "{e:#}");
    }

    /// 连续失败 3 次后熔断：后续请求快速失败，不再每次白等超时。
    #[test]
    fn sidecar_opens_circuit_after_repeated_failures() {
        let Some(b) = python_backend(
            "import sys
sys.exit(1)
",
        ) else {
            return;
        };
        for _ in 0..3 {
            let _ = b.run(&["a".into()]);
        }
        let t0 = std::time::Instant::now();
        let e = b.run(&["a".into()]).unwrap_err();
        assert!(format!("{e:#}").contains("熔断"), "{e:#}");
        assert!(t0.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn cjk_is_detected_for_lexicon_path() {
        assert!(contains_cjk("你好 world"));
        assert!(!contains_cjk("hello world"));
    }
}

// ================================================================ 接 misaki 的两条路
//
// 1) 原生 crate（需要联网拉依赖，本机离线时无法 `cargo add`）
//    ```toml
//    # crates/voice-core/Cargo.toml
//    misaki = { version = "*", optional = true }
//    [features]
//    misaki = ["dep:misaki"]
//    ```
//    ```rust
//    #[cfg(feature = "misaki")]
//    pub struct MisakiBackend { inner: misaki::G2p }
//    #[cfg(feature = "misaki")]
//    impl G2pBackend for MisakiBackend {
//        fn name(&self) -> &'static str { "misaki" }
//        fn phonemize(&self, text: &str) -> Phonemized {
//            // misaki 直接输出 Kokoro 音素（已是 tokens.txt 的符号集），
//            // 只需按句切分并填 Phonemized{sentence_ipa, words, oov}
//            todo!("接口按 misaki crate 实际 API 填；其余链路已就绪")
//        }
//    }
//    ```
//    选择该后端只需把 `TtsEngine::load` 里的 `Box::new(CmuBackend::…)` 换成
//    `Box::new(MisakiBackend::…)`；`KokoroTokenMap` 与 `phoneme_codepoints`
//    链路完全不用动。
//
// 2) sidecar（今天就能用，无需 Rust 依赖）
//    环境变量 `VOICE_G2P_CMD='python -m misaki --ipa -'`（每行一句 → 每行一个
//    IPA 串），`TtsEngine::load` 会优先采用它；失败直接报错，不退回闭集。
