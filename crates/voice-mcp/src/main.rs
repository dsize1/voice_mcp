//! voice-mcp —— 把本地语音 I/O 能力封装为 MCP 工具。
//!
//! 职责边界（见 docs/adr）：**只做"测量"与"输入输出"，不做教学**。
//! 教学由 Skill + LLM 完成。
//!
//! 所有工具都会把结果留证到 `$VOICE_HOME`（默认 D:/voice_data）。

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::handler::server::tool::ToolRouter;
use rmcp::{ServiceExt, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use voice_core::audio::{self, capture, play};
use voice_core::engine::Engines;
use voice_core::{Config, ManifestEntry, Store};

// ------------------------------------------------------------------ 状态
struct App {
    cfg: Config,
    store: Store,
    /// 引擎惰性加载（启动即连上 MCP，不阻塞客户端）
    engines: Mutex<Option<Engines>>,
}

impl App {
    fn new(cfg: Config) -> Result<Self> {
        let store = Store::open(&cfg.data_dir)?;
        Ok(Self {
            cfg,
            store,
            engines: Mutex::new(None),
        })
    }

    /// 取引擎（首次调用时加载，约 2.5 s）
    fn with_engines<T>(&self, f: impl FnOnce(&Engines) -> Result<T>) -> Result<T> {
        let mut guard = self.engines.lock().unwrap();
        if guard.is_none() {
            *guard = Some(Engines::load(self.cfg.clone())?);
        }
        f(guard.as_ref().unwrap())
    }

    fn resolve(&self, p: &str) -> Result<std::path::PathBuf> {
        if p.eq_ignore_ascii_case("last") {
            return self
                .store
                .last_take()
                .context("没有上一次 take，请先 record 或 transcribe 一个文件");
        }
        let path = std::path::PathBuf::from(p);
        if !path.is_file() {
            anyhow::bail!("文件不存在: {}", path.display());
        }
        Ok(path)
    }
}

// ------------------------------------------------------------------ 参数
#[derive(Debug, Deserialize, JsonSchema, Default)]
struct TranscribeParams {
    /// 音频文件绝对路径，或 "last" 表示最近一次 take（支持 wav/m4a/mp3/flac/ogg）
    path: String,
    /// auto | en | zh | mixed（默认 auto）
    #[serde(default)]
    lang: Option<String>,
    /// 强制引擎：sense-voice | whisper（默认按语言路由）
    #[serde(default)]
    engine: Option<String>,
    /// 场景标签，写入留证文件名与 manifest
    #[serde(default)]
    scene: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
struct SynthesizeParams {
    /// 待合成文本。**一段文本只能有一种语言**：中文讲解与英文示范请分两次调用
    text: String,
    /// 音色名（如 af_heart / bf_emma / am_michael / zf_xiaobei）或 sid 数字
    #[serde(default)]
    voice: Option<String>,
    /// 语速，1.0 为常速；跟读示范建议 0.75
    #[serde(default)]
    speed: Option<f32>,
    /// 是否在服务端直接播放（Pi 的 TUI 无法播放音频，必须由服务端出声）
    #[serde(default)]
    play: bool,
    /// 同时生成 0.75x 慢速版（跟读用）
    #[serde(default)]
    slow_too: bool,
    #[serde(default)]
    scene: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
struct RecordParams {
    /// 录音上限（秒）
    #[serde(default)]
    max_sec: Option<f32>,
    /// 静音多久自动停止（秒）
    #[serde(default)]
    silence_sec: Option<f32>,
    /// 录完立刻转写
    #[serde(default)]
    transcribe: bool,
    /// 转写语言 auto | en | zh | mixed
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    scene: Option<String>,
    /// 输入设备名子串（蓝牙耳机用 "Hands-Free"）；不填用配置 mic 或系统默认
    #[serde(default)]
    device: Option<String>,
    /// 开始前等说话的超时（秒）。默认 8；自由说/长独白建议 20——
    /// 8 s 太短时会出现“人已经在说、工具却报没检测到说话”。
    #[serde(default)]
    start_timeout_sec: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
struct AssessParams {
    /// 音频文件绝对路径，或 "last"
    path: String,
    /// 参考文本（跟读/朗读的目标句）。为空则只做语流分析
    #[serde(default)]
    reference: Option<String>,
    /// auto | en | zh | mixed
    #[serde(default)]
    lang: Option<String>,
    /// 同时跑音素级 GOP（需 reference；首次调用加载 1.2 GB 模型，约 1–2 s）
    #[serde(default)]
    with_gop: bool,
    /// 强制引擎 sense-voice | whisper
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    scene: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
struct PathParams {
    /// 音频文件绝对路径，或 "last"
    path: String,
}


#[derive(Debug, Deserialize, JsonSchema, Default)]
struct PrepareParams {
    /// 音频文件绝对路径，或 "last"
    path: String,
    /// 去掉首尾静音
    #[serde(default = "default_true")]
    trim_silence: bool,
}

fn default_true() -> bool {
    true
}

// ------------------------------------------------------------------ 输出
#[derive(Debug, Serialize, JsonSchema)]
struct OkResult {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl OkResult {
    fn ok(v: serde_json::Value) -> Self {
        Self { ok: true, detail: Some(v), error: None }
    }
}

fn err_ok(e: impl std::fmt::Display) -> OkResult {
    OkResult { ok: false, detail: None, error: Some(e.to_string()) }
}

// ------------------------------------------------------------------ 服务
struct VoiceServer {
    app: Arc<App>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router(server_handler)]
impl VoiceServer {
    /// 语音转文字。支持 wav/m4a/mp3/flac/ogg（Win11 录音机 .m4a 直读）。
    /// 返回词级时间戳、token 时间戳、使用的引擎与语言，并自动把原始音频、
    /// 16k 派生音频、转写 JSON 一起落盘留证（目录在返回的 session 字段里）。
    #[tool(
        name = "transcribe",
        description = "语音转文字（中英，含词级时间戳）。入参 path 支持绝对路径或 \"last\"。结果自动留证。"
    )]
    async fn transcribe(&self, Parameters(p): Parameters<TranscribeParams>) -> Json<OkResult> {
        let app = self.app.clone();
        let r = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
            let src = app.resolve(&p.path)?;
            let session = app.store.new_session("dictate")?;
            let seq = session.next_seq()?;
            let scene = p.scene.clone().unwrap_or_else(|| "dictate".into());
            let prefix = session.take_prefix(seq, &scene);
            let orig = session.import_original(&src, &prefix)?;

            let t0 = std::time::Instant::now();
            let decoded = audio::load_16k(&orig)?;
            let decode_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let norm = session.write_norm_wav(&prefix, &decoded.samples, decoded.sample_rate)?;

            let lang = p.lang.clone().unwrap_or_else(|| "auto".into());
            let tr = app.with_engines(|e| {
                e.transcribe_long(&decoded.samples, decoded.sample_rate, &lang, p.engine.as_deref())
            })?;

            session.write_json(&prefix, "asr", &tr)?;
            session.write_json(
                &prefix,
                "audio",
                &serde_json::json!({ "info": decoded.info, "decode_ms": decode_ms }),
            )?;
            voice_core::store::finish_take(
                &app.store,
                ManifestEntry {
                    ts: chrono::Local::now().to_rfc3339(),
                    session: session.id.clone(),
                    seq,
                    scene: scene.clone(),
                    src: orig.to_string_lossy().to_string(),
                    norm_wav: Some(norm.to_string_lossy().to_string()),
                    lang: Some(tr.lang.clone()),
                    engine: Some(tr.engine.clone()),
                    dur: Some(tr.duration),
                    text: Some(tr.text.clone()),
                    degraded: Some(tr.degraded),
                    feedback_ref: None,
                },
            )?;

            Ok(serde_json::json!({
                "text": tr.text,
                "lang": tr.lang,
                "engine": tr.engine,
                "duration": round3(tr.duration),
                "infer_ms": round3(tr.infer_ms as f32),
                "degraded": tr.degraded,
                "words": tr.words.iter().map(|w| serde_json::json!({
                    "w": w.w, "t0": round3(w.t0), "t1": round3(w.t1)
                })).collect::<Vec<_>>(),
                "tokens": tr.tokens.iter().map(|w| serde_json::json!({
                    "t": w.w, "t0": round3(w.t0)
                })).collect::<Vec<_>>(),
                "audio": {
                    "src": orig, "norm_wav": norm,
                    "source_sr": decoded.info.source_sr,
                    "band_limited": decoded.info.band_limited,
                    "peak_dbfs": round3(decoded.info.peak_dbfs),
                    "silent_ratio": round3(decoded.info.silent_ratio),
                },
                "session": session.id,
                "prefix": prefix,
            }))
        })
        .await;

        match r {
            Ok(Ok(v)) => Json(OkResult::ok(v)),
            Ok(Err(e)) => Json(err_ok(format!("{e:#}"))),
            Err(e) => Json(err_ok(format!("任务失败: {e}"))),
        }
    }

    /// 文字转语音。返回生成的 wav 绝对路径；play=true 时服务端直接播放。
    /// 中文讲解与英文示范必须分开调用（一段文本只允许一种语言）。
    #[tool(
        name = "synthesize",
        description = "文字转语音（示范音，24kHz）。voice 可传音色名或 sid；play=true 由服务端播放；slow_too 同时生成 0.75x 慢速跟读版。"
    )]
    async fn synthesize(&self, Parameters(p): Parameters<SynthesizeParams>) -> Json<OkResult> {
        let app = self.app.clone();
        let r = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
            let session = app.store.new_session("tts")?;
            let seq = session.next_seq()?;
            let scene = p.scene.clone().unwrap_or_else(|| "tts".into());
            let prefix = session.take_prefix(seq, &scene);
            let speeds = voice_core::engine::demo_speeds(
                p.slow_too,
                p.speed,
                app.cfg.synth_speed,
            );
            let voice = p.voice.clone().unwrap_or_else(|| "af_heart".into());

            let files = app.with_engines(|e| {
                let sid = e.tts.resolve_sid(&voice);
                let mut out = Vec::new();
                for (label, sp) in &speeds {
                    let s = e.tts.synthesize(&p.text, sid, *sp)?;
                    let path = session.write_reference(
                        &prefix, label, &s.samples, s.sample_rate, &p.text, &s.voice, *sp,
                    )?;
                    out.push(serde_json::json!({
                        "label": label, "speed": sp, "path": path,
                        "duration": round3(s.duration),
                        "gen_ms": round3(s.gen_ms as f32),
                        "rtf": round3((s.gen_ms / 1000.0 / s.duration as f64) as f32),
                    }));
                }
                Ok((sid, out))
            })?;

            if p.play {
                if let Some(first) = files.1.first() {
                    if let Some(path) = first["path"].as_str() {
                        play::play_wav(path)?;
                    }
                }
            }

            Ok(serde_json::json!({
                "voice": voice,
                "sid": files.0,
                "files": files.1,
                "session": session.id,
                "prefix": prefix,
            }))
        })
        .await;

        match r {
            Ok(Ok(v)) => Json(OkResult::ok(v)),
            Ok(Err(e)) => Json(err_ok(format!("{e:#}"))),
            Err(e) => Json(err_ok(format!("任务失败: {e}"))),
        }
    }

    /// 录音（麦克风，静音自动停止）。需要系统有可用输入设备。
    #[tool(
        name = "record",
        description = "从麦克风录音，说完静音自动停止；transcribe=true 时直接返回转写与时间戳。需要可用麦克风。"
    )]
    async fn record(&self, Parameters(p): Parameters<RecordParams>) -> Json<OkResult> {
        let app = self.app.clone();
        let r = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
            let opts = capture::RecordOptions {
                max_sec: p.max_sec.unwrap_or(app.cfg.record_max_sec),
                silence_sec: p.silence_sec.unwrap_or(app.cfg.record_silence_sec),
                prefer_sr: app.cfg.capture_sr,
                start_timeout_sec: p.start_timeout_sec.unwrap_or(8.0),
                ..Default::default()
            };
            let device = p.device.clone().or_else(|| app.cfg.mic.clone());
            let cap = capture::record_with(device.as_deref(), &opts)?;
            let session = app.store.new_session("drill")?;
            let seq = session.next_seq()?;
            let scene = p.scene.clone().unwrap_or_else(|| "drill".into());
            let prefix = session.take_prefix(seq, &scene);

            let (trimmed, offset) = app.with_engines(|e| Ok(e.vad.trim_silence(&cap.samples, 0.15)))?;
            // 原始证据：设备采样率原样落盘
            let raw_path = session
                .dir
                .join("takes")
                .join(format!("{prefix}.raw{}.wav", cap.device_sr));
            voice_core::audio::wav::write_wav_i16(&raw_path, &cap.raw, cap.device_sr)?;
            let norm = session.write_norm_wav(&prefix, &trimmed, voice_core::INTERNAL_SR)?;
            session.write_json(
                &prefix,
                "capture",
                &serde_json::json!({
                    "device": cap.device_name, "device_sr": cap.device_sr,
                    "auto_stopped": cap.auto_stopped, "captured_sec": cap.duration,
                    "trim_offset": offset, "envelope_db": cap.envelope_db,
                }),
            )?;

            let mut tr_json = serde_json::Value::Null;
            let lang = p.lang.clone().unwrap_or_else(|| "auto".into());
            let tr = if p.transcribe {
                let tr = app.with_engines(|e| {
                    e.transcribe_long(&trimmed, voice_core::INTERNAL_SR, &lang, None)
                })?;
                session.write_json(&prefix, "asr", &tr)?;
                tr_json = serde_json::json!({
                    "text": tr.text, "lang": tr.lang, "engine": tr.engine,
                    "degraded": tr.degraded,
                    "words": tr.words.iter().map(|w| serde_json::json!({
                        "w": w.w, "t0": round3(w.t0), "t1": round3(w.t1)
                    })).collect::<Vec<_>>(),
                });
                Some(tr)
            } else {
                None
            };

            voice_core::store::finish_take(
                &app.store,
                ManifestEntry {
                    ts: chrono::Local::now().to_rfc3339(),
                    session: session.id.clone(),
                    seq,
                    scene: scene.clone(),
                    src: raw_path.to_string_lossy().to_string(),
                    norm_wav: Some(norm.to_string_lossy().to_string()),
                    lang: tr.as_ref().map(|t| t.lang.clone()),
                    engine: tr.as_ref().map(|t| t.engine.clone()),
                    dur: Some(trimmed.len() as f32 / voice_core::INTERNAL_SR as f32),
                    text: tr.as_ref().map(|t| t.text.clone()),
                    degraded: tr.as_ref().map(|t| t.degraded),
                    feedback_ref: None,
                },
            )?;

            Ok(serde_json::json!({
                "wav": norm,
                "src": raw_path,
                "duration": round3(trimmed.len() as f32 / voice_core::INTERNAL_SR as f32),
                "auto_stopped": cap.auto_stopped,
                "device": cap.device_name,
                "device_sr": cap.device_sr,
                "snr_db": round3(cap.snr()),
                "peak_dbfs": round3(cap.peak_dbfs()),
                "level_warning": level_warning(cap.peak_dbfs(), cap.snr()),
                "transcript": tr_json,
                "session": session.id,
                "prefix": prefix,
            }))
        })
        .await;

        match r {
            Ok(Ok(v)) => Json(OkResult::ok(v)),
            Ok(Err(e)) => Json(err_ok(format!("{e:#}"))),
            Err(e) => Json(err_ok(format!("任务失败: {e}"))),
        }
    }

    /// 播放音频文件（服务端出声）。
    #[tool(name = "play", description = "在服务端播放指定 wav 文件（Pi 的终端无法播放音频）。")]
    async fn play(&self, Parameters(p): Parameters<PathParams>) -> Json<OkResult> {
        let app = self.app.clone();
        let r = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
            let path = app.resolve(&p.path)?;
            play::play_wav(&path)?;
            Ok(serde_json::json!({ "played": path }))
        })
        .await;
        match r {
            Ok(Ok(v)) => Json(OkResult::ok(v)),
            Ok(Err(e)) => Json(err_ok(format!("{e:#}"))),
            Err(e) => Json(err_ok(format!("任务失败: {e}"))),
        }
    }

    /// 音频体检与预处理：解码、去首尾静音、输出 16k 派生文件、返回声学指标。
    #[tool(
        name = "prepare_audio",
        description = "音频体检：解码任意格式、可选去首尾静音、生成 16k 派生文件，返回时长/峰值/静音率/SR/带宽受限标记。"
    )]
    async fn prepare_audio(&self, Parameters(p): Parameters<PrepareParams>) -> Json<OkResult> {
        let app = self.app.clone();
        let r = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
            let src = app.resolve(&p.path)?;
            let session = app.store.new_session("prepare")?;
            let seq = session.next_seq()?;
            let prefix = session.take_prefix(seq, "prepare");
            let orig = session.import_original(&src, &prefix)?;
            let decoded = audio::load_16k(&orig)?;
            let (samples, offset) = if p.trim_silence {
                app.with_engines(|e| Ok(e.vad.trim_silence(&decoded.samples, 0.15)))?
            } else {
                (decoded.samples.clone(), 0.0)
            };
            let norm = session.write_norm_wav(&prefix, &samples, decoded.sample_rate)?;
            let info = audio::info(&samples, decoded.sample_rate);
            Ok(serde_json::json!({
                "src": orig, "norm_wav": norm,
                "trimmed_offset": round3(offset),
                "info": info,
                "session": session.id,
            }))
        })
        .await;
        match r {
            Ok(Ok(v)) => Json(OkResult::ok(v)),
            Ok(Err(e)) => Json(err_ok(format!("{e:#}"))),
            Err(e) => Json(err_ok(format!("任务失败: {e}"))),
        }
    }

    /// 列出可用音色
    #[tool(name = "voices", description = "列出 54 个可用音色（名称/语言/性别）及当前默认音色。")]
    async fn voices(&self) -> Json<OkResult> {
        let app = self.app.clone();
        let r = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
            app.with_engines(|e| {
                Ok(serde_json::json!({
                    "count": e.tts.num_speakers(),
                    "sample_rate": e.tts.sample_rate(),
                    "default_sid": app.cfg.default_sid,
                    "voices": e.tts.voices(),
                }))
            })
        })
        .await;
        match r {
            Ok(Ok(v)) => Json(OkResult::ok(v)),
            Ok(Err(e)) => Json(err_ok(format!("{e:#}"))),
            Err(e) => Json(err_ok(format!("任务失败: {e}"))),
        }
    }

    /// 运行状态：配置、模型、引擎加载情况、录音设备
    #[tool(
        name = "status",
        description = "运行状态自检：模型文件是否齐全、引擎是否已加载、麦克风是否可用、数据目录与留证位置。"
    )]
    async fn status(&self) -> Json<OkResult> {
        let app = self.app.clone();
        let r = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
            let models: Vec<serde_json::Value> = app
                .cfg
                .model_files()
                .into_iter()
                .map(|(name, path)| {
                    serde_json::json!({
                        "name": name,
                        "path": path,
                        "present": path.is_file(),
                    })
                })
                .collect();
            let loaded = app.engines.lock().unwrap().is_some();
            let manifest = app.store.read_manifest().unwrap_or_default();
            Ok(serde_json::json!({
                "version": voice_core::CORE_VERSION,
                "sherpa_onnx": voice_core::sherpa_version(),
                "models_dir": app.cfg.models_dir,
                "data_dir": app.cfg.data_dir,
                "provider": app.cfg.provider,
                "en_engine": app.cfg.en_engine,
                "engines_loaded": loaded,
                "mic": capture::default_input_device(),
                "watch_dirs": app.cfg.watch_dirs.iter().map(|d| serde_json::json!({
                    "path": d, "exists": d.is_dir()
                })).collect::<Vec<_>>(),
                "models": models,
                "manifest_entries": manifest.len(),
                "last_take": app.store.last_take(),
            }))
        })
        .await;
        match r {
            Ok(Ok(v)) => Json(OkResult::ok(v)),
            Ok(Err(e)) => Json(err_ok(format!("{e:#}"))),
            Err(e) => Json(err_ok(format!("任务失败: {e}"))),
        }
    }

    /// 评测：一次完成 ASR + 语流指标 + 词级差异，全部落盘留证。
    /// 这是口语训练的核心工具：返回的 metrics 是给 LLM 解读的"测量结果"，不是教学话术。
    #[tool(
        name = "assess",
        description = "评测一段录音：转写 + 语流（语速/停顿/词界/填充词）+ 与参考文本的词级差异（漏读/多读/替换，含音形相似度与原因推断）。返回结构化指标并留证。"
    )]
    async fn assess(&self, Parameters(p): Parameters<AssessParams>) -> Json<OkResult> {
        let app = self.app.clone();
        let r = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
            let src = app.resolve(&p.path)?;
            // 源在本机 sessions/ 内 → 就地复用该 take：不新建会话、不复制音频、
            // metrics/md 直接长在 record 的目录下（STATE.md 第 4 条“5 件套”成立）。
            // 外部文件（录音机目录/cases_audio）→ 保持原行为（新建 assess 会话 + 导入副本）。
            let reuse = app.store.session_for_take(&src);
            let (session, prefix, orig, feed) = match reuse {
                Some((s, pre)) => {
                    // 喂**原始文件**：保留 source_sr / band_limited 语义（
                    // 48 kHz 麦录不能被报成“源音频 ≤16 kHz，不要断言 /θ/ 与 /s/”）；
                    // 已生成的 .16k.wav 只当派生证据，不覆写、不逆用。
                    let p16 = s.dir.join("takes").join(format!("{pre}.16k.wav"));
                    let norm = if p16.is_file() { p16 } else { std::path::PathBuf::new() };
                    (s, pre, src.clone(), (src.clone(), norm))
                }
                None => {
                    let s = app.store.new_session("assess")?;
                    let seq = s.next_seq()?;
                    let scene = p.scene.clone().unwrap_or_else(|| "assess".into());
                    let pre = s.take_prefix(seq, &scene);
                    let orig0 = s.import_original(&src, &pre)?;
                    let sr = audio::load_16k(&orig0)?.info.source_sr;
                    let orig = align_original_name(&s.dir, orig0, &pre, sr);
                    (s, pre, orig.clone(), (orig.clone(), std::path::PathBuf::new()))
                }
            };
            let (feed, pre_norm) = feed;
            let seq = prefix
                .split('_')
                .next()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(1);
            let scene = p
                .scene
                .clone()
                .unwrap_or_else(|| session.kind.clone());

            let decoded = audio::load_16k(&feed)?;
            let norm = if pre_norm.as_os_str().is_empty() {
                session.write_norm_wav(&prefix, &decoded.samples, decoded.sample_rate)?
            } else {
                pre_norm
            };

            let lang = p.lang.clone().unwrap_or_else(|| "auto".into());
            let assessment = app.with_engines(|e| {
                voice_core::assess::assess(
                    e,
                    voice_core::AssessRequest {
                        samples: &decoded.samples,
                        sample_rate: decoded.sample_rate,
                        lang: &lang,
                        engine: p.engine.as_deref(),
                        reference: p.reference.as_deref(),
                        source_sr: Some(decoded.info.source_sr),
                        with_gop: p.with_gop,
                    },
                )
            })?;

            // 留证：metrics + 转写。同一 take 重复评测时把上一份 metrics 留成 .prev
            let metrics_path = session.dir.join("takes").join(format!("{prefix}.metrics.json"));
            if metrics_path.is_file() {
                let _ = std::fs::copy(
                    &metrics_path,
                    session.dir.join("takes").join(format!("{prefix}.metrics.prev.json")),
                );
            }
            session.write_json(&prefix, "metrics", &assessment)?;
            session.write_json(&prefix, "asr", &assessment.transcript)?;
            let feedback_path = session.dir.join("takes").join(format!("{prefix}.md"));
            if !feedback_path.is_file() {
                std::fs::write(
                    &feedback_path,
                    format!(
                        "# 反馈 · {prefix}

                         <!-- Agent 在这里写本轮反馈；结构：问题 / 怎么改 / 示范 / 再来一次 -->

                         - 参考文本：{}
- 识别结果：{}
",
                        p.reference.as_deref().unwrap_or("(未提供，仅语流分析)"),
                        assessment.transcript.text
                    ),
                )?;
            }
            session.write_session_json(Some(serde_json::json!({
                "scorer_version": assessment.scorer_version,
                "last_prefix": prefix,
            })))?;

            voice_core::store::finish_take(
                &app.store,
                ManifestEntry {
                    ts: chrono::Local::now().to_rfc3339(),
                    session: session.id.clone(),
                    seq,
                    scene: scene.clone(),
                    src: orig.to_string_lossy().to_string(),
                    norm_wav: Some(norm.to_string_lossy().to_string()),
                    lang: Some(assessment.transcript.lang.clone()),
                    engine: Some(assessment.transcript.engine.clone()),
                    dur: Some(assessment.transcript.duration),
                    text: Some(assessment.transcript.text.clone()),
                    degraded: Some(assessment.degraded),
                    feedback_ref: Some(feedback_path.to_string_lossy().to_string()),
                },
            )?;

            let issues = assessment
                .text_diff
                .as_ref()
                .map(|d| d.issues.clone())
                .unwrap_or_default();
            let summary = serde_json::json!({
                "text": assessment.transcript.text,
                "lang": assessment.transcript.lang,
                "accuracy": assessment.text_diff.as_ref().map(|d| round3(d.accuracy)),
                "substitutions": assessment.text_diff.as_ref().map(|d| d.substitutions),
                "deletions": assessment.text_diff.as_ref().map(|d| d.deletions),
                "insertions": assessment.text_diff.as_ref().map(|d| d.insertions),
                "speech_rate": assessment.flow.speech_rate,
                "articulation_rate": assessment.flow.articulation_rate,
                "rate_unit": assessment.flow.rate_unit,
                "pause_count": assessment.flow.pause_count,
                "pause_longest": assessment.flow.pause_longest,
                "word_boundary_breaks": assessment.flow.word_boundary_breaks,
                "filler_rate": assessment.flow.filler_rate,
                "repetitions": assessment.flow.repetitions,
                "degraded": assessment.degraded,
                "issue_count": issues.len(),
                "timestamp_alignment": assessment
                    .text_diff
                    .as_ref()
                    .map(|d| d.timestamp_alignment),
            });

            // GOP 摘要：只回最可疑的 5 个——精度有限，给多了会诱导 LLM 逐个下结论
            let gop_json = assessment.gop.as_ref().map(|g| {
                let mut v: Vec<&voice_core::gop::PhoneScore> = g.phones.iter().collect();
                v.sort_by(|a, b| a.z.partial_cmp(&b.z).unwrap_or(std::cmp::Ordering::Equal));
                serde_json::json!({
                    "version": g.version,
                    "phones_total": g.phones.len(),
                    "oov": g.oov,
                    "unmapped": g.unmapped,
                    "words_worst": g.words.iter().take(3).map(|w| serde_json::json!({
                        "word": w.word, "gop_mean": round3(w.gop_mean),
                        "t0_ms": w.t0_ms, "t1_ms": w.t1_ms,
                        "worst_phone": w.worst_phone, "worst_z": w.worst_z,
                    })).collect::<Vec<_>>(),
                    "worst": v.iter().take(5).map(|p| serde_json::json!({
                        "phone": p.phone, "heard": p.best_symbol,
                        "t0_ms": p.t0_ms, "t1_ms": p.t1_ms,
                        "gop": round3(p.gop), "z": p.z,
                    })).collect::<Vec<_>>(),
                })
            });

            Ok(serde_json::json!({
                "summary": summary,
                "issues": issues,
                "flow": {
                    "duration": round3(assessment.flow.duration),
                    "analysis_window": assessment.flow.analysis_window,
                    "speech_duration": round3(assessment.flow.speech_duration),
                    "speech_ratio": round3(assessment.flow.speech_ratio),
                    "pause_count": assessment.flow.pause_count,
                    "pause_total": assessment.flow.pause_total,
                    "pause_mean": assessment.flow.pause_mean,
                    "pause_longest": assessment.flow.pause_longest,
                    "pauses": assessment.flow.pauses,
                    "word_boundary_breaks": assessment.flow.word_boundary_breaks,
                    "word_gaps": assessment.flow.word_gaps,
                    "fillers": assessment.flow.fillers,
                    "filler_rate": assessment.flow.filler_rate,
                    "repetitions": assessment.flow.repetitions,
                    "longest_run_words": assessment.flow.longest_run_words,
                },
                "words": assessment.transcript.words.iter().map(|w| serde_json::json!({
                    "w": w.w, "t0": round3(w.t0), "t1": round3(w.t1)
                })).collect::<Vec<_>>(),
                "audio": {
                    "source_sr": assessment.audio.source_sr,
                    "band_limited": assessment.audio.band_limited,
                    "peak_dbfs": round3(assessment.audio.peak_dbfs),
                    "silent_ratio": round3(assessment.audio.silent_ratio),
                    "snr_db": assessment.snr_db,
                },
                "gop": gop_json,
                "limitations": assessment.limitations,
                "scorer_version": assessment.scorer_version,
                "evidence": {
                    "src": orig,
                    "norm_wav": norm,
                    "metrics_json": session.dir.join("takes").join(format!("{prefix}.metrics.json")),
                    "feedback_md": feedback_path,
                },
                "session": session.id,
                "prefix": prefix,
            }))
        })
        .await;
        match r {
            Ok(Ok(v)) => Json(OkResult::ok(v)),
            Ok(Err(e)) => Json(err_ok(format!("{e:#}"))),
            Err(e) => Json(err_ok(format!("任务失败: {e}"))),
        }
    }
}

/// 电平/信噪比的即时提示：录音当下就告诉用户"这条不能用"，别等到评测完才发现。
fn level_warning(peak_dbfs: f32, snr_db: f32) -> Option<String> {
    if peak_dbfs > -1.0 {
        return Some(format!(
            "峰值 {peak_dbfs:.1f} dBFS 接近满刻度（可能削波）：请把系统输入音量调低一档再录"
        ));
    }
    if snr_db < voice_core::assess::SNR_MIN_DB {
        return Some(format!(
            "信噪比仅 {snr_db:.0} dB（要求 ≥ {:.0} dB）：麦克风太远或环境太吵，这条录音不足以做诊断",
            voice_core::assess::SNR_MIN_DB
        ));
    }
    if peak_dbfs < -30.0 {
        return Some(format!("峰值仅 {peak_dbfs:.0} dBFS，电平偏低，建议把输入音量调高或靠近麦克风"));
    }
    None
}

fn round3(v: f32) -> f32 {
    voice_core::round_to(v, 3)
}

/// 源文件命名与 record 侧对齐：非 16 kHz 的 wav 存成 `<prefix>.raw<sr>.wav`，
/// 这样 24 kHz 示范音与 48 kHz 麦克风录音看文件名就能区分。
fn align_original_name(
    dir: &std::path::Path,
    orig: std::path::PathBuf,
    prefix: &str,
    sr: u32,
) -> std::path::PathBuf {
    let is_wav = orig
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("wav"))
        .unwrap_or(false);
    if !is_wav || sr == voice_core::INTERNAL_SR {
        return orig;
    }
    let want = dir.join("takes").join(format!("{prefix}.raw{sr}.wav"));
    if want == orig {
        return orig;
    }
    match std::fs::rename(&orig, &want) {
        Ok(()) => want,
        Err(_) => orig,
    }
}

// ------------------------------------------------------------------ 入口
async fn run_stdio() -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_dirs()?;
    let app = Arc::new(App::new(cfg)?);
    let server = VoiceServer {
        app,
        tool_router: VoiceServer::tool_router(),
    };
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr) // stdio 传输时日志必须走 stderr
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "voice_mcp=info".into()),
        )
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let res = rt.block_on(run_stdio());
    if let Err(e) = &res {
        eprintln!("[voice-mcp] 退出: {e:#}");
    }
    res
}
