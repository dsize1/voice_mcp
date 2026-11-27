//! 配置：模型/数据目录、引擎选择、录音参数。
//!
//! 优先级：默认值 ← `$VOICE_HOME/config.json` ← 环境变量。
//!
//! | 变量 | 默认 |
//! |---|---|
//! | `VOICE_MODELS_DIR` | `C:/voice_models` |
//! | `VOICE_HOME` | `D:/voice_data` |
//! | `VOICE_PROVIDER` | `cpu` |
//! | `VOICE_THREADS` | 4 |
//! | `VOICE_TTS_WPM` | 140（雅思 130–160） |
//! | `VOICE_TTS_LEXICON` | $VOICE_HOME/tts-lexicon.txt |

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 模型根目录（只读，不放 git）
    pub models_dir: PathBuf,
    /// 数据根目录：留证、档案、索引
    pub data_dir: PathBuf,
    /// ONNX Runtime execution provider: cpu / cuda / directml
    pub provider: String,
    pub num_threads: i32,
    /// 英文通道默认引擎："sense-voice" 或 "whisper"（默认 sense-voice，见 ADR-0002 更正）
    /// "mixed"（中英混说）始终走 whisper
    pub en_engine: String,
    /// 单次录音上限（秒）
    pub record_max_sec: f32,
    /// 录音自动停止的静音判定时长（秒）
    ///
    /// 场景建议：单词 1.2 / 短句 1.8 / 整句 2.2 / 自由说 2.5。
    /// 太小会把刻意停顿当句尾（实测 1.0 s 会丢掉停顿后的词）。
    pub record_silence_sec: f32,
    /// 未指定音色时的默认 sid
    pub default_sid: i32,
    /// 示范音默认目标语速（wpm）。雅思听说练习默认 140，建议区间 130–160。
    pub tts_wpm: f32,
    /// 项目级 TTS 词表（覆盖内置 lexicon；默认 `$VOICE_HOME/tts-lexicon.txt`）
    pub tts_lexicon: Option<PathBuf>,
    /// 监视目录（Win11 录音机输出目录），可为空
    pub watch_dirs: Vec<PathBuf>,
    /// 高精度采集采样率（cpal）
    pub capture_sr: u32,
    /// 输入设备名子串（蓝牙耳机需指定 "Hands-Free"）；None = 系统默认
    pub mic: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            models_dir: PathBuf::from(r"C:\voice_models"),
            data_dir: PathBuf::from(r"D:\voice_data"),
            provider: "cpu".into(),
            num_threads: 4,
            en_engine: "sense-voice".into(),
            record_max_sec: 30.0,
            record_silence_sec: 2.0,
            default_sid: 0,
            tts_wpm: 140.0,
            tts_lexicon: None,
            capture_sr: 48_000,
            mic: None,
            watch_dirs: default_watch_dirs(),
        }
    }
}

fn default_watch_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let docs = PathBuf::from(&home).join("Documents");
        // 中英文系统的两种目录名
        v.push(docs.join("录音"));
        v.push(docs.join("Sound Recordings"));
    }
    v
}

impl Config {
    pub fn load() -> Result<Self> {
        let mut cfg = Config::default();

        // 1) 环境变量决定 data_dir，再由此定位 config.json
        if let Some(home) = std::env::var_os("VOICE_HOME") {
            cfg.data_dir = PathBuf::from(home);
        }
        let file = cfg.data_dir.join("config.json");
        // 2) 文件覆盖
        if file.is_file() {
            let raw = std::fs::read_to_string(&file)
                .with_context(|| format!("读配置失败: {}", file.display()))?;
            cfg = serde_json::from_str(&raw)
                .with_context(|| format!("解析配置失败: {}", file.display()))?;
        }
        // 3) 环境变量覆盖
        if let Some(v) = std::env::var_os("VOICE_MODELS_DIR") {
            cfg.models_dir = PathBuf::from(v);
        }
        if let Some(v) = std::env::var_os("VOICE_HOME") {
            cfg.data_dir = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("VOICE_PROVIDER") {
            cfg.provider = v;
        }
        if let Ok(v) = std::env::var("VOICE_THREADS")
            && let Ok(n) = v.parse()
        {
            cfg.num_threads = n;
        }
        if let Ok(v) = std::env::var("VOICE_TTS_WPM")
            && let Ok(n) = v.parse::<f32>()
        {
            cfg.tts_wpm = n.clamp(80.0, 220.0);
        }
        if let Some(v) = std::env::var_os("VOICE_TTS_LEXICON") {
            cfg.tts_lexicon = Some(PathBuf::from(v));
        }
        Ok(cfg)
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("创建数据目录失败: {}", self.data_dir.display()))?;
        std::fs::create_dir_all(self.data_dir.join("sessions"))?;
        Ok(())
    }

    pub fn model(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.models_dir.join(rel)
    }

    pub fn sense_voice_model(&self) -> PathBuf {
        self.model("sense-voice/model.int8.onnx")
    }
    pub fn sense_voice_tokens(&self) -> PathBuf {
        self.model("sense-voice/tokens.txt")
    }
    pub fn whisper_encoder(&self) -> PathBuf {
        self.model("whisper-turbo/turbo-encoder.int8.onnx")
    }
    pub fn whisper_decoder(&self) -> PathBuf {
        self.model("whisper-turbo/turbo-decoder.int8.onnx")
    }
    pub fn whisper_tokens(&self) -> PathBuf {
        self.model("whisper-turbo/turbo-tokens.txt")
    }
    pub fn kokoro_model(&self) -> PathBuf {
        self.model("kokoro-multi-lang/model.onnx")
    }
    pub fn kokoro_voices(&self) -> PathBuf {
        self.model("kokoro-multi-lang/voices.bin")
    }
    pub fn kokoro_tokens(&self) -> PathBuf {
        self.model("kokoro-multi-lang/tokens.txt")
    }
    pub fn kokoro_lexicon(&self) -> PathBuf {
        self.model("kokoro-multi-lang/lexicon-en-us.txt")
    }
    /// 开放词表 G2P 用的 CMUdict（TTS 文本前端的必需件）
    pub fn g2p_cmudict(&self) -> PathBuf {
        self.model("g2p/cmudict.dict")
    }

    pub fn silero_vad(&self) -> PathBuf {
        self.model("silero-vad/onnx/model.onnx")
    }

    /// 模型文件清单（doctor 用）
    pub fn model_files(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("sense-voice model", self.sense_voice_model()),
            ("sense-voice tokens", self.sense_voice_tokens()),
            ("whisper encoder", self.whisper_encoder()),
            ("whisper decoder", self.whisper_decoder()),
            ("whisper tokens", self.whisper_tokens()),
            ("kokoro model", self.kokoro_model()),
            ("kokoro voices", self.kokoro_voices()),
            ("kokoro tokens", self.kokoro_tokens()),
            ("g2p cmudict", self.g2p_cmudict()),
            ("silero vad", self.silero_vad()),
        ]
    }
}
