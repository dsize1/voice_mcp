//! 音频子系统：解码、重采样、WAV 写出、采集、播放、基础声学统计。

pub mod capture;
pub mod decode;
pub mod play;
pub mod resample;
pub mod wav;

pub use decode::{AudioInfo, DecodedAudio, decode_to_mono};

use anyhow::Result;

/// 把任意音频文件解码成内部标准形态（16 kHz mono f32）。
pub fn load_16k(path: impl AsRef<std::path::Path>) -> Result<DecodedAudio> {
    decode_to_mono(path, crate::INTERNAL_SR)
}

/// 写 16 kHz WAV（留证用）
pub fn save_16k(path: impl AsRef<std::path::Path>, samples: &[f32]) -> Result<()> {
    wav::write_wav_i16(path, samples, crate::INTERNAL_SR)
}

/// 基础体检：时长/峰值/有效值/削波/静音率
pub fn info(samples: &[f32], sr: u32) -> AudioInfo {
    decode::analyze_pub(samples, sr, 1)
}

/// 稳健 SNR 估计（dB）。两条分支，因为"语音占空比"差异极大：
/// - **≥4 s 的长录音**：`p90 − p10`（抗 AGC 尖峰）
/// - **<4 s 的短词/短句**：`top-5% 帧均值 − p10`
///   原因：单词 drill 里语音只占 10–25% 的帧，p90 会落在噪声里（实测 `Thank you.` 只有 7.7 dB，
///   但实际听得清楚、ASR 全对；改用 top-5% 后是 16.9 dB）。
///   合成噪声梯度验证：名义 4 dB 档在短录音分支下仍是 12.5 dB（<13，照旧拦截）。
///
/// 为什么不用"峰值 − 底噪"：蓝牙耳机有 AGC，单个瞬态峰值不可靠；
/// p95/p10 对偶发爆音和瞬时静音都不敏感，且不依赖 VAD（VAD 在低 SNR 下本身就错）。
///
/// 为什么用 p90 而不是 p95：蓝牙 AGC 会产生少量尖峰帧，p95 会被尖峰拉到 −30 dBFS 左右，
/// 让"语音很轻、噪声很高"的录音仍然算出 15 dB 的假高 SNR（实测：两条作废录音分别是
/// p95−p10 = 13.0 / 15.4 dB，而 p90−p10 = 11.5 / 11.0 dB）。
///
/// 实测分布（本机蓝牙耳机）：可用录音 p90−p10 = 14.3–19.5 dB；ASR 开始出碎片的录音 ≤ 11.5 dB。
pub fn snr_db(samples: &[f32], sr: u32) -> f32 {
    let hop = ((sr as usize) / 50).max(1); // 20 ms
    if samples.len() < hop * 5 {
        return 0.0;
    }
    let mut dbs: Vec<f32> = samples
        .chunks(hop)
        .map(|c| {
            let sq: f32 = c.iter().map(|v| v * v).sum();
            let rms = (sq / c.len() as f32).sqrt();
            if rms <= 1e-9 {
                -120.0
            } else {
                20.0 * rms.log10()
            }
        })
        .collect();
    dbs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = dbs.len();
    let p = |q: f32| dbs[(((n - 1) as f32) * q) as usize];
    let noise = p(0.10);
    let speech = if n < 200 {
        let start = n - (n / 20).max(1);
        dbs[start..].iter().sum::<f32>() / (n - start) as f32
    } else {
        p(0.90)
    };
    (speech - noise).max(0.0)
}

#[cfg(test)]
mod snr_tests {
    use super::snr_db;

    #[test]
    fn quiet_noise_gives_low_snr() {
        // 全程同幅噪声 → p95 与 p10 接近，SNR 应很小
        let sr = 16_000;
        let noise: Vec<f32> = (0..sr)
            .map(|i| if i % 2 == 0 { 0.001 } else { -0.001 })
            .collect();
        assert!(snr_db(&noise, sr) < 6.0, "纯噪声不该被判成高 SNR");
    }

    #[test]
    fn short_utterance_uses_top_frames() {
        // 2 s：0.4 s 语音（占 20% 帧）+ 1.6 s 噪声 —— 短录音分支应给出高 SNR
        let sr = 16_000;
        let n = sr as usize * 2;
        let speech_end = (n as f32 * 0.2) as usize;
        let v: Vec<f32> = (0..n)
            .map(|i| if i < speech_end { 0.2 } else { 0.002 })
            .collect();
        let s = snr_db(&v, sr);
        assert!(s > 25.0, "短录音里稀疏语音也应算出高 SNR，实际 {s}");
    }

    #[test]
    fn loud_段_and_quiet_段_gives_high_snr() {
        // 前 30% 是响亮语音、后 70% 是安静底噪（**按帧**划分，不能按采样交错）
        let sr = 16_000;
        let n = sr as usize;
        let cut = n * 3 / 10;
        let v: Vec<f32> = (0..n).map(|i| if i < cut { 0.3 } else { 0.0005 }).collect();
        let s = snr_db(&v, sr);
        assert!(s > 40.0, "语音段/静音段分明的音频 SNR 应很高，实际 {s}");
    }
}
