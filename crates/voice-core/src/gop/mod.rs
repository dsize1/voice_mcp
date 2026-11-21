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
pub mod phones;

pub use ctc::{align, greedy, Aligned, PhoneRef};
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
    fn empty_input_is_safe() {
        assert!(speaker_normalize(&[]).is_empty());
    }
}
