# ADR-0008: 技术欠债登记（Debt Register）

- 状态：Accepted（活文档，每还一条就更新；最后更新 **2026-11-26**：D12/D19/D20/D39/D45/D47/D49/D51 已还，D04/D52/D53 部分偿还；未还：D46/D48/D50 + D06/D09/D11/D15/D18/D21/D29/D35/D36/D38/D40/D41/D44）
- 日期：2026-11-19
- 相关：ADR-0001（运行时路线）、ADR-0006（评测边界）、ADR-0007（韵律与 GOP）

## 背景

这套系统在 3 天内从 0 建到可跑通训练闭环，过程中有大量**"先能跑、后补正确性"**的取舍。
如果不把欠债显式登记，会有两个后果：

1. 后来者（包括三个月后的我）不知道哪些地方是"验证过的"、哪些是"看起来对但没测过"；
2. LLM 在 Skill 里会按"能力完备"的假设说话，而实际能力有洞。

**登记规则**：每条债必须写清 **证据**（怎么发现的）+ **触发偿付条件**（什么时候必须还）。
没有触发条件的不许登记——那是牢骚，不是债。

## 严重度定义

| 级别 | 含义 |
|---|---|
| **S1** | 会产出**错误结论**或让某条主路径（中文/录音/纠音）不可用 |
| **S2** | 影响可用性、可维护性、可分发性，但不会直接误导用户 |
| **S3** | 整洁度/一致性，随时可修 |

---

## 一、正确性（S1 为主）

| ID | 欠债 | 证据 | 级别 | 触发偿付条件 | 预估 |
|---|---|---|---|---|---|
| D01 | `assess` 在 manifest 写的 `feedback_ref` 曾指向**不存在的 .md**（只算路径不建文件） | 提交前自查发现；已修（现在会写入带模板的空反馈文件） | ~~S1~~ ✅已还 | – | – |
| D02 | VAD 静默失败时（`speech_duration≈0` 但转写出词）仍照常输出语流指标，`speech_rate`/`articulation_rate` 会变成荒谬值 | 实测曾出现 `articulation_rate=1891 wpm`（VAD 状态 bug 的次生现象）。现已在 `assess` 里检测 `words≥2 且 speech_duration<0.2` → `degraded=true` + limitations 追加警告 | ~~S1~~ ✅已还 | – | – |
| D03 | 词时间戳 → 规范化词序列曾用**按比例映射**，缩写展开/数字转词后静默错位 | **已修**：`normalize::Unit` 带来源 token 序号；token 数与 ASR 词数一致时按序号精确取时间，同一 token 展开的多个词在其时间窗内均分，否则才退回比例估算并标 `estimated`。新增 `timestamp_alignment`(exact/estimated/missing) 与 `reference_surface`（`I'm` 不再拆成 2 条问题）。6 个新单测 + 真机回归（`third→straight` 仍是 exact @4.50–5.04） | ~~S1~~ ✅已还 | – | – |
| D04 | whisper 路径：token→词合并、标点、`enable_token_timestamps=false` 的降级路径缺乏回归；`mixed` 首次调用惰性加载 1.6 s | **部分偿还（2026-11-25）**：抽出纯函数 `route_engine()` + `routes_lang_to_engine` 单测（zh/mixed/en/auto/强制/未知引擎回落）；新增 3 个 fixture 单测（whisper `Ġ` 前缀+标点、被下一个词截断的子词、无 token → 空词表 + degraded）。**仍缺**：真机 whisper 音频端到端回归（需要模型，CI 里跑不了） | S1（对中英混说用户）→ 降为 S2 | 出现真实中英混说场景时需要端到端证据 | 0.3d：`cases` 里加一条 `--engine whisper` 用例 |
| D05 | 中文 TTS 从未实测（`lang` 字段在 sherpa-onnx ≥2.0 已被忽略，只认 lexicon） | **已实测**：`zf_xiaobei`(sid 46) 合成 3.73 s / RTF 0.26；回读校验 `我觉得这个句子读得不错，再练一次。` → ASR 得 `…你再连一次。`（14/15 字一致）。`zm_yunjian`(sid 50) 同样正常。中文合成可用 | ~~S1~~ ✅已还 | – | – |
| D06 | Kokoro 的 音色名↔sid 表按 hexgrad 官方 voices 目录**字典序推断**，仅 `af_heart→sid=3` 一处自洽，未人耳校准 | 无 | S2 | 用户抱怨音色不对，或要用特定音色做人设 | 1h：跑 `voice-cli tts-probe` 逐个试听 |
| D07 | `record` 全链路实测 | **已还**：真机 48 kHz 单声道 F32 采集 → 重采样 16 kHz → 自适应阈值自动停止（5.74 s 停、语音段 1.92 s）→ VAD 精修 → ASR（41 ms，accuracy 1.0）→ 原始 `.raw48000.wav` 留证，全链路跑通 | ~~S1~~ ✅已还 | – | – |
| D08 | 自动停止阈值原为固定 −45 dBFS，对不同麦克风增益失效 | **已还**：改为**开场 0.4 s 自适应校准**（噪声 p95 + 8 dB，夹到 [−60, −20]）。实测本机蓝牙端点噪声 p99 = −46.0 dBFS，固定阈值只高 1 dB，必然失灵 | ~~S2~~ ✅已还 | – | – |

## 二、评测精度（能力债，非 bug）

| ID | 欠债 | 级别 | 触发偿付条件 | 预估 |
|---|---|---|---|---|
| D09 | 词级时间戳是 CTC token 级（±0.5 s）→ 词界停顿、连读判断可信度低 | S2 | ADR-0007 的 D12 上线时自然解决；或先换带 cross-attention 的 whisper 导出 | 见 ADR-0007 |
| D10 | 音形编码不区分**元音长短**（bit/beat、bad/bed 同 key） | S2 | 同上（需音素级） | 见 ADR-0007 |
| D11 | 停顿 250 ms / 词界 200 ms / 语速区间 80–190 wpm 全是**经验值**，未用真实数据校准 | S2 | ≥10 次真实训练后（ADR-0007 的同一触发条件） | 0.5d：用 manifest 分布反推分位数 |
| D12 | 无 F0/重音/升降调（韵律完全空缺），`tone_flat` 这类问题只能靠人耳定性 | **已还**：2026-11-23 韵律上线（YIN + 半音域 + 句末升降/平调/音域/重音候选/DTW），见 ADR-0007；12 条人工标注验收仍未做 | ~~S2~~ ✅已还 | – | – |
| D13 | 中文只做**字级** diff，无拼音层 | S2 | 有中文发音诊断需求时 | 2d（引入拼音对齐） |
| D14 | `accuracy = 1-(S+D+I)/N_ref` 对长句不公平（分母大），且不区分错误类型权重 | S3 | 用户开始练长句/Part 2 时 | 0.5d：改用 WER + 关键错误加权 |
| D41 | 英美拼写/发音变体表 `US_UK_VARIANTS` 需人工维护，只覆盖 50 对高频词；遇到表外变体仍会误报 | 训练中 `theatre/theater` 类误报 | S3 | 出现新的表外变体误报 | 0.5d/次：按真实误报补表，并加回归用例 |
| D46 | **G2P 同形异音词只取 CMUdict 第一个变体**：`live` 首变体是 `L AY V`（形容词），`I live in…` 的动词 `/lɪv/` 会被判低；`read` 同样（首变体 `R EH D`） | 2026-11-24 自然句：`live` 词级 Δ−4.61，但 GOP 的 V 实为 /v/（5.79/6.42）；TTS 基线用同一错误变体，无法自救 | S2 | 下一次 GOP 标定 / Route A 实现前 | 0.5d：按变体逐一取 max-GOP，或按词性/上下文选变体；补 live/read/use 回归用例 |
| D47 | 短语音（<2 s）SNR 估计误杀单词 drill | **已还（2026-11-25）**：`snr_verdict(snr, speech_duration, peak)` 纯函数 + 4 个单测。VAD 语音跨度 <3 s 且 peak ≥−25 dBFS → `SnrVerdict::LowButShort`：**不置 degraded、照跑 GOP**，只在 `limitations` 写明“SNR 会被低估”。历史失败样本（1.8 s word drill、SNR 12.6）已被覆盖 | ~~S2~~ ✅已还 | – | – |
| D48 | **readiness/free-speech gate 只读 JSON 标志，未自动从 history 计算**：`readiness.stage`/`missing` 目前手动写，升/降级没有代码判定 | 2026-11-24 gate 上线；`plan` 只消费 `free_speech_enabled` | S2 | 下一次升/降级或第二个学习者 | 1d：`voice-core::readiness` 评估器 + CLI `learner readiness`，写回 history 证据 |
| D49 | 声学闸门通过 ≠ 文本可用 | **已还（2026-11-25）**：`assess.text_agreement`（词级 LCS 一致率 `2·|LCS|/(|a|+|b|)`）。给了 `reference` 且非中文时用另一个引擎复核，`<0.7` → `degraded=true` + limitation（只出 flow/prosody）；第二引擎不可用只记 limitation。3 个单测 + 9 条录音回归零误杀（clean case agreement=1.0） | ~~S2~~ ✅已还 | – | – |
| D50 | 韵律的 12 条人工标注验收未做（升降/平/重音一致率 ≥80%）；短句窗口与 `unreliable` 已加，但无真值标定 | ADR-0007 验收项；目前只有 10 单测 + 少量真机 | S2 | 下一次语调专项训练 | 0.5d + 标注 2h |
| D51 | TTS 参考音未验证就当基准 | **已还（2026-11-24/25）**：OOV 预检 + 项目 overlay + `tts-verify`（双 ASR/音素/WPM/3 次）+ `gop_baseline_valid`；**`bank refs` 把同一套验证接进批量材料生成**（失败自动换目标语速 → 换音色，只有 verified 落盘）；10/10 IELTS prompt verified 3/3（含音素）。另修：WPM 改按语音跨度、语速校正保留最优候选、`onset_gap_ms` 起始伪影检测。见 `docs/audit/tts-2026-11-24.md` §12 | ~~S1~~ ✅已还 | – | – |
| D52 | **TTS 产出被当成“标准/基线”**：① `assess(baseline_tts=true)` 的 GOP 基线一律是合成音，且只做**内容**回读自检（词对就算过），音素读坏的基线会被当标准；② 韵律 DTW 的参考音大量是 `synthesize` 产出的 `.ref_norm/.ref_slow`，等于拿合成音的语调当人标准；③ `bank` 里的 `ref_audio=tts` 只在 plan 文本里提一句，代码不区分；④ 素材库 4 997 条 NCE 明明带**真人原声切片**，却没人用它当基线 | 2026-11-25 复盘 TTS 审计：TTS 自己会读错（country/thank/tokyo 回读不一致）、与 G2P 共用同一套音素变体（同形异音无法自救，D46）、合成语调不是人标准 | S1 | 任何“比标准示范低 X 分”的结论（已发生） | **部分偿还（2026-11-25）**：`assess(bank_id=…)` 真人原声优先（`gop_baseline_source=native`），韵律 `reference.source` 标注 native/tts/unknown，基线**加音素自检**（最差词级 GOP < −1.5 → invalid），CLI `--bank-id` + MCP `bank_id` + SKILL 口径。残留：IELTS prompt（10 条）没有真人音；历史 `metrics.json` 里的 Δ 是 TTS 基线算的，**不可与新结果横比**，需重跑；**新偏差**：NCE 原生切片 16 kHz vs 用户录音 48 kHz 的带宽不一致，摩擦音 GOP 会偏（要严格比需两侧同带宽） |
| D53 | **SenseVoice 的 `lang` 参数不生效**：模型按 `language=auto` 加载，`transcribe()` 里的 `lang` 只用于路由，不传给识别器 → 英文素材偶尔解成中文/日文（实测佩奇音频出现「大抖。」「人と命ね。」）；`--lang en` 挡不住 | 2026-11-26 `bank ingest-audio` 实测 | S2 | 英文/中文素材入库或 `engine=sense-voice` 结论异常时 | **部分偿还**：`wrong_lang()` 语言守卫——请求 en/zh 却拿到另一种语言时换另一个引擎重听，仍不对就 `degraded=true`。彻底修需按语言分别加载识别器（内存 +200 MB/个）或等 sherpa-onnx 支持 per-stream 语言 |
| D54 | **`synthesize_at_wpm` 语速标定散布**：同一目标 140 wpm，实测落到 96–190 wpm（wl06 89.9、wl08 168.2、wl04 190.5）；`rank()` 只在被尝试的候选里选，`speed→wpm` 非单调时容易落在跳变另一侧 | 2026-11-27 前端改造后逐句实测（`voice-cli tts-regress`） | S2 | 任何“目标 140 wpm / 130–160 在带内”的承诺；雅思语速结论 | **未偿还**：已声明“140 wpm 暂不可靠”。修法：标定改为二分/查表 + 结果缓存，并在 `tts-regress` 里加 wpm 带内断言（当前门禁只判漏词，wpm 仅报告） |
| D55 | **`SidecarBackend` 子进程生命周期**：`VOICE_G2P_CMD` 起的外部 G2P 无健康检查、无单次调用超时；子进程崩溃后不会自愈、某行卡住会阻塞管道、Windows 句柄泄漏更隐蔽 | 2026-11-27 评审（接 misaki 前必补） | S2 | 长驻 MCP server 的全部 TTS 请求 | **未偿还**：接 misaki 时同时加 `kill_on_drop` + 单次超时（读超时即杀进程并报错）+ 一次重启重试；不许静默退回闭集 |
| D56 | **中英混说音质**：分段合成后拼接，单音色下另一语言口音不纯（`zf_xiaobei` 读 `Sarah's laptop`、`am_michael` 读中文均跑调），拼接点有 220–520 ms 停顿与 +10…+19 dB 电平跳变 | 2026-11-27 听测（`docs/audit/tts-2026-11-27-frontend.md`） | S3 | 混说句的实际可用性（英语教学材料不受影响：契约仍是“一段文本一种语言”） | **未偿还**：需 misaki-zh（中文走 G2P，与英文同一条 `phoneme_codepoints` 链路）才能谈混说音质；当前只保证结构不变量 |
| D54 | **`synthesize_at_wpm` 语速标定落错**：同目标 140 wpm，实测 96–190 wpm（wl04 190.5、wl08 168.2、wl06 89.9） | 2026-11-27 前端改造后逐句实测 | S2 | “140 wpm / 130–160 在带内”的承诺；雅思语速结论 | **已定位（2026-11-27）**：不是模型不准，也不是 wpm 分母，而是 `rank()` 的第一键是 0/1 的 `off_band`——190 wpm 与 128 wpm 并列（都算越界），第二键“无起始伪影”把 190 wpm 抬成最优。改法：第一键换成**离目标带宽的连续距离** `excess`，artifact 降为第二键。修复后 wl06 142.0 / wl08 136.1 / wl01 139.9 / wl10 140.5。**残留**：wl04 仍 124.4（低于 130 带下沿），伪影重试上限 7 次用尽；若要硬保证，需按候选网格搜索或把带下沿纳入退出条件。另：混说句的 wpm 口径也修了（中文按字计，`这个 API…` 从 20.7 → 146.0），见 `spoken_units()` |

## 三、工程韧性

| ID | 欠债 | 证据 | 级别 | 触发偿付条件 | 预估 |
|---|---|---|---|---|---|
| D15 | 全局 `Mutex<Option<Engines>>` 在 ASR/TTS 期间持锁，**并发调用串行化** | `voice-mcp/src/main.rs` 的 `with_engines` | S2 | 出现第二个并发调用方（HTTP 模式/热键 daemon） | 0.5d：拆成 per-engine 锁 |
| D16 | 无**取消/中断**：`record` 阻塞最长 30 s、`play` 阻塞到播完，都只能靠客户端超时兜底 | – | S2 | 用户抱怨"卡住没法停" | 1d：`record` 改流式 + 支持 abort |
| D17 | 引擎加载**失败不缓存**：每次调用都会重试并重新付出加载代价（whisper 1.6 s） | `With_engines` 只缓存成功 | S3 | 出现一次引擎加载失败 | 0.5d |
| D18 | 原生 DLL 需**手工** `cp .cache/sherpa/lib/*.dll target/release/`，忘掉就启动即崩（Windows loader 报错，没有友好提示） | README 快速开始第 3 步 | S2 | 第二次有人在这台机器上构建 | 0.5d：`build.rs` 或 cargo xtask 自动复制 |
| D19 | 无 `rust-toolchain.toml`、无 MSRV 声明 | **已还**：`rust-toolchain.toml`（stable + rustfmt/clippy）+ `Cargo.toml` `rust-version = "1.99"`（实测版本；edition 2024 理论下限 1.85 未验证） | ~~S3~~ ✅已还 | – | – |
| D20 | 无 CI：`cargo test`/`clippy`/`fmt` 都没有门禁 | **已还（2026-11-25）**：`tools/check.ps1` + `.github/workflows/ci.yml`；GitHub Actions 在 `windows-latest` 上绿两次（`530e668` 2026-11-24、`3cf8736` 2026-11-25，run 45643953511），148 单测 + clippy `-D warnings` + fmt 全过。残留：CI 里没有音频 fixture（另记 D21） | ~~S2~~ ✅已还 | – | – |
| D45 | **没有门禁导致 HEAD 编译失败无人发现**：`{peak_dbfs:.1f}` 在 `4f57a36` 引入后陪着 3 个提交 | `cargo build --release` 报 `unknown format trait f` | ~~S1~~ ✅已还 | – | – |
| D21 | 回归用例 | **部分偿还**：`cases/*.json` 只存断言（音频留本机，`VOICE_CASES_DIR` 指过去），`voice-cli cases` 一条命令跑全部；已覆盖 16k/48k/TTS/自由说/漏词 5 个场景。仍缺：没有**合成** fixture（正弦/噪声），CI 里无法无音频跑 decode/flow | S2 | 接 CI 前 | 2h |
| D22 | `assess` 的 MCP 版与 CLI `assess_cmd` **逻辑重复**，两处维护 | 两个文件各 ~80 行 | S3 | 参数再变一次时 | 2h：抽到 `voice-core::pipeline` |
| D23 | `spike` crate 在 workspace 默认成员里，`cargo build --release` 会编译 8 个二进制 | 曾提交；已用 `default-members` 修复 | ~~S3~~ ✅已还 | – | – |
| D24 | `OkResult.detail: Option<Value>` 丢类型，输出无 schema 约束（只能在文档里写契约） | – | S3 | 想用 schema 做自动校验/生成客户端时 | 1d |

## 四、数据与运维

| ID | 欠债 | 级别 | 触发偿付条件 | 预估 |
|---|---|---|---|---|
| D25 | `manifest.jsonl` 无轮转；`sessions/` 无保留策略（ADR-0004 提过"仅保留最近 N 天派生文件"，未实现） | S3 | 数据目录 > 5 GB | 3h |
| D40 | 教材导语不在 LRC 里 → 每课固定 10–12 条 `extra` 噪声（`accuracy` 被拉低，需人工/规则剔除） | S2 | 做批量评分/自动组卷时 | 0.5d：导语词典 + bank 入库时标 `instruction` |
| D26 | `watch_state.json` 只增不减，源文件删除后条目仍在 | S3 | 同上 | 1h |
| D27 | `learner.json` 的 schema 只存在于 SKILL.md 的散文里，**无代码校验、无版本迁移** | S2 | 改一次字段名时（会静默丢档案） | 2h：`voice-core::profile` + `version` 字段 + 迁移 |
| D28 | `docs/security-hashes.txt` 覆盖不全 | ~~S3~~ ✅已还：`verify_models.py --write-baseline` 全量输出（12 个模型文件 + 3 个 DLL + 1 个轮子 = 22 条） | – | – |

## 五、分发（当前最大的"产品债"）

| ID | 欠债 | 级别 | 触发偿付条件 | 预估 |
|---|---|---|---|---|
| D29 | Skill 靠 `tools/install_skills.ps1` 建 junction 装到 `~/.pi/agent/skills`；项目 `.pi/settings.json` 又需项目授信。**没有真正的安装包形态** | S2 | 第二个人要用 | 0.5d：打成 Pi package（见 `docs/packages.md`） |
| D30 | Windows-only 假设（默认路径、录音机目录名、DLL 名、`mklink /J`） | S2 | 出现 mac/linux 用户 | 1d |
| D31 | 无预编译二进制 / 无 GitHub Release，新用户必须本地装 Rust + MSVC | S2 | 出现非 Rust 用户 | 1d（CI 矩阵） |
| D32 | 无 `LICENSE` 文件（README 只写了许可说明） | ~~S3~~ ✅已还 | – | – |
| D33 | 项目级 `.pi/mcp.json`、`.pi/settings.json` 因未授信被忽略，与实际生效的全局配置存在**双份真相** | 每次 `pi mcp list` 都会提示 | S3 | 下次交接 | 1h：README 写清"哪个是权威" |
| D38 | 分块边界仍可能切断词：超长连续朗读（无 VAD 停顿）会按 20 s 硬切，跨块的词会被拆成两条转写 | S3 | 出现长独白（Part 2）素材时 | 0.5d：块间加 0.3 s 重叠 + 末尾词去重 |
| D39 | `record` 起始等待硬编码 8 s，真机上“人已经在说、工具报没检测到说话” | **已还**：`RecordOptions.start_timeout_sec` + CLI `--start-timeout` + MCP `start_timeout_sec`（默认仍 8，建议自由说 20） | ~~S2~~ ✅已还 | – | – |
| D37 | 短功能词漏读目前只靠启发式（词长 ≤4 且在功能词表内）降级，没有真正做双引擎交叉验证 | S2 | 出现误判投诉或要提升英文精度 | 0.5d：`assess` 加 `cross_check=true`，对 omitted 的功能词跑 whisper 复核 |
| D36 | 蓝牙耳机端点底噪高达 **p50 = −54.7 / p99 = −46.0 dBFS**（正常麦克风自噪约 −70 dBFS），说明设备侧 AGC/降噪在强推电平。后果：能量（intensity）维度不能用于重音判断，韵律只能靠时长 + 音高 | S2 | P2.5 韵律上线前必须定口径 | 0.5d（改为时长+音高为主，能量仅作参考并标注设备） |
| D35 | 蓝牙耳机（A2DP 端点）在 WASAPI 下"可打开、回调在跑、数据是稀疏零值"；cpal 只枚举到该端点，无法选中未激活的 HFP 端点，只能靠文案引导用户去系统设置切换 | S2 | 有第二个蓝牙麦用户 | 1d（用 windows crate 直接走 WASAPI 端点枚举 + MMDevice 选择） |
| D34 | 中文 TTS 回读出现 1 处字词偏差（`再练一次`→`你再连一次`），未确定是 TTS 声调/连读问题还是 ASR 误听；中文音色自然度也未经人耳确认 | 上表 D05 的实测输出 | S3 | 用户反馈"中文讲解听不清" | 1h：人工听 + 若确认是 TTS 则换 `lexicon-zh` 或改分段 |

---

## 六、偿付顺序（我的建议）

不是为了"看起来整洁"，而是按**会误导用户的程度**排：

| 顺序 | 债 | 为什么先还 | 何时 |
|---|---|---|---|
| 1 | ~~D03 时间戳错位~~ | 已还（见第七节） | – |
| 2 | **D07 record 实测** | 没有麦克风，短句跟读体验缺失（`mic=null`） | 硬件到位后立即 |
| 3 | ~~D18 + D19 + D21~~ | D19 已还；D18 仍靠 `check.ps1` 加 PATH 缓解；D21 只差合成 fixture | – |
| 4 | ~~D20 CI~~ | **已还**：门禁落地 + GitHub Actions 绿（2026-11-25） | – |
| 5 | D12/D09 韵律与 GOP | 能力提升，但需真实数据校准阈值 | ADR-0007 触发条件 |

**暂不偿付**：D25/D26/D30/D31（都是"规模变大才痛"的问题），D24/D14（洁癖级）。

## 七、已还清（保留记账）

| 债 | 怎么还的 |
|---|---|
| VAD 一次性喂整段只返回最后一段 | 改为按 `window_size=512` 分块（`engine/vad.rs`） |
| VAD `clear()` 累积采样偏移（第 2 次调用时间戳 +6 s） | 改用 `reset()`；`vad_probe` 永久保留对比证据 |
| VAD 越界时间戳污染语流指标 | `flow::analyze` 增加防御性裁剪 |
| sherpa-onnx 原生库拉不下来（GitHub 不可用） | 改从 PyPI 轮子取 + `dumpbin`/`lib` 补 import library（ADR-0005） |
| 模型下载 122 MB 要 15 分钟 | hf-mirror 8 路分段并发（~1.1 MB/s） |
| whisper 无 token 时间戳 | 确认该权重未导出 cross-attention → 降为副通道，默认用 SenseVoice |
| espeak-ng 375 个无用文件 | 确认 sherpa-onnx ≥2.0 已不用 espeak-ng，从下载清单剔除 |
| `assess` 悬空 `feedback_ref` | 现在会先落盘带模板的 `.md`，Agent 再追加内容 |
| VAD 静默失败无检测 | `assess` 增加 `words≥2 且 speech_duration<0.2` 判定 → `degraded` + limitations |
| 无 LICENSE / spike 进默认构建 | 补 MIT LICENSE；workspace 加 `default-members` |
| 哈希基线只有 8 条 | `--write-baseline` 全量输出（22 条：12 模型 + 3 DLL + 1 轮子） |
| 中文 TTS 未验证 | 实测可用（RTF 0.26，回读 14/15 字），残留质性问题另立 D34 |
| 词时间戳比例映射静默错位（D03） | 规范化词携带 token 序号，精确对齐；同 token 合并上报；新增 `timestamp_alignment` |
| 采集到假音频却当正常处理（蓝牙零值） | `is_effectively_silent`：峰值 <−60 dBFS **或** 过半 10 ms 帧全零 → 报错并给 4 条可操作指引（含 Hands-Free 端点提示） |
| `capture_sr` 是死配置（从不生效） | 新增 `preferred_config()`：设备支持时主动要单声道 48 kHz F32，否则退回默认 |
| `band_limited` 恒为 true（拿 16 kHz 分析域当源采样率判） | `AssessRequest.source_sr` + `apply_source()`，按**源**采样率判定；R7 复测 24 kHz → `band_limited: false` |
| 无法指定输入设备 | `RecordOptions`/CLI `--device`/MCP `record.device`/环境变量 `VOICE_MIC`（按名称子串匹配） |
| 固定 −45 dBFS 自动停止阈值 | 开场 0.4 s 自适应校准（噪声 p95 + 8 dB，夹到 [−60,−20]），阈值会打印出来便于排查 |
| 录音只存 16k 派生，原始证据丢失 | 现在先落 `{prefix}.raw{device_sr}.wav`（如 `.raw48000.wav`）作 `src`，16k 才是派生；`assess(path="last")` 因此能拿到真实源采样率（band_limited 不再误报） |
| 一直不说话也返回一段噪声当录音 | 未检测到说话直接报错，并提示先跑 `voice-cli mic` 看底噪 |
| 自动停止被尾部环境噪声拖住（实测多录 1.1 s） | 第二段相对判据：`stop_threshold = max(底噪阈值, 语音峰值 − 12 dB)`，噪声一落就停；带 `silence_sec` 缓冲避免切词尾 |
| 录音文件的语速被前导/尾部静音稀释（150 wpm 被算成 73 wpm） | `flow.analysis_window` = 首末语音段跨度，语速/占比改用窗口做分母；文件时长另存 |
| **长音频单次解码静默截断 + 时间戳压缩 0.77**（81 s 音频只转出前 57 s 内容）| `Engines::transcribe_long`：VAD 分段 → 每块 ≤20 s 分块解码 → 时间戳加偏移 → 拼接；`plan_chunks` 有 3 个单测（合并小间隙 / 硬切超长段 / 全静音兜底） |
| ASR 把合成词拆开造成假问题（lunchtime → lunch time） | `merge_split_compounds`：拼接后是参考词且两部分都不是独立参考词时才合并 |
| ASR 漏掉弱读短功能词被误判成用户漏读 | `Hint::LikelyAsrMiss`（词长≤4 且在功能词表）+ severity 降为 medium，note 明确写"不能直接判定"，并要求复核 |
| 1.0 s 静音就判句尾，刻意停顿会丢后半句 | 默认 `silence_sec` 提到 2.0，Skill 里按场景给 1.2/1.8/2.2/2.5 |
| 没有构建门禁，HEAD 编译失败无人发现（D45） | `tools/check.ps1`（fmt → clippy -D warnings → test）+ `.github/workflows/ci.yml`；同时清掉 36 条 clippy、全仓 fmt |
| `record` 起始等待硬编码（D39） | `start_timeout_sec` 贯通 core/CLI/MCP（默认 8，自由说建议 20） |
| 无 toolchain/MSRV 声明（D19） | `rust-toolchain.toml`（stable + rustfmt/clippy）+ `rust-version = "1.99"`（实测） |
| 无韵律（D12） | 2026-11-23 纯 Rust 韵律上线：YIN + 半音域 + 句末升降/平调/音域/重音候选/DTW；`prosody.unreliable` 闸门 |
| 短语音 SNR 误杀单词 drill（D47） | `snr_verdict`：语音跨度 <3 s 且 peak ≥−25 → 只记 limitation，不降级、GOP 照跑 |
| 声学过闸但 ASR 听不清（D49） | `assess.text_agreement`（双引擎 LCS 一致率）<0.7 → degraded，只出 flow/prosody |
| TTS 材料未验证就当基准（D51） | `tts-verify` + `bank refs` 批量接入（换语速/换音色回退）；10/10 IELTS prompt 3/3（含音素） |
| TTS 语速口径：按总时长算导致 140→128、校正震荡 | WPM 改按语音跨度；校正保留最优候选；新增起始伪影 `onset_gap_ms` 检测与规避 |
| 会话目录同秒相撞（并行 `tts-verify` 覆盖 report） | `Store::new_session` 撞了就加 `-2` 序号 + 单测 |
| TTS 当基线/标准（D52，部分） | 真人原声优先基线 + 音素自检 + 参考音来源标签（`native`/`tts`/`unknown`） |

## 八、维护约定

- 修一条就把对应行的级别改成 `~~S?~~ ✅已还` 并挪到第七节，**不要删行**（保留判断依据）。
- 新欠债必须先写"触发偿付条件"，否则不入表。
- ADR-0001/0006/0007 已覆盖的能力缺口（音素级 GOP、韵律、100% Rust 迁移）**不在本表重复登记**，只引用。


## 2026-11-22 状态更新（GOP / 素材库 / 数据集工作后）

| 债务 | 变化 |
|---|---|
| D09 词级时间戳 ±0.5 s | **部分偿还**：音素级 GOP 已可用（`gop.phones[].t0/t1`），但 CTC spike 使每个音素只有 1–2 帧，仍不能用于"元音时长" |
| D10 音形编码不区分元音长短 | **部分偿还**：`gop.phones[].heard` 能直接看出 /ɪ/ vs /iː/（本机实测 seat/sit 区分成功），但 GOP 分数本身仍不区分 |
| D12 无韵律（F0/重音/升降调） | **已还**（2026-11-23）：`assess(with_prosody=true)` 上线；12 条人工标注验收仍未做，另记 D50 |
| D21 回归用例 | **已还**：9 条（含 r08 英美变体 / r09 用户确认真值）；仍缺合成 fixture 供 CI 无音频跑 |
| D27 learner.json 无 schema | 未偿还（字段已增至 history/error_stats/drill_queue/gop_words） |
| D38 分块边界切词 | 未偿还；实测 30.8 s TTS 仍丢 `hometown`/`universities`（`docs/phoneme-gop.md`） |
| **新增 D42** | 蓝牙耳机麦克风增益自动漂移（同一小时内 peak −11.8 ~ −33.5 dBFS）→ 已加绝对电平闸门与 rec 即时提示，但**无法根治**（用户侧设备行为） |
| **新增 D43** | 音素级精确率受基率锁死（上限 0.45）；下一步方向是"可疑词定向重录"降基率，而不是继续训更大的模型 |
| **新增 D44** | `bank` 的 tag 是关键词启发式，召回偏低（`theta_to_s` 只命中 237/4 997）；已用 so762 统计数据做权重，但覆盖仍靠人工维护变体表 |
| **新增 D45** | 无门禁 → HEAD 编译失败 3 个提交无人发现；**已还**（`check.ps1` + CI，Actions 已绿） |
| D19 / D39 | **已还**：toolchain+MSRV；`start_timeout_sec` 可配置 |
| D20 | **已还（2026-11-25）**：GitHub Actions 在 `windows-latest` 上绿（148 单测 + clippy -D warnings + fmt） |
| D11 / ADR-0007 触发条件 | **已满足**：`learner.json` history 14 条（含 2026-11-23 自由说） → 停顿/语速阈值校准与韵律分析可以开始 |
