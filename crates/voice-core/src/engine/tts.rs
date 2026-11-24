//! TTS 引擎：Kokoro-82M 多语言（24 kHz，54 音色）。
//!
//! 工程规则（ADR-0002 决策三）：**一段文本只允许一种语言**，
//! 中文讲解与英文示范必须拆成两次调用。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};

use crate::config::Config;

/// Kokoro v1.0 的 54 个音色（顺序与 voices.bin 一致，按 hexgrad 官方 voices 目录字典序）。
pub const KOKORO_VOICES: [&str; 54] = [
    "af_alloy",
    "af_aoede",
    "af_bella",
    "af_heart",
    "af_jessica",
    "af_kore",
    "af_nicole",
    "af_nova",
    "af_river",
    "af_sarah",
    "af_sky",
    "am_adam",
    "am_echo",
    "am_eric",
    "am_fenrir",
    "am_liam",
    "am_michael",
    "am_onyx",
    "am_puck",
    "am_santa",
    "bf_alice",
    "bf_emma",
    "bf_isabella",
    "bf_lily",
    "bm_daniel",
    "bm_fable",
    "bm_george",
    "bm_lewis",
    "ef_dora",
    "em_alex",
    "em_santa",
    "ff_siwis",
    "hf_alpha",
    "hf_beta",
    "hm_omega",
    "hm_psi",
    "if_sara",
    "im_nicola",
    "jf_alpha",
    "jf_gongitsune",
    "jf_nezumi",
    "jf_tebukuro",
    "jm_kumo",
    "pf_dora",
    "pm_alex",
    "pm_santa",
    "zf_xiaobei",
    "zf_xiaoni",
    "zf_xiaoxiao",
    "zf_xiaoyi",
    "zm_yunjian",
    "zm_yunxi",
    "zm_yunxia",
    "zm_yunyang",
];

/// 雅思听力/口语练习的目标语速区间（wpm）。
///
/// 来源：Cambridge IELTS 听力 Section 1–4 与口语示范约 130–160 wpm；
/// 教学默认取中位数 140，避免 `speed=1.0`（实测 220+ wpm）或固定 0.75（160–200 wpm）。
pub const IELTS_WPM_MIN: f32 = 130.0;
pub const IELTS_WPM_MAX: f32 = 160.0;
pub const IELTS_WPM_DEFAULT: f32 = 140.0;
/// 慢速示范目标（约 NCE 原声的 0.9 倍，仍高于“逐词顿开”）
pub const TTS_SLOW_WPM: f32 = 110.0;

/// 一次合成请求的目标：显式倍率（高级用法）或目标 wpm（默认）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpeechTarget {
    Speed(f32),
    Wpm(f32),
}

/// 决定一次示范要生成哪几个语速版本。
///
/// - 不传 `speed`：`norm` 用目标 wpm（默认 140），`slow` 用 110 wpm；
/// - 传 `speed`：尊重显式倍率；`slow` 取 `speed × 0.75`（兼容旧调用）。
pub fn demo_targets(
    slow_too: bool,
    speed: Option<f32>,
    default_wpm: f32,
) -> Vec<(&'static str, SpeechTarget)> {
    let default_wpm = default_wpm.clamp(80.0, 220.0);
    match (slow_too, speed) {
        (true, Some(sp)) => vec![
            ("slow", SpeechTarget::Speed((sp * 0.75).max(0.45))),
            ("norm", SpeechTarget::Speed(sp)),
        ],
        (true, None) => vec![
            ("slow", SpeechTarget::Wpm((default_wpm - 30.0).max(90.0))),
            ("norm", SpeechTarget::Wpm(default_wpm)),
        ],
        (false, Some(sp)) => vec![("norm", SpeechTarget::Speed(sp))],
        (false, None) => vec![("norm", SpeechTarget::Wpm(default_wpm))],
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Synthesized {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub duration: f32,
    /// 音色名（未知时为 "sid=N"）
    pub voice: String,
    pub sid: i32,
    pub speed: f32,
    /// 文本词数（ASCII 词；中文为 0）
    pub words: usize,
    /// 实际语速（词/分钟）
    pub wpm: f32,
    /// 本次请求的目标 wpm（显式 speed 时为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_wpm: Option<f32>,
    pub gen_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceInfo {
    pub sid: i32,
    pub name: String,
    /// en-us / en-gb / zh / ja / ...
    pub lang: String,
    pub gender: String,
}

pub struct TtsEngine {
    tts: OfflineTts,
    sample_rate: u32,
    num_speakers: i32,
    /// 已加载 lexicon 的全部词条（小写）；用于合成前 OOV 预检
    lexicon_words: HashSet<String>,
    /// 项目级 overlay 路径（错误提示用）
    lexicon_path: PathBuf,
}

impl TtsEngine {
    pub fn load(cfg: &Config) -> Result<Self> {
        let model = cfg.kokoro_model();
        let voices = cfg.kokoro_voices();
        let tokens = cfg.kokoro_tokens();
        if !model.is_file() {
            bail!("缺少 Kokoro 模型: {}", model.display());
        }
        if !voices.is_file() || !tokens.is_file() {
            bail!("缺少 Kokoro 音色/词表文件: {}", voices.display());
        }

        // 项目 overlay 放在最前；C++ 侧按顺序加载，先命中先赢。
        let overlay = cfg
            .tts_lexicon
            .clone()
            .unwrap_or_else(|| cfg.data_dir.join("tts-lexicon.txt"));
        // 顺序很关键：sherpa-onnx 对重复词条是“后加载覆盖先加载”，
        // 因此自定义 overlay 必须放在最后（export 等内置词才能被覆盖）。
        let mut paths: Vec<PathBuf> = Vec::new();
        for name in [
            "lexicon-us-en.txt",
            "lexicon-gb-en.txt",
            "lexicon-zh.txt",
            "lexicon-ja.txt",
            "lexicon-ko.txt",
        ] {
            let p = cfg.model(format!("kokoro-multi-lang/{name}"));
            if p.is_file() {
                paths.push(p);
            }
        }
        if overlay.is_file() {
            paths.push(overlay.clone());
        }
        let lex: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let lexicon_words: HashSet<String> =
            paths.iter().flat_map(|p| parse_lexicon_words(p)).collect();

        let config = OfflineTtsConfig {
            model: OfflineTtsModelConfig {
                kokoro: OfflineTtsKokoroModelConfig {
                    model: Some(model.to_string_lossy().to_string()),
                    voices: Some(voices.to_string_lossy().to_string()),
                    tokens: Some(tokens.to_string_lossy().to_string()),
                    lexicon: if lex.is_empty() {
                        None
                    } else {
                        Some(lex.join(","))
                    },
                    ..Default::default()
                },
                num_threads: cfg.num_threads,
                provider: Some(cfg.provider.clone()),
                ..Default::default()
            },
            max_num_sentences: 1,
            ..Default::default()
        };

        let tts = OfflineTts::create(&config).ok_or_else(|| anyhow!("TTS 创建失败"))?;
        let sample_rate = tts.sample_rate() as u32;
        let num_speakers = tts.num_speakers();
        Ok(Self {
            tts,
            sample_rate,
            num_speakers,
            lexicon_words,
            lexicon_path: overlay,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn num_speakers(&self) -> i32 {
        self.num_speakers
    }

    pub fn voices(&self) -> Vec<VoiceInfo> {
        let n = self.num_speakers.max(0) as usize;
        (0..n)
            .map(|sid| {
                let name = KOKORO_VOICES
                    .get(sid)
                    .copied()
                    .unwrap_or("unknown")
                    .to_string();
                VoiceInfo {
                    sid: sid as i32,
                    lang: voice_lang(&name),
                    gender: voice_gender(&name),
                    name,
                }
            })
            .collect()
    }

    /// 音色名或 sid 字符串 → sid
    pub fn resolve_sid(&self, voice: &str) -> i32 {
        if let Ok(n) = voice.trim().parse::<i32>() {
            return n.clamp(0, self.num_speakers.max(1) - 1);
        }
        KOKORO_VOICES
            .iter()
            .position(|v| v.eq_ignore_ascii_case(voice.trim()))
            .map(|i| i as i32)
            .unwrap_or(0)
    }

    pub fn synthesize(&self, text: &str, sid: i32, speed: f32) -> Result<Synthesized> {
        if text.trim().is_empty() {
            bail!("待合成文本为空");
        }
        let oov = self.oov_words(text);
        if !oov.is_empty() {
            bail!(
                "TTS 词表缺少 {}；请在 {} 增加词条（如 `iphone ˈ I f ˌ O n`）或改写文本（如 iPhone→eye phone）",
                oov.join(", "),
                self.lexicon_path.display()
            );
        }
        let gcfg = GenerationConfig {
            sid,
            speed,
            ..Default::default()
        };
        let t0 = std::time::Instant::now();
        let audio = self
            .tts
            .generate_with_config::<fn(&[f32], f32) -> bool>(text, &gcfg, None)
            .ok_or_else(|| anyhow!("合成失败（文本语言与 lexicon 不匹配？）"))?;
        let gen_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let samples = audio.samples().to_vec();
        let duration = samples.len() as f32 / self.sample_rate as f32;
        let words = count_words(text);
        let wpm = if duration > 0.0 {
            words as f32 / duration * 60.0
        } else {
            0.0
        };
        Ok(Synthesized {
            samples,
            sample_rate: self.sample_rate,
            duration,
            voice: KOKORO_VOICES
                .get(sid as usize)
                .copied()
                .unwrap_or("unknown")
                .to_string(),
            sid,
            speed,
            words,
            wpm,
            target_wpm: None,
            gen_ms,
        })
    }

    /// 按目标 wpm 合成：先 1.0 测速，再算倍率；必要时二次校正。
    ///
    /// 短文本（<3 词）与中文不做 WPM 校准（WPM 不可靠），用接近自然的 0.8/1.0。
    pub fn synthesize_at_wpm(&self, text: &str, sid: i32, target_wpm: f32) -> Result<Synthesized> {
        let target = target_wpm.clamp(80.0, 220.0);
        let words = count_words(text);
        if !text.is_ascii() {
            // 中文/混合：WPM 不适用；用旧倍率保证 slow/norm 仍有差异
            let sp = if target < 120.0 { 0.75 } else { 1.0 };
            // 短文本/中文的 wpm 不可靠：不写 target，避免误读
            return self.synthesize(text, sid, sp);
        }
        if words < 3 {
            // 单词/两词：WPM 不可靠，按目标/220 估一个倍率（140→0.64，110→0.50）
            let sp = (target / 220.0).clamp(0.45, 1.0);
            return self.synthesize(text, sid, sp);
        }
        let probe = self.synthesize(text, sid, 1.0)?;
        let measured = probe.wpm.max(1.0);
        let mut speed = (target / measured).clamp(0.4, 1.2);
        let mut out = self.synthesize(text, sid, speed)?;
        // 最多再校正 2 次：Kokoro 的 speed→wpm 不是严格线性，长句尤其明显
        for _ in 0..2 {
            if (out.wpm - target).abs() <= target * 0.03 {
                break;
            }
            speed = (speed * target / out.wpm.max(1.0)).clamp(0.4, 1.2);
            out = self.synthesize(text, sid, speed)?;
        }
        out.target_wpm = Some(target);
        Ok(out)
    }

    /// 合成前 OOV 预检：返回不在 lexicon 里的 ASCII 词（小写、去重）。
    ///
    /// 支持 contraction/连字符：`that's` → `that` + `'s`、`re-export` → `re` + `export`。
    pub fn oov_words(&self, text: &str) -> Vec<String> {
        oov_in_text(&self.lexicon_words, text)
    }
}

/// 读 lexicon 的词条（只取每行第一个字段，去掉 `(2)` 变体后缀）。
fn parse_lexicon_words(path: &Path) -> HashSet<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashSet::new();
    };
    text.lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(|w| {
            w.trim_start_matches('\'')
                .split('(')
                .next()
                .unwrap_or(w)
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn oov_in_text(known: &HashSet<String>, text: &str) -> Vec<String> {
    let text = text.replace('’', "'");
    if !text.is_ascii() {
        return Vec::new(); // 中文/混说交给 lexicon-zh；WPM/OOV 检查只针对英文
    }
    let mut out: Vec<String> = Vec::new();
    for raw in text.split_whitespace() {
        let cleaned: String = raw
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '\'' || *c == '-')
            .collect();
        let w = cleaned
            .trim_matches(|c| c == '\'' || c == '-')
            .to_lowercase();
        if w.is_empty() || w.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if !known_word(known, &w) && !out.iter().any(|x| x == &w) {
            out.push(w);
        }
    }
    out
}

fn known_word(known: &HashSet<String>, w: &str) -> bool {
    if known.contains(w) {
        return true;
    }
    if let Some((base, suffix)) = w.rsplit_once('\'')
        && matches!(suffix, "s" | "t" | "re" | "ve" | "ll" | "d" | "m")
        && known.contains(base)
    {
        return true;
    }
    if w.contains('-') {
        return w.split('-').all(|p| !p.is_empty() && known.contains(p));
    }
    false
}

/// ASCII 词数（标点不算；缩写算 1 个词）。中文返回 0，调用方应跳过 WPM 校准。
pub fn count_words(text: &str) -> usize {
    text.split_whitespace()
        .filter(|w| w.chars().any(|c| c.is_ascii_alphanumeric()))
        .count()
}

fn voice_lang(name: &str) -> String {
    match name.get(0..2) {
        Some("af") | Some("am") => "en-us".into(),
        Some("bf") | Some("bm") => "en-gb".into(),
        Some("ef") | Some("em") => "es".into(),
        Some("ff") => "fr".into(),
        Some("hf") | Some("hm") => "hi".into(),
        Some("if") | Some("im") => "it".into(),
        Some("jf") | Some("jm") => "ja".into(),
        Some("pf") | Some("pm") => "pt-br".into(),
        Some("zf") | Some("zm") => "zh".into(),
        _ => "unknown".into(),
    }
}

fn voice_gender(name: &str) -> String {
    match name.as_bytes().get(1) {
        Some(b'f') => "female".into(),
        Some(b'm') => "male".into(),
        _ => "unknown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_table_is_complete() {
        assert_eq!(KOKORO_VOICES.len(), 54);
        let mut sorted = KOKORO_VOICES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 54, "音色名有重复");
    }

    #[test]
    fn demo_targets_default_to_wpm() {
        assert_eq!(
            demo_targets(false, None, 140.0),
            vec![("norm", SpeechTarget::Wpm(140.0))]
        );
        assert_eq!(
            demo_targets(true, None, 140.0),
            vec![
                ("slow", SpeechTarget::Wpm(110.0)),
                ("norm", SpeechTarget::Wpm(140.0))
            ]
        );
    }

    #[test]
    fn demo_targets_honor_explicit_speed() {
        assert_eq!(
            demo_targets(false, Some(0.9), 140.0),
            vec![("norm", SpeechTarget::Speed(0.9))]
        );
        let v = demo_targets(true, Some(0.9), 140.0);
        assert!(matches!(v[0], ("slow", SpeechTarget::Speed(s)) if (s - 0.675).abs() < 1e-6));
        assert!(matches!(v[1], ("norm", SpeechTarget::Speed(s)) if (s - 0.9).abs() < 1e-6));
    }

    #[test]
    fn count_words_ignores_punctuation_and_empty() {
        assert_eq!(count_words("On Monday, we found a bug!"), 6);
        assert_eq!(count_words("don't stop"), 2);
        assert_eq!(count_words("   "), 0);
        assert_eq!(count_words("你好 world"), 1, "中文不计入 ASCII 词");
    }

    #[test]
    fn oov_detects_missing_words_and_allows_contractions() {
        let known: HashSet<String> = ["i", "know", "that", "s", "export", "tool", "don't"]
            .into_iter()
            .map(str::to_string)
            .collect();
        assert!(oov_in_text(&known, "I know that's export tool").is_empty());
        assert!(oov_in_text(&known, "I don't know").is_empty());
        assert_eq!(oov_in_text(&known, "I know iPhone"), vec!["iphone"]);
        assert_eq!(
            oov_in_text(&known, "zzzqx, iPhone!"),
            vec!["zzzqx", "iphone"]
        );
    }

    #[test]
    fn wpm_math_is_reasonable() {
        // 140 wpm ⇔ 10 词约 4.29 s
        let words = 10.0f32;
        let dur = 4.286f32;
        assert!((words / dur * 60.0 - 140.0).abs() < 0.5);
    }

    #[test]
    fn parses_lang_and_gender() {
        assert_eq!(voice_lang("af_heart"), "en-us");
        assert_eq!(voice_lang("bf_emma"), "en-gb");
        assert_eq!(voice_lang("zf_xiaobei"), "zh");
        assert_eq!(voice_gender("bf_emma"), "female");
        assert_eq!(voice_gender("am_michael"), "male");
    }
}
