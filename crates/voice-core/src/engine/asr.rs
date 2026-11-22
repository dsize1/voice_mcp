//! ASR 引擎：SenseVoice（主力，中英+时间戳）与 whisper-turbo（中英混说/标点增强）。
//!
//! 路由规则见 ADR-0002（含 P0 更正：SenseVoice 也有 token 时间戳，因此英文默认走它）。

use std::sync::OnceLock;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig,
    OfflineWhisperModelConfig,
};

use crate::config::Config;

/// 词级对齐结果
#[derive(Debug, Clone, Serialize)]
pub struct Word {
    pub w: String,
    pub t0: f32,
    pub t1: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    pub text: String,
    /// 实际使用的语言标签：zh / en / mixed
    pub lang: String,
    pub engine: String,
    pub duration: f32,
    /// 词级对齐（可能为空 → degraded）
    pub words: Vec<Word>,
    /// token 级原始时间戳（不经合并，保留全部信息供上层使用）
    pub tokens: Vec<Word>,
    /// 本次结果是否降级（无词级时间戳 / 引擎回退）
    pub degraded: bool,
    /// 推理耗时（毫秒）
    pub infer_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AsrKind {
    SenseVoice,
    Whisper,
}

impl AsrKind {
    pub fn name(&self) -> &'static str {
        match self {
            AsrKind::SenseVoice => "sense-voice",
            AsrKind::Whisper => "whisper-turbo",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "sense-voice" | "sensevoice" | "sv" => Some(AsrKind::SenseVoice),
            "whisper" | "whisper-turbo" => Some(AsrKind::Whisper),
            _ => None,
        }
    }
}

pub struct AsrEngine {
    kind: AsrKind,
    rec: OfflineRecognizer,
}

impl AsrEngine {
    pub fn load(cfg: &Config, kind: AsrKind) -> Result<Self> {
        let mut config = OfflineRecognizerConfig::default();
        match kind {
            AsrKind::SenseVoice => {
                let model = cfg.sense_voice_model();
                let tokens = cfg.sense_voice_tokens();
                if !model.is_file() {
                    bail!("缺少 SenseVoice 模型: {}", model.display());
                }
                config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                    model: Some(path_str(&model)),
                    language: Some("auto".into()),
                    use_itn: true,
                };
                config.model_config.tokens = Some(path_str(&tokens));
                config.model_config.model_type = Some("sense_voice".into());
            }
            AsrKind::Whisper => {
                let enc = cfg.whisper_encoder();
                let dec = cfg.whisper_decoder();
                if !enc.is_file() || !dec.is_file() {
                    bail!("缺少 whisper 模型: {}", enc.display());
                }
                config.model_config.whisper = OfflineWhisperModelConfig {
                    encoder: Some(path_str(&enc)),
                    decoder: Some(path_str(&dec)),
                    language: Some("en".into()),
                    task: Some("transcribe".into()),
                    tail_paddings: -1,
                    enable_token_timestamps: false, // 该权重未导出 cross-attention
                    enable_segment_timestamps: true,
                };
                config.model_config.tokens = Some(path_str(&cfg.whisper_tokens()));
                config.model_config.model_type = Some("whisper".into());
            }
        }
        config.model_config.num_threads = cfg.num_threads;
        config.model_config.provider = Some(cfg.provider.clone());

        let rec = OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("ASR 引擎创建失败: {}", kind.name()))?;
        Ok(Self { kind, rec })
    }

    pub fn kind(&self) -> AsrKind {
        self.kind
    }

    /// 对 16 kHz mono 采样做识别。
    pub fn transcribe(&self, samples: &[f32], sr: u32, lang: &str) -> Result<Transcript> {
        let t0 = std::time::Instant::now();
        let stream = self.rec.create_stream();
        stream.accept_waveform(sr as i32, samples);
        self.rec.decode(&stream);
        let r = stream.get_result().ok_or_else(|| anyhow!("ASR 无结果"))?;
        let infer_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let ts = r.timestamps.clone().unwrap_or_default();
        let tokens: Vec<Word> = r
            .tokens
            .iter()
            .enumerate()
            .map(|(i, tok)| {
                let start = ts.get(i).copied().unwrap_or(f32::NAN);
                let end = ts.get(i + 1).copied().unwrap_or(start + 0.2);
                Word {
                    w: tok.clone(),
                    t0: start,
                    t1: end,
                }
            })
            .collect();

        let degraded = ts.is_empty();
        let words = if degraded {
            Vec::new()
        } else {
            words_from_tokens(&r.text, &tokens)
        };

        let duration = samples.len() as f32 / sr as f32;
        Ok(Transcript {
            text: r.text.trim().to_string(),
            lang: lang.to_string(),
            engine: self.kind.name().to_string(),
            duration,
            words,
            tokens,
            degraded,
            infer_ms,
        })
    }
}

/// 双引擎 + 路由
pub struct Asr {
    cfg: Config,
    sense: AsrEngine,
    whisper: OnceLock<AsrEngine>,
    whisper_err: OnceLock<String>,
}

impl Asr {
    pub fn load(cfg: &Config) -> Result<Self> {
        let sense = AsrEngine::load(cfg, AsrKind::SenseVoice)?;
        Ok(Self {
            cfg: cfg.clone(),
            sense,
            whisper: OnceLock::new(),
            whisper_err: OnceLock::new(),
        })
    }

    fn whisper(&self) -> Result<&AsrEngine> {
        if let Some(e) = self.whisper.get() {
            return Ok(e);
        }
        if let Some(err) = self.whisper_err.get() {
            bail!("{err}");
        }
        match AsrEngine::load(&self.cfg, AsrKind::Whisper) {
            Ok(e) => Ok(self.whisper.get_or_init(|| e)),
            Err(e) => {
                let msg = format!("whisper 引擎不可用: {e}");
                let _ = self.whisper_err.set(msg.clone());
                bail!(msg)
            }
        }
    }

    /// `lang`: auto | en | zh | mixed。`engine`: 可选强制引擎名。
    pub fn transcribe(
        &self,
        samples: &[f32],
        sr: u32,
        lang: &str,
        engine: Option<&str>,
    ) -> Result<Transcript> {
        let forced = engine.and_then(AsrKind::parse);
        let lang_lc = lang.trim().to_ascii_lowercase();

        let primary = match forced {
            Some(k) => k,
            None => match lang_lc.as_str() {
                "zh" => AsrKind::SenseVoice,
                "mixed" => AsrKind::Whisper,
                "en" => AsrKind::parse(&self.cfg.en_engine).unwrap_or(AsrKind::SenseVoice),
                _ => AsrKind::SenseVoice, // auto
            },
        };

        let attempt = |k: AsrKind| -> Result<Transcript> {
            let e = match k {
                AsrKind::SenseVoice => &self.sense,
                AsrKind::Whisper => self.whisper()?,
            };
            e.transcribe(
                samples,
                sr,
                if lang_lc.is_empty() { "auto" } else { &lang_lc },
            )
        };

        let mut t = match attempt(primary) {
            Ok(t) => t,
            Err(e) => {
                // 降级链：whisper ↔ sense-voice
                let alt = match primary {
                    AsrKind::Whisper => AsrKind::SenseVoice,
                    AsrKind::SenseVoice => AsrKind::Whisper,
                };
                let mut t =
                    attempt(alt).map_err(|e2| anyhow!("两个 ASR 引擎都失败: {e} / {e2}"))?;
                t.degraded = true;
                t
            }
        };

        // auto 时按文本内容判定语言
        if matches!(lang_lc.as_str(), "" | "auto") {
            t.lang = detect_lang(&t.text);
        }
        Ok(t)
    }
}

pub fn detect_lang(text: &str) -> String {
    let has_cjk = text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    let has_latin = text.chars().any(|c| c.is_ascii_alphabetic());
    match (has_cjk, has_latin) {
        (true, true) => "mixed".into(),
        (true, false) => "zh".into(),
        _ => "en".into(),
    }
}

fn path_str(p: &std::path::Path) -> String {
    p.to_string_lossy().to_string()
}

fn norm_token(tok: &str) -> String {
    tok.chars()
        .filter(|c| !matches!(c, '\u{2581}' | '\u{0120}')) // ▁ Ġ
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

fn norm_word(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .collect::<String>()
        .to_lowercase()
}

/// 取前 n 个**字符**的安全前缀（不会切在 UTF-8 边界中间）。
fn char_prefix(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// 把 token 序列映射到文本里的词边界，得到词级时间戳。
///
/// 策略：先按空白切词（含 CJK 的文本按字符切），再用贪心方式把 token 逐个拼进当前词，
/// 直到拼接结果覆盖该词；时间取首 token 起点到下一个词首 token 起点。
fn words_from_tokens(text: &str, tokens: &[Word]) -> Vec<Word> {
    if tokens.is_empty() || text.is_empty() {
        return Vec::new();
    }
    let has_cjk = text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    let raw_words: Vec<String> = if has_cjk {
        // 中文按字切；中英混说时把连续的 ASCII 字母数字聚成一个词（不能拆成单字母）。
        // **标点必须当分隔符丢掉**：否则 `，` `。` 会被当成“词”，把 word_count 与 cpm 抬高。
        let mut v: Vec<String> = Vec::new();
        let mut cur = String::new();
        for c in text.chars().filter(|c| !c.is_whitespace()) {
            let is_word_char = c.is_ascii_alphanumeric()
                || c == '\''
                || ('\u{4e00}'..='\u{9fff}').contains(&c)
                || ('\u{3400}'..='\u{4dbf}').contains(&c)
                || ('\u{3040}'..='\u{30ff}').contains(&c);
            if c.is_ascii_alphanumeric() || c == '\'' {
                cur.push(c);
                continue;
            }
            if !cur.is_empty() {
                v.push(std::mem::take(&mut cur));
            }
            if is_word_char {
                v.push(c.to_string());
            }
        }
        if !cur.is_empty() {
            v.push(cur);
        }
        v
    } else {
        text.split_whitespace().map(|s| s.to_string()).collect()
    };

    let mut out = Vec::with_capacity(raw_words.len());
    let mut ti = 0usize;
    for (wi, w) in raw_words.iter().enumerate() {
        let target = norm_word(w);
        if ti >= tokens.len() {
            break;
        }
        let start_idx = ti;
        let mut acc = String::new();
        while ti < tokens.len() {
            acc.push_str(&norm_token(&tokens[ti].w));
            ti += 1;
            // 用字符前缀比较，不能在 UTF-8 中间截断（中文/日文/emoji 会直接 panic）
            let head_chars = target.chars().count();
            let head = char_prefix(&acc, head_chars);
            if !target.starts_with(head) {
                break;
            }
            if head.chars().count() >= head_chars {
                break;
            }
        }
        if acc.is_empty() {
            continue;
        }
        let t0 = tokens[start_idx].t0;
        let t1 = tokens.get(ti).map(|t| t.t0).unwrap_or_else(|| {
            // 最后一个词：用 token 终点 + 经验值
            let last = &tokens[ti - 1];
            last.t1.max(last.t0 + 0.1)
        });
        let _ = wi;
        out.push(Word {
            w: w.clone(),
            t0,
            t1: t1.max(t0),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(w: &str, t: f32) -> Word {
        Word {
            w: w.into(),
            t0: t,
            t1: t + 0.2,
        }
    }

    #[test]
    fn maps_words_when_tokens_are_words() {
        let toks = vec![
            tok("I", 0.0),
            tok("think", 0.3),
            tok("this", 0.8),
            tok("is", 1.0),
            tok("the", 1.2),
            tok("third", 1.4),
            tok("time", 1.9),
        ];
        let ws = words_from_tokens("I think this is the third time", &toks);
        assert_eq!(ws.len(), 7);
        assert_eq!(ws[5].w, "third");
        assert!((ws[5].t0 - 1.4).abs() < 1e-6);
        assert!((ws[6].t1 - 2.1).abs() < 1e-6);
    }

    #[test]
    fn handles_cjk_without_panicking() {
        // 中文是 3 字节字符，按字节截断会 panic（真机验收时踩到的）
        let toks = vec![
            tok("我", 0.0),
            tok("想", 0.2),
            tok("这", 0.4),
            tok("是", 0.6),
            tok("第", 0.8),
            tok("三", 1.0),
            tok("次", 1.2),
        ];
        let ws = words_from_tokens("我想这是第三次", &toks);
        assert_eq!(ws.len(), 7);
        assert_eq!(ws[0].w, "我");
        assert!((ws[0].t0 - 0.0).abs() < 1e-6);
        assert!((ws[6].t0 - 1.2).abs() < 1e-6);
    }

    #[test]
    fn handles_mixed_script() {
        let toks = vec![
            tok("我", 0.0),
            tok("\u{2581}think", 0.3),
            tok("这", 0.7),
            tok("是", 0.9),
        ];
        let ws = words_from_tokens("我 think 这是", &toks);
        assert_eq!(ws.len(), 4);
        assert_eq!(ws[1].w, "think");
    }

    #[test]
    fn cjk_punctuation_is_not_a_word() {
        let toks = vec![
            tok("我", 0.0),
            tok("想", 0.2),
            tok("，", 0.4),
            tok("这", 0.6),
        ];
        let ws = words_from_tokens("我想，这", &toks);
        assert_eq!(ws.len(), 3, "标点不能算作一个词");
        assert_eq!(
            ws.iter().map(|w| w.w.as_str()).collect::<Vec<_>>(),
            vec!["我", "想", "这"]
        );
    }

    #[test]
    fn merges_subword_tokens() {
        let toks = vec![
            tok("\u{2581}pro", 0.0),
            tok("nun", 0.2),
            tok("ci", 0.4),
            tok("ation", 0.6),
            tok("\u{2581}test", 0.9),
        ];
        let ws = words_from_tokens("pronunciation test", &toks);
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].w, "pronunciation");
        assert!((ws[0].t1 - 0.9).abs() < 1e-6);
    }

    #[test]
    fn detects_language() {
        assert_eq!(detect_lang("hello world"), "en");
        assert_eq!(detect_lang("你好"), "zh");
        assert_eq!(detect_lang("I mean 就是那样"), "mixed");
    }
}
