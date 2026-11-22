//! TTS 引擎：Kokoro-82M 多语言（24 kHz，54 音色）。
//!
//! 工程规则（ADR-0002 决策三）：**一段文本只允许一种语言**，
//! 中文讲解与英文示范必须拆成两次调用。

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

/// 跟读示范的慢速倍率（跟 skill 约定的固定值）
pub const SLOW_SPEED: f32 = 0.75;

/// 决定一次示范要生成哪几个语速版本。
///
/// 语义（曾经踩过的坑）：`slow_too=true` 表示"慢速 + 常速"一对，此时 `speed`
/// 若等于慢速值（0.75）就**不能**被当成常速再次使用，否则两个文件语速相同、
/// 常速示范实际缺席。因此 slow 分支里常速取 `speed` 仅在它明显快于慢速时生效，
/// 否则回落到配置默认（1.0）。
pub fn demo_speeds(
    slow_too: bool,
    speed: Option<f32>,
    default_speed: f32,
) -> Vec<(&'static str, f32)> {
    let norm = speed
        .filter(|s| *s > SLOW_SPEED + 0.01)
        .unwrap_or(default_speed);
    if slow_too {
        vec![("slow", SLOW_SPEED), ("norm", norm)]
    } else {
        vec![("norm", speed.unwrap_or(default_speed))]
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

        // 收集存在的 lexicon 文件（多种语言可共存，逗号分隔）
        let mut lex: Vec<String> = Vec::new();
        for name in [
            "lexicon-us-en.txt",
            "lexicon-gb-en.txt",
            "lexicon-zh.txt",
            "lexicon-ja.txt",
            "lexicon-ko.txt",
        ] {
            let p = cfg.model(format!("kokoro-multi-lang/{name}"));
            if p.is_file() {
                lex.push(p.to_string_lossy().to_string());
            }
        }

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
            gen_ms,
        })
    }
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
    fn demo_speeds_slow_pair_is_not_duplicated() {
        // 传 0.75 + slow_too：常速必须是 1.0，不能也是 0.75
        let v = demo_speeds(true, Some(0.75), 1.0);
        assert_eq!(v, vec![("slow", 0.75), ("norm", 1.0)]);
        // 不传 speed：常速用配置默认
        assert_eq!(
            demo_speeds(true, None, 1.0),
            vec![("slow", 0.75), ("norm", 1.0)]
        );
        // 显式给一个更快语速：尊重它
        assert_eq!(
            demo_speeds(true, Some(1.2), 1.0),
            vec![("slow", 0.75), ("norm", 1.2)]
        );
        // 不要慢速版时 speed 原样生效
        assert_eq!(demo_speeds(false, Some(0.9), 1.0), vec![("norm", 0.9)]);
        assert_eq!(demo_speeds(false, None, 1.0), vec![("norm", 1.0)]);
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
