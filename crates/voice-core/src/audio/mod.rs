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
