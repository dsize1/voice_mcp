//! 音素级 GOP-SF（ADR-0007 待做项 4 的纯 Rust 实现，S1）。
//!
//! 分工：
//! - **音素 CTC 模型**（ONNX，`facebook/wav2vec2-lv-60-espeak-cv-ft`）给逐帧音素后验；
//! - **本模块**做 log-softmax → CTC-Viterbi 对齐 → GOP 打分 → 说话人内归一化。
//!
//! 为什么不在 Rust 里跑强制对齐器：CTC 模型的参考序列本身就是音素串，
//! 用 Viterbi 在 (L 手机 × T 帧) 网格上做一次 DP 就能同时得到"边界 + GOP"，
//! 不需要额外的对齐器（MMS_FA/NeMo 的价值是边界更准，不是"能不能算 GOP"）。
//!
//! 已知边界（写进 `limitations`，不要对外当结论）：
//! - 模型音素集是 espeak IPA，参考音素来自 CMUdict 的 ARPAbet，两者用**等价类**对齐；
//!   等价类内的音素无法区分（例如 AA 与 AH 都映射到 ʌ/ɑ），这类区分不做断言。
//! - 带宽 < 24 kHz 或蓝牙 AGC 设备上，后验本身不可靠 → 上游必须拦住。

pub mod ctc;
pub mod g2p;
pub mod model;
pub mod phones;

pub use ctc::{align, greedy, Aligned, PhoneRef};
pub use g2p::{G2p, TextPhones};
pub use model::PhonemeCtc;

use std::sync::{Mutex, OnceLock};

/// 全局惰性加载的 GOP 引擎（模型 1.2 GB，只在真正需要时载入）。
///
/// 为什么不放进 `Engines`：ASR/TTS 是每轮必用，GOP 是可选增强；
/// 把它常驻会白占 1.2 GB 内存 + 2 s 启动。
static GOP_ENGINE: OnceLock<Mutex<Option<GopEngine>>> = OnceLock::new();

pub struct GopEngine {
    pub model: PhonemeCtc,
    pub g2p: G2p,
    /// 实际使用的模型目录（用于在 limitations 里说明是微调版还是基线版）
    pub model_dir: String,
}

impl GopEngine {
    /// 默认路径：`$VOICE_PHONEME_MODEL` / `$VOICE_G2P_DICT` 可覆盖。
    pub fn load_default() -> Result<Self> {
        let preferred = std::env::var("VOICE_PHONEME_MODEL")
            .unwrap_or_else(|_| model::MODEL_DIR.to_string());
        let model_dir = if std::path::Path::new(&preferred).join("model.onnx").is_file() {
            preferred
        } else if std::path::Path::new(model::MODEL_DIR_BASELINE).join("model.onnx").is_file() {
            eprintln!("[gop] 未找到微调模型 {preferred}，回退基线 {}", model::MODEL_DIR_BASELINE);
            model::MODEL_DIR_BASELINE.to_string()
        } else {
            preferred // 让 load 报出准确错误
        };
        let dict = std::env::var("VOICE_G2P_DICT")
            .unwrap_or_else(|_| "C:/voice_models/g2p/cmudict.dict".to_string());
        Ok(Self {
            model: PhonemeCtc::load(&model_dir)?,
            g2p: G2p::load(std::path::Path::new(&dict))?,
            model_dir,
        })
    }
}

/// 在全局引擎上跑一次闭包（首次调用会加载模型，约 1–2 s）。
pub fn with_engine<T>(f: impl FnOnce(&mut GopEngine) -> Result<T>) -> Result<T> {
    let cell = GOP_ENGINE.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().map_err(|_| anyhow::anyhow!("GOP 引擎锁被 poison"))?;
    if guard.is_none() {
        *guard = Some(GopEngine::load_default()?);
    }
    f(guard.as_mut().expect("刚初始化"))
}

/// 是否值得跑 GOP（纯判断，便于单测）。
///
/// 三个前提：有参考文本、带宽够（≥24 kHz）、有音素模型。
pub fn should_run(with_gop: bool, has_reference: bool, band_limited: bool) -> bool {
    with_gop && has_reference && !band_limited
}

/// 给 `limitations` 用的固定措辞：GOP 现在只能当排序信号。
pub const GOP_LIMITATION: &str =
    "音素级 GOP 精度有限（so762 标定：FP=10% 时精确率 0.15–0.24），只能作为句内排序与趋势参考，不能单独判定某个音素读错";

use anyhow::Result;
use serde::Serialize;

/// 单个音素的诊断结果（给上层/LLM 用的最小结构）。
#[derive(Debug, Clone, Serialize)]
pub struct PhoneScore {
    pub phone: String,
    /// 模型在该区间最常命中/最优的符号（解释"听起来像什么"）
    pub best_symbol: String,
    pub t0_ms: u32,
    pub t1_ms: u32,
    /// 原始 GOP（越负越可疑）
    pub gop: f32,
    /// 说话人内 z 分（阈值判据用这个，不是 gop）
    pub z: f32,
}

/// 词级聚合：**用原始 GOP 的均值**，不要用 z 分。
///
/// 标定结果（so762，未见过的说话人，分层 150 句）：
/// 词级 mean-GOP 的 AUROC **0.887**，FP=10% 时精确率 **0.457**、召回 0.577；
/// 阈值收到 `gop_mean < −0.7` 时精确率 **0.596**、召回 0.262（FP 2.6%）。
/// 而音素级 z 分只有 0.17 —— 因为 z 分会把"整词都偏低"这个信号归一化掉，
/// 而音素级还要受基率限制（错读率 7.5% ⇒ FP=10% 时精确率上限 0.45）。
#[derive(Debug, Clone, Serialize)]
pub struct WordScore {
    pub word: String,
    /// 该词所有音素 GOP 的均值（越负越可疑）
    pub gop_mean: f32,
    pub t0_ms: u32,
    pub t1_ms: u32,
    /// 该词内最可疑的音素（解释用）
    pub worst_phone: String,
    pub worst_z: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct GopResult {
    pub version: String,
    pub frames: usize,
    pub hop_ms: u32,
    pub phones: Vec<PhoneScore>,
    /// 词典里没有的词：它们的音素**不在**结果里，不能当成"漏读"
    pub oov: Vec<String>,
    /// 有音素但没有可用候选（映射缺失）：同样不能当结论
    pub unmapped: Vec<String>,
    /// 词级聚合（**推荐对外使用这一层**，见 `WordScore` 的标定数字）
    pub words: Vec<WordScore>,
}

/// 把音素级结果按词分组（纯函数，便于单测）。
///
/// 只统计**有音素且成功对齐**的词；`word_of` 与 `aligned` 等长。
pub fn group_words(
    words: &[g2p::WordPhones],
    word_of: &[usize],
    aligned: &[Aligned],
    z: &[f32],
    hop_ms: u32,
) -> Vec<WordScore> {
    use std::collections::BTreeMap;
    let mut acc: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (k, w) in word_of.iter().enumerate() {
        acc.entry(*w).or_default().push(k);
    }
    let mut out = Vec::new();
    for (wi, idxs) in acc {
        let Some(w) = words.get(wi) else { continue };
        let gop_mean = idxs.iter().map(|k| aligned[*k].gop).sum::<f32>() / idxs.len() as f32;
        let (worst_k, worst_z) = idxs
            .iter()
            .map(|k| (*k, z[*k]))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or((idxs[0], 0.0));
        out.push(WordScore {
            word: w.word.clone(),
            gop_mean: crate::round_to(gop_mean, 2),
            t0_ms: aligned[idxs[0]].t0 as u32 * hop_ms,
            t1_ms: aligned[*idxs.last().unwrap()].t1 as u32 * hop_ms,
            worst_phone: aligned[worst_k].label.clone(),
            worst_z: crate::round_to(worst_z, 2),
        });
    }
    out.sort_by(|a, b| a.gop_mean.partial_cmp(&b.gop_mean).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// 端到端：16 kHz 音频 + 参考文本 → 每个音素的 GOP。
pub fn score_text(
    model: &mut PhonemeCtc,
    samples: &[f32],
    text: &str,
    g2p: &G2p,
) -> Result<GopResult> {
    let logp = model.log_probs(samples)?;
    let tp = g2p.text_to_phones(text);
    let mut refs = Vec::with_capacity(tp.phones.len());
    let mut word_of: Vec<usize> = Vec::with_capacity(tp.phones.len());
    let mut unmapped = Vec::new();
    for (wi, w) in tp.words.iter().enumerate() {
        for p in &w.phones {
            let cands = model.phones.candidates(p);
            if cands.is_empty() {
                if !unmapped.contains(p) {
                    unmapped.push(p.clone());
                }
                continue;
            }
            refs.push(PhoneRef { label: p.clone(), cands });
            word_of.push(wi);
        }
    }
    let aligned = align(&logp, &refs, model.phones.blank);
    let hop = model.hop_ms;
    match aligned {
        None => Ok(GopResult {
            version: GOP_VERSION.to_string(),
            frames: logp.len(),
            hop_ms: hop,
            phones: Vec::new(),
            oov: tp.oov,
            unmapped,
            words: Vec::new(),
        }),
        Some(a) => {
            let gops: Vec<f32> = a.iter().map(|x| x.gop).collect();
            let z = speaker_normalize(&gops);
            let phones = a
                .iter()
                .zip(z.iter())
                .map(|(x, zs)| PhoneScore {
                    phone: x.label.clone(),
                    best_symbol: model.phones.vocab.get(x.best).cloned().unwrap_or_default(),
                    t0_ms: x.t0 as u32 * hop,
                    t1_ms: x.t1 as u32 * hop,
                    gop: x.gop,
                    z: crate::round_to(*zs, 2),
                })
                .collect();
            let words = group_words(&tp.words, &word_of, &a, &z, hop);
            Ok(GopResult {
                version: GOP_VERSION.to_string(),
                frames: logp.len(),
                hop_ms: hop,
                phones,
                oov: tp.oov,
                unmapped,
                words,
            })
        }
    }
}
pub use phones::{espeak_candidates, norm_symbol, PhoneSet};

/// 评分器版本：写进 metrics，算法升级后可回溯/重算
pub const GOP_VERSION: &str = "gop-sf-0.1";

/// GOP 原始分 → 相对该说话人的 z 分（用中位数与 MAD，抗离群）。
///
/// 为什么必须做：不同说话人/录音设备的绝对后验差很多，
/// 同一句话的 GOP 在两个人之间不可比；z 化之后阈值才有意义。
pub fn speaker_normalize(gops: &[f32]) -> Vec<f32> {
    if gops.is_empty() {
        return Vec::new();
    }
    let mut s = gops.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med = s[s.len() / 2];
    let mut dev: Vec<f32> = s.iter().map(|v| (v - med).abs()).collect();
    dev.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mad = dev[dev.len() / 2].max(1e-3);
    // 1.4826 让 MAD 与标准差同量纲
    gops.iter().map(|v| (v - med) / (1.4826 * mad)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_is_robust_to_outliers() {
        // 一个离群值不应把其余点的 z 分全部拉偏
        let gops = vec![-0.2, -0.1, -0.15, -0.18, -5.0];
        let z = speaker_normalize(&gops);
        assert_eq!(z.len(), 5);
        let normal: Vec<f32> = z[..4].to_vec();
        for v in normal {
            assert!(v.abs() < 2.0, "正常音素的 z 分不应很大: {v}");
        }
        assert!(z[4] < -2.0, "离群音素应显著为负: {}", z[4]);
    }

    #[test]
    fn groups_phones_by_word_and_keeps_worst() {
        let words = vec![
            g2p::WordPhones { word: "think".into(), phones: vec!["TH".into(), "IH".into()] },
            g2p::WordPhones { word: "so".into(), phones: vec!["S".into(), "OW".into()] },
        ];
        let word_of = vec![0usize, 0, 1, 1];
        let aligned = vec![
            Aligned { label: "TH".into(), t0: 10, t1: 14, gop: -2.0, best: 1 },
            Aligned { label: "IH".into(), t0: 14, t1: 18, gop: 1.0, best: 2 },
            Aligned { label: "S".into(), t0: 18, t1: 20, gop: 3.0, best: 3 },
            Aligned { label: "OW".into(), t0: 20, t1: 24, gop: 3.0, best: 4 },
        ];
        let z = vec![-1.5f32, 0.5, 1.0, 1.0];
        let ws = group_words(&words, &word_of, &aligned, &z, 20);
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].word, "think", "gop 更低的词排前面");
        assert!((ws[0].gop_mean + 0.5).abs() < 1e-6, "think 均值 = (−2+1)/2");
        assert_eq!(ws[0].worst_phone, "TH");
        assert_eq!(ws[0].t0_ms, 200);
        assert_eq!(ws[1].word, "so");
    }

    #[test]
    fn empty_input_is_safe() {
        assert!(speaker_normalize(&[]).is_empty());
    }
}
