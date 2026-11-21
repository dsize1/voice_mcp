//! voice-cli —— 客户端工具：自检、转写、合成、播放、录音、音色校准。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use voice_core::audio::{self, capture, play};
use voice_core::engine::{Engines, Transcript};
use voice_core::{Config, ManifestEntry, Store};

#[derive(Parser)]
#[command(name = "voice-cli", version, about = "本地语音 I/O 客户端（中英双语）")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 环境自检：模型、原生库、麦克风、数据目录
    Doctor {
        /// 同时加载引擎并做一次真实推理（较慢，约 2-3 秒）
        #[arg(long)]
        load: bool,
    },
    /// 语音转文字（会把原始音频与转写结果留证）
    Stt {
        /// 音频路径，或 "last" 表示最近一次 take
        path: String,
        #[arg(long, default_value = "auto")]
        lang: String,
        #[arg(long)]
        engine: Option<String>,
        /// 场景标签，用于留证文件名
        #[arg(long, default_value = "dictate")]
        scene: String,
        /// 只输出纯文本
        #[arg(long)]
        plain: bool,
    },
    /// 文字转语音（示范音）
    Say {
        text: String,
        #[arg(long)]
        voice: Option<String>,
        #[arg(long)]
        speed: Option<f32>,
        /// 同时生成 0.75x 慢速版
        #[arg(long)]
        slow_too: bool,
        /// 生成后立即播放
        #[arg(long)]
        play: bool,
        #[arg(long, default_value = "tts")]
        scene: String,
    },
    /// 播放 wav 文件
    Play { path: String },
    /// 评测：转写 + 语流 + 与参考文本的词级差异
    Assess {
        /// 音频路径或 "last"
        path: String,
        /// 参考文本（跟读目标句）；不填只做语流分析
        #[arg(long)]
        reference: Option<String>,
        #[arg(long, default_value = "auto")]
        lang: String,
        #[arg(long)]
        engine: Option<String>,
        #[arg(long, default_value = "assess")]
        scene: String,
    },
    /// 用 LRC 字幕当基准，逐行核对转写与时间戳（教材音频自检 / 生成跟读素材）
    Lesson {
        /// 音频路径
        audio: String,
        /// 配套 LRC（双语，[mm:ss.xx]英文 | 中文）
        #[arg(long)]
        lrc: String,
        #[arg(long, default_value = "en")]
        lang: String,
        /// 只看逐行明细
        #[arg(long)]
        verbose: bool,
        /// 把 LRC 按本音轨重新定时并写到该路径（源 LRC 与本音轨版本不一致时很有用）
        #[arg(long)]
        write_lrc: Option<String>,
    },
    /// 跟读素材库（ADR-0009）：ingest / list / show / drill / stats
    Bank {
        #[command(subcommand)]
        cmd: BankCmd,
    },
    /// 跑录音回归用例（cases/*.json，只存断言不存音频）
    Cases {
        /// 用例目录
        #[arg(long, default_value = "cases")]
        dir: String,
        /// 只跑某个用例
        #[arg(long)]
        only: Option<String>,
    },
    /// 麦克风诊断：设备能力 + 底噪统计（用于校准自动停止阈值）
    Mic {
        /// 采集秒数（保持安静）
        #[arg(long, default_value_t = 3.0)]
        secs: f32,
    },
    /// 录音（自动停止）并可立即转写
    Rec {
        #[arg(long, default_value_t = 30.0)]
        max_sec: f32,
        /// 多久没声音算说完（秒）。单词 1.2 / 短句 1.8 / 整句 2.2 / 自由说 2.5
        #[arg(long, default_value_t = 2.0)]
        silence: f32,
        /// 等说话的超时（秒）。默认 8；自由说建议 20
        #[arg(long, default_value_t = 8.0)]
        start_timeout: f32,
        /// 录完立即转写
        #[arg(long)]
        stt: bool,
        #[arg(long, default_value = "en")]
        lang: String,
        #[arg(long, default_value = "drill")]
        scene: String,
    },
    /// 监视录音目录，新录音自动导入 + 转写（Win11 录音机 / 手机传回均可）
    Watch {
        /// 覆盖监视目录（默认取配置里的 Win11 录音机目录）
        #[arg(long)]
        dir: Option<String>,
        #[arg(long, default_value = "en")]
        lang: String,
    },
    /// 为全部 54 个音色各合成一句，供人耳校准音色名 ↔ sid 映射
    TtsProbe {
        #[arg(long, default_value = "The quick brown fox jumps over the lazy dog.")]
        text: String,
    },
}


#[derive(Subcommand)]
enum BankCmd {
    /// 把 LRC 教材切成逐句素材入库（走质量门槛，不合格进 quarantine）
    Ingest {
        /// 本地目录（含 `<NN>.<Title>.mp3` + 同名 `.lrc`）
        #[arg(long)]
        from_dir: Option<String>,
        /// 书号，如 NCE2（不填就从路径猜）
        #[arg(long, default_value = "")]
        book: String,
        /// 最多处理几课
        #[arg(long)]
        limit: Option<usize>,
        /// 只算质量门槛，不写文件
        #[arg(long)]
        dry_run: bool,
        /// 已存在的 id 也覆盖
        #[arg(long)]
        overwrite: bool,
    },
    /// 列素材
    List {
        #[arg(long)]
        level: Option<u8>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        book: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// 看一条素材（含质量指标与音轨是否存在）
    Show { id: String },
    /// 播原生示范并打印句子/中文
    Drill {
        id: String,
        /// 不播音频，只打印
        #[arg(long)]
        quiet: bool,
    },
    /// 统计：条数/时长/难度分布/标签排行/quarantine 数
    Stats,
    /// 重算全部素材的 tags（纯文本规则，不动音频）
    Retag,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load()?;
    match cli.cmd {
        Cmd::Doctor { load } => doctor(&cfg, load),
        Cmd::Stt { path, lang, engine, scene, plain } => {
            stt(&cfg, &path, &lang, engine.as_deref(), &scene, plain)
        }
        Cmd::Say { text, voice, speed, slow_too, play, scene } => {
            say(&cfg, &text, voice.as_deref(), speed, slow_too, play, &scene)
        }
        Cmd::Play { path } => {
            play::play_wav(&path)?;
            Ok(())
        }
        Cmd::Lesson { audio: a, lrc, lang, verbose, write_lrc } => {
            lesson(&cfg, &a, &lrc, &lang, verbose, write_lrc.as_deref())
        }
        Cmd::Cases { dir, only } => run_cases(&cfg, &dir, only.as_deref()),
        Cmd::Bank { cmd } => match cmd {
            BankCmd::Ingest { from_dir, book, limit, dry_run, overwrite } => {
                bank_ingest(&cfg, from_dir.as_deref(), &book, limit, dry_run, overwrite)
            }
            BankCmd::List { level, tag, book, limit } => {
                bank_list(&cfg, level, tag.as_deref(), book.as_deref(), limit)
            }
            BankCmd::Show { id } => bank_show(&cfg, &id),
            BankCmd::Drill { id, quiet } => bank_drill(&cfg, &id, quiet),
            BankCmd::Stats => bank_stats(&cfg),
            BankCmd::Retag => {
                let n = voice_core::bank::retag(Path::new(&cfg.data_dir))?;
                println!("{}", serde_json::json!({"retagged": n}));
                Ok(())
            }
        },
        Cmd::Mic { secs } => mic(&cfg, &secs),
        Cmd::Rec { max_sec, silence, start_timeout, stt: then_stt, lang, scene } => {
            rec(&cfg, max_sec, silence, start_timeout, then_stt, &lang, &scene)
        }
        Cmd::Assess { path, reference, lang, engine, scene } => {
            assess_cmd(&cfg, &path, reference.as_deref(), &lang, engine.as_deref(), &scene)
        }
        Cmd::Watch { dir, lang } => watch(&cfg, dir.as_deref(), &lang),
        Cmd::TtsProbe { text } => tts_probe(&cfg, &text),
    }
}

// ------------------------------------------------------------------ doctor
fn doctor(cfg: &Config, load: bool) -> Result<()> {
    let mut ok = 0;
    let mut bad = 0;

    println!("== 环境自检");
    println!("模型目录 : {}", cfg.models_dir.display());
    println!("数据目录 : {}", cfg.data_dir.display());
    println!("sherpa-onnx: {}", voice_core::sherpa_version());
    println!("voice-core : {}", voice_core::CORE_VERSION);
    println!("provider   : {}  threads={}", cfg.provider, cfg.num_threads);

    println!("\n-- 模型文件");
    for (name, path) in cfg.model_files() {
        if path.is_file() {
            let mb = std::fs::metadata(&path).map(|m| m.len() as f64 / 1e6).unwrap_or(0.0);
            println!("  [OK]   {name:<20} {mb:>8.1} MB  {}", path.display());
            ok += 1;
        } else {
            println!("  [MISS] {name:<20} {}", path.display());
            bad += 1;
        }
    }

    println!("\n-- 数据目录");
    match cfg.ensure_dirs() {
        Ok(_) => {
            let probe = cfg.data_dir.join(".write_probe");
            match std::fs::write(&probe, b"1").and_then(|_| std::fs::remove_file(&probe)) {
                Ok(_) => {
                    println!("  [OK]   可写 {}", cfg.data_dir.display());
                    ok += 1;
                }
                Err(e) => {
                    println!("  [FAIL] 不可写: {e}");
                    bad += 1;
                }
            }
        }
        Err(e) => {
            println!("  [FAIL] {e}");
            bad += 1;
        }
    }

    println!("\n-- 录音设备");
    match capture::default_input_device() {
        Some(d) => {
            println!("  [OK]   默认: {d}");
            ok += 1;
        }
        None => {
            println!("  [WARN] 没有默认输入设备");
        }
    }
    for d in capture::input_devices() {
        println!("         可用: {d}");
    }
    if let Ok(r) = capture::measure_noise_with(cfg.mic.as_deref(), 0.6) {
        if r.narrowband {
            println!(
                "  [WARN] 默认输入是窄带设备（{} Hz）：蓝牙 HFP 常见，摩擦音细节受限；                 纠音建议用 USB/有线麦克风（48 kHz）",
                r.device_sr
            );
        } else {
            println!("  [OK]   输入采样率 {} Hz（宽带）", r.device_sr);
        }
    }

    println!("\n-- 监视目录（Win11 录音机）");
    for d in &cfg.watch_dirs {
        if d.is_dir() {
            let n = std::fs::read_dir(d)
                .map(|it| it.filter_map(|e| e.ok()).count())
                .unwrap_or(0);
            println!("  [OK]   {} ({n} 个文件)", d.display());
        } else {
            println!("  [--]   {} 不存在", d.display());
        }
    }

    if let Some(last) = Store::open(&cfg.data_dir)?.last_take() {
        println!("\n-- 最近 take 解码验证");
        match audio::load_16k(&last) {
            Ok(a) => {
                println!(
                    "  [OK]   {} -> {:.2}s @16k, peak {:.1} dBFS",
                    last.file_name().unwrap_or_default().to_string_lossy(),
                    a.info.duration,
                    a.info.peak_dbfs
                );
                ok += 1;
            }
            Err(e) => {
                println!("  [FAIL] {e}");
                bad += 1;
            }
        }
    }

    if load {
        println!("\n-- 加载引擎");
        let t = std::time::Instant::now();
        match Engines::load(cfg.clone()) {
            Ok(e) => {
                println!(
                    "  [OK]   ASR(sense-voice) + TTS(kokoro {} 音色 @{} Hz) + VAD 加载完成 {} ms",
                    e.tts.num_speakers(),
                    e.tts.sample_rate(),
                    t.elapsed().as_millis()
                );
                ok += 1;
            }
            Err(e) => {
                println!("  [FAIL] {e}");
                bad += 1;
            }
        }
    }

    println!("\n结论: OK={ok} FAIL={bad}");
    if bad > 0 {
        bail!("自检未通过");
    }
    Ok(())
}

// ------------------------------------------------------------------ 公共
fn resolve_path(store: &Store, p: &str) -> Result<PathBuf> {
    if p.eq_ignore_ascii_case("last") {
        return store
            .last_take()
            .context("没有找到上一次 take（先录一次音）");
    }
    let path = PathBuf::from(p);
    if !path.is_file() {
        bail!("文件不存在: {}", path.display());
    }
    Ok(path)
}

fn print_transcript(t: &Transcript, plain: bool, extras: serde_json::Value) {
    if plain {
        println!("{}", t.text);
        return;
    }
    let v = serde_json::json!({
        "text": t.text,
        "lang": t.lang,
        "engine": t.engine,
        "duration": round3(t.duration),
        "infer_ms": round3(t.infer_ms as f32),
        "degraded": t.degraded,
        "words": t.words.iter().map(|w| serde_json::json!({
            "w": w.w, "t0": round3(w.t0), "t1": round3(w.t1)
        })).collect::<Vec<_>>(),
        "extras": extras,
    });
    println!("{}", serde_json::to_string_pretty(&v).unwrap());
}

fn round3(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

// ------------------------------------------------------------------ stt
fn stt(
    cfg: &Config,
    path: &str,
    lang: &str,
    engine: Option<&str>,
    scene: &str,
    plain: bool,
) -> Result<()> {
    let store = Store::open(&cfg.data_dir)?;
    let src = resolve_path(&store, path)?;
    let session = store.new_session("dictate")?;

    let seq = session.next_seq()?;
    let prefix = session.take_prefix(seq, scene);
    let orig = session.import_original(&src, &prefix)?;

    let t0 = std::time::Instant::now();
    let decoded = audio::load_16k(&orig)?;
    let norm = session.write_norm_wav(&prefix, &decoded.samples, decoded.sample_rate)?;
    let decode_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let engines = Engines::load(cfg.clone())?;
    let t = engines.transcribe_long(&decoded.samples, decoded.sample_rate, lang, engine)?;

    session.write_json(&prefix, "asr", &t)?;
    session.write_json(
        &prefix,
        "audio",
        &serde_json::json!({ "info": decoded.info, "decode_ms": decode_ms }),
    )?;

    let entry = ManifestEntry {
        ts: chrono::Local::now().to_rfc3339(),
        session: session.id.clone(),
        seq,
        scene: scene.to_string(),
        src: orig.to_string_lossy().to_string(),
        norm_wav: Some(norm.to_string_lossy().to_string()),
        lang: Some(t.lang.clone()),
        engine: Some(t.engine.clone()),
        dur: Some(t.duration),
        text: Some(t.text.clone()),
        degraded: Some(t.degraded),
        feedback_ref: None,
    };
    voice_core::store::finish_take(&store, entry)?;

    print_transcript(
        &t,
        plain,
        serde_json::json!({
            "session": session.id,
            "prefix": prefix,
            "src": orig,
            "norm_wav": norm,
        }),
    );
    Ok(())
}

// ------------------------------------------------------------------ say
fn say(
    cfg: &Config,
    text: &str,
    voice: Option<&str>,
    speed: Option<f32>,
    slow_too: bool,
    do_play: bool,
    scene: &str,
) -> Result<()> {
    let store = Store::open(&cfg.data_dir)?;
    let session = store.new_session("tts")?;
    let engines = Engines::load(cfg.clone())?;
    let tts = &engines.tts;

    let voice_name = voice.unwrap_or("af_heart");
    let sid = tts.resolve_sid(voice_name);

    let seq = session.next_seq()?;
    let prefix = session.take_prefix(seq, scene);

    let mut out = Vec::new();
    let speeds = voice_core::engine::demo_speeds(slow_too, speed, cfg.synth_speed);
    for (label, sp) in &speeds {
        let s = tts.synthesize(text, sid, *sp)?;
        let p = session.write_reference(&prefix, label, &s.samples, s.sample_rate, text, &s.voice, *sp)?;
        out.push(serde_json::json!({
            "label": label, "speed": sp, "path": p,
            "dur": round3(s.duration), "gen_ms": round3(s.gen_ms as f32),
            "rtf": round3((s.gen_ms / 1000.0 / s.duration as f64) as f32),
        }));
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "text": text, "sid": sid,
            "voice": voice_core::engine::KOKORO_VOICES.get(sid as usize),
            "sample_rate": tts.sample_rate(),
            "files": out,
            "session": session.id,
            "prefix": prefix,
        }))?
    );

    if do_play {
        if let Some(first) = out.first() {
            if let Some(p) = first["path"].as_str() {
                play::play_wav(p)?;
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ lesson
/// 用 LRC 真值核对：整体 accuracy + 逐行文本/起始时间偏差
fn lesson(
    cfg: &Config,
    audio_path: &str,
    lrc_path: &str,
    lang: &str,
    verbose: bool,
    write_lrc: Option<&str>,
) -> Result<()> {
    use voice_core::assess::lrc;
    let lrc_text = std::fs::read_to_string(lrc_path)
        .with_context(|| format!("读 LRC 失败: {lrc_path}"))?;
    let decoded = audio::load_16k(audio_path)?;
    let duration = decoded.samples.len() as f32 / decoded.sample_rate as f32;
    let lines = lrc::parse(&lrc_text, duration);
    if lines.is_empty() {
        bail!("LRC 里没有可用的时间轴行");
    }
    let reference = lrc::full_reference(&lines);

    let engines = Engines::load(cfg.clone())?;
    let a = voice_core::assess::assess(
        &engines,
        voice_core::AssessRequest {
            samples: &decoded.samples,
            sample_rate: decoded.sample_rate,
            lang,
            engine: None,
            reference: Some(&reference),
            source_sr: Some(decoded.info.source_sr),
        },
    )?;
    let td = a.text_diff.as_ref();

    // 逐行：用全局对齐把「参考词 → 识别时间」映射出来，避免时间窗启发式
    use voice_core::assess::normalize;
    let mut line_rows = Vec::new();
    let mut ref_cursor = 0usize;
    let mut line_ok = 0usize;
    let mut matched_sim: Vec<f32> = Vec::new();

    for l in &lines {
        let ref_words = normalize::words(&l.en);
        let start = ref_cursor;
        let end = start + ref_words.len();
        ref_cursor = end;

        // 该行参考词在全局对齐里对应的识别词与时间
        let mut said: Vec<String> = Vec::new();
        let mut times: Vec<f32> = Vec::new();
        if let Some(td) = td {
            for p in &td.alignment {
                let Some(ri) = p.ref_index else { continue };
                if ri < start || ri >= end {
                    continue;
                }
                if let Some(s) = &p.said {
                    said.push(s.clone());
                    times.push(p.t0);
                }
            }
        }
        let said_text = said.join(" ");
        let sim = voice_core::assess::diff::similarity(
            &ref_words.join(" "),
            &normalize::words(&said_text).join(" "),
        );
        matched_sim.push(sim);
        if sim >= 0.8 {
            line_ok += 1;
        }
        // 行内第一个匹配到的时间 vs LRC 行起点
        let start_offset = times.first().map(|t| t - l.t0);
        line_rows.push(serde_json::json!({
            "t0": l.t0,
            "sim": (sim * 100.0).round() / 100.0,
            "start_offset": start_offset.map(|v| (v * 100.0).round() / 100.0),
            "ref": l.en,
            "said": said_text,
        }));
    }

    let offsets: Vec<f32> = line_rows
        .iter()
        .filter_map(|r| r["start_offset"].as_f64())
        .map(|v| v as f32)
        .collect();
    let mean_off = if offsets.is_empty() { 0.0 } else { offsets.iter().sum::<f32>() / offsets.len() as f32 };
    let max_off = offsets.iter().fold(0.0f32, |a, b| a.max(b.abs()));
    let mean_sim = if matched_sim.is_empty() { 0.0 } else { matched_sim.iter().sum::<f32>() / matched_sim.len() as f32 };

    // ---- 线性拟合 t_hyp = a * t_lrc + b：判断 LRC 是否属于这条音轨 ----
    let fit = linear_fit(&line_rows);
    let mut retimed = serde_json::Value::Null;
    if let Some(out) = write_lrc {
        if let Some((a, b)) = fit {
            let mut buf = String::new();
            for l in &lines {
                let t = (a * l.t0 + b).max(0.0);
                let mm = (t / 60.0).floor() as u32;
                let ss = t - mm as f32 * 60.0;
                let body = if l.zh.is_empty() {
                    l.en.clone()
                } else {
                    format!("{} | {}", l.en, l.zh)
                };
                buf.push_str(&format!("[{mm:02}:{ss:05.2}]{body}
"));
            }
            std::fs::write(out, buf)?;
            retimed = serde_json::json!(out);
        }
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "audio": audio_path,
            "duration": (duration * 100.0).round() / 100.0,
            "source_sr": decoded.info.source_sr,
            "band_limited": decoded.info.band_limited,
            "asr": { "engine": a.transcript.engine, "infer_ms": (a.transcript.infer_ms * 10.0).round() / 10.0 },
            "overall": {
                "accuracy": td.map(|d| (d.accuracy * 1000.0).round() / 1000.0),
                "issues": td.map(|d| d.issues.len()),
                "alignment": td.map(|d| format!("{:?}", d.timestamp_alignment).to_lowercase()),
            },
            "lines": { "total": lines.len(), "matched_80pct": line_ok,
                       "mean_line_sim": voice_core::round_to(mean_sim, 2),
                       "mean_start_offset": voice_core::round_to(mean_off, 2),
                       "max_abs_start_offset": voice_core::round_to(max_off, 2) },
            "lrc_fit": fit.map(|(a, b)| serde_json::json!({
                "scale": voice_core::round_to(a, 3),
                "offset_sec": voice_core::round_to(b, 2),
                "consistent": (a - 1.0).abs() < 0.02 && b.abs() < 0.6,
                "hint": if (a - 1.0).abs() < 0.02 && b.abs() < 0.6 {
                    "LRC 与本音轨一致".to_string()
                } else {
                    format!("LRC 与本音轨不一致：行时间整体差 {:.0}%，可能来自别的剪辑版本；可用 --write-lrc 重定时", (a - 1.0) * 100.0)
                },
            })),
            "retimed_lrc": retimed,
            "detail": if verbose { serde_json::json!(line_rows) } else { serde_json::json!([]) },
        }))?
    );
    Ok(())
}

/// 最小二乘拟合 t_hyp = a * t_lrc + b（用每行起点与首个匹配识别词的时间）
fn linear_fit(rows: &[serde_json::Value]) -> Option<(f32, f32)> {
    let pts: Vec<(f64, f64)> = rows
        .iter()
        .filter_map(|r| {
            let t = r["t0"].as_f64()?;
            let off = r["start_offset"].as_f64()?;
            Some((t, t + off))
        })
        .collect();
    if pts.len() < 2 {
        return None;
    }
    let n = pts.len() as f64;
    let sx: f64 = pts.iter().map(|p| p.0).sum();
    let sy: f64 = pts.iter().map(|p| p.1).sum();
    let sxx: f64 = pts.iter().map(|p| p.0 * p.0).sum();
    let sxy: f64 = pts.iter().map(|p| p.0 * p.1).sum();
    let den = n * sxx - sx * sx;
    if den.abs() < 1e-9 {
        return None;
    }
    let a = (n * sxy - sx * sy) / den;
    let b = (sy - a * sx) / n;
    Some((a as f32, b as f32))
}

// ------------------------------------------------------------------ cases
fn run_cases(cfg: &Config, dir: &str, only: Option<&str>) -> Result<()> {
    use voice_core::assess::cases;
    let all = cases::load_dir(dir)?;
    if all.is_empty() {
        bail!("用例目录里没有 .json: {dir}");
    }
    let selected: Vec<_> = all
        .into_iter()
        .filter(|c| only.map(|o| c.id.contains(o)).unwrap_or(true))
        .collect();

    let engines = Engines::load(cfg.clone())?;
    let store = Store::open(&cfg.data_dir)?;
    let (mut pass, mut fail) = (0usize, 0usize);
    let mut failures = Vec::new();

    for case in &selected {
        let path = cases::resolve_audio(&case.audio);
        if !path.is_file() {
            println!("[SKIP] {} 音频不存在: {}", case.id, path.display());
            continue;
        }
        let decoded = audio::load_16k(&path)?;
        // 参考文本：优先 reference_file
        let reference: Option<String> = match (&case.reference_file, &case.reference) {
            (Some(f), _) => Some(std::fs::read_to_string(cases::resolve_audio(f))?),
            (None, Some(t)) => Some(t.clone()),
            _ => None,
        };
        let a = voice_core::assess::assess(
            &engines,
            voice_core::AssessRequest {
                samples: &decoded.samples,
                sample_rate: decoded.sample_rate,
                lang: &case.lang,
                engine: case.engine.as_deref(),
                reference: reference.as_deref(),
                source_sr: Some(decoded.info.source_sr),
            },
        )?;
        // 留证：用例结果也落盘，便于事后对比
        let session = store.new_session("case")?;
        let seq = session.next_seq()?;
        let prefix = session.take_prefix(seq, &case.id);
        session.write_json(&prefix, "metrics", &a)?;

        let r = cases::check(case, &a);
        if r.passed {
            pass += 1;
            println!("[PASS] {}", r.id);
            println!("       {}", r.summary);
        } else {
            fail += 1;
            println!("[FAIL] {}", r.id);
            println!("       {}", r.summary);
            for f in &r.failures {
                println!("         ✗ {f}");
            }
            failures.push(r.id.clone());
        }
    }

    println!("
{} 个用例: {pass} 通过, {fail} 失败", pass + fail);
    if fail > 0 {
        bail!("失败用例: {}", failures.join(", "));
    }
    Ok(())
}

// ------------------------------------------------------------------ mic
fn mic(cfg: &Config, secs: &f32) -> Result<()> {
    println!("== 输入设备能力");
    let report = capture::device_report();
    for d in report["devices"].as_array().cloned().unwrap_or_default() {
        println!("  - {}", d["name"].as_str().unwrap_or("?"));
        if let Some(s) = d["supported"].as_array() {
            for f in s {
                println!(
                    "      {:>4} ch {}  {}..{} Hz",
                    f["channels"], f["format"].as_str().unwrap_or("?"),
                    f["min_rate"], f["max_rate"]
                );
            }
        }
    }
    println!("
  默认: {}", report["default"].as_str().unwrap_or("(无)"));
    println!("  配置 mic: {}", cfg.mic.as_deref().unwrap_or("(未指定，用默认)"));
    println!("
== 底噪测量（保持安静 {secs}s）");
    let r = capture::measure_noise_with(cfg.mic.as_deref(), *secs)?;
    println!("{}", serde_json::to_string_pretty(&r)?);
    if r.narrowband {
        println!(
            "
⚠️  该设备采样率 {} Hz ≤16 kHz：蓝牙耳机（HFP）常见，摩擦音细节受限。
   纠音训练建议改用 USB / 有线麦克风（48 kHz）。",
            r.device_sr
        );
    }
    Ok(())
}

// ------------------------------------------------------------------ rec
fn rec(
    cfg: &Config,
    max_sec: f32,
    silence: f32,
    start_timeout: f32,
    then_stt: bool,
    lang: &str,
    scene: &str,
) -> Result<()> {
    let store = Store::open(&cfg.data_dir)?;
    let session = store.new_session("drill")?;

    eprintln!("[voice] 开始录音（上限 {max_sec}s，静音 {silence}s 自动停止，等说话 {start_timeout}s）… 请说话");
    let opts = capture::RecordOptions {
        max_sec,
        silence_sec: silence,
        prefer_sr: cfg.capture_sr,
        start_timeout_sec: start_timeout,
        ..Default::default()
    };
    let cap = capture::record_with(cfg.mic.as_deref(), &opts)?;
    eprintln!(
        "[voice] 采集完成 {:.2}s @{} Hz（设备 {} Hz{}）",
        cap.duration,
        cap.sample_rate,
        cap.device_sr,
        if cap.auto_stopped { "，静音自动停止" } else { "" }
    );

    // 用 VAD 精修首尾静音
    let engines = Engines::load(cfg.clone())?;
    let (trimmed, offset) = engines.vad.trim_silence(&cap.samples, 0.15);

    let seq = session.next_seq()?;
    let prefix = session.take_prefix(seq, scene);
    // 原始证据：设备采样率原样落盘（48k 麦克风就是 48k），16k 是可重建的派生
    let raw_path = session.dir.join("takes").join(format!("{prefix}.raw{}.wav", cap.device_sr));
    voice_core::audio::wav::write_wav_i16(&raw_path, &cap.raw, cap.device_sr)?;
    let norm = session.write_norm_wav(&prefix, &trimmed, voice_core::INTERNAL_SR)?;
    session.write_json(
        &prefix,
        "capture",
        &serde_json::json!({
            "device": cap.device_name, "device_sr": cap.device_sr,
            "auto_stopped": cap.auto_stopped, "duration": cap.duration,
            "silence_sec": silence, "max_sec": max_sec,
            "trim_offset": offset, "envelope_db": cap.envelope_db,
        }),
    )?;

    let mut t = None;
    if then_stt {
        let tr = engines.transcribe_long(&trimmed, voice_core::INTERNAL_SR, lang, None)?;
        session.write_json(&prefix, "asr", &tr)?;
        t = Some(tr);
    }

    let entry = ManifestEntry {
        ts: chrono::Local::now().to_rfc3339(),
        session: session.id.clone(),
        seq,
        scene: scene.to_string(),
        src: raw_path.to_string_lossy().to_string(),
        norm_wav: Some(norm.to_string_lossy().to_string()),
        lang: t.as_ref().map(|x| x.lang.clone()),
        engine: t.as_ref().map(|x| x.engine.clone()),
        dur: Some(trimmed.len() as f32 / voice_core::INTERNAL_SR as f32),
        text: t.as_ref().map(|x| x.text.clone()),
        degraded: t.as_ref().map(|x| x.degraded),
        feedback_ref: None,
    };
    voice_core::store::finish_take(&store, entry)?;

    match t {
        Some(tr) => print_transcript(
            &tr,
            false,
            serde_json::json!({ "wav": norm, "src": raw_path, "device_sr": cap.device_sr }),
        ),
        None => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "wav": norm, "src": raw_path, "device_sr": cap.device_sr,
                "auto_stopped": cap.auto_stopped, "session": session.id
            }))?
        ),
    }
    Ok(())
}

// ------------------------------------------------------------------ assess
fn assess_cmd(
    cfg: &Config,
    path: &str,
    reference: Option<&str>,
    lang: &str,
    engine: Option<&str>,
    scene: &str,
) -> Result<()> {
    let store = Store::open(&cfg.data_dir)?;
    let src = resolve_path(&store, path)?;
    // 与 MCP assess 一致：源在 sessions/ 内就地写回，不复制、不新建会话
    let (session, prefix, orig, feed, pre_norm) = match store.session_for_take(&src) {
        Some((s, pre)) => {
            // 喂原始文件（保留 source_sr/band_limited 语义）；.16k.wav 只当派生证据
            let p16 = s.dir.join("takes").join(format!("{pre}.16k.wav"));
            let pre_norm = if p16.is_file() { Some(p16) } else { None };
            (s, pre, src.clone(), src.clone(), pre_norm)
        }
        None => {
            let s = store.new_session("assess")?;
            let seq = s.next_seq()?;
            let pre = s.take_prefix(seq, scene);
            let orig = s.import_original(&src, &pre)?;
            (s, pre, orig.clone(), orig, None)
        }
    };
    let seq = prefix
        .split('_')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);
    let decoded = audio::load_16k(&feed)?;
    let norm = match pre_norm {
        Some(p) => p,
        None => session.write_norm_wav(&prefix, &decoded.samples, decoded.sample_rate)?,
    };

    let engines = Engines::load(cfg.clone())?;
    let a = voice_core::assess::assess(
        &engines,
        voice_core::AssessRequest {
            samples: &decoded.samples,
            sample_rate: decoded.sample_rate,
            lang,
            engine,
            reference,
            source_sr: Some(decoded.info.source_sr),
        },
    )?;

    let metrics_path = session.dir.join("takes").join(format!("{prefix}.metrics.json"));
    if metrics_path.is_file() {
        let _ = std::fs::copy(
            &metrics_path,
            session.dir.join("takes").join(format!("{prefix}.metrics.prev.json")),
        );
    }
    session.write_json(&prefix, "metrics", &a)?;
    session.write_json(&prefix, "asr", &a.transcript)?;
    // 反馈骨架（与 MCP assess 一致，保证“5 件套”在任何入口都成立）
    let feedback = session.dir.join("takes").join(format!("{prefix}.md"));
    if !feedback.is_file() {
        std::fs::write(
            &feedback,
            format!(
                "# 反馈 · {prefix}\n\n<!-- Agent 在这里写本轮反馈；结构：问题 / 怎么改 / 示范 / 再来一次 -->\n\n- 参考文本：{}\n- 识别结果：{}\n",
                reference.unwrap_or("(未提供，仅语流分析)"),
                a.transcript.text
            ),
        )?;
    }
    voice_core::store::finish_take(
        &store,
        ManifestEntry {
            ts: chrono::Local::now().to_rfc3339(),
            session: session.id.clone(),
            seq,
            scene: scene.to_string(),
            src: orig.to_string_lossy().to_string(),
            norm_wav: Some(norm.to_string_lossy().to_string()),
            lang: Some(a.transcript.lang.clone()),
            engine: Some(a.transcript.engine.clone()),
            dur: Some(a.transcript.duration),
            text: Some(a.transcript.text.clone()),
            degraded: Some(a.degraded),
            feedback_ref: Some(feedback.to_string_lossy().to_string()),
        },
    )?;

    println!("{}", serde_json::to_string_pretty(&a)?);
    Ok(())
}

// ------------------------------------------------------------------ watch
const AUDIO_EXTS: [&str; 6] = ["m4a", "wav", "mp3", "flac", "ogg", "aac"];

fn watch_state_path(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("watch_state.json")
}

fn load_seen(cfg: &Config) -> std::collections::BTreeMap<String, u64> {
    let p = watch_state_path(cfg);
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_seen(cfg: &Config, seen: &std::collections::BTreeMap<String, u64>) {
    if let Ok(s) = serde_json::to_string_pretty(seen) {
        let _ = std::fs::write(watch_state_path(cfg), s);
    }
}

fn watch(cfg: &Config, dir: Option<&str>, lang: &str) -> Result<()> {
    let dirs: Vec<PathBuf> = match dir {
        Some(d) => vec![PathBuf::from(d)],
        None => cfg.watch_dirs.iter().filter(|d| d.is_dir()).cloned().collect(),
    };
    if dirs.is_empty() {
        bail!("没有可监视的目录（用 --dir 指定）");
    }
    let store = Store::open(&cfg.data_dir)?;
    let mut seen = load_seen(cfg);

    // 首次运行：把已存在的文件全部登记为基线，避免历史文件被当成新录音
    if seen.is_empty() {
        for d in &dirs {
            for entry in std::fs::read_dir(d)?.filter_map(|e| e.ok()) {
                let p = entry.path();
                if is_audio(&p) {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    seen.insert(p.to_string_lossy().to_string(), size);
                }
            }
        }
        save_seen(cfg, &seen);
        println!("[watch] 基线登记 {} 个已存在文件（不会被导入）", seen.len());
    }

    println!("[watch] 监视中（Ctrl+C 退出）:");
    for d in &dirs {
        println!("        {}", d.display());
    }

    let mut engines: Option<Engines> = None;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(700));
        for d in &dirs {
            let Ok(rd) = std::fs::read_dir(d) else { continue };
            let mut candidates: Vec<(PathBuf, u64)> = Vec::new();
            for entry in rd.filter_map(|e| e.ok()) {
                let p = entry.path();
                if !is_audio(&p) {
                    continue;
                }
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let key = p.to_string_lossy().to_string();
                match seen.get(&key) {
                    // 之前记录过同样大小 → 已处理
                    Some(&s0) if s0 == size => continue,
                    // 大小还在变化 → 等下一轮
                    Some(_) => continue,
                    None => candidates.push((p, size)),
                }
            }
            for (path, size) in candidates {
                // 再等一轮确认写盘稳定
                std::thread::sleep(std::time::Duration::from_millis(800));
                let size2 = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                if size2 != size || size2 == 0 {
                    continue;
                }
                match import_and_transcribe(cfg, &store, &mut engines, &path, lang) {
                    Ok(txt) => println!("[watch] {} -> {}", path.file_name().unwrap_or_default().to_string_lossy(), txt),
                    Err(e) => eprintln!("[watch] {} 处理失败: {e:#}", path.display()),
                }
                seen.insert(path.to_string_lossy().to_string(), size2);
                save_seen(cfg, &seen);
            }
        }
    }
}

fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn import_and_transcribe(
    cfg: &Config,
    store: &Store,
    engines: &mut Option<Engines>,
    src: &Path,
    lang: &str,
) -> Result<String> {
    let session = store.new_session("watch")?;
    let seq = session.next_seq()?;
    let scene = src.file_stem().and_then(|s| s.to_str()).unwrap_or("watch");
    let prefix = session.take_prefix(seq, scene);
    let orig = session.import_original(src, &prefix)?;
    let decoded = audio::load_16k(&orig)?;
    let norm = session.write_norm_wav(&prefix, &decoded.samples, decoded.sample_rate)?;

    if engines.is_none() {
        *engines = Some(Engines::load(cfg.clone())?);
    }
    let e = engines.as_ref().unwrap();
    let tr = e.transcribe_long(&decoded.samples, decoded.sample_rate, lang, None)?;
    let (trimmed, _) = e.vad.trim_silence(&decoded.samples, 0.15);
    let _ = trimmed;

    session.write_json(&prefix, "asr", &tr)?;
    voice_core::store::finish_take(
        store,
        ManifestEntry {
            ts: chrono::Local::now().to_rfc3339(),
            session: session.id.clone(),
            seq,
            scene: scene.to_string(),
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
    Ok(tr.text)
}

// ------------------------------------------------------------------ tts-probe
fn tts_probe(cfg: &Config, text: &str) -> Result<()> {
    let out = cfg.data_dir.join("probe").join("voices");
    std::fs::create_dir_all(&out)?;
    let engines = Engines::load(cfg.clone())?;
    let tts = &engines.tts;
    println!("为 {} 个音色生成样本 -> {}", tts.num_speakers(), out.display());
    for v in tts.voices() {
        let s = tts.synthesize(text, v.sid, 1.0)?;
        let p = out.join(format!("{:02}_{}_{}.wav", v.sid, v.lang, v.name));
        voice_core::audio::wav::write_wav_i16(&p, &s.samples, s.sample_rate)?;
        println!("  {:<20} {}", v.name, p.file_name().unwrap_or_default().to_string_lossy());
    }
    println!("完成。逐个试听即可校准 音色名 ↔ sid 映射（当前表来自 hexgrad 官方 voices 字典序）");
    Ok(())
}

// 未使用但保留：后续 watch/serve 会用到
#[allow(dead_code)]
fn shared_engines(cfg: &Config) -> Result<Arc<Mutex<Engines>>> {
    Ok(Arc::new(Mutex::new(Engines::load(cfg.clone())?)))
}

#[allow(dead_code)]
fn ensure_parent(p: &Path) -> Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ bank (ADR-0009)
/// f32 → 干净的 f64（去掉 float 噪声，JSON 里不出现 96.80000305175781）
fn f1(v: f32) -> f64 {
    (v as f64 * 10.0).round() / 10.0
}
fn f2(v: f32) -> f64 {
    (v as f64 * 100.0).round() / 100.0
}
fn f3(v: f32) -> f64 {
    (v as f64 * 1000.0).round() / 1000.0
}

fn bank_ingest(
    cfg: &Config,
    from_dir: Option<&str>,
    book: &str,
    limit: Option<usize>,
    dry_run: bool,
    overwrite: bool,
) -> Result<()> {
    let Some(dir) = from_dir else {
        bail!(
            "目前只支持 --from-dir（网络拉取请用外部脚本准备好 <NN>.<Title>.mp3 + .lrc，见 ADR-0009 D2）"
        )
    };
    let home = Path::new(&cfg.data_dir);
    let engines = Engines::load(cfg.clone())?;
    let opts = voice_core::bank::IngestOptions {
        book: book.to_string(),
        overwrite,
        dry_run,
    };
    let out = voice_core::bank::ingest_dir(home, Path::new(dir), &opts, &engines, limit)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "lessons": out.len(),
            "kept": out.iter().map(|r| r.kept).sum::<usize>(),
            "rejected": out.iter().map(|r| r.rejected).sum::<usize>(),
            "dry_run": dry_run,
            "detail": out.iter().map(|r| serde_json::json!({
                "lesson": r.lesson_slug, "kept": r.kept, "rejected": r.rejected
            })).collect::<Vec<_>>(),
        }))?
    );
    Ok(())
}

fn bank_list(
    cfg: &Config,
    level: Option<u8>,
    tag: Option<&str>,
    book: Option<&str>,
    limit: usize,
) -> Result<()> {
    let home = Path::new(&cfg.data_dir);
    let mut idx = voice_core::bank::read_index(home)?;
    idx.retain(|e| level.map(|l| e.level == l).unwrap_or(true));
    idx.retain(|e| book.map(|b| e.book.eq_ignore_ascii_case(b)).unwrap_or(true));
    if let Some(t) = tag {
        idx.retain(|e| e.tags.iter().any(|x| x == t));
    }
    idx.truncate(limit);
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "count": idx.len(),
            "items": idx.iter().map(|e| serde_json::json!({
                "id": e.id, "level": e.level, "wpm": f1(e.wpm), "words": e.words,
                "text": e.text, "zh": e.zh, "tags": e.tags,
            })).collect::<Vec<_>>(),
        }))?
    );
    Ok(())
}

fn bank_show(cfg: &Config, id: &str) -> Result<()> {
    let home = Path::new(&cfg.data_dir);
    let idx = voice_core::bank::read_index(home)?;
    let Some(e) = idx.iter().find(|e| e.id == id) else {
        bail!("素材库里没有 id={id}（先跑 bank list 看看）");
    };
    let src = home.join(&e.src);
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "id": e.id, "book": e.book, "lesson": e.lesson, "seq": e.seq,
            "text": e.text, "zh": e.zh,
            "t0": f2(e.t0), "t1": f2(e.t1), "dur": f2(e.dur), "words": e.words, "wpm": f1(e.wpm), "level": e.level,
            "tags": e.tags,
            "quality": {
                "line_sim": f3(e.quality.line_sim),
                "asr": e.quality.asr,
                "band_limited": e.quality.band_limited,
                "lrc_fit_scale": f3(e.quality.lrc_fit_scale),
                "speech_ratio": f3(e.quality.speech_ratio),
            },
            "audio": e.src, "audio_exists": src.is_file(),
        }))?
    );
    Ok(())
}

fn bank_drill(cfg: &Config, id: &str, quiet: bool) -> Result<()> {
    let home = Path::new(&cfg.data_dir);
    let idx = voice_core::bank::read_index(home)?;
    let Some(e) = idx.iter().find(|e| e.id == id) else {
        bail!("素材库里没有 id={id}");
    };
    let src = home.join(&e.src);
    if !src.is_file() {
        bail!("素材音频缺失: {}", src.display());
    }
    println!("{}", e.text);
    if !e.zh.is_empty() {
        println!("{}", e.zh);
    }
    println!(
        "[{}] level {} · {} 词 · {:.0} wpm · {:.2}s · tags: {}",
        e.id,
        e.level,
        e.words,
        e.wpm,
        e.dur,
        if e.tags.is_empty() { "-".to_string() } else { e.tags.join(",") }
    );
    if !quiet {
        play::play_wav(&src)?;
    }
    Ok(())
}

fn bank_stats(cfg: &Config) -> Result<()> {
    let home = Path::new(&cfg.data_dir);
    let s = voice_core::bank::stats(home)?;
    println!("{}", serde_json::to_string_pretty(&s)?);
    Ok(())
}
