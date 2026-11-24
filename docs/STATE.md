# 当前状态与下一步（交接用）

> 写于 2026-11-19，最后更新 2026-11-24。新会话先读 [GOALS.md](GOALS.md)（近期/远期目标与进度），
> 再读这一页、README / CONTRACT / ADR。

## 一句话现状

**P3 闭环 + P4 素材库（NCE 4 997 + IELTS prompt 10 = 5 007 条）+ 音素级 GOP（词级口径）+ 韵律分析均已上线并在真机验证。**
`crates` 10 400+ 行 Rust，**97 个单测 + 9 个录音回归用例**全绿；`fmt` / `clippy -D warnings` / `test`
已由 `tools/check.ps1` 与 GitHub Actions 守住（ADR-0008 D45）。
下一步：雅思素材入库 + 按 `error_stats` 做定向训练。

## 已实测的关键数字（别再猜）

| 项 | 数值 |
|---|---|
| ASR SenseVoice int8 | 6 s 音频 82 ms（RTF 0.013）；81 s 音频分块后 2.3 s（RTF 0.028） |
| TTS Kokoro 82M | RTF 0.21，24 kHz，54 音色；**默认 140 wpm**（130–160），slow=110；短文本/中文不按 WPM 校准 |
| 引擎冷启动 | 1.7 s（ASR 与 TTS 并行加载） |
| `.m4a`/mp3 解码 | 纯 Rust（symphonia），6 s 音频 1.8 ms |
| 麦克风（EDIFIER W820NB 蓝牙 HFP 端点） | 48 kHz 单声道 F32 可用；底噪 p50 −54.7 / p99 −46 dBFS（**偏高，AGC 强**） |
| 长音频 | **必须分块**（>30 s 单次解码会静默截断 + 时间戳压到 0.77 倍） |
| 教材对齐精度 | NCE2 L2：19/19 行文本相似度 1.00，行起始偏差 +0.07~+0.29 s，LRC 拟合 scale 0.999 |
| **音素 CTC（wav2vec2-espeak）** | ONNX 1 205 MB，CPU **RTF 0.04–0.05**；int8 量化漂移 8.21 → 弃用 |
| **GOP 判别力（so762 未见说话人）** | 句级 ρ 0.67；**词级 mean-GOP：AUROC 0.887、P@FP10 0.457**；音素级 0.769/0.170（基率上限 0.45） |
| **MDD 微调（6ep/8L）** | 词级 AUROC 0.883→0.887、音素级 0.690→0.769；训练 65 s/epoch（RTX 5070 Ti cu128） |
| **混淆对信号（TH→s）** | 命中精确率 0.50、召回 0.67（仅 TH 系列可对用户说） |
| **韵律（YIN，纯 Rust）** | 51.8 s 真人：F0 中位 116.3 Hz、起伏 3.31 st、音域 12.61 st、句末 −0.38 st→flat；TTS 同文本 DTW 0.11 st |
| 素材库 | **5 007 条 / 7.1 h**（NCE 4 997 + IELTS prompt 10），quarantine 1 221，可直接练 3 378 |
| 素材库 ingest | 276 课 ≈ 10.5 min；`bank stats` 是唯一权威计数 |

## 环境

```
模型   C:/voice_models     （1.58 GB，9 个文件，已供应链校验）
数据   D:/voice_data       （config.json 里 mic="EDIFIER"，silence=2.2 s）
素材   D:/voice_data/cases_audio （回归用例音频，VOICE_CASES_DIR 指向它）
代码   D:/code/voice_mcp_service
Pi     ~/.pi/agent/mcp.json 里 voice = target/release/voice-mcp.exe（exposure=direct, timeout=180）
Skill  tools/install_skills.ps1 已 junction 到 ~/.pi/agent/skills/
```

## 常用命令

```bash
cargo build --release && cp .cache/sherpa/lib/*.dll target/release/
./tools/check.ps1                                # 门禁：fmt + clippy -D warnings + test（提交前必跑）
./tools/check.ps1 -Cases                         # 额外跑 9 条录音回归（需 VOICE_CASES_DIR）
./target/release/voice-cli.exe doctor --load     # 12 项自检
./target/release/voice-cli.exe mic --secs 3      # 麦克风能力 + 底噪 + 阈值建议
./target/release/voice-cli.exe rec --max-sec 20 --stt --lang en
./target/release/voice-cli.exe assess <audio|last> --reference "原句" --with-gop   # 词级+音素级 GOP + TTS 基线
./target/release/voice-cli.exe assess <audio|last> --with-prosody --reference-audio <示范.wav>  # 韵律 + DTW
./target/release/voice-cli.exe gop <audio> --text "原句"                          # 只跑 GOP（看 heard/confusion）
./target/release/voice-cli.exe bank plan --minutes 15 --limit 8                   # 定向训练计划（含 60 s 自由说槽位）
./target/release/voice-cli.exe bank plan --minutes 15 --limit 8 --no-free-speech # 强制省略自由说槽位
./target/release/voice-cli.exe bank plan --minutes 15 --limit 8 --free-speech    # 强制包含（试一次）
./target/release/voice-cli.exe bank pick --tag theta_to_s --level 2 --limit 3     # 按 tag 权重取材
./target/release/voice-cli.exe bank drill nce1/019/003                            # 播原生音频
./target/release/voice-cli.exe lesson <mp3> --lrc <lrc> --verbose
VOICE_CASES_DIR=/d/voice_data/cases_audio ./target/release/voice-cli.exe cases
```

## 阻塞与前置条件

1. **蓝牙耳机必须处于连接状态**才能在系统里看到输入设备（休眠时 `mic` 会明确报错）。
   设备名会带 `2- ` 之类前缀且会变 → 配置用品牌子串 `EDIFIER`，匹配器已做"宽松匹配 + 宽带优先"。
2. Skill 需要 `/reload`（或重启 pi）才出现在会话里；`pi -p "列出可用 skills"` 可验证。
3. 没有麦克风时一切文件路径功能仍可用（`assess path=/stt`），只是不能 `record`。

## P3 真机训练闭环（2026-11-21 已实跑验收）

设备：EDIFIER W820NB（蓝牙 HFP，48 kHz 单声道）。留证：`sessions/2026-11-21/*_drill/takes/`。
会话小结：`sessions/2026-11-21/233336_drill/session.md`。建档：`D:/voice_data/learner.json`。

- [x] 1. `mcp__voice__*` 8 个工具与 2 个 Skill 都在（`pi -p "列出可用 skills"` 实测 3 个 skill，含 archify）
- [x] 2. 建档：3 句诊断（accuracy 1.00 / 1.00 / 1.00）+ retry → `learner.json`（band 6.0，confidence low：未做自由说）
- [x] 3. 单轮训练：`synthesize(slow_too, play)` → `record(silence_sec=2.2)` → `assess(reference=句)` → 只报 1 条（语速 214.7 wpm）→ `retry` 对比（192.7 wpm，断点 0.96 s）
- [x] 4. 留证完整：同一 take 目录内 `.raw48000.wav` + `.16k.wav` + `.asr.json` + `.metrics.json` + `.md`（+ `.capture.json`）；重复评测留 `.metrics.prev.json`
- [x] 5. Skill 铁律在实测中全部生效：`unclear` 不下结论、whisper（`timestamp_alignment=missing`）不报秒数、单轮只报 1 条
- [x] 6. 实跑暴露的问题已修（共 5 个）：`slow_too` 语速对、assess 证据分裂、中文 panic、16 kHz 派生污染 `band_limited`、英美拼写变体与 ASR 伪缩略误报

**无稿自由说改为 readiness gate（2026-11-24）**：当前 `readiness.stage = L2+`，槽位关闭；
`band_estimate` 6.0（5.5–6.5），confidence medium，只作**跟读/朗读下界**。
达标 L3（连续 3 条 acc≥0.90 等）后再做 30–45 s 提纲独白，L4 才开启 60 s 无稿自由说。
详见 [docs/levels.md](levels.md)。`bank plan` 默认按 `readiness.free_speech_enabled` 自动决定槽位。
（`record` 的 8 s 起始等待已可通过 `start_timeout_sec` 调整，D39 已还。）

## 已知债务（详见 ADR-0008）

S1 级已全部偿还。剩余主要项：
D04 whisper 路径无回归 · D06 音色表未人耳校准 · D09/D11 时间戳与阈值（韵律已上线，D12 已还）
D15 全局锁串行 · D18 DLL 手工复制 · D20/D21 CI（**D20 已有 CI，待首跑**；仍无合成 fixture）· D29 Skill 分发 · D35 蓝牙端点选择 · D36 AGC 强导致能量维度不可用于重音 · D38 分块边界可能切词
D40 教材导语不在 LRC 里 → 每课固定 10–12 条 `extra` 噪声 · D41 英美变体表需人工维护（只覆盖 50 对高频词）· D46 G2P 同形异音词只取首变体（live/read）· D47 短语音 SNR 误杀单词 drill · D48 readiness 未自动评估 · D49 双引擎听不清时无文本闸门 · D50 韵律 12 条标注验收未做 · **D51 TTS 仅 lexicon 且未验证**（OOV 预检 + overlay + `tts-verify` + `gop_baseline_valid` 已上线；批量材料接入未做）

已还：D19 rust-toolchain/MSRV · D39 `record.start_timeout_sec` · D45 构建门禁。（D18 只缓解：`check.ps1` 把 DLL 目录加进 PATH。）

## 下一批工作（用户已确认顺序）

1. ~~**P3 真机训练闭环**~~（2026-11-21 完成）
2. ~~**P4 跟读素材库**~~（2026-11-22 全量完成）：`bank ingest/list/show/drill/stats/retag`，NCE1–4 共 276 课 → 4 997 条（另加 10 条 IELTS prompt，当前 5 007）
3. ~~补一次自由说 30–60 s~~（2026-11-23 完成；**2026-11-24 改为 readiness gate**：L2+ 关闭，L3/L4 达标后开启）
4. ~~韵律分析（F0 / 句末升降 / 平调 / 重音候选 / DTW）~~（**完成** 2026-11-23，见下节）
5. **Route A（无参考自由说伪 GOP）**：已立 [ADR-0010](adr/0010-free-speech-pseudo-gop.md)，
   **Proposed / Deferred**，不在当前迭代；触发条件见 ADR。
6. **回到原定路线**：素材库定向训练（`bank plan`）→ 雅思素材入库（缺源音频）→ 按 `error_stats` 组卷

## GOP / 诊断层现状（2026-11-22）

**能力**：`assess(with_gop=true)` 返回
`gop.words_worst[]`（词级，对外）、`gop.worst[]`（音素级，仅排序）、`gop_baseline`（TTS 标准发音对照）、
`gop.worst[].confusion`（混淆对，如 `TH→s`，`confusion_reportable` 当前仅 TH 为 true）。

**硬规则（SKILL 4.0/4.1 已固化）**：
1. `degraded=true`（snr<13 / peak<−30 dBFS / 削波>0.1%）→ **不报任何发音结论**，只给重录建议；
2. 词级 vs 基线：`Δ ≤ −3 且基线 ≥ 4` 才可说"低于示范"；基线 <0.3 的词一律不报（模型偏好）；
3. 音素级禁止断言；只有 `confusion` 且 `TH→s` 允许说"模型听到的是 /s/ 而不是 /θ/"；
4. 整词替换靠词级 diff（GOP 会漏，见 `cases/r09_why_when.json`）。

**已知限制**：音素级精确率天花板 0.45（基率 7.5%）；CTC spike 导致每个音素只有 1–2 帧，
**不能用时长判断元音长短**；MDD 进一步提升空间 ≈0（词级已贴天花板），下一步方向是"降基率"
（可疑词定向重录复核）。

## 韵律分析现状（2026-11-23）

`assess(with_prosody=true)` 或 `reference_audio=<示范音>` 返回 `prosody`：
F0 中位/标准差/音域、句末斜率与升降方向、平调判定、重音候选 top5、节奏、与示范音的 F0 形状 DTW。

算法与验证见 [ADR-0007](adr/0007-prosody-and-gop.md)；**只给数值+位置，不下"语调不好"的结论**。
SKILL 4.2 已固化措辞规则（先看句型再解读升降、`stress_top` 只是候选、`energy_z` 不可用于重音）。

已知边界：
- **未做 12 条人工标注验收**（ADR 验收项：升降/平/重音一致率 ≥80%），当前只有 8 条单测 + 1 条真机 + DTW 端到端；
- 重音候选的时长来自 ASR 词时间戳（±0.5 s），只能说"第几个词"；
- 蓝牙 AGC 让 `energy_z` 失真（D36），只作第 3 特征；
- `voiced_ratio` 含静音帧，不要与 `flow.speech_ratio` 混用。

**注意**：本次改的是 `voice-mcp` 源码，正在运行的 MCP 进程仍是旧二进制；重启 Pi / MCP 后
`assess` 才会接受 `with_prosody` 参数（CLI 已可直接用 `--with-prosody`）。

## Route A 立项（2026-11-23，暂不实现）

无参考自由说的伪参考 GOP 已写入 [ADR-0010](adr/0010-free-speech-pseudo-gop.md)：
可行性实测（51.8 s 样本：一致词覆盖率 **84.1%**、共识词 CMUdict 覆盖 100%、端到端 ≈22 s/60 s、
共识里存在 `i mistake` 这类双引擎同错）、设计、R1–R9 风险、V1–V10 验收线都已固化。

**决策：先不开工。** 当前迭代继续原定路线；A 的触发条件是 D04 whisper 回归已还，
且用户要求自由说纠音——**自由说已于 2026-11-24 改为 readiness gate（L2+ 关闭，L4 才开启）**，
L4 之前该触发条件不会成立；A 仅保留为未来选项。

为了以后能接上，当前代码保持：`Transcript.words/tokens` 双保留、`normalize`/`phonetic`
公开且确定性、`assess` 单入口 + feature flag、伪参考结果只新增字段不合并进 `gop`。

## 真机训练记录（2026-11-22 夜）

`D:/voice_data/sessions/2026-11-22/`，`learner.json` history 14 条（含 2026-11-23 三条自由说）。已修：
`why→when`（Δ −4.27→−0.39）、`thank` 的 /θ/（TH→s → heard θ，Δ −0.05）、`valley`（acc 0.83→1.00）。
待练：`short_long_vowel`（/ɪ/ vs /iː/，seat 第 1 次仍读成 sɪt）、`ae_to_e`（sit→set）、`village` 的 /ɪlɪ/。

**唯一的硬件障碍**：EDIFIER W820NB 的麦克风增益在同小时内漂移 −11.8 ~ −33.5 dBFS，
开练前先 `rec` 说句 test 确认 `peak` 在 −20 ~ −12 dBFS。

## 门禁与文档对齐（2026-11-22 晚）

**为什么做**：`4f57a36` 在 `format!` 里写了 `{peak_dbfs:.1f}`（Rust 只接受 `{:.1}`），
`voice-mcp` 直接编译失败，但后面 3 个提交都没人发现 —— 因为没有门禁。

已落地：

- 修掉那 1 处编译错误；`cargo fmt --all` 全仓格式化；清掉全部 36 条 clippy 警告
  （26 条 collapsible_if 自动修，4 条手工：needless_range_loop / field_reassign ×2 / too_many_arguments 加注）。
- `tools/check.ps1`：`fmt --check` → `clippy --all-targets -D warnings` → `test`，
  可选 `-Cases` 跑录音回归。本地实测：**97 单测 + 9 用例全绿**。
- `.github/workflows/ci.yml`：windows-latest 上跑同一套脚本，先 `fetch_deps.ps1` 取原生库。
  **尚未在 GitHub 上首跑**（D20 只算部分偿还）。
- `rust-toolchain.toml` + `Cargo.toml` 的 `rust-version = "1.99"`（实测版本；edition 2024 的理论下限 1.85 未验证）。

同批对齐的文档漂移：

| 文档 | 旧 | 新 |
|---|---|---|
| CONTRACT | 工具清单“7 个” | 8 个（transcribe/synthesize/record/play/prepare_audio/voices/status/assess） |
| CONTRACT | `record.silence_sec` 默认 1.0；无 `start_timeout_sec` | 默认 2.0；已加 `start_timeout_sec`（默认 8） |
| CONTRACT | GOP “句级 ρ 0.67；音素级 AUROC 0.63–0.66 / P 0.15–0.24” | 补词级 AUROC 0.887 / P@FP10 0.457；音素级 0.769 / 0.170；补 `gop.words_worst` / `gop.worst` / `confusion` / `gop_baseline` 字段 |
| STATE | 素材 4 997 / ready 3 368 / 3 384（两个数） | 5 007 / ready 3 378（`bank stats` + 索引实测） |
| STATE | history 11 条 | 14 条（后补三条自由说） |
| STATE | D39 仍列为未还 | 已还（`record.start_timeout_sec`） |
| README / docs/README | 债务“33 条” | 45 条（D01–D45） |

## 自由说定档（2026-11-23）

录音条件：样本 A/B 为 EDIFIER W820NB（Hands-Free，48 kHz，`record` 自动停止）；样本 C 为系统「录音机」m4a。三种都走 `assess(reference=不传)`。

| 样本 | 语音/窗口 | speech_rate | articulation | pauses（最长） | speech_ratio | run | peak/SNR |
|---|---|---|---|---|---|---|---|
| C `2026-11-23/205337_assess` **主样本** | 32.3 / 48.8 s | 100.9 | 152.2 | 17（1.85 s） | 0.663 | 9 词 | −18.7 dBFS / 24.2 dB |
| A `2026-11-23/204916_drill` | 13.5 / 19.0 s | 100.9 | 142.2 | 9（0.89 s） | 0.709 | 10 词 | −12.4 dBFS / 24.4 dB |
| B `2026-11-23/204215_dictate` | 13.9 / 28.1 s | 76.9 | 155.5 | 8（**3.64 s**） | 0.494 | 9 词 | −19.1 dBFS / 27.1 dB |

结论：单段 51.8 s（语音 32.3 s）**已达标**。`band_estimate` 维持 **6.0**（区间 5.5–6.5），`confidence` **low → medium**。
三条都是工作汇报，**不是雅思题**；转写有 ASR 误听（frontend→front 等），无 reference 不据此判发音。

短板不在发音而在**独白连续性**：停顿 ~10.5 次/30 s、最长一口气 9 词、`word_boundary_breaks` 15/76 词。
`focus` 已改为“60 s 少停顿 + 意群连读”。下次同题自测目标：pause ≤ 8 次/45 s、longest_run ≥ 12 词。
教训：`record` 会被 3.5–5 s 的思考停顿切断（三次尝试 4.4 / 19.5 / 18.6 s）→ 长独白一律用系统录音机，见 SKILL 第 7 节。
