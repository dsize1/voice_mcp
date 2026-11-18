//! 引擎集合：启动时一次性加载并常驻（避免"首次调用极慢"）。

pub mod asr;
pub mod tts;
pub mod vad;

pub use asr::{Asr, AsrEngine, AsrKind, Transcript, Word};
pub use tts::{KOKORO_VOICES, Synthesized, TtsEngine, VoiceInfo};

use anyhow::Result;

use crate::config::Config;

pub struct Engines {
    pub cfg: Config,
    pub asr: Asr,
    pub tts: TtsEngine,
    pub vad: vad::Vad,
}

impl Engines {
    /// 加载全部引擎。ASR 与 TTS 并行加载，把启动时间从 ~3s 压到 ~2s。
    pub fn load(cfg: Config) -> Result<Self> {
        let cfg_for_tts = cfg.clone();
        let tts_handle = std::thread::spawn(move || TtsEngine::load(&cfg_for_tts));
        let asr = Asr::load(&cfg)?;
        let tts = tts_handle
            .join()
            .map_err(|_| anyhow::anyhow!("TTS 加载线程崩溃"))??;
        let vad = vad::Vad::load(&cfg, crate::INTERNAL_SR)?;
        Ok(Self { cfg, asr, tts, vad })
    }
}
