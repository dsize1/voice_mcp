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
use crate::engine::g2p_ipa::{
    self, CmuBackend, G2pBackend, KokoroTokenMap, LtsPolicy, SidecarBackend,
};

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

/// 默认示范音色（**男声**，2026-11-25 用户定）。
///
/// 为什么不是 af_heart：af_heart 在 140 wpm 附近会出现“起始多一小段音”的伪影，
/// 双 ASR 都会听成开头多一个 “And”（audit §11/§12）；am_michael 在同一批句子上
/// 3/3 verified（10 条 IELTS prompt 里的主力音色）。
/// 这是**默认值**，不是“唯一可用音色”：材料侧仍逐条验证，必要时回退 am_fenrir。
pub const DEFAULT_VOICE: &str = "am_michael";

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
    /// 实际语速（词/分钟）——按**语音跨度**计（去掉首尾静音，见 `speech_span`）
    pub wpm: f32,
    /// 语音跨度（秒）：首个到最后一个有声帧，不含首尾静音
    pub speech_duration: f32,
    /// 起始伪影的停顿长度（ms）。Some(值) 表示“先出一个短音 → 停顿 → 才进正文”，
    /// 实测 af_heart 在某些速度点会把这段读成多一个 “And”。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onset_gap_ms: Option<f32>,
    /// 本次请求的目标 wpm（显式 speed 时为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_wpm: Option<f32>,
    /// 实际送进声学模型的音素串（开放词表 G2P 产出；中文路径为 None）。
    /// 留证与回归断言用：**任何缺词都能在这里看出来**。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phonemes: Option<String>,
    /// 用规则 LTS 猜读音的词（空数组 = 全部词都有可靠依据）。
    /// 材料侧可据此决定是否人工复听（`VOICE_LTS=0` 可强制关闭，退回报错）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lts_words: Vec<String>,
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
    /// 默认文本前端：开放词表 G2P（英文路径**不再查引擎词表**）
    g2p: Box<dyn G2pBackend>,
    /// 音素字符 → Kokoro token id（换模型/换词表时的硬门）
    tokens: KokoroTokenMap,
    /// 是否允许中英混说（默认 false：产品契约“一段文本一种语言”，混说音质必然降级）
    allow_mixed: bool,
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

        // 项目 overlay：既是 C++ 侧的最后一份词表，也是 Rust 侧 G2P 的
        // 高优先级覆盖层（同一份文件、两种用法，避免两处不一致）。
        let overlay = cfg
            .tts_lexicon
            .clone()
            .unwrap_or_else(|| cfg.data_dir.join("tts-lexicon.txt"));
        // 英文不再交给引擎词表（v2.x 的 closed lexicon 会静默丢词），
        // 只保留中文词表给 zh 路径。sherpa-onnx 对重复词条是“后加载覆盖先加载”，
        // 所以 overlay 放最后。
        let mut paths: Vec<PathBuf> = Vec::new();
        let zh = cfg.model("kokoro-multi-lang/lexicon-zh.txt");
        if zh.is_file() {
            paths.push(zh);
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

        // 文本前端：优先外部 sidecar（如 misaki / espeak-ng），否则内置 CMUdict 后端。
        let cmu_path = cfg.g2p_cmudict();
        // 英文 lexicon 作为 G2P 的**高优先级覆盖层**：命中就用 Kokoro 自己的发音
        // （与声学模型训练分布一致），未命中才落到 CMUdict + 形态规则。
        // 顺序即优先级（先加载者优先，后加载者只补空缺）：美音优先，
        // 因为是 `am_michael`／美音示范；英音词表只填空缺。
        // 反了会把 `last` 读成 /lɑːst/（听成 "lost"）、`on` 读成 /ɒn/ —— 与声学模型不匹配。
        let en_lexicons: Vec<PathBuf> = ["lexicon-us-en.txt", "lexicon-gb-en.txt"]
            .iter()
            .map(|n| cfg.model(format!("kokoro-multi-lang/{n}")))
            .filter(|p| p.is_file())
            .collect();
        let g2p: Box<dyn G2pBackend> = match SidecarBackend::from_env() {
            Some(side) => Box::new(side),
            None => Box::new(
                CmuBackend::load(&cmu_path, Some(&overlay), &en_lexicons)?.with_lts(
                    if cfg.allow_lts {
                        LtsPolicy::On
                    } else {
                        LtsPolicy::Off
                    },
                ),
            ),
        };
        let tokens = KokoroTokenMap::load(&tokens)?;
        Ok(Self {
            tts,
            sample_rate,
            num_speakers,
            lexicon_words,
            lexicon_path: overlay,
            g2p,
            tokens,
            allow_mixed: cfg.allow_mixed,
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
        // 中英混说：默认拒答（契约是“一段文本只允许一种语言”）。
        // 单音色下另一语言必然跑调，拼接点还有 0.2–0.5 s 停顿 —— 产品入口不该
        // 输出已知降级的音频。内部实验用 VOICE_ALLOW_MIXED=1 / `say --allow-mixed`。
        if !self.allow_mixed && g2p_ipa::is_mixed(text) {
            bail!(
                "暂不支持中英混说合成（单音色下音质必然降级）。请拆成两次调用：英文一次、中文一次（契约见 ADR-0002）；内部实验可用 voice-cli say --allow-mixed"
            );
        }
        // 分段：CJK 段走引擎中文词表；非 CJK 段走开放词表 G2P → phoneme_codepoints。
        // 中英混说（"这个 API 返回 404 error"）靠这一步才不会出现“整句都走中文词表
        // 于是英文词被静默丢掉”的回退，也不会让中文段被英文音素化。
        let segs = g2p_ipa::split_segments(text);
        let mut samples: Vec<f32> = Vec::new();
        let mut gen_ms = 0.0f64;
        let mut phonemes: Vec<String> = Vec::new();
        let mut lts_words: Vec<String> = Vec::new();
        let mut en_text_seen = false;

        for (is_cjk, seg) in &segs {
            if seg.trim().is_empty() {
                continue;
            }
            if *is_cjk {
                let (part, ms) = self.synth_raw(seg, sid, speed, None)?;
                samples.extend(part);
                gen_ms += ms;
            } else {
                let (cps, ipa, lts) = self.english_codepoints(seg)?;
                for w in lts {
                    if !lts_words.contains(&w) {
                        lts_words.push(w);
                    }
                }
                en_text_seen = true;
                let (part, ms) = self.synth_raw(seg, sid, speed, Some(cps))?;
                samples.extend(part);
                gen_ms += ms;
                phonemes.push(ipa);
            }
        }
        if samples.is_empty() {
            bail!("合成结果为空（文本里没有可发的内容）");
        }

        let duration = samples.len() as f32 / self.sample_rate as f32;
        let words = count_words(text);
        // wpm 的“单位数”：英文按词、中文按字（一字≈一音节）。
        // 混说句里如果只数 ASCII 词，`这个 API 返回 404 error，我们明天再查。`
        // 会被算成 3 词 / 4 秒 → 20 wpm，纯属尺子错。
        let units = spoken_units(text);
        // 语速按语音跨度算：Kokoro 会在首尾留 0.1–0.4 s 近静音，按总时长算会
        // 系统性低估（实测长句目标 140 量到 128，导致语速校正循环来回震荡）。
        let speech_duration = speech_span(&samples, self.sample_rate)
            .map(|(a, b)| b - a)
            .filter(|d| *d >= 0.2)
            .unwrap_or(duration);
        let wpm = if speech_duration > 0.0 {
            units as f32 / speech_duration * 60.0
        } else {
            0.0
        };
        // 只有 ≥3 词的句子才查起始伪影：单词/两词 drill 里 fricative+塞音本身就有一段
        // 低电平（如 `Thank you`），查了全是噪声，而短文本本来也不做 WPM 校准。
        let onset_gap_ms = if words >= 3 {
            onset_gap_ms(&samples, self.sample_rate)
        } else {
            None
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
            speech_duration,
            onset_gap_ms,
            target_wpm: None,
            phonemes: if en_text_seen {
                Some(phonemes.join(g2p_ipa::SEGMENT_JOINER))
            } else {
                None
            },
            lts_words,
            gen_ms,
        })
    }

    /// 单段合成（英文段带 `phoneme_codepoints`，中文段交给引擎词表）。
    fn synth_raw(
        &self,
        text: &str,
        sid: i32,
        speed: f32,
        phoneme_cps: Option<Vec<Vec<i32>>>,
    ) -> Result<(Vec<f32>, f64)> {
        let gcfg = GenerationConfig {
            sid,
            speed,
            phoneme_codepoints: phoneme_cps,
            ..Default::default()
        };
        let t0 = std::time::Instant::now();
        let audio = self
            .tts
            .generate_with_config::<fn(&[f32], f32) -> bool>(text, &gcfg, None)
            .ok_or_else(|| anyhow!("合成失败（文本语言与 lexicon 不匹配？）"))?;
        let gen_ms = t0.elapsed().as_secs_f64() * 1000.0;
        Ok((audio.samples().to_vec(), gen_ms))
    }

    /// 英文段 → 逐句音素码点 + LTS 兜底词（证据）。**不查引擎词表**；覆盖不了直接报错。
    fn english_codepoints(&self, text: &str) -> Result<(Vec<Vec<i32>>, String, Vec<String>)> {
        let ph = self.g2p.phonemize(text);
        if !ph.oov.is_empty() {
            bail!(
                "G2P({}) 覆盖不了 {}；请在 {} 为其指定发音（`word 音素…`）",
                self.g2p.name(),
                ph.oov.join(", "),
                self.lexicon_path.display()
            );
        }
        if ph.is_empty() {
            bail!("G2P({}) 产出空音素，拒绝合成", self.g2p.name());
        }
        let cps = self.tokens.encode_sentences(&ph.sentence_ipa)?;
        let lts: Vec<String> = ph
            .words
            .iter()
            .filter(|w| w.source == g2p_ipa::PhoneSource::Lts)
            .map(|w| w.word.clone())
            .collect();
        Ok((cps, ph.ipa(), lts))
    }

    /// 文本前端预览（留证 / 回归断言用，不合成音频）。
    ///
    /// 只音素化**英文段**：中文段由引擎词表处理，不应出现在这里（也就不会被
    /// 误报成 OOV）。
    pub fn phonemize_preview(&self, text: &str) -> crate::engine::g2p_ipa::Phonemized {
        let mut out = crate::engine::g2p_ipa::Phonemized::default();
        for (is_cjk, seg) in g2p_ipa::split_segments(text) {
            if is_cjk || seg.trim().is_empty() {
                continue;
            }
            let ph = self.g2p.phonemize(&seg);
            out.words.extend(ph.words);
            out.sentence_ipa.extend(ph.sentence_ipa);
            for w in ph.oov {
                if !out.oov.contains(&w) {
                    out.oov.push(w);
                }
            }
        }
        out
    }

    /// 合成前 OOV 预检。英文交给 G2P（**不再用“base+'s 就算已知”的宽容规则**），
    /// 中文仍查中文词表。
    pub fn oov_words(&self, text: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (is_cjk, seg) in g2p_ipa::split_segments(text) {
            if seg.trim().is_empty() {
                continue;
            }
            let got = if is_cjk {
                oov_in_text(&self.lexicon_words, &seg)
            } else {
                self.g2p.phonemize(&seg).oov
            };
            for w in got {
                if !out.contains(&w) {
                    out.push(w);
                }
            }
        }
        out
    }

    /// 文本前端标识（写进审计/留证，避免“前端换了导致 OOV 曲线突变”被误读）
    pub fn frontend_id(&self) -> String {
        format!("g2p-ipa/1.0[{}]+lexicon-zh", self.g2p.name())
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
        // 语速校正：Kokoro 的 speed→wpm 不是连续函数（某些速度点之间时长跳变 0.8 s），
        // 旧实现“只采用最后一次结果”会落回跳变另一侧（实测 af_heart 目标 140 得到 128）。
        // 这里改为**保留最优候选**：先看语速是否落在目标 ±6%，再看有无起始伪影，最后看距离。
        let mut best = probe.clone();
        let mut speed = (target / probe.wpm.max(1.0)).clamp(0.4, 1.2);
        let mut tried: Vec<f32> = vec![1.0];
        for _ in 0..4 {
            if tried.iter().any(|s| (s - speed).abs() < 1e-3) {
                break;
            }
            tried.push(speed);
            let out = self.synthesize(text, sid, speed)?;
            if rank(&out, target) < rank(&best, target) {
                best = out;
            }
            if best.onset_gap_ms.is_none() && (best.wpm - target).abs() <= target * 0.03 {
                break;
            }
            if best.wpm <= 1.0 {
                break;
            }
            speed = (speed * target / best.wpm).clamp(0.4, 1.2);
        }
        // 兜底：**只要还没进目标带宽**，就继续换速度试（策略：换速度/换音色，不裁音频；见 audit §11）。
        // 不能只在“有起始伪影”时重试：`rank` 的第一键已改为带宽距离，
        // 越界且无伪影的候选也必须让位给“更靠近带宽、但带轻微伪影”的候选（D54 残留 wl04）。
        if best.onset_gap_ms.is_some() || rank(&best, target).0 > 0.0 {
            let base = best.speed;
            for k in [1.04f32, 0.96, 1.08, 0.92, 1.12, 0.88, 1.16] {
                if tried.len() >= 9 {
                    break;
                }
                let sp = (base * k).clamp(0.4, 1.2);
                if tried.iter().any(|t| (t - sp).abs() < 1e-3) {
                    continue;
                }
                let out = self.synthesize(text, sid, sp)?;
                tried.push(sp);
                if rank(&out, target) < rank(&best, target) {
                    best = out.clone();
                }
                if (best.wpm - target).abs() <= target * 0.06 && best.onset_gap_ms.is_none() {
                    break;
                }
            }
        }
        let mut out = best;
        out.target_wpm = Some(target);
        Ok(out)
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

/// 语速统计单位：英文按词（含缩写、数字串算一个词），中文按字。
pub fn spoken_units(text: &str) -> usize {
    let mut units = 0usize;
    for tok in text.split_whitespace() {
        if tok.chars().any(crate::engine::g2p_ipa::is_cjk_char) {
            units += tok
                .chars()
                .filter(|c| crate::engine::g2p_ipa::is_cjk_char(*c))
                .count();
        } else if tok.chars().any(|c| c.is_ascii_alphanumeric()) {
            units += 1;
        }
    }
    units
}

/// 10 ms 帧的 RMS 电平（dBFS）。
fn frame_dbs(samples: &[f32], sr: u32) -> (Vec<f32>, f32) {
    let hop = (sr as usize / 100).max(1);
    let dbs: Vec<f32> = samples
        .chunks(hop)
        .map(|c| {
            let ms: f32 = c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32;
            let rms = ms.sqrt();
            if rms <= 1e-9 {
                -120.0
            } else {
                20.0 * rms.log10()
            }
        })
        .collect();
    let hop_sec = hop as f32 / sr.max(1) as f32;
    (dbs, hop_sec)
}

/// 语音跨度（秒）：首个到最后一个“接近峰值”的 10 ms 帧，去掉首尾静音。
///
/// 门限取 `峰值 − 40 dB`：比 VAD 宽松，但足以排除首尾数字静音与低幅起始伪影。
fn speech_span(samples: &[f32], sr: u32) -> Option<(f32, f32)> {
    let (dbs, hop_sec) = frame_dbs(samples, sr);
    if dbs.len() < 3 {
        return None;
    }
    let peak = dbs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !peak.is_finite() || peak < -70.0 {
        return None;
    }
    let thr = peak - 40.0;
    let first = dbs.iter().position(|d| *d >= thr)?;
    let last = dbs.iter().rposition(|d| *d >= thr)?;
    if last <= first {
        return None;
    }
    Some((first as f32 * hop_sec, (last + 1) as f32 * hop_sec))
}

/// 起始伪影：首段短音 → 停顿（≥20 ms）→ 正文。
///
/// 实测（audit tts-2026-11-24 §11）：af_heart 在某些速度点先发 0.10–0.16 s 低幅浊音，
/// 停顿后才是正文，双 ASR 都会多听出一个 “And”。判定条件三条同时成立：
/// 1. 停顿 ≥ 20 ms 且在开始 0.6 s 内；
/// 2. 停顿前那段 ≤ 300 ms（伪影很短，真词不会这么短）；
/// 3. 停顿前比停顿后“整体偏轻”（≥3 dB）——排除词内爆破音闭塞（那是响的一侧在前后文里）。
fn onset_gap_ms(samples: &[f32], sr: u32) -> Option<f32> {
    let (dbs, hop_sec) = frame_dbs(samples, sr);
    if dbs.len() < 10 {
        return None;
    }
    let peak = dbs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !peak.is_finite() || peak < -70.0 {
        return None;
    }
    // 注意：peak 是**帧 RMS** 峰值（比采样峰值低 6–10 dB），所以门限用 −25 dB
    // 而不是 −35（实测 af_heart 的 s5/天气句伪影帧在 −36…−45 dB 之间）。
    let thr = peak - 25.0;
    let limit = ((0.6 / hop_sec) as usize).min(dbs.len());
    let first = dbs[..limit].iter().position(|d| *d >= thr)?;
    let mut i = first;
    while i < limit {
        if dbs[i] >= thr {
            i += 1;
            continue;
        }
        let quiet_start = i;
        while i < limit && dbs[i] < thr {
            i += 1;
        }
        let quiet = i - quiet_start;
        if quiet >= 2 && quiet_start > first {
            let pre = &dbs[first..quiet_start];
            let post_end = (i + 8).min(dbs.len());
            let post = &dbs[i..post_end];
            if pre.len() <= 30 && post.len() >= 3 && post.iter().any(|d| *d >= thr) {
                let pre_mean = pre.iter().sum::<f32>() / pre.len() as f32;
                let post_mean = post.iter().sum::<f32>() / post.len() as f32;
                if pre_mean + 3.0 < post_mean {
                    return Some(quiet as f32 * hop_sec * 1000.0);
                }
            }
        }
    }
    None
}

/// 候选排序：先看是否落在目标 ±6% 内，再看有无起始伪影，最后看距离。
///
/// 顺序很重要：如果只看“无伪影”，很慢的候选（实测 93 wpm）会脍过
/// 140 wpm 但带起始伪影的候选——语速直接超出雅思区间，材料不可用。
fn rank(s: &Synthesized, target: f32) -> (f32, u8, f32) {
    // 第一键必须是**离目标带宽多远**（连续量），不能是 0/1 的 off_band：
    // 否则“190 wpm”和“128 wpm”并列（都算越界），第二键（无起始伪影）会把
    // 190 wpm 的候选抬成最优 —— 实测 wl04 就是这么落到 190.5 wpm 的。
    let dist = (s.wpm - target).abs();
    let excess = (dist - target * 0.06).max(0.0);
    let artifact = u8::from(s.onset_gap_ms.is_some());
    (excess, artifact, dist)
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
    fn default_voice_is_a_male_english_voice_in_the_table() {
        assert!(
            KOKORO_VOICES.contains(&DEFAULT_VOICE),
            "默认音色必须在音色表里"
        );
        assert!(
            DEFAULT_VOICE.starts_with("am_") || DEFAULT_VOICE.starts_with("bm_"),
            "默认音色按用户决定应为男声，实际 {DEFAULT_VOICE}"
        );
        assert_eq!(voice_lang(DEFAULT_VOICE), "en-us");
        assert_eq!(voice_gender(DEFAULT_VOICE), "male");
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

    /// 生成 24 kHz 测试信号：若干段（振幅, 秒）拼接。
    fn synth(segments: &[(f32, f32)]) -> Vec<f32> {
        let sr = 24_000usize;
        let mut out = Vec::new();
        for (amp, secs) in segments {
            let n = (*secs * sr as f32) as usize;
            for i in 0..n {
                let t = i as f32 / sr as f32;
                out.push(amp * (2.0 * std::f32::consts::PI * 180.0 * t).sin());
            }
        }
        out
    }

    #[test]
    fn speech_span_trims_leading_and_trailing_silence() {
        let s = synth(&[(0.0, 0.2), (0.3, 1.0), (0.0, 0.3)]);
        let (a, b) = speech_span(&s, 24_000).expect("应能找到语音跨度");
        assert!((a - 0.2).abs() < 0.02, "起点 {a}");
        assert!((b - 1.2).abs() < 0.02, "终点 {b}");
    }

    #[test]
    fn onset_gap_flags_quiet_blob_before_body() {
        // 0.1 s 静音 → 0.15 s 低幅“a” → 0.04 s 停顿 → 0.5 s 正文
        let s = synth(&[(0.0, 0.1), (0.03, 0.15), (0.0, 0.04), (0.3, 0.5)]);
        let gap = onset_gap_ms(&s, 24_000).expect("应识别出起始伪影");
        assert!((20.0..=60.0).contains(&gap), "gap={gap}");
    }

    #[test]
    fn onset_gap_ignores_word_internal_closure() {
        // 词内爆破音闭塞：停顿前比停顿后更响（真词），不能当成伪影
        let s = synth(&[(0.0, 0.1), (0.3, 0.2), (0.0, 0.05), (0.25, 0.4)]);
        assert!(onset_gap_ms(&s, 24_000).is_none());
    }

    #[test]
    fn onset_gap_none_for_continuous_speech() {
        let s = synth(&[(0.0, 0.1), (0.3, 0.6), (0.0, 0.2)]);
        assert!(onset_gap_ms(&s, 24_000).is_none());
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
