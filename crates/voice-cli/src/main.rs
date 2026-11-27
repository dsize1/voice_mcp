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
        /// 显式倍率（高级用法）；不传则按目标 140 wpm 自动校准
        #[arg(long)]
        speed: Option<f32>,
        /// 同时生成 110 wpm 慢速 + 140 wpm 常速一对
        #[arg(long)]
        slow_too: bool,
        /// 生成后立即播放
        #[arg(long)]
        play: bool,
        #[arg(long, default_value = "tts")]
        scene: String,
        /// 放行中英混说（分段合成后拼接；音质降级，仅内部实验用）
        #[arg(long)]
        allow_mixed: bool,
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
        /// 同时跑音素级 GOP（需 --reference）
        #[arg(long)]
        with_gop: bool,
        /// 同时跑韵律分析（F0 轮廓 / 句末升降 / 平调 / 重音候选）
        #[arg(long)]
        with_prosody: bool,
        /// 参考音（示范音）路径；提供时自动开启韵律并做 F0 形状 DTW 对比
        #[arg(long)]
        reference_audio: Option<String>,
        /// 素材库 id：自动填参考文本；`ref_audio=native` 时用**真人原声切片**当 GOP 基线
        /// 与韵律参考（不再合成 TTS）
        #[arg(long)]
        bank_id: Option<String>,
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
    /// 音素级 GOP：给一段录音 + 参考文本，输出每个音素的偏离分数
    Gop {
        /// 音频路径或 "last"
        path: String,
        /// 参考文本
        #[arg(long)]
        text: Option<String>,
        /// 参考文本文件（长句用这个）
        #[arg(long)]
        text_file: Option<String>,
        /// 只看最可疑的 N 个音素
        #[arg(long, default_value_t = 10)]
        top: usize,
        /// 模型目录（默认 C:/voice_models/phoneme-ctc）
        #[arg(long)]
        model: Option<String>,
        /// CMUdict 路径（默认 C:/voice_models/g2p/cmudict.dict）
        #[arg(long)]
        g2p: Option<String>,
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
    /// 只读审计现有 TTS wav：双 ASR + OOV + WPM + 音素（不修改文件）
    TtsAudit {
        /// JSONL 清单：{"id","path","text","kind","voice","speed"}
        #[arg(long)]
        manifest: String,
        /// 输出 report.jsonl
        #[arg(long)]
        out: String,
        /// 跳过音素检查（更快）
        #[arg(long)]
        no_phones: bool,
    },
    /// TTS 参考音验证：合成 N 次，双 ASR 回读 + 音素检查 + WPM
    TtsVerify {
        /// 待验证的文本（英文；中文不支持 OOV/ASR 严格校验）
        text: String,
        /// 音色，可重复；默认 am_michael
        #[arg(long = "voice")]
        voices: Vec<String>,
        /// 每个音色重复几次（默认 3，要求 3/3）
        #[arg(long, default_value_t = 3)]
        runs: usize,
        /// 目标 wpm（默认 config.tts_wpm / 140）
        #[arg(long)]
        wpm: Option<f32>,
        /// 跳过音素级检查（更快，只做 ASR + WPM）
        #[arg(long)]
        no_phones: bool,
    },
    /// 漏词回归门禁：语料逐句合成 → 双 ASR 回读 → **逐词必须一个不少**
    ///
    /// 防的是这一类事故：前端闭集/词表缺失导致某词被静默跳过，
    /// 音频听起来“有点怪”但没人发现（2026-11-27 `client's` / `Where's`）。
    /// 任何一句出现漏词或 G2P 未覆盖 → 退出码 2（可直接进 CI）。
    TtsRegress {
        /// JSONL 语料：每行 {"id": "...", "text": "..."}
        corpus: String,
        /// 目标 wpm（默认 config.tts_wpm / 140）
        #[arg(long)]
        wpm: Option<f32>,
        /// 跳过音素级检查（更快）
        #[arg(long)]
        no_phones: bool,
        /// 逐句报告输出路径（JSONL）
        #[arg(long)]
        out: Option<String>,
        /// 固定倍率（隔离“语速标定”这个变量；漏词门禁建议用固定倍率跑）
        #[arg(long)]
        speed: Option<f32>,
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
        corpus: Option<String>,
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
    /// 按 learner.json 的 drill_queue / error_stats 生成定向训练计划（原定路线 P5）
    Plan {
        /// learner.json 路径（默认 $VOICE_HOME/learner.json）
        #[arg(long)]
        learner: Option<String>,
        /// 时间预算（分钟）
        #[arg(long, default_value_t = 15.0)]
        minutes: f32,
        /// 最多几项
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// 覆盖难度（默认按 band 推断）
        #[arg(long)]
        level: Option<u8>,
        /// 可复现洗牌
        #[arg(long)]
        seed: Option<u64>,
        #[arg(long)]
        corpus: Option<String>,
        /// 排除某个语料集合，可重复（不指定 --corpus 时默认排除 peppa）
        #[arg(long = "exclude-corpus")]
        exclude_corpus: Vec<String>,
        /// 雅思 Part（自由说题面优先匹配）
        #[arg(long)]
        part: Option<u8>,
        /// 只打印，不写会话目录
        #[arg(long)]
        dry_run: bool,
        /// 输出 JSON（默认 Markdown）
        #[arg(long)]
        json: bool,
        /// 强制包含 60 s 自由说槽位（默认按 learner.readiness 决定）
        #[arg(long, conflicts_with = "no_free_speech")]
        free_speech: bool,
        /// 强制省略自由说槽位
        #[arg(long, conflicts_with = "free_speech")]
        no_free_speech: bool,
    },
    /// 统计：条数/时长/难度分布/标签排行/quarantine 数
    Stats,
    /// 按 focus 取材：给 Skill 用（默认排除导语/片段）
    Pick {
        /// focus 标签，可重复：--tag theta_to_s --tag final_consonant_drop
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        level: Option<u8>,
        #[arg(long)]
        book: Option<String>,
        /// 语料集合：nce | ielts | custom
        #[arg(long)]
        corpus: Option<String>,
        /// 雅思 Part（1/2/3）
        #[arg(long)]
        part: Option<u8>,
        /// 雅思话题（匹配 exam.topics）
        #[arg(long)]
        topic: Option<String>,
        /// read_aloud | prompt
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 5)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// 可复现洗牌
        #[arg(long)]
        seed: Option<u64>,
        /// 按 tag 实测权重排序（默认开）；关掉则退回“命中几个 tag”
        #[arg(long)]
        no_weighted: bool,
        /// 连导语/片段一起给（默认排除）
        #[arg(long)]
        allow_noise: bool,
        /// 排除某个语料集合，可重复（默认排除 peppa 这类非训练素材）
        #[arg(long = "exclude-corpus")]
        exclude_corpus: Vec<String>,
    },
    /// 单文件真人音频入库（无 LRC）：自动切句 + 双引擎验证文本 + 质量门槛
    IngestAudio {
        /// 音频路径（mp3/m4a/wav/flac）
        audio: String,
        /// 语料集合（默认 ielts）
        #[arg(long, default_value = "ielts")]
        corpus: String,
        /// 来源标签（默认 IELTS）
        #[arg(long, default_value = "IELTS")]
        book: String,
        /// 课/段标签（默认取文件名）
        #[arg(long)]
        lesson: Option<String>,
        /// 双引擎词级一致率门槛（默认 0.7）
        #[arg(long, default_value_t = 0.7)]
        min_agree: f32,
        /// 单句最长秒数（超过硬切，默认 12）
        #[arg(long, default_value_t = 12.0)]
        max_sec: f32,
        /// 只处理这段区间（秒）：只想取某一集里的某个片段时用
        #[arg(long, default_value_t = 0.0)]
        start: f32,
        #[arg(long)]
        end: Option<f32>,
        /// 最多入库几句
        #[arg(long)]
        max_chunks: Option<usize>,
        /// 字幕文件（SRT/VTT）：给了就用字幕文本当答案键（text_source=srt）
        #[arg(long)]
        srt: Option<String>,
        /// 只算，不写文件（会打印逐句预览）
        #[arg(long)]
        dry_run: bool,
    },
    /// 把索引升级到当前 schema（补 corpus/kind/ref_audio）
    Migrate,
    /// 导入只有题面的素材（雅思口语题库 JSON 数组）
    Import { file: String },
    /// 重算全部素材的 tags（纯文本规则，不动音频）
    Retag,
    /// 批量生成示范音（TTS）并逐条接入 `tts-verify` 验证（D51 / N11）
    Refs {
        /// 指定素材 id（可重复）；与 --kind/--corpus 一起用时取交集
        ids: Vec<String>,
        /// 只取某一类：read_aloud | prompt
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        corpus: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        /// 音色，可重复；按顺序回退（默认 am_michael → am_fenrir）
        #[arg(long = "voice")]
        voices: Vec<String>,
        /// 目标语速（默认 config.tts_wpm / 140）
        #[arg(long)]
        wpm: Option<f32>,
        /// 每条重复验证几次（3/3 才算 verified）
        #[arg(long, default_value_t = 3)]
        runs: usize,
        /// 同时生成 110 wpm 慢速版
        #[arg(long)]
        slow_too: bool,
        /// 跳过音素级检查（更快）
        #[arg(long)]
        no_phones: bool,
        /// 输出目录（写 wav + metadata.json + report.jsonl）
        #[arg(long)]
        out: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut cfg = Config::load()?;
    match cli.cmd {
        Cmd::Doctor { load } => doctor(&cfg, load),
        Cmd::Stt {
            path,
            lang,
            engine,
            scene,
            plain,
        } => stt(&cfg, &path, &lang, engine.as_deref(), &scene, plain),
        Cmd::Say {
            text,
            voice,
            speed,
            slow_too,
            play,
            scene,
            allow_mixed,
        } => {
            if allow_mixed {
                // 内部实验用：放行中英混说（产品入口 default 拒答）
                cfg.allow_mixed = true;
            }
            say(&cfg, &text, voice.as_deref(), speed, slow_too, play, &scene)
        }
        Cmd::Play { path } => {
            play::play_wav(&path)?;
            Ok(())
        }
        Cmd::Lesson {
            audio: a,
            lrc,
            lang,
            verbose,
            write_lrc,
        } => lesson(&cfg, &a, &lrc, &lang, verbose, write_lrc.as_deref()),
        Cmd::Gop {
            path,
            text,
            text_file,
            top,
            model,
            g2p,
        } => gop_cmd(
            &cfg,
            &path,
            text.as_deref(),
            text_file.as_deref(),
            top,
            model.as_deref(),
            g2p.as_deref(),
        ),
        Cmd::Cases { dir, only } => run_cases(&cfg, &dir, only.as_deref()),
        Cmd::Bank { cmd } => match cmd {
            BankCmd::Ingest {
                from_dir,
                book,
                limit,
                dry_run,
                overwrite,
            } => bank_ingest(&cfg, from_dir.as_deref(), &book, limit, dry_run, overwrite),
            BankCmd::List {
                corpus,
                level,
                tag,
                book,
                limit,
            } => bank_list(
                &cfg,
                corpus.as_deref(),
                level,
                tag.as_deref(),
                book.as_deref(),
                limit,
            ),
            BankCmd::Show { id } => bank_show(&cfg, &id),
            BankCmd::Drill { id, quiet } => bank_drill(&cfg, &id, quiet),
            BankCmd::IngestAudio {
                audio,
                corpus,
                book,
                lesson,
                min_agree,
                max_sec,
                start,
                end,
                max_chunks,
                srt,
                dry_run,
            } => bank_ingest_audio(
                &cfg,
                &audio,
                &corpus,
                &book,
                lesson.as_deref(),
                min_agree,
                max_sec,
                start,
                end,
                max_chunks,
                srt.as_deref(),
                dry_run,
            ),
            BankCmd::Migrate => {
                let n = voice_core::bank::migrate(Path::new(&cfg.data_dir))?;
                println!("{}", serde_json::json!({"migrated": n}));
                Ok(())
            }
            BankCmd::Import { file } => {
                let text =
                    std::fs::read_to_string(&file).with_context(|| format!("读取 {file} 失败"))?;
                let docs: Vec<serde_json::Value> = serde_json::from_str(&text)
                    .with_context(|| format!("{file} 必须是 JSON 数组"))?;
                let (added, skipped) =
                    voice_core::bank::import_prompts(Path::new(&cfg.data_dir), &docs)?;
                println!(
                    "{}",
                    serde_json::json!({"added": added, "skipped": skipped})
                );
                Ok(())
            }
            BankCmd::Stats => bank_stats(&cfg),
            BankCmd::Plan {
                learner,
                minutes,
                limit,
                level,
                seed,
                corpus,
                exclude_corpus,
                part,
                dry_run,
                json,
                free_speech,
                no_free_speech,
            } => bank_plan(
                &cfg,
                learner.as_deref(),
                minutes,
                limit,
                level,
                seed,
                corpus,
                exclude_corpus,
                part,
                dry_run,
                json,
                if free_speech {
                    Some(true)
                } else if no_free_speech {
                    Some(false)
                } else {
                    None
                },
            ),
            BankCmd::Pick {
                tags,
                level,
                book,
                corpus,
                part,
                topic,
                kind,
                limit,
                offset,
                seed,
                allow_noise,
                no_weighted,
                exclude_corpus,
            } => bank_pick(
                &cfg,
                BankPickArgs {
                    tags,
                    level,
                    book,
                    corpus,
                    part,
                    topic,
                    kind,
                    limit,
                    offset,
                    seed,
                    allow_noise,
                    weighted: !no_weighted,
                    exclude_corpora: exclude_corpus,
                },
            ),
            BankCmd::Retag => {
                let n = voice_core::bank::retag(Path::new(&cfg.data_dir))?;
                println!("{}", serde_json::json!({"retagged": n}));
                Ok(())
            }
            BankCmd::Refs {
                ids,
                kind,
                corpus,
                limit,
                voices,
                wpm,
                runs,
                slow_too,
                no_phones,
                out,
            } => bank_refs(
                &cfg,
                BankRefsArgs {
                    ids,
                    kind,
                    corpus,
                    limit,
                    voices,
                    wpm,
                    runs,
                    slow_too,
                    no_phones,
                    out,
                },
            ),
        },
        Cmd::Mic { secs } => mic(&cfg, &secs),
        Cmd::Rec {
            max_sec,
            silence,
            start_timeout,
            stt: then_stt,
            lang,
            scene,
        } => rec(
            &cfg,
            max_sec,
            silence,
            start_timeout,
            then_stt,
            &lang,
            &scene,
        ),
        Cmd::Assess {
            path,
            reference,
            lang,
            engine,
            with_gop,
            with_prosody,
            reference_audio,
            bank_id,
            scene,
        } => assess_cmd(
            &cfg,
            &path,
            reference.as_deref(),
            &lang,
            engine.as_deref(),
            with_gop,
            with_prosody,
            reference_audio.as_deref(),
            bank_id.as_deref(),
            &scene,
        ),
        Cmd::Watch { dir, lang } => watch(&cfg, dir.as_deref(), &lang),
        Cmd::TtsProbe { text } => tts_probe(&cfg, &text),
        Cmd::TtsAudit {
            manifest,
            out,
            no_phones,
        } => tts_audit(&cfg, &manifest, &out, no_phones),
        Cmd::TtsVerify {
            text,
            voices,
            runs,
            wpm,
            no_phones,
        } => tts_verify(&cfg, &text, &voices, runs, wpm, no_phones),
        Cmd::TtsRegress {
            corpus,
            wpm,
            no_phones,
            out,
            speed,
        } => tts_regress(&cfg, &corpus, wpm, speed, no_phones, out.as_deref()),
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
            let mb = std::fs::metadata(&path)
                .map(|m| m.len() as f64 / 1e6)
                .unwrap_or(0.0);
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

    let voice_name = voice.unwrap_or(voice_core::engine::DEFAULT_VOICE);
    let sid = tts.resolve_sid(voice_name);

    let seq = session.next_seq()?;
    let prefix = session.take_prefix(seq, scene);

    let mut out = Vec::new();
    let targets = voice_core::engine::demo_targets(slow_too, speed, cfg.tts_wpm);
    for (label, target) in &targets {
        let s = match target {
            voice_core::engine::SpeechTarget::Wpm(w) => tts.synthesize_at_wpm(text, sid, *w)?,
            voice_core::engine::SpeechTarget::Speed(sp) => tts.synthesize(text, sid, *sp)?,
        };
        let p = session.write_reference(
            &prefix,
            label,
            &s.samples,
            s.sample_rate,
            text,
            &s.voice,
            s.speed,
            s.wpm,
            s.target_wpm,
        )?;
        out.push(serde_json::json!({
            "label": label, "speed": round3(s.speed), "path": p,
            "wpm": round3(s.wpm), "target_wpm": s.target_wpm,
            "speech_duration": round3(s.speech_duration),
            // Some(...) = 起始“多出一个音”的伪影（实测 af_heart 会多读成 “And”）
            "onset_gap_ms": s.onset_gap_ms.map(round3),
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

    if do_play
        && let Some(first) = out.first()
        && let Some(p) = first["path"].as_str()
    {
        play::play_wav(p)?;
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
    let lrc_text =
        std::fs::read_to_string(lrc_path).with_context(|| format!("读 LRC 失败: {lrc_path}"))?;
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
            with_gop: false,
            baseline_tts: false,
            with_prosody: false,
            baseline_audio: None,
            reference_audio: None,
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
    let mean_off = if offsets.is_empty() {
        0.0
    } else {
        offsets.iter().sum::<f32>() / offsets.len() as f32
    };
    let max_off = offsets.iter().fold(0.0f32, |a, b| a.max(b.abs()));
    let mean_sim = if matched_sim.is_empty() {
        0.0
    } else {
        matched_sim.iter().sum::<f32>() / matched_sim.len() as f32
    };

    // ---- 线性拟合 t_hyp = a * t_lrc + b：判断 LRC 是否属于这条音轨 ----
    let fit = linear_fit(&line_rows);
    let mut retimed = serde_json::Value::Null;
    if let Some(out) = write_lrc
        && let Some((a, b)) = fit
    {
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
            buf.push_str(&format!(
                "[{mm:02}:{ss:05.2}]{body}
"
            ));
        }
        std::fs::write(out, buf)?;
        retimed = serde_json::json!(out);
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
                with_gop: false,
                baseline_tts: false,
                with_prosody: false,
                baseline_audio: None,
                reference_audio: None,
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

    println!(
        "
{} 个用例: {pass} 通过, {fail} 失败",
        pass + fail
    );
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
                    f["channels"],
                    f["format"].as_str().unwrap_or("?"),
                    f["min_rate"],
                    f["max_rate"]
                );
            }
        }
    }
    println!(
        "
  默认: {}",
        report["default"].as_str().unwrap_or("(无)")
    );
    println!(
        "  配置 mic: {}",
        cfg.mic.as_deref().unwrap_or("(未指定，用默认)")
    );
    println!(
        "
== 底噪测量（保持安静 {secs}s）"
    );
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

    eprintln!(
        "[voice] 开始录音（上限 {max_sec}s，静音 {silence}s 自动停止，等说话 {start_timeout}s）… 请说话"
    );
    let opts = capture::RecordOptions {
        max_sec,
        silence_sec: silence,
        prefer_sr: cfg.capture_sr,
        start_timeout_sec: start_timeout,
        ..Default::default()
    };
    let cap = capture::record_with(cfg.mic.as_deref(), &opts)?;
    // 录音当下就给电平结论：等评测完再发现"信噪比不足"太晚了
    let snr = cap.snr();
    let peak = cap.peak_dbfs();
    eprintln!(
        "[voice] 电平：peak {:.1} dBFS，snr {:.1} dB（目标 peak −20~−12 dBFS，snr ≥ {:.0}）",
        peak,
        snr,
        voice_core::assess::SNR_MIN_DB
    );
    if snr < voice_core::assess::SNR_MIN_DB {
        eprintln!("[voice] ⚠ 信噪比不足：靠近麦克风或调高输入音量后重录");
    } else if peak < -26.0 {
        eprintln!("[voice] ⚠ 电平偏低（{peak:.0} dBFS）：建议调高输入音量，否则 ASR/GOP 精度会掉");
    } else if peak > -1.0 {
        eprintln!("[voice] ⚠ 电平过高（{peak:.0} dBFS）：可能削波，建议调低输入音量");
    }
    eprintln!(
        "[voice] 采集完成 {:.2}s @{} Hz（设备 {} Hz{}）",
        cap.duration,
        cap.sample_rate,
        cap.device_sr,
        if cap.auto_stopped {
            "，静音自动停止"
        } else {
            ""
        }
    );

    // 用 VAD 精修首尾静音
    let engines = Engines::load(cfg.clone())?;
    let (trimmed, offset) = engines.vad.trim_silence(&cap.samples, 0.15);

    let seq = session.next_seq()?;
    let prefix = session.take_prefix(seq, scene);
    // 原始证据：设备采样率原样落盘（48k 麦克风就是 48k），16k 是可重建的派生
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
#[allow(clippy::too_many_arguments)] // CLI 子命令参数天然扁平
fn assess_cmd(
    cfg: &Config,
    path: &str,
    reference: Option<&str>,
    lang: &str,
    engine: Option<&str>,
    with_gop: bool,
    with_prosody: bool,
    reference_audio: Option<&str>,
    bank_id: Option<&str>,
    scene: &str,
) -> Result<()> {
    // 素材库参考：文本来自索引；有真人原声切片就当基线/韵律参考（TTS 只作兜底）
    let bank_ref = match bank_id {
        Some(id) => match voice_core::bank::reference_for(Path::new(&cfg.data_dir), id)? {
            Some(r) => Some(r),
            None => return Err(anyhow::anyhow!("素材库里没有 id={id}（先跑 bank list）")),
        },
        None => None,
    };
    let reference = reference.or(bank_ref.as_ref().map(|r| r.text.as_str()));
    let baseline_audio_owned = bank_ref.as_ref().and_then(|r| r.native_audio.clone());
    if let Some(r) = &bank_ref
        && r.native_audio.is_none()
    {
        eprintln!(
            "[assess] {} 是 {} 素材（无真人原声）→ 基线只能用 TTS，结论按“与合成示范的差异”解读",
            r.id, r.ref_audio
        );
    }
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
            with_gop,
            baseline_tts: with_gop && baseline_audio_owned.is_none(), // 有真人参考就不合成 TTS
            with_prosody,
            baseline_audio: baseline_audio_owned.as_deref(),
            reference_audio,
        },
    )?;

    let metrics_path = session
        .dir
        .join("takes")
        .join(format!("{prefix}.metrics.json"));
    if metrics_path.is_file() {
        let _ = std::fs::copy(
            &metrics_path,
            session
                .dir
                .join("takes")
                .join(format!("{prefix}.metrics.prev.json")),
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
        None => cfg
            .watch_dirs
            .iter()
            .filter(|d| d.is_dir())
            .cloned()
            .collect(),
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
            let Ok(rd) = std::fs::read_dir(d) else {
                continue;
            };
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
                    Ok(txt) => println!(
                        "[watch] {} -> {}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        txt
                    ),
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

/// 只读审计现有 TTS 资产：一行 manifest → 一行 report。
/// ---------------------------------------------------------------- 漏词回归（tts-regress）
/// 比较用的词形归一化：小写、去标点、去所有格 `'s`。
///
/// 为什么去掉 `'s`：ASR 常把 `Apple's` 写成 `Apple`（书写差异），
/// 但**真正漏词**时整个词根都会消失（`client's` → 连 `client` 都没有）。
/// 去掉 `'s` 既容忍 ASR 的书写差异，又能抓住“整词被跳过”。
fn gate_words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in voice_core::assess::normalize::tokens(text) {
        let w = raw.to_lowercase().replace('\u{2019}', "'");
        let w = w.trim_matches('\'').to_string();
        let w = match w.strip_suffix("'s") {
            Some(b) => b.to_string(),
            None => w,
        };
        let w = w.replace('\'', "");
        if w.is_empty() {
            continue;
        }
        // 数字有多种合理读法（基数 / 年份成对 / 状态码逐位）。
        // 用前缀 \u{1} 标记"这是一个数字的多候选读法"，比对时命中任一条即算听到。
        if w.chars().all(|c| c.is_ascii_digit()) {
            let cands = voice_core::engine::g2p_ipa::number_candidates(&w);
            if !cands.is_empty() {
                for c in cands {
                    out.push(format!("\u{1}{c}"));
                }
                continue;
            }
        }
        out.push(w);
    }
    out
}

fn lev_ratio(a: &str, b: &str) -> f32 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let d = prev[b.len()] as f32;
    1.0 - d / a.len().max(b.len()) as f32
}

fn similar(a: &str, b: &str) -> bool {
    a == b || (a.len() >= 3 && lev_ratio(a, b) >= 0.8)
}

/// 参考文本里“在回读中找不到对应词”的词（数字按候选读法匹配）。
fn lost_words(reference: &str, heard: &str) -> Vec<String> {
    let want = gate_words(reference);
    let got = gate_words(heard);
    let plain: Vec<&String> = got.iter().filter(|w| !w.starts_with('\u{1}')).collect();
    let mut lost: Vec<String> = Vec::new();
    for w in &want {
        if let Some(cands) = w.strip_prefix('\u{1}') {
            // 连续子序列匹配：候选读法的词序列在回读里按序出现即算听到
            let cw: Vec<&str> = cands.split_whitespace().collect();
            let hit = !cw.is_empty()
                && plain.windows(cw.len().min(plain.len())).any(|win| {
                    win.len() == cw.len() && win.iter().zip(&cw).all(|(g, c)| similar(g, c))
                });
            if !hit {
                lost.push(cands.to_string());
            }
            continue;
        }
        if !plain.iter().any(|g| similar(w, g)) {
            lost.push(w.clone());
        }
    }
    lost
}

fn tts_regress(
    cfg: &Config,
    corpus: &str,
    wpm: Option<f32>,
    speed: Option<f32>,
    no_phones: bool,
    out: Option<&str>,
) -> Result<()> {
    let engines = Engines::load(cfg.clone())?;
    let sid = engines.tts.resolve_sid(voice_core::engine::DEFAULT_VOICE);
    let target = wpm.unwrap_or(cfg.tts_wpm);
    let text = std::fs::read_to_string(corpus).with_context(|| format!("读取 {corpus} 失败"))?;
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut failed = 0usize;
    let mut total = 0usize;

    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let m: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("{corpus} 第 {} 行不是 JSON", lineno + 1))?;
        let id = m["id"].as_str().unwrap_or("?").to_string();
        let intended = m["text"].as_str().unwrap_or("").to_string();
        let expect = m["expect"].as_str().unwrap_or("pass");
        if intended.trim().is_empty() {
            continue;
        }
        total += 1;

        // 期望“必须被拒绝”的用例：中英混说默认拒答（产品契约：一段文本一种语言）
        if expect == "reject_mixed" {
            let ph = engines.tts.phonemize_preview(&intended);
            let attempt = engines.tts.synthesize(&intended, sid, target);
            let rejected = attempt.is_err();
            let msg = attempt.err().map(|e| format!("{e:#}")).unwrap_or_default();
            let ok = rejected && msg.contains("混说");
            if !ok {
                failed += 1;
            }
            rows.push(serde_json::json!({
                "id": id, "ok": ok, "check": "reject_mixed", "text": intended,
                "g2p_oov": ph.oov, "phoneme_ipa": ph.ipa(), "rejected": rejected,
                "error": msg, "note": "混说必须被拒答（含英文段音素可用，但产品入口不出降级音频）",
            }));
            continue;
        }

        // 期望“必须被拒绝”的用例（例如生造专名：G2P 无依据，就该报错而不是合成半句）
        if expect == "oov" {
            let ph = engines.tts.phonemize_preview(&intended);
            let ok = !ph.oov.is_empty();
            if !ok {
                failed += 1;
            }
            rows.push(serde_json::json!({
                "id": id, "ok": ok, "text": intended, "expect": expect,
                "g2p_oov": ph.oov, "note": "生造词必须报 OOV，不允许静默合成"
            }));
            continue;
        }

        // ① 前端证据：不合成也能判断“有没有词根本没拿到音素”
        let ph = engines.tts.phonemize_preview(&intended);
        let g2p_oov = ph.oov.clone();
        let ipa = ph.ipa();

        // ② 合成（走默认前端：开放词表 G2P → phoneme_codepoints）
        // 固定倍率时不做 wpm 标定：门禁只判“内容有没有缺”，语速另算。
        let s = match speed {
            Some(sp) => engines.tts.synthesize(&intended, sid, sp)?,
            None => engines.tts.synthesize_at_wpm(&intended, sid, target)?,
        };
        let pcm = voice_core::audio::resample::resample(
            &s.samples,
            s.sample_rate,
            voice_core::INTERNAL_SR,
        );

        // ③ 双 ASR 回读（漏词口径：逐词比对）
        let sv =
            engines
                .asr
                .transcribe(&pcm, voice_core::INTERNAL_SR, "en", Some("sense-voice"))?;
        let wh = engines
            .asr
            .transcribe(&pcm, voice_core::INTERNAL_SR, "en", Some("whisper"))?;
        let check = m["check"].as_str().unwrap_or("full");
        let en_only = |v: Vec<String>| -> Vec<String> {
            v.into_iter()
                .filter(|w| {
                    // 非 CJK 词才算“英文段丢词”
                    let mut any = false;
                    for c in w.chars() {
                        if c as u32 > 0x2FFF {
                            any = true;
                        }
                    }
                    !any
                })
                .collect()
        };
        let (lost_sv, lost_wh) = {
            let a = lost_words(&intended, &sv.text);
            let b = lost_words(&intended, &wh.text);
            if check == "en_words" {
                (en_only(a), en_only(b))
            } else {
                (a, b)
            }
        };

        // ④ 音素级（可选）：音频本身是否读坏了
        let phone_min = if no_phones {
            None
        } else {
            let g = voice_core::gop::with_engine(|e| {
                voice_core::gop::score_text(&mut e.model, &pcm, &intended, &e.g2p)
            })?;
            g.words
                .iter()
                .map(|w| w.gop_mean)
                .fold(f32::INFINITY, f32::min)
                .into()
        };
        let phone_bad = matches!(phone_min, Some(v) if v < -1.5);

        // 判定口径：**两个引擎同时丢**才算 TTS 缺词；单引擎丢是 ASR 自己的毛病
        // （实测 SenseVoice 会把 "client's report" 听成 "lions report Tan"）。
        let both_lost: Vec<String> = lost_sv
            .iter()
            .filter(|w| lost_wh.contains(w))
            .cloned()
            .collect();
        // 中英混说：Kokoro 没有双语 voice（英文 voice 读中文、中文 voice 读英文都会跑调），
        // 所以混说用例只锁**英文段**不被吞/不被中文音素化，中文段音质不进门禁。
        if check == "structure" {
            // 中英混说：Kokoro 没有双语 voice（英文 voice 读中文 / 中文 voice 读英文都会跑调），
            // 所以混说用例**只锁结构**：英文段必须拿到音素、中文段不得漏进 G2P、
            // 整句合成不报错。音质属 voice 选型问题，不进门禁。
            let ok = g2p_oov.is_empty() && s.phonemes.is_some();
            if !ok {
                failed += 1;
            }
            rows.push(serde_json::json!({
                "id": id, "ok": ok, "check": check, "text": intended,
                "phonemes": s.phonemes.clone(), "phoneme_ipa": ipa,
                "g2p_oov": g2p_oov, "duration": (s.duration * 1000.0).round() / 1000.0,
                "note": "混说：只验证英文段音素化与中文段隔离，不判音质",
            }));
            continue;
        }

        let must_missing: Vec<String> = if check == "en_words" {
            let got = gate_words(&wh.text);
            let joined: String = got.join("");
            m["must_have"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .filter(|w| {
                            let wl = w.to_lowercase();
                            // ASR 可能把缩写拆成 "a p i"，用拼接串兜一层
                            !got.iter().any(|g| similar(&wl, g)) && !joined.contains(&wl)
                        })
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // 门禁只看“有没有丢词”。`phone_min_gop` 仅作报告：我们的 G2P 现在会沿用
        // Kokoro lexicon 的发音惯例（`ði`、`ᵊ`、闪音 ɾ、次重音 ˌ），与 GOP 参考
        // （CMUdict ARPAbet）不是同一套标注，负分不代表读错。
        // `tolerance=count`：允许“词界被 ASR 重写”（`isn't ready`→`tizen ready`、
        // `finished and`→`finished then`）——这类改写**不减少词数**；
        // 真正的漏词必然让回读词数变少。用它把“音素流没有词间连接建模”这一已知弱点
        // 与“整词消失”分开。
        // 默认按“词数不得减少”判（真漏词必然减词）；strict 只影响是否额外要求 lost 为空。
        let tolerance = m["tolerance"].as_str().unwrap_or("count");
        let ref_n = gate_words(&intended).len();
        let wh_n = gate_words(&wh.text).len();
        let sv_n = gate_words(&sv.text).len();
        let count_ok = tolerance == "count" && wh_n >= ref_n && sv_n + 1 >= ref_n;
        let ok = g2p_oov.is_empty()
            && s.phonemes.is_some()
            && must_missing.is_empty()
            && (both_lost.is_empty() || count_ok);
        if !ok {
            failed += 1;
        }
        rows.push(serde_json::json!({
            "id": id,
            "ok": ok,
            "text": intended,
            "phonemes": s.phonemes.clone(),
            "phoneme_ipa": ipa,
            "g2p_oov": g2p_oov,
            "lost_sense": lost_sv,
            "lost_whisper": lost_wh,
            "lost_both": both_lost,
            "asr_sense": sv.text,
            "asr_whisper": wh.text,
            "phone_min_gop": phone_min.map(|v: f32| (v * 100.0).round() / 100.0),
            "phone_warn": phone_bad,
            "check": check,
            "tolerance": tolerance,
            "words_ref": ref_n, "words_whisper": wh_n, "words_sense": sv_n,
            "must_missing": must_missing,
            "duration": (s.duration * 1000.0).round() / 1000.0,
            "speech_duration": (s.speech_duration * 1000.0).round() / 1000.0,
            "speed": s.speed,
            "phoneme_count": ipa.chars().filter(|c| *c != ' ').count(),
            "wpm": (s.wpm * 10.0).round() / 10.0,
        }));
    }

    if let Some(p) = out {
        let mut body = String::new();
        for r in &rows {
            body.push_str(&r.to_string());
            body.push('\n');
        }
        std::fs::write(p, body).with_context(|| format!("写 {p} 失败"))?;
    }
    println!(
        "{}",
        serde_json::json!({
            "corpus": corpus,
            "cases": total,
            "pass": total - failed,
            "fail": failed,
            "verdict": if failed == 0 { "pass" } else { "fail" },
            "rows": rows,
        })
    );
    if failed > 0 {
        eprintln!("漏词回归失败：{failed}/{total} 句（退出码 2）");
        std::process::exit(2);
    }
    Ok(())
}

fn tts_audit(cfg: &Config, manifest: &str, out: &str, no_phones: bool) -> Result<()> {
    let text = std::fs::read_to_string(manifest)
        .with_context(|| format!("读取 manifest 失败: {manifest}"))?;
    let engines = Engines::load(cfg.clone())?;
    let mut rows: Vec<serde_json::Value> = Vec::new();

    for (lineno, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let m: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("manifest 第 {} 行不是 JSON", lineno + 1))?;
        let id = m["id"].as_str().unwrap_or("?").to_string();
        let path = m["path"].as_str().unwrap_or("").to_string();
        let intended = m["text"].as_str().unwrap_or("").to_string();
        let kind = m["kind"].as_str().unwrap_or("asset");
        let voice = m["voice"].as_str();
        let speed = m["speed"].as_f64();

        let row = (|| -> Result<serde_json::Value> {
            let decoded =
                voice_core::audio::load_16k(&path).with_context(|| format!("解码失败: {path}"))?;
            let duration = decoded.samples.len() as f32 / decoded.sample_rate as f32;
            let words = voice_core::engine::count_words(&intended);
            let wpm = if duration > 0.0 {
                words as f32 / duration * 60.0
            } else {
                0.0
            };
            let ascii = intended.is_ascii() && !intended.trim().is_empty();
            let oov = if ascii {
                engines.tts.oov_words(&intended)
            } else {
                Vec::new()
            };
            let (sense, whisper, sense_ok, whisper_ok) = if ascii {
                let sv = engines.asr.transcribe(
                    &decoded.samples,
                    decoded.sample_rate,
                    "en",
                    Some("sense-voice"),
                )?;
                let wh = engines.asr.transcribe(
                    &decoded.samples,
                    decoded.sample_rate,
                    "en",
                    Some("whisper"),
                )?;
                let want = voice_core::assess::normalize::words(&intended);
                (
                    Some(sv.text.clone()),
                    Some(wh.text.clone()),
                    voice_core::assess::normalize::words(&sv.text) == want,
                    voice_core::assess::normalize::words(&wh.text) == want,
                )
            } else {
                (None, None, false, false)
            };
            let (phone_min, phone_suspect) = if no_phones || !ascii {
                (None, false)
            } else {
                let g = voice_core::gop::with_engine(|e| {
                    voice_core::gop::score_text(&mut e.model, &decoded.samples, &intended, &e.g2p)
                })?;
                let min = g
                    .words
                    .iter()
                    .map(|w| w.gop_mean)
                    .fold(f32::INFINITY, f32::min);
                let min = if min.is_finite() { Some(min) } else { None };
                (min, min.map(|v| v < -1.5).unwrap_or(false))
            };

            let mut reasons: Vec<&str> = Vec::new();
            if intended.trim().is_empty() {
                reasons.push("metadata_missing_text");
            }
            if !intended.is_ascii() {
                reasons.push("unsupported_language");
            }
            if !oov.is_empty() {
                reasons.push("oov");
            }
            if ascii && !sense_ok {
                reasons.push("sense_mismatch");
            }
            if ascii && !whisper_ok {
                reasons.push("whisper_mismatch");
            }
            if ascii && wpm > 0.0 && !(100.0..=180.0).contains(&wpm) {
                reasons.push("wpm_out_of_range");
            }
            if phone_suspect {
                reasons.push("phone_suspect");
            }
            let reject = reasons.iter().any(|r| {
                matches!(
                    *r,
                    "oov" | "phone_suspect" | "wpm_out_of_range" | "metadata_missing_text"
                )
            }) || (ascii && !sense_ok && !whisper_ok);
            let review =
                !reject && (ascii && (!sense_ok || !whisper_ok || !(130.0..=160.0).contains(&wpm)));
            let verdict = if !intended.is_ascii() {
                "unsupported"
            } else if reject {
                "reject"
            } else if review {
                "review"
            } else {
                "pass"
            };

            Ok(serde_json::json!({
                "id": id, "kind": kind, "path": path,
                "text": intended,
                "voice": voice, "speed": speed,
                "duration": round3(duration), "words": words,
                "wpm": round3(wpm),
                "oov": oov,
                // 前端版本标记：2026-11-27 起英文不再查引擎词表，
                // OOV 列语义变了，曲线跨版本不可直接比较。
                "frontend": engines.tts.frontend_id(),
                "asr_sense": sense, "asr_whisper": whisper,
                "sense_match": sense_ok, "whisper_match": whisper_ok,
                "phone_min_gop": phone_min, "phone_suspect": phone_suspect,
                "verdict": verdict, "reasons": reasons,
            }))
        })();

        match row {
            Ok(v) => rows.push(v),
            Err(e) => rows.push(serde_json::json!({
                "id": id, "kind": kind, "path": path, "verdict": "error",
                "reasons": ["decode_or_engine_error"], "error": format!("{e:#}"),
            })),
        }
    }

    let body = rows
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join(
            "
",
        );
    if let Some(parent) = Path::new(out).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        out,
        format!(
            "{body}
"
        ),
    )?;
    let n = rows.len();
    let count = |k: &str| rows.iter().filter(|r| r["verdict"] == k).count();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "frontend": engines.tts.frontend_id(),
            "assets": n,
            "pass": count("pass"), "review": count("review"),
            "reject": count("reject"), "unsupported": count("unsupported"),
            "error": count("error"), "out": out,
        }))?
    );
    Ok(())
}

/// 一次合成结果证据化校验：双 ASR 回读 + WPM + 可选音素 GOP。
///
/// `tts-verify` 与 `bank refs`（批量材料生成）共用这一个函数，
/// 保证“verified 资产”的口径只有一处定义（D51 / N11）。
fn verify_synthesized(
    engines: &Engines,
    s: &voice_core::engine::Synthesized,
    text: &str,
    no_phones: bool,
) -> Result<serde_json::Value> {
    let pcm =
        voice_core::audio::resample::resample(&s.samples, s.sample_rate, voice_core::INTERNAL_SR);
    let sv = engines
        .asr
        .transcribe(&pcm, voice_core::INTERNAL_SR, "en", Some("sense-voice"))?;
    let wh = engines
        .asr
        .transcribe(&pcm, voice_core::INTERNAL_SR, "en", Some("whisper"))?;
    let want = voice_core::assess::normalize::words(text);
    let sv_words = voice_core::assess::normalize::words(&sv.text);
    let wh_words = voice_core::assess::normalize::words(&wh.text);
    let sv_ok = sv_words == want;
    // whisper 会把合成词拆成两个词（`hometown` → `home town`）：只算“词界不一致”，
    // 不能当成内容错（但也不能算 verified）。
    let boundary_only = !want.is_empty() && wh_words != want && wh_words.join("") == want.join("");
    let wh_ok = wh_words == want || boundary_only;
    // 开头多出一个词（实测 af_heart 在某些慢速点会多读成 “And”）：正文对得上，只是前面多词
    let extra_leading = |got: &[String]| -> bool {
        !want.is_empty() && got.len() > want.len() && got.ends_with(&want[..])
    };
    let prefix_insert = !sv_ok && extra_leading(&sv_words) || !wh_ok && extra_leading(&wh_words);

    // 130–160 为“达标”；100–180 之外才算硬失败，避免 128/153 这种边界值被直接 reject。
    let wpm_ok = s
        .target_wpm
        .map(|_| {
            (voice_core::engine::IELTS_WPM_MIN..=voice_core::engine::IELTS_WPM_MAX).contains(&s.wpm)
        })
        .unwrap_or(true);
    let wpm_extreme = s.wpm > 0.0 && !(100.0..=180.0).contains(&s.wpm);

    let (phone_min, phone_suspect, confusions) = if no_phones {
        (None, false, Vec::<String>::new())
    } else {
        let g = voice_core::gop::with_engine(|e| {
            voice_core::gop::score_text(&mut e.model, &pcm, text, &e.g2p)
        })?;
        let min = g
            .words
            .iter()
            .map(|w| w.gop_mean)
            .fold(f32::INFINITY, f32::min);
        let min = if min.is_finite() { Some(min) } else { None };
        let conf: Vec<String> = g
            .phones
            .iter()
            .filter_map(|p| p.confusion.clone())
            .collect();
        // 词级 GOP 太低说明 TTS 音频本身偏离参考音素
        let suspect = min.map(|m| m < -1.5).unwrap_or(false);
        (min, suspect, conf)
    };

    let mut reasons: Vec<String> = Vec::new();
    if !sv_ok {
        reasons.push("sense_mismatch".into());
    }
    if !wh_ok {
        reasons.push("whisper_mismatch".into());
    }
    if !wpm_ok {
        reasons.push("wpm_out_of_range".into());
    }
    if phone_suspect {
        reasons.push("phone_suspect".into());
    }
    if s.onset_gap_ms.is_some() {
        reasons.push("onset_artifact".into());
    }
    if prefix_insert {
        reasons.push("leading_insert".into());
    }
    if boundary_only {
        reasons.push("word_boundary".into());
    }

    // 结构上可疑（起始有多余的一小段）且 ASR 也多听出词 → 确定为材料缺陷，直接 reject；
    // 只有结构信号、ASR 读写正常 → 只降到 review（自然语流里也会有短闭塞，不能硬卡）。
    let onset_only = s.onset_gap_ms.is_some() && !prefix_insert;
    let usable = (sv_ok || wh_ok) && !wpm_extreme && !phone_suspect;
    let verdict = if !usable || (s.onset_gap_ms.is_some() && prefix_insert) {
        "reject"
    } else if sv_ok && wh_ok && !boundary_only && wpm_ok && !onset_only {
        "verified"
    } else {
        "review"
    };
    Ok(serde_json::json!({
        "verdict": verdict,
        "pass": verdict != "reject",
        "speed": round3(s.speed), "wpm": round3(s.wpm),
        "target_wpm": s.target_wpm,
        "speech_duration": round3(s.speech_duration),
        "onset_gap_ms": s.onset_gap_ms.map(round3),
        "asr_sense": sv.text, "asr_whisper": wh.text,
        "sense_match": sv_ok, "whisper_match": wh_ok, "wpm_ok": wpm_ok,
        "prefix_insert": prefix_insert, "word_boundary_only": boundary_only,
        "phone_min_gop": phone_min, "phone_suspect": phone_suspect,
        "confusions": confusions, "reasons": reasons,
    }))
}

/// TTS 参考音验证：合成 N 次，双 ASR 回读 + 音素检查 + WPM；失败退出码 2。
#[allow(clippy::too_many_arguments)]
fn tts_verify(
    cfg: &Config,
    text: &str,
    voices: &[String],
    runs: usize,
    wpm: Option<f32>,
    no_phones: bool,
) -> Result<()> {
    let store = Store::open(&cfg.data_dir)?;
    let session = store.new_session("tts_verify")?;
    let seq = session.next_seq()?;
    let prefix = session.take_prefix(seq, "tts_verify");
    let engines = Engines::load(cfg.clone())?;
    let target = wpm.unwrap_or(cfg.tts_wpm);
    let voices: Vec<String> = if voices.is_empty() {
        vec!["am_michael".into()]
    } else {
        voices.to_vec()
    };
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut any_reject = false;
    let mut all_verified = true;

    for voice in &voices {
        let sid = engines.tts.resolve_sid(voice);
        for run in 1..=runs.max(1) {
            let row = (|| -> Result<serde_json::Value> {
                let s = engines.tts.synthesize_at_wpm(text, sid, target)?;
                // 先回读校验，再落盘（落盘只做留证）
                let mut v = verify_synthesized(&engines, &s, text, no_phones)?;
                let label = format!("{voice}_{run}");
                let path = session.write_reference(
                    &prefix,
                    &label,
                    &s.samples,
                    s.sample_rate,
                    text,
                    &s.voice,
                    s.speed,
                    s.wpm,
                    s.target_wpm,
                )?;
                v["voice"] = serde_json::json!(voice);
                v["run"] = serde_json::json!(run);
                v["wav"] = serde_json::json!(path);
                Ok(v)
            })();
            match row {
                Ok(v) => {
                    any_reject |= v["verdict"] == "reject";
                    all_verified &= v["verdict"] == "verified";
                    rows.push(v);
                }
                Err(e) => {
                    any_reject = true;
                    all_verified = false;
                    rows.push(serde_json::json!({
                        "voice": voice, "run": run, "pass": false,
                        "verdict": "reject", "error": format!("{e:#}"),
                    }));
                }
            }
        }
    }

    let pass_runs = rows
        .iter()
        .filter(|r| r["pass"].as_bool().unwrap_or(false))
        .count();
    let summary_verdict = if any_reject {
        "reject"
    } else if all_verified {
        "verified"
    } else {
        "review"
    };
    let report = rows
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join(
            "
",
        );
    std::fs::write(
        session.dir.join("report.jsonl"),
        format!(
            "{report}
"
        ),
    )?;
    let summary = serde_json::json!({
        "text": text, "target_wpm": target,
        "runs": runs, "voices": voices,
        "pass_runs": pass_runs, "total_runs": rows.len(),
        "verdict": summary_verdict,
        "report": session.dir.join("report.jsonl"),
    });
    std::fs::write(
        session.dir.join("summary.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if summary_verdict == "reject" {
        eprintln!(
            "[tts-verify] 未通过（{pass_runs}/{}）；报告 {}",
            rows.len(),
            session.dir.display()
        );
        std::process::exit(2);
    }
    if summary_verdict == "review" {
        eprintln!(
            "[tts-verify] 需人工复核（单引擎不一致/边界语速）；报告 {}",
            session.dir.display()
        );
    }
    Ok(())
}

/// `bank ingest-audio`：没有 LRC 的单文件真人音频 → 逐句素材。
#[allow(clippy::too_many_arguments)]
fn bank_ingest_audio(
    cfg: &Config,
    audio: &str,
    corpus: &str,
    book: &str,
    lesson: Option<&str>,
    min_agree: f32,
    max_sec: f32,
    start: f32,
    end: Option<f32>,
    max_chunks: Option<usize>,
    srt: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    // 没显式给 --srt 时，自动找同名 .srt / .vtt（放一起就能用）
    let srt_path: Option<std::path::PathBuf> = match srt {
        Some(p) => Some(std::path::PathBuf::from(p)),
        None => {
            let a = std::path::Path::new(audio);
            let stem = a.with_extension("");
            ["srt", "vtt", "SRT", "VTT"]
                .iter()
                .map(|ext| stem.with_extension(ext))
                .find(|p| p.is_file())
        }
    };
    let has_srt = srt_path.is_some();
    if let Some(p) = &srt_path {
        eprintln!(
            "[bank ingest-audio] 字幕：{}（答案键来自字幕）",
            p.display()
        );
    }
    let engines = Engines::load(cfg.clone())?;
    let opt = voice_core::bank::AudioIngestOptions {
        corpus: corpus.to_string(),
        book: book.to_string(),
        lesson: lesson.unwrap_or_default().to_string(),
        min_agree,
        max_sec,
        start_sec: start,
        end_sec: end,
        max_chunks,
        srt: srt_path,
        dry_run,
    };
    let r =
        voice_core::bank::ingest_audio(&engines, Path::new(&cfg.data_dir), Path::new(audio), &opt)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "lesson": r.lesson_slug,
            "kept": r.kept,
            "rejected": r.rejected,
            "source_sr": r.source_sr,
            "band_limited": r.band_limited,
            "text_source": if has_srt { "srt" } else { "asr" },
            "entries": r.entries,
            "preview": r.preview.iter().map(|(id, text, dur)| serde_json::json!({
                "id": id, "dur": round3(*dur), "text": text
            })).collect::<Vec<_>>(),
            "rejects": r.rejects.iter().map(|x| serde_json::json!({
                "seq": x.seq, "text": x.text, "reasons": x.reasons
            })).collect::<Vec<_>>(),
            "dry_run": dry_run,
        }))?
    );
    if r.kept > 0 {
        eprintln!(
            "[bank ingest-audio] {} 句入库（拒 {}）；验证：voice-cli assess <录音> --bank-id {} --with-gop",
            r.kept,
            r.rejected,
            r.entries.first().cloned().unwrap_or_default()
        );
    }
    Ok(())
}

struct BankRefsArgs {
    ids: Vec<String>,
    kind: Option<String>,
    corpus: Option<String>,
    limit: Option<usize>,
    voices: Vec<String>,
    wpm: Option<f32>,
    runs: usize,
    slow_too: bool,
    no_phones: bool,
    out: String,
}

/// 批量生成示范音（TTS）并逐条接入 `tts-verify`（D51 / N11）。
///
/// 产出的每个 wav 都带双 ASR 回读、WPM、起始伪影与（可选）音素证据；
/// `metadata.json` 里的 `status` 只能是 verified / review / reject。
fn bank_refs(cfg: &Config, a: BankRefsArgs) -> Result<()> {
    use voice_core::bank;

    let home = Path::new(&cfg.data_dir);
    let idx = bank::read_index(home)?;
    let mut picked: Vec<bank::BankEntry> = idx
        .into_iter()
        .filter(|e| {
            a.kind
                .as_deref()
                .map(|k| e.kind.eq_ignore_ascii_case(k))
                .unwrap_or(true)
                && a.corpus
                    .as_deref()
                    .map(|c| e.corpus.eq_ignore_ascii_case(c))
                    .unwrap_or(true)
        })
        .collect();
    if !a.ids.is_empty() {
        // 保持用户给的顺序
        let mut ordered = Vec::new();
        for id in &a.ids {
            if let Some(pos) = picked.iter().position(|e| &e.id == id) {
                ordered.push(picked.remove(pos));
            } else {
                bail!("素材库里没有 id={id}");
            }
        }
        picked = ordered;
    }
    if let Some(n) = a.limit {
        picked.truncate(n);
    }
    if picked.is_empty() {
        bail!("没有匹配的素材（--kind/--corpus/ids 全空？先跑 bank list）");
    }

    let out = PathBuf::from(&a.out);
    std::fs::create_dir_all(&out)?;
    let store = Store::open(&cfg.data_dir)?;
    let session = store.new_session("bank_refs")?;
    let engines = Engines::load(cfg.clone())?;
    let target = a.wpm.unwrap_or(cfg.tts_wpm);
    // 音色回退链：同一句在不同音色下读坏的情况差别很大（实测 "Describe a place" 在
    // am_michael/af_heart 下会丢 a，am_fenrir 下干净）。按顺序试到 verified 为止。
    let voices: Vec<String> = if a.voices.is_empty() {
        vec!["am_michael".into(), "am_fenrir".into()]
    } else {
        a.voices.clone()
    };
    let runs = a.runs.max(1);

    let mut meta: Vec<serde_json::Value> = Vec::new();
    let mut report: Vec<serde_json::Value> = Vec::new();
    let mut counts = [0usize; 3]; // verified / review / reject

    for e in &picked {
        let slug_i = e.id.replace(['/', '\\', ' '], "_");
        let mut variants: Vec<(&str, f32)> = vec![("norm", target)];
        if a.slow_too {
            variants.push(("slow", voice_core::engine::TTS_SLOW_WPM));
        }
        let mut entry_done = false;
        let mut pending: Vec<serde_json::Value> = Vec::new();
        for (voice_i, voice) in voices.iter().enumerate() {
            if entry_done {
                break;
            }
            let voice = voice.clone();
            let sid = engines.tts.resolve_sid(&voice);
            let mut failed_variant = false;
            for (variant, base_wpm) in &variants {
                let variant = *variant;
                let base_wpm = *base_wpm;
                let mut slug = if variant == "norm" {
                    slug_i.clone()
                } else {
                    format!("{slug_i}.{variant}")
                };
                if voice_i > 0 {
                    // 回退音色的产物另存一份，不覆盖主力音色的音频
                    slug = format!("{slug}.{voice}");
                }
                // 自动重试：合成音在 140 wpm 附近会吞掉弱读词（实测 "Describe a place" → "Describe place"），
                // 换个目标语速（仍在 130–160）通常能修好。每个目标都要 3/3 才算 verified。
                let mut attempts: Vec<f32> = vec![base_wpm];
                for d in [4.0f32, -4.0] {
                    let w = base_wpm + d;
                    if (voice_core::engine::IELTS_WPM_MIN..=voice_core::engine::IELTS_WPM_MAX)
                        .contains(&w)
                        && !attempts.iter().any(|x| (x - w).abs() < 0.01)
                    {
                        attempts.push(w);
                    }
                }
                let n_attempts = attempts.len();
                for (attempt_i, wpm) in attempts.iter().enumerate() {
                    let mut rows: Vec<serde_json::Value> = Vec::new();
                    let mut any_reject = false;
                    let mut all_verified = true;
                    let mut wav_path = String::new();
                    let mut dur = 0.0f64;
                    for run in 1..=runs {
                        let row = (|| -> Result<serde_json::Value> {
                            let s = engines.tts.synthesize_at_wpm(&e.text, sid, *wpm)?;
                            let mut v = verify_synthesized(&engines, &s, &e.text, a.no_phones)?;
                            let label = format!("{slug}_{run}");
                            let ev = session.write_reference(
                                &slug,
                                &label,
                                &s.samples,
                                s.sample_rate,
                                &e.text,
                                &s.voice,
                                s.speed,
                                s.wpm,
                                s.target_wpm,
                            )?;
                            if run == 1 {
                                let name = if attempt_i == 0 {
                                    format!("{slug}.wav")
                                } else {
                                    format!("{slug}.wpm{:.0}.wav", wpm)
                                };
                                let p = out.join(name);
                                voice_core::audio::wav::write_wav_i16(
                                    &p,
                                    &s.samples,
                                    s.sample_rate,
                                )?;
                                wav_path = p.to_string_lossy().to_string();
                                dur = s.duration as f64;
                            }
                            v["id"] = serde_json::json!(e.id);
                            v["variant"] = serde_json::json!(variant);
                            v["run"] = serde_json::json!(run);
                            v["attempt"] = serde_json::json!(attempt_i);
                            v["evidence"] = serde_json::json!(ev);
                            Ok(v)
                        })();
                        match row {
                            Ok(v) => {
                                any_reject |= v["verdict"] == "reject";
                                all_verified &= v["verdict"] == "verified";
                                rows.push(v);
                            }
                            Err(err) => {
                                any_reject = true;
                                all_verified = false;
                                rows.push(serde_json::json!({
                                    "id": e.id, "variant": variant, "run": run,
                                    "pass": false, "verdict": "reject",
                                    "error": format!("{err:#}"),
                                }));
                            }
                        }
                    }
                    let status = if any_reject {
                        "reject"
                    } else if all_verified {
                        "verified"
                    } else {
                        "review"
                    };
                    let pass_runs = rows
                        .iter()
                        .filter(|r| r["pass"].as_bool().unwrap_or(false))
                        .count();
                    let accepted = status == "verified";
                    let first = rows.first().cloned().unwrap_or_default();
                    let entry = serde_json::json!({
                        "id": e.id,
                        "variant": variant,
                        "text": e.text,
                        "level": e.level,
                        "voice": voice,
                        "status": status,
                        "attempt": attempt_i,
                        "target_wpm": wpm,
                        "runs": runs,
                        "pass_runs": pass_runs,
                        "wpm": first["wpm"],
                        "speed": first["speed"],
                        "duration": round3(dur as f32),
                        "onset_gap_ms": first["onset_gap_ms"],
                        "reasons": first["reasons"],
                        "wav": wav_path,
                        "evidence_dir": session.dir.to_string_lossy().to_string(),
                    });
                    report.extend(rows);
                    eprintln!(
                        "[bank refs] {:<28} {variant:<4} wpm{:<5.0} {status:<8} {}/{}",
                        e.id, wpm, runs, pass_runs
                    );
                    if accepted {
                        // 只有 verified 的尝试进入 metadata；前面失败的尝试只留在 report.jsonl
                        pending.push(entry);
                        break;
                    }
                    if attempt_i + 1 == n_attempts {
                        // 所有目标语速都失败：登记最后一次判定，并标记该变体未过
                        pending.push(entry);
                        failed_variant = true;
                    } else {
                        eprintln!("[bank refs]   ↳ 换目标语速重试（{} wpm 未通过）", wpm);
                    }
                }
            }
            if !failed_variant {
                entry_done = true;
            } else if voice_i + 1 < voices.len() {
                eprintln!("[bank refs]   ↳ 换音色重试（{} 未通过：{}）", voice, e.id);
            }
        }
        if entry_done {
            // 成功：只留 verified 的产物（失败音色的尝试在 report.jsonl 里）
            for m in pending.iter().filter(|m| m["status"] == "verified") {
                counts[0] += 1;
                meta.push(m.clone());
            }
        } else {
            for m in pending {
                match m["status"].as_str().unwrap_or("reject") {
                    "review" => counts[1] += 1,
                    _ => counts[2] += 1,
                }
                meta.push(m);
            }
        }
    }

    let meta_path = out.join("metadata.json");
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;
    let report_path = out.join("report.jsonl");
    let body = report
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join(
            "
",
        );
    std::fs::write(&report_path, format!("{body}\n"))?;

    let summary = serde_json::json!({
        "assets": meta.len(),
        "verified": counts[0], "review": counts[1], "reject": counts[2],
        "voices": voices, "target_wpm": target, "runs": runs,
        "metadata": meta_path.to_string_lossy(),
        "report": report_path.to_string_lossy(),
        "evidence_dir": session.dir.to_string_lossy(),
    });
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if counts[2] > 0 {
        eprintln!(
            "[bank refs] {} 项未通过：不进教学/听力材料（报告 {}）",
            counts[2],
            report_path.display()
        );
        std::process::exit(2);
    }
    Ok(())
}

fn tts_probe(cfg: &Config, text: &str) -> Result<()> {
    let out = cfg.data_dir.join("probe").join("voices");
    std::fs::create_dir_all(&out)?;
    let engines = Engines::load(cfg.clone())?;
    let tts = &engines.tts;
    println!(
        "为 {} 个音色生成样本 -> {}",
        tts.num_speakers(),
        out.display()
    );
    for v in tts.voices() {
        let s = tts.synthesize(text, v.sid, 1.0)?;
        let p = out.join(format!("{:02}_{}_{}.wav", v.sid, v.lang, v.name));
        voice_core::audio::wav::write_wav_i16(&p, &s.samples, s.sample_rate)?;
        println!(
            "  {:<20} {}",
            v.name,
            p.file_name().unwrap_or_default().to_string_lossy()
        );
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
    corpus: Option<&str>,
    level: Option<u8>,
    tag: Option<&str>,
    book: Option<&str>,
    limit: usize,
) -> Result<()> {
    let home = Path::new(&cfg.data_dir);
    let mut idx = voice_core::bank::read_index(home)?;
    idx.retain(|e| {
        corpus
            .map(|c| e.corpus.eq_ignore_ascii_case(c))
            .unwrap_or(true)
    });
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
                "id": e.id, "corpus": e.corpus, "kind": e.kind,
                "part": e.exam.as_ref().and_then(|x| x.part),
                "level": e.level, "wpm": f1(e.wpm), "words": e.words,
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
            "text_source": e.text_source,
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
    if e.src.is_empty() || !src.is_file() {
        // prompt 类素材没有原生音频：不报错，提示改用 TTS 示范
        println!(
            "（无原生音频 → 用 synthesize 生成示范音；kind={} ref_audio={}）",
            e.kind, e.ref_audio
        );
        println!("（评测基线只能用 TTS：不能说“语调/重音不地道”，只能说“与合成示范的差异”）");
        return Ok(());
    }
    println!("{}", e.text);
    if !e.zh.is_empty() {
        println!("{}", e.zh);
    }
    let exam = e
        .exam
        .as_ref()
        .and_then(|x| x.part)
        .map(|p| format!(" · IELTS Part {p}"))
        .unwrap_or_default();
    println!(
        "[{}] {}{} level {} · {} 词 · {:.0} wpm · {:.2}s · tags: {}",
        e.id,
        e.corpus.to_uppercase(),
        exam,
        e.level,
        e.words,
        e.wpm,
        e.dur,
        if e.tags.is_empty() {
            "-".to_string()
        } else {
            e.tags.join(",")
        }
    );
    if !quiet {
        play::play_wav(&src)?;
    }
    if e.ref_audio == "native" {
        println!(
            "真人原声参考：{} → 评测用它当基线：assess <你的录音> --bank-id {} --with-gop",
            e.src, e.id
        );
    }
    Ok(())
}

struct BankPickArgs {
    tags: Vec<String>,
    level: Option<u8>,
    book: Option<String>,
    corpus: Option<String>,
    part: Option<u8>,
    topic: Option<String>,
    kind: Option<String>,
    limit: usize,
    offset: usize,
    seed: Option<u64>,
    allow_noise: bool,
    weighted: bool,
    exclude_corpora: Vec<String>,
}

fn bank_pick(cfg: &Config, a: BankPickArgs) -> Result<()> {
    let home = Path::new(&cfg.data_dir);
    let explicit_corpus = a.corpus.is_some();
    let q = voice_core::bank::PickQuery {
        corpus: a.corpus,
        part: a.part,
        topic: a.topic,
        kind: a.kind,
        level: a.level,
        tags: a.tags,
        book: a.book,
        exclude_tags: if a.allow_noise {
            Vec::new()
        } else {
            voice_core::bank::PickQuery::default().exclude_tags
        },
        exclude_corpora: if explicit_corpus {
            // 显式点了语料（如 --corpus peppa）就尊重它，不再默认排除
            a.exclude_corpora
        } else {
            let mut v = a.exclude_corpora.clone();
            for c in voice_core::bank::default_exclude_corpora() {
                if !v.iter().any(|x| x.eq_ignore_ascii_case(&c)) {
                    v.push(c);
                }
            }
            v
        },
        limit: a.limit,
        offset: a.offset,
        weighted: a.weighted,
        seed: a.seed,
    };
    let picked = voice_core::bank::pick(home, &q)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "count": picked.len(),
            "items": picked.iter().map(|e| serde_json::json!({
                "id": e.id,
                "text": e.text,
                "zh": e.zh,
                "level": e.level,
                "words": e.words,
                "wpm": f1(e.wpm),
                "dur": f2(e.dur),
                "corpus": e.corpus,
                "kind": e.kind,
                "ref_audio": e.ref_audio,
                "exam": e.exam.as_ref().map(|x| serde_json::json!({
                    "framework": x.framework, "part": x.part, "topics": x.topics, "task": x.task
                })),
                "tags": e.tags,
                "tag_weights": e.tags.iter().map(|t| serde_json::json!({
                    "tag": t, "weight": f3(voice_core::bank::tag_weight(t))
                })).collect::<Vec<_>>(),
                "audio": e.src,
            })).collect::<Vec<_>>(),
        }))?
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)] // CLI 子命令参数天然扁平
fn bank_plan(
    cfg: &Config,
    learner: Option<&str>,
    minutes: f32,
    limit: usize,
    level: Option<u8>,
    seed: Option<u64>,
    corpus: Option<String>,
    exclude_corpus: Vec<String>,
    part: Option<u8>,
    dry_run: bool,
    json: bool,
    include_free_speech: Option<bool>,
) -> Result<()> {
    let learner_path = learner
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(&cfg.data_dir).join("learner.json"));
    let text = std::fs::read_to_string(&learner_path)
        .with_context(|| format!("读取 {} 失败", learner_path.display()))?;
    let learner: voice_core::plan::Learner = serde_json::from_str(&text)
        .with_context(|| format!("{} 不是合法的 learner.json", learner_path.display()))?;
    let entries = voice_core::bank::read_index(Path::new(&cfg.data_dir))?;
    // 默认排除非训练语料（peppa 这类）；显式 --corpus 时以用户为准
    let mut exclude_corpora = exclude_corpus;
    if corpus.is_none() {
        for c in voice_core::bank::default_exclude_corpora() {
            if !exclude_corpora.iter().any(|x| x.eq_ignore_ascii_case(&c)) {
                exclude_corpora.push(c);
            }
        }
    }
    let req = voice_core::plan::PlanRequest {
        minutes,
        limit,
        level,
        seed,
        exclude_corpora,
        corpus,
        part,
        include_free_speech,
    };
    let plan = voice_core::plan::build(&entries, &learner, &req);

    if !dry_run {
        let store = Store::open(&cfg.data_dir)?;
        let session = store.new_session("plan")?;
        std::fs::write(
            session.dir.join("plan.json"),
            serde_json::to_string_pretty(&plan)?,
        )?;
        std::fs::write(session.dir.join("plan.md"), plan.to_markdown())?;
        eprintln!("[plan] 已写入 {}", session.dir.display());
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        print!("{}", plan.to_markdown());
    }
    Ok(())
}

fn bank_stats(cfg: &Config) -> Result<()> {
    let home = Path::new(&cfg.data_dir);
    let s = voice_core::bank::stats(home)?;
    println!("{}", serde_json::to_string_pretty(&s)?);
    Ok(())
}

// ------------------------------------------------------------------ GOP（S1）
fn gop_cmd(
    cfg: &Config,
    path: &str,
    text: Option<&str>,
    text_file: Option<&str>,
    top: usize,
    model_dir: Option<&str>,
    g2p_path: Option<&str>,
) -> Result<()> {
    use voice_core::gop::{self, G2p, PhonemeCtc};

    let store = Store::open(&cfg.data_dir)?;
    let src = resolve_path(&store, path)?;
    let decoded = audio::load_16k(&src)?;
    let reference = match (text, text_file) {
        (Some(t), _) => t.to_string(),
        (None, Some(f)) => {
            std::fs::read_to_string(f).with_context(|| format!("读取参考文本失败: {f}"))?
        }
        (None, None) => bail!("需要 --text 或 --text-file（GOP 必须有参考音素）"),
    };

    let model_dir = model_dir.unwrap_or("C:/voice_models/phoneme-ctc");
    let g2p_path = g2p_path.unwrap_or("C:/voice_models/g2p/cmudict.dict");
    let mut model = PhonemeCtc::load(model_dir)?;
    let g2p = G2p::load(Path::new(g2p_path))?;
    let r = gop::score_text(&mut model, &decoded.samples, &reference, &g2p)?;

    let mut worst: Vec<_> = r.phones.iter().collect();
    worst.sort_by(|a, b| a.z.partial_cmp(&b.z).unwrap_or(std::cmp::Ordering::Equal));
    let mean_z = if r.phones.is_empty() {
        0.0
    } else {
        r.phones.iter().map(|p| p.z).sum::<f32>() / r.phones.len() as f32
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "audio": src.to_string_lossy(),
            "source_sr": decoded.info.source_sr,
            "band_limited": decoded.info.band_limited,
            "g2p_entries": g2p.entries(),
            "frames": r.frames,
            "hop_ms": r.hop_ms,
            "phones_total": r.phones.len(),
            "mean_z": f2(mean_z),
            "oov": r.oov,
            "unmapped": r.unmapped,
            "words_worst": r.words.iter().take(5).map(|w| serde_json::json!({
                "word": w.word, "gop_mean": f2(w.gop_mean),
                "t0_ms": w.t0_ms, "t1_ms": w.t1_ms,
                "worst_phone": w.worst_phone, "worst_z": f2(w.worst_z),
            })).collect::<Vec<_>>(),
            "worst": worst.iter().take(top).map(|p| serde_json::json!({
                "phone": p.phone, "heard": p.best_symbol,
                "t0_ms": p.t0_ms, "t1_ms": p.t1_ms,
                "gop": f2(p.gop), "z": f2(p.z),
                "confusion": p.confusion,
            })).collect::<Vec<_>>(),
        }))?
    );
    Ok(())
}
