# Voice MCP Service

> **把"声音"变成 MCP 工具：本地离线、中英双语、纯 Rust 的语音输入输出 + 发音诊断服务。**
> 配合 Pi 的 Skill，构成一套能指出「你哪里读错了、错在哪个词」的英语口语教练。

```text
你说一句话 → ASR 转写 + 词级时间戳 → 与参考文本做差异对齐 → 语流指标
           → 音素级 GOP（词级口径，带 TTS 对照基线）→ LLM 只做解读与给一条具体建议
```

`RTF 0.013`（SenseVoice int8，CPU）· 音素 CTC `RTF 0.04–0.05` · 全程离线无出站请求 · 单文件二进制

---

## 目录

- [设计原则](#设计原则)
- [目标与进度](docs/GOALS.md)（近期 / 远期）
- [能力现状](#能力现状)
- [架构](#架构)
- [快速开始](#快速开始)
- [配置](#配置)
- [MCP 工具](#mcp-工具)
- [CLI](#cli)
- [Skills（口语教练 / 语音录入）](#skills)
- [留证机制](#留证机制)
- [实测性能](#实测性能)
- [供应链安全](#供应链安全)
- [设计决策（ADR）](#设计决策adr)
- [已知限制](#已知限制)
- [路线图](#路线图)
- [目录结构](#目录结构)

---

## 设计原则

这套东西最容易做错的地方不是模型，而是**职责边界**。三条铁律：

### 1. MCP 只做"测量"，Skill + LLM 做"教学"

Pi 里的 LLM **听不到音频**，它只能看到文本和数字。所以：

- MCP 的职责是产出 `{"kind":"substitution","reference":"third","said":"straight","t0":4.5,"phonetic_similarity":0.40}`
  这样的**测量结果**
- "怎么教、说什么、先修哪个"由 Skill 里的教学规则 + LLM 决定
- TTS 生成的示范音是给**你的耳朵**听的，不是给模型听的

这条原则决定了两件事：**评分逻辑不写进 MCP**，以及 **MCP 的输出必须是结构化 JSON 而不是一段话**。

### 2. 不确定就说不确定

`third → straight` 到底是"你发错了"还是"ASR 听错了"？从文本层面**无法判断**。
系统会输出 `hint: "unclear"` 并要求教练给出验证动作，而不是猜一个结论——
**错误的纠音建议比不给建议更糟**。详见 [ADR-0006](docs/adr/0006-assessment-scope.md)。

### 3. 每次调用都留证，只追加不覆盖

原始录音、派生音频、转写 JSON、指标 JSON、反馈 markdown，全部落盘并可 grep。
这是"趋势分析"和"事后复听"的前提，也让每次模型升级/算法升级都可回溯。

---

## 能力现状

| 能力 | 状态 | 说明 |
|---|---|---|
| 任意格式解码（wav/m4a/mp3/flac/ogg） | ✅ | 纯 Rust（symphonia），Win11 录音机 `.m4a` 直读，1.8 ms / 6 s |
| ASR（中英 + 词级时间戳） | ✅ | SenseVoice int8，RTF 0.013，CPU 即可 |
| ASR（中英混说 / 标点增强） | ✅ | whisper-large-v3-turbo int8，按需加载 |
| TTS（示范音，54 音色） | ✅ | Kokoro-82M，24 kHz，RTF 0.21 |
| 麦克风采集 + 静音自动停 | ✅ 实测通过（48 kHz 单声道，自适应阈值） | cpal / WASAPI；蓝牙耳机需选 Hands-Free 端点 |
| 播放（服务端出声） | ✅ | rodio（Pi 的 TUI 放不了音频，必须服务端播） |
| 目录监听（录音机新文件自动导入） | ✅ | 轮询 + 大小稳定判定 + 状态文件去重 |
| 语流指标（语速/停顿/词界/填充词/重复） | ✅ | Silero VAD + 词时间戳 |
| 词级差异（漏读/多读/替换 + 音形相似度） | ✅ | Levenshtein 对齐 + 自研音形编码 |
| **音素级 GOP（词级口径）** | ✅ | 音素 CTC（wav2vec2-espeak ONNX）+ CTC-Viterbi + 说话人内归一化；so762 词级 AUROC 0.887 |
| **TTS 对照基线** | ✅ | 同一参考文本用标准发音跑一遍 GOP，区分"用户读错"与"模型分辨不出" |
| **混淆对信号**（θ→s 等） | ✅ | 仅 TH 系列达到可用精度（P 0.50 / R 0.67），其余只作内部标记 |
| **跟读素材库** | ✅ | NCE1–4 共 4 997 条逐句切片 + 10 条 IELTS prompt（共 5 007 条，7.1 h），tag 权重来自真实错读率统计 |
| **音频质量闸门** | ✅ | SNR<13 dB / peak<−30 dBFS / 削波>0.1% → `degraded`，不出诊断结论；短语音（<3 s、peak ≥−25）按 D47 豁免 |
| **文本可用性闸门** | ✅ | 双引擎词级一致率 <0.7 → `degraded`，只出语流/韵律，不出词级/音素级结论（D49） |
| **TTS 示范音验证** | ✅ | OOV 预检 + 双 ASR/音素/WPM/起始伪影；`bank refs` 批量生成、逐条 3/3 `verified` 才落盘（D51） |
| **定向训练计划** | ✅ | `bank plan`：drill_queue → error_stats/focus tags → 同难度补足；固定含 60 s 自由说槽位 |
| **韵律分析**（F0 轮廓/句末升降/平调/音域/重音候选/DTW） | ✅ | 纯 Rust（YIN）；`assess(with_prosody=true)` 或 `reference_audio`，只给数值不下结论 |
| 流式 / HTTP 模式 / 热键听写 | ⏳ P4 | – |

---

## 架构

```text
┌─────────────────────────────────────────────────────────────┐
│  Pi（LLM + Skill）                                           │
│  ielts-speaking-coach：决定"练什么、怎么反馈、先修哪个"        │
│  voice-dictation     ：把口语整理成可用的文字                  │
└──────────────────────┬──────────────────────────────────────┘
                       │ MCP (stdio)
┌──────────────────────▼──────────────────────────────────────┐
│  voice-mcp（Rust, 常驻，8 个工具）                            │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ voice-core                                           │   │
│  │  audio   : decode / resample(Kaiser-sinc) / capture   │   │
│  │            / play / wav / 声学体检                     │   │
│  │  engine  : asr(SenseVoice + whisper 路由)             │   │
│  │            tts(Kokoro) / vad(Silero)                  │   │
│  │  assess  : normalize / diff(音形编码) / flow           │   │
│  │  gop     : 音素 CTC(ONNX) / CTC-Viterbi / GOP / 词级聚合 │   │
│  │  bank    : 素材库（ingest / pick / 质量门槛 / tag 权重）  │   │
│  │  store   : 留证台账（session / take / manifest）        │   │
│  └──────────────────────────────────────────────────────┘   │
└──────────────────────┬──────────────────────────────────────┘
                       │
┌──────────────────────▼──────────────────────────────────────┐
│  sherpa-onnx 2.0.1（in-process C API via PyPI 轮子里的 DLL）  │
│  onnxruntime 1.31（load-dynamic，音素 CTC 走 ort）            │
└─────────────────────────────────────────────────────────────┘

旁路：voice-cli（doctor / stt / say / play / rec / watch / assess / gop / bank / lesson / cases / mic / tts-probe）
```

**为什么 ASR/TTS 用 sherpa-onnx 而不是自己拼 ONNX**：它一个原生库同时提供
ASR（18 种模型类型：whisper / SenseVoice / paraformer / nemo / qwen3-asr…）、
TTS（Kokoro / VITS / Matcha）、VAD（Silero）、说话人嵌入，省掉了 3 套前后处理与特征提取代码。
且它的 Windows 共享库可以直接从 PyPI 轮子里取（不依赖 GitHub、不需要 C++ 工具链），
详见 [ADR-0005](docs/adr/0005-deps-and-model-acquisition.md)。

---

## 快速开始

### 前置

- Windows 10/11（其他平台可编译，但默认路径与录音机目录是 Windows 的）
- Rust 1.99+（实测；edition 2024 的理论下限 1.85 未验证；MSVC 工具链，`x86_64-pc-windows-msvc`）
- Python 3.9+（**只用于拉依赖脚本**，运行时不需要 Python）
- 磁盘：模型 1.6 GB + 留证数据

### 1. 拉原生库（≈20 秒，不用编译 C++）

```powershell
powershell -ExecutionPolicy Bypass -File tools/fetch_deps.ps1
```

它从 PyPI 轮子里解出 `sherpa-onnx-c-api.dll` + `onnxruntime.dll`，并用 `dumpbin`+`lib` 补出缺失的
import library，落到 `.cache/sherpa/lib/`（`.cargo/config.toml` 里的 `SHERPA_ONNX_LIB_DIR` 已指向该目录）。

### 2. 拉模型（≈1.58 GB，多连接分段下载）

```powershell
powershell -ExecutionPolicy Bypass -File tools/fetch_models.ps1            # 全部
powershell -ExecutionPolicy Bypass -File tools/fetch_models.ps1 -List      # 只看清单
powershell -ExecutionPolicy Bypass -File tools/fetch_models.ps1 -Sets vad,kokoro
```

默认装到 `C:/voice_models/`。下载源是 hf-mirror（8 路分段并发，约 1.1 MB/s）。

> **音素级 GOP 需要额外 2.4 GB**（可选，见 3.5）：音素 CTC 模型 fp32 1 205 MB + MDD 微调版 1 205 MB。
> 不需要 GOP 时 `assess(with_gop=false)` 不会加载它们。

### 3. 构建

```bash
cargo build --release
cp .cache/sherpa/lib/*.dll target/release/      # DLL 需与 exe 同目录
./tools/check.ps1                               # 门禁：fmt + clippy -D warnings + test
```

产物：

```
target/release/voice-mcp.exe   # MCP 服务
target/release/voice-cli.exe   # 客户端
```

### 4. 音素级 GOP（可选，约 2.4 GB）

```bash
python tools/export_phoneme_ctc.py --out C:/voice_models/phoneme-ctc   # 一次性：导出 fp32 ONNX
mkdir -p C:/voice_models/g2p && pip install cmudict                     # CMUdict（135 166 词）
cp <onnxruntime 包>/capi/onnxruntime.dll C:/voice_models/onnxruntime/   # ort 动态加载（需 ≥1.20）
```

- 模型指纹写在 `C:/voice_models/phoneme-ctc/metadata.json`，加载时会校验 sha256，不符即拒绝启用；
- 想用 MDD 微调版：`VOICE_PHONEME_MODEL=C:/voice_models/phoneme-ctc-ft`（默认即此，找不到回退基线）；
- 标定与提升实验见 [docs/phoneme-gop.md](docs/phoneme-gop.md)、[docs/mdd.md](docs/mdd.md)。

### 5. 自检

```bash
./target/release/voice-cli.exe doctor --load
```

```
[OK] 9 个模型文件齐全
[OK] 数据目录可写 D:\voice_data
[OK] 最近 take 解码验证 001_dictate.m4a -> 6.08s @16k, peak -0.2 dBFS
[OK] ASR + TTS(54 音色 @24kHz) + VAD 加载完成 1726 ms
结论: OK=12 FAIL=0
```

### 6. 接入 Pi

```bash
pi mcp add voice -- <项目绝对路径>/target/release/voice-mcp.exe
```

然后编辑 `~/.pi/agent/mcp.json`，把 `exposure` 改成 `direct`（默认 `codemode` 只能在脚本里调），
并抬高 `timeout`（录音/长音频需要）：

```json
{
  "mcpServers": {
    "voice": {
      "command": "D:/code/voice_mcp_service/target/release/voice-mcp.exe",
      "exposure": "direct",
      "timeout": 180,
      "description": "本地语音输入输出与音频体检（中英双语，离线）"
    }
  }
}
```

装 Skill（用户级，跨项目可用）：

```powershell
powershell -ExecutionPolicy Bypass -File tools/install_skills.ps1
```

验证：

```bash
pi mcp list          # 应显示 voice: connected, 8 tools (direct)
pi -p "列出你现在可用的 skills"
```

然后在 Pi 里直接说：

```
练一下口语
把刚才说的记下来
这个 m4a 转成文字
```

---

## 配置

优先级：**默认值 ← `$VOICE_HOME/config.json` ← 环境变量**

| 变量 | 默认 | 说明 |
|---|---|---|
| `VOICE_MODELS_DIR` | `C:/voice_models` | 模型根目录（只读） |
| `VOICE_HOME` | `D:/voice_data` | 留证/档案/索引目录 |
| `VOICE_PROVIDER` | `cpu` | `cpu` / `cuda`（GPU 未打通，见 ADR-0005） |
| `VOICE_THREADS` | `4` | ONNX Runtime 线程数 |
| `VOICE_TTS_WPM` | `140` | 示范音目标语速（雅思 130–160；慢速 110） |
| `VOICE_TTS_LEXICON` | `$VOICE_HOME/tts-lexicon.txt` | 项目级 TTS 词表（覆盖内置 lexicon；OOV 预检） |
| `VOICE_MIC` | 空（系统默认） | 输入设备名子串。**蓝牙耳机必须指定**，如 `Hands-Free` |

`config.json` 还支持：`en_engine`（英文默认引擎，默认 `sense-voice`）、`record_max_sec`、
`record_silence_sec`、`capture_sr`、`default_sid`、`tts_wpm`、`tts_lexicon`、`watch_dirs`。

---

## MCP 工具

共 8 个。完整出入参契约见 **[docs/CONTRACT.md](docs/CONTRACT.md)**。

| 工具 | 一句话 |
|---|---|
| `assess` | **核心**：转写 + 语流 + 词级差异；`with_gop=true` 追加音素级 GOP（词级口径），`baseline_tts=true` 追加标准发音对照基线 |
| `transcribe` | 纯转写（词级 + token 级时间戳） |
| `synthesize` | 示范音（默认 **am_michael 男声** + 140 wpm；`slow_too` 出 110/140 一对；`play=true` 服务端出声） |
| `record` | 麦克风录音，静音自动停（需麦克风） |
| `play` | 播放 wav |
| `prepare_audio` | 音频体检 + 去首尾静音 + 16k 派生 |
| `voices` | 列出 54 个音色 |
| `status` | 自检（含 `mic` 是否为 null） |

所有工具返回同一个信封，失败不抛协议错误：

```json
{ "ok": true,  "detail": { ... }, "error": null }
{ "ok": false, "detail": null,    "error": "人类可读原因" }
```

---

## CLI

```bash
voice-cli doctor [--load]                     # 自检（--load 会真实加载引擎）
voice-cli mic [--secs 3]                      # 麦克风诊断：支持的格式 + 底噪分位数 + 阈值建议
voice-cli stt <path|last> [--lang en] [--plain]# 转写（留证）
voice-cli say "text" [--voice am_michael] [--speed <倍率>] [--slow-too] [--play] # 默认 140 wpm / 男声
voice-cli tts-verify "text" [--voice am_michael] [--runs 3] [--no-phones]      # TTS 参考音验证（双 ASR/音素/WPM/起始伪影）
voice-cli bank refs [ids...] --kind prompt --runs 3 --out DIR                   # 批量生成示范音并逐条验证（verified 才落盘）
voice-cli bank ingest-audio <mp3> --lesson <label> [--srt s.srt] [--dry-run]    # 单文件真人音频入库（有字幕用字幕，无字幕走 VAD + 双引擎验证）
voice-cli tts-audit --manifest <jsonl> --out <report.jsonl> [--no-phones]    # 只读审计现有 TTS 资产
voice-cli play <path>
voice-cli assess <path|last> [--reference "原句"] [--with-gop]   # 评测（含词级 GOP + TTS 基线）
voice-cli gop <path|last> --text "原句" [--top N]  # 只跑音素级 GOP（输出 heard / confusion）
voice-cli bank ingest|list|show|drill|stats|pick|plan|refs|migrate|import|retag # 跟读素材库 / 定向计划 / TTS 示范音
voice-cli lesson <mp3> --lrc <lrc> [--verbose] [--write-lrc]              # 教材对齐自检
voice-cli cases [--only <id>]                     # 跑录音回归用例
voice-cli rec [--max-sec 30] [--silence 1.0] [--stt]   # 录音（需麦克风）
voice-cli watch [--dir <目录>] [--lang en]    # 监听录音目录，新文件自动导入+转写
voice-cli tts-probe                           # 为 54 个音色各生成一句，供人耳校准
```

`watch` 默认监视 Win11 录音机的输出目录（`Documents\录音` / `Documents\Sound Recordings`），
首次运行会把已存在文件登记为基线，只处理**新**录音。

---

## Skills

| Skill | 用途 |
|---|---|
| [`ielts-speaking-coach`](skills/ielts-speaking-coach/SKILL.md) | 口语陪练 + 纠音，目标雅思 7 分。固定五步循环：播示范 → 跟读 → 评测 → 只指出 1 个问题 → 重读对比 |
| [`voice-dictation`](skills/voice-dictation/SKILL.md) | 语音录入 / 听写。口语转书面文字（删填充词、补标点、中英混说不翻译） |

教练 Skill 的几个关键设计：

- **教学语言阶梯 L1–L4**：L1 全中文讲解 + 单词级慢速示范（薄弱用户压力小），L4 全英文讨论 Part 3；
  按 `learner.json` 自动升降档，用户随时可以说"用中文讲"
- **一次只修 1–2 个问题**，其余进 `error_stats` 排队
- **问题优先级**：可理解度（漏读/替换）> 流利度（停顿/词界/填充词）> 韵律 > 词汇语法
- **档案**：`$VOICE_HOME/learner.json` 记录水平区间、focus、易错音素榜、练习队列、历史趋势
- 参考资料（按需加载）：`phoneme-map.md`（17 个高频错误 + 最小对立对 + 一句口诀）、
  `band-descriptors.md`（官方描述 → 可测量代理指标）、`drills.md`（10 种训练法）、`ielts-topics.md`（题库 + 7 分范文）

---

## 回归用例

真实录音的回归用 JSON 断言描述，**音频留本机不进仓库**：

```bash
export VOICE_CASES_DIR="D:/voice_data/cases_audio"   # 放你的 wav/m4a
voice-cli lesson <mp3> --lrc <lrc>  # 用 LRC 当基准逐行核对转写与时间戳
voice-cli cases                    # 跑 cases/*.json
voice-cli cases --only r03         # 只跑一条
```

现有 **9 条**（覆盖 16 kHz 录音机 / 48 kHz 麦克风 / TTS 完美音 / 自由说 / 漏词误判 / 教材原生音频 / 真实误读）：

| 用例 | 锁住什么 |
|---|---|
| `r01_third_16k` | 16 kHz 音频下 `third→straight`、`band_limited=true`、`hint=unclear` |
| `r02_mic48k_clean` | 48 kHz 读对：`accuracy=1.0`、0 问题、`band_limited=false` |
| `r03_mic48k_pause_drop_is` | ASR 漏掉 `is` 时必须是 `likely_asr_miss` + `medium`，不能当 high 指控 |
| `r04_tts_perfect` | TTS 音频零误报（证明 diff 流水线本身干净） |
| `r05_free_speech_flow` | 无参考文本时语速落在合理区间（分析窗口生效） |
| `r06_nce2_l2_native` | 教材原生 24 kHz MP3（81 s）：mp3 解码、长音频分块、合成词合并 |
| `r07_nce2_l2_free` | 同一教材音频不做参考比对时 `articulation_rate ∈ [110,200]` |
| `r08_nce2_l1_us` | 英美拼写变体（theatre/theater）与 ASR 伪缩略（did't）**不得**算发音错误 |
| `r09_why_when` | **用户确认真值**：`why→when` 必须报 substitution；同一条 GOP 会漏检 → 必须交叉验证 |

`r09` 的来源值得一看：真人真机录音里用户把 `why` 读成 `when` 并确认是自己的错，
而 GOP 词级给出 +2.24（漏检）—— 这条用例把"整词替换靠词级 diff、GOP 只做辅助"钉死。

---

## 留证机制

```text
D:/voice_data/
├── learner.json                 # 学习者档案（Skill 维护）
├── manifest.jsonl               # 全局追加索引，可 grep
├── last_take                    # 最近一次 take 的原始音频（path:"last" 指向它）
├── watch_state.json             # 目录监听的已处理文件指纹
└── sessions/2026-11-19/003545_assess/
    ├── session.json
    └── takes/
        ├── 001_read_third.m4a        # 原始输入，永不修改
        ├── 001_read_third.16k.wav    # 派生（可重建）
        ├── 001_read_third.asr.json   # 转写 + words[] + tokens[]
        ├── 001_read_third.metrics.json  # 全部评测数值 + scorer_version
        ├── 001_read_third.md         # 该 take 的反馈（Agent 写）
        └── 001_read_third.ref_{slow,norm}.wav  # 示范音
```

`manifest.jsonl` 一行：

```json
{"ts":"2026-11-19T00:25:10+08:00","session":"2026-11-19/002508_dictate","seq":1,
 "scene":"read_third","src":"...001_read_third.m4a","lang":"en","engine":"sense-voice",
 "dur":6.08,"text":"I think this is the straight time.","degraded":false,"feedback_ref":null}
```

---

## 实测性能

本机：Core Ultra 5 245K / 48 GB / RTX 5070 Ti 16 GB（CPU 推理，未启用 GPU EP）

| 项 | 数值 |
|---|---|
| 模型常驻总大小 | 1.58 GB（9 个文件，全部预加载）；**GOP 音素模型另加 1.2–2.4 GB（按需懒加载）** |
| 引擎冷启动 | 1726 ms（ASR 与 TTS 并行加载） |
| `.m4a` 解码（6.08 s） | 1.8 ms（RTF 0.0003） |
| 麦克风采集（48 kHz → 16 kHz，2.22 s 语音） | 自动停止 5.74 s 停；ASR 41 ms（RTF 0.019） |
| SenseVoice 转写（6.08 s） | 82 ms（**RTF 0.013**） |
| whisper-turbo 转写（6.08 s） | 883 ms（RTF 0.145，8 线程） |
| Kokoro 合成（1.7 s 音频） | 372 ms（**RTF 0.21**） |
| 端到端 `assess`（解码+ASR+flow+diff） | ≈ 120 ms |
| 音素 CTC 推理（3.4 s 音频） | **110–173 ms（RTF 0.04–0.05）**，fp32 ONNX 1 205 MB（int8 漂移过大已弃用） |
| `assess(with_gop)` 首次调用 | +1–2 s（加载 1.2 GB 模型，之后常驻） |
| GOP 标定（so762，未见说话人） | 句级 ρ 0.67；**词级 AUROC 0.887 / P@FP10 0.457**；音素级 0.769 / 0.170 |
| MDD 微调 | 音素级 AUROC 0.690 → 0.769；训练 65 s/epoch（RTX 5070 Ti, cu128） |
| 目录监听响应 | 新文件落盘后 ≈ 1.5–3 s 内完成导入+转写 |

---

## 供应链安全

模型是数据、DLL 是代码，风险不同，分层校验（`python tools/verify_models.py`，可重跑，失败退出码 1）：

| 检查 | 结果 |
|---|---|
| 367 个模型文件 sha256 与上游清单一致 | ✅ |
| ONNX 无自定义算子域 / 无 `external_data` 外部引用 | ✅ |
| 无伪装成数据的可执行文件（MZ/ELF/shebang） | ✅ |
| 3 个 DLL 未命中 23 项危险 Win32 API（注入/下载/提权） | ✅ |
| DLL 内嵌 URL 逐条人工核验（全是论文/文档链接） | ✅ |
| PyPI 轮子双镜像 sha256 交叉一致 | ✅ |
| 已知不良：DLL 未签名（开源轮子常态） | ⚠️ 知情 |

评测数据集与音素模型同样有四层校验（`python tools/fetch_datasets.py`）：

| 检查 | 结果 |
|---|---|
| speechocean762（645 MB）sha256 == 上游 LFS oid + 钉死 revision | ✅ |
| Parquet 魔数 + 行数/说话人/分值域/ARPAbet 音素集/音频时长等语义不变量 | ✅ |
| 音素 CTC ONNX 的 sha256 与 `metadata.json` 一致 | ✅ |
| 失败即移入 `.quarantine/`，退出码 2，绝不静默继续 | ✅ |

详见 [docs/security-supply-chain.md](docs/security-supply-chain.md)、[docs/dataset-pins.json](docs/dataset-pins.json)
与基线哈希 [docs/security-hashes.txt](docs/security-hashes.txt)。

---

## 设计决策（ADR）

| ADR | 内容 |
|---|---|
| [0001](docs/adr/0001-runtime-strategy.md) | 运行时路线：当前 Rust 主干 + Python 评分 sidecar，长期 100% Rust |
| [0002](docs/adr/0002-asr-routing.md) | ASR 双引擎与语言路由 + 教学语言阶梯 L1–L4 |
| [0003](docs/adr/0003-model-selection.md) | 模型选型与显存预算 |
| [0004](docs/adr/0004-audio-io-and-evidence.md) | 文件优先的 I/O 契约与留证机制 |
| [0005](docs/adr/0005-deps-and-model-acquisition.md) | 依赖与模型获取策略（GitHub 不可用时的替代通路） |
| [0006](docs/adr/0006-assessment-scope.md) | 评测层能力边界（三态推断、为什么不能硬猜） |
| [0007](docs/adr/0007-prosody-and-gop.md) | 韵律分析与音素级 GOP（**两者都已上线**，纯 Rust；见 [docs/phoneme-gop.md](docs/phoneme-gop.md) / [docs/mdd.md](docs/mdd.md)） |
| [0009](docs/adr/0009-drill-library.md) | 跟读素材库：NCE1–4 逐句切片、质量门槛、难度分级（**Accepted，5 007 条已入库**） |
| [0010](docs/adr/0010-free-speech-pseudo-gop.md) | **Route A：无参考自由说的伪参考 GOP**（立项，Deferred；含可行性实测与 Go/No-Go） |
| [0008](docs/adr/0008-technical-debt.md) | **技术欠债登记**：51 条债、严重度、触发偿付条件、偿付顺序 |

---

## 已知限制

> 完整清单（含验证状态与偿付触发条件）见 [ADR-0008 技术欠债登记](docs/adr/0008-technical-debt.md)。


1. **蓝牙耳机不要用默认端点**：实测 EDIFIER W820NB 的默认输入（A2DP）能打开、回调在跑，但录到的是
   稀疏零值（2.4% 非零、p90 −120 dBFS）。请到「设置 → 系统 → 声音 → 输入」选名称含 **Hands-Free** 的端点，
   或用 `VOICE_MIC="Hands-Free"`。代码里已有电平防线，会明确报错而不是把假音频当正常录音。
   纠音本身也建议用 USB/有线麦：蓝牙 HFP 只有 16 kHz 且带强降噪，会削掉 /θ/ /s/ 的摩擦音细节。
2. **16 kHz 源音频**（Win11 录音机固定输出）→ 6–8 kHz 摩擦音细节有限，
   `band_limited: true` 时不要据 /θ/ vs /s/ 下结论。要更高精度需要 48 kHz 采集（`cpal`）。
3. **SenseVoice 时间戳是 CTC token 级**（±0.5 s）→ "第几秒"只能当参考。
4. **音素级 GOP 只能当排序信号**：so762 标定 FP=10% 时音素级精确率仅 0.17（基率 7.5% ⇒ 数学上限 0.45），
   所以对外只用**词级**（AUROC 0.887 / P 0.457），且必须带 TTS 对照基线；"你读错了 /θ/" 这类断言一律禁止。
   只有 `TH→s` 混淆对（P 0.50 / R 0.67）允许说"模型听到的是 /s/"。
5. **音频质量闸门**：`snr < 13 dB`、`peak < −30 dBFS`、削波 > 0.1% 任一命中 → `degraded=true`，
   不出任何发音/流利度结论（实测低电平录音会让 ASR 整句丢失）。蓝牙耳机增益会自动漂移，开练前先确认 `peak` 在 −20 ~ −12 dBFS。
6. **音形编码不区分元音长短**（bit/beat、bad/bed 同 key）；CTC spike 使每个音素只有 1–2 帧，
   所以**不能用 GOP 判断元音时长**（/ɪ/ vs /iː/ 只能靠 `heard` 与实际听感）。
7. **中文只做字级比较**，无拼音层。
8. `accuracy` 只用于**纵向趋势**，不要换算成雅思分数对外承诺。
9. **长音频必须走分块解码**（`transcribe_long`）：单次喂 >30 s 音频会让时间戳压缩、尾部内容丢失。
   这个坑实测会让"81 s 音频只转出 57 s"。业务层请勿直接调用 `asr.transcribe`。
10. GPU EP 未打通（`VOICE_PROVIDER=cuda` 暂不可用）；训练侧已用 CUDA 12.8（`torch 2.10+cu128`）。
11. 自动停止在 AGC 强的设备上仍可能略拖尾（已用「相对语音峰值」判据缓解）；
   底噪高于 −50 dBFS 的设备建议先关掉系统「音频增强」，否则噪声会吃掉摩擦音。

---

## 路线图

| 阶段 | 内容 | 状态 |
|---|---|---|
| P0 | 环境验证（解码 / 原生库 / ASR / TTS / 供应链校验） | ✅ |
| P1 | 音频层 + 8 个 MCP 工具 + CLI + 目录监听 | ✅ |
| P2 | 评测层：词级 diff + flow + 韵律 | ✅ |
| P2.5 | 音素级 GOP（纯 Rust：ONNX + CTC-Viterbi + 词级聚合 + TTS 基线） | ✅ 2026-11-22 |
| P2.5 | 韵律分析（F0 轮廓 / 句末升降 / 平调 / 重音候选 / DTW） | ✅ 2026-11-23 |
| P3.5 | MDD 微调（speechocean762 微调音素 CTC） | ✅ 音素级 AUROC 0.690→0.769 |
| P3 | `ielts-speaking-coach` / `voice-dictation` Skill | ✅ |
| P4 | **跟读素材库**（NCE 276 课 → 4 997 条 + 10 条 IELTS prompt，见 [ADR-0009](docs/adr/0009-drill-library.md)） | ✅ |
| P4 | 热键听写 daemon / HTTP 模式 / 流式 TTS / 音色复刻 | – |

---

## 目录结构

```text
.
├── Cargo.toml                  # workspace
├── crates/
│   ├── voice-core/             # 核心库（无 MCP 依赖）
│   │   └── src/{config, store, audio/*, engine/*, assess/*, gop/*, bank/*}
│   ├── voice-mcp/              # MCP 服务（rmcp 3.11）
│   ├── voice-cli/              # 客户端 CLI
│   └── spike/                  # P0 验证用的小程序（保留作为证据）
├── skills/
│   ├── ielts-speaking-coach/   # SKILL.md + references/{phoneme-map,band-descriptors,drills,ielts-topics}
│   └── voice-dictation/
├── docs/
│   ├── CONTRACT.md             # MCP 工具契约（含给 Agent 的解读规则）
│   ├── GOALS.md                # 目标与进度：近期 / 中期 / 远期（含验收）
│   ├── STATE.md                # 交接页：现状 / 实测数字 / 待办 / 债务
│   ├── levels.md               # 水平与就绪等级 L0–L4 + 自由说 gate
│   ├── phoneme-gop.md          # 音素级 GOP：选型、标定、混淆对、边界
│   ├── mdd.md                  # MDD 微调实验：协议、结果、环境
│   ├── datasets.md             # 评测数据集：校验方法、数据画像、tag 权重
│   ├── dataset-pins.json       # 数据集指纹（revision + sha256）
│   ├── adr/                    # 0001–0009
│   └── security-{supply-chain.md,hashes.txt}
├── tools/
│   ├── fetch_deps.ps1          # 原生库（PyPI 轮子 → .cache）
│   ├── fetch_models.ps1        # 模型（hf-mirror 多连接）
│   ├── verify_models.py        # 模型供应链校验
│   ├── fetch_datasets.py       # 评测数据集下载 + 四层校验
│   ├── derive_tag_weights.py   # 从 so762 推导素材 tag 权重
│   ├── export_phoneme_ctc.py   # 音素 CTC → ONNX（一次性）
│   ├── calibrate_gop.py        # GOP 标定 / A-B（AUROC、P/R）
│   ├── mdd_train.py            # MDD 微调（dump/train/export/eval）
│   └── install_skills.ps1      # Skill 安装（junction）
└── .pi/{mcp.json,settings.json}  # Pi 项目级配置
```

---

## 许可与模型条款

本项目代码可自由修改复用。引入的模型权重遵循各自许可：

| 模型 | 许可 |
|---|---|
| SenseVoice-Small | Apache-2.0（FunAudioLLM） |
| whisper-large-v3-turbo | MIT |
| Kokoro-82M | Apache-2.0 |
| Silero VAD | MIT |
| sherpa-onnx / onnxruntime | Apache-2.0 / MIT |
| MMS_FA（P2.5 计划引入） | **CC-BY-NC 4.0（仅限非商用）** |
