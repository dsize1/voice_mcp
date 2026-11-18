//! voice-core — 音频 I/O、ASR/TTS 引擎封装、发音评测调度的公共库。
//!
//! 设计约束（见 docs/adr）：
//! - 纯 Rust，不依赖外部二进制（ffmpeg 仅作可选兜底）；
//! - 内部统一 `mono f32 @ 16 kHz`，原始文件永不被修改；
//! - 所有输出留证（音频 + 文字）到 `D:/voice_data`。

pub mod assess;
pub mod audio;
pub mod config;
pub mod engine;
pub mod store;

pub use assess::{Assessment, AssessRequest};
pub use config::Config;
pub use engine::{Engines, Transcript, Word};
pub use store::{ManifestEntry, Session, Store};

/// 内部统一采样率：ASR / VAD 的输入要求。
pub const INTERNAL_SR: u32 = 16_000;
/// TTS 输出采样率（Kokoro）。
pub const TTS_SR: u32 = 24_000;
/// 版本号，写入 metrics 便于算法升级后回溯
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// sherpa-onnx 原生库版本（doctor 用）
pub fn sherpa_version() -> &'static str {
    sherpa_onnx::version()
}
