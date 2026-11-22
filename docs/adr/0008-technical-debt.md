# ADR-0008: 技术欠债登记（Debt Register）

- 状态：Accepted（活文档，每还一条就更新）
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
| D04 | whisper 路径只做过一次手工验证：token→词合并、标点、`enable_token_timestamps=false` 的降级路径都**没有回归测试**；`mixed` 路由首次调用要惰性加载 1.6 s，会顶到 Pi 的交互预期 | `crates/spike/src/bin/asr_whisper.rs` 的输出显示 `tokens: []` | S1（对中英混说用户） | 出现第一个真实中英混说使用场景 | 0.5d：补 3 个 fixture + 一个 `asr_routing` 集成测试 |
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
| D12 | 无 F0/重音/升降调（韵律完全空缺），`tone_flat` 这类问题只能靠人耳定性 | S2 | ADR-0007 触发条件满足 | 3–4d |
| D13 | 中文只做**字级** diff，无拼音层 | S2 | 有中文发音诊断需求时 | 2d（引入拼音对齐） |
| D14 | `accuracy = 1-(S+D+I)/N_ref` 对长句不公平（分母大），且不区分错误类型权重 | S3 | 用户开始练长句/Part 2 时 | 0.5d：改用 WER + 关键错误加权 |

## 三、工程韧性

| ID | 欠债 | 证据 | 级别 | 触发偿付条件 | 预估 |
|---|---|---|---|---|---|
| D15 | 全局 `Mutex<Option<Engines>>` 在 ASR/TTS 期间持锁，**并发调用串行化** | `voice-mcp/src/main.rs` 的 `with_engines` | S2 | 出现第二个并发调用方（HTTP 模式/热键 daemon） | 0.5d：拆成 per-engine 锁 |
| D16 | 无**取消/中断**：`record` 阻塞最长 30 s、`play` 阻塞到播完，都只能靠客户端超时兜底 | – | S2 | 用户抱怨"卡住没法停" | 1d：`record` 改流式 + 支持 abort |
| D17 | 引擎加载**失败不缓存**：每次调用都会重试并重新付出加载代价（whisper 1.6 s） | `With_engines` 只缓存成功 | S3 | 出现一次引擎加载失败 | 0.5d |
| D18 | 原生 DLL 需**手工** `cp .cache/sherpa/lib/*.dll target/release/`，忘掉就启动即崩（Windows loader 报错，没有友好提示） | README 快速开始第 3 步 | S2 | 第二次有人在这台机器上构建 | 0.5d：`build.rs` 或 cargo xtask 自动复制 |
| D19 | 无 `rust-toolchain.toml`、无 MSRV 声明（edition 2024 → 1.85+），`Cargo.lock` 已提交 | – | S3 | 出现第二个开发环境 | 10 min |
| D20 | 无 CI：`cargo test`/`clippy`/`fmt` 都没有门禁 | 当前零警告，但靠自觉 | S2 | 第二次提交代码时 | 0.5d（需先解决 D21） |
| D21 | 回归用例 | **部分偿还**：`cases/*.json` 只存断言（音频留本机，`VOICE_CASES_DIR` 指过去），`voice-cli cases` 一条命令跑全部；已覆盖 16k/48k/TTS/自由说/漏词 5 个场景。仍缺：没有**合成** fixture（正弦/噪声），CI 里无法无音频跑 decode/flow | S2 | 接 CI 前 | 2h |
| D22 | `assess` 的 MCP 版与 CLI `assess_cmd` **逻辑重复**，两处维护 | 两个文件各 ~80 行 | S3 | 参数再变一次时 | 2h：抽到 `voice-core::pipeline` |
| D23 | `spike` crate 在 workspace 默认成员里，`cargo build --release` 会编译 8 个二进制 | 曾提交；已用 `default-members` 修复 | ~~S3~~ ✅已还 | – | – |
| D24 | `OkResult.detail: Option<Value>` 丢类型，输出无 schema 约束（只能在文档里写契约） | – | S3 | 想用 schema 做自动校验/生成客户端时 | 1d |

## 四、数据与运维

| ID | 欠债 | 级别 | 触发偿付条件 | 预估 |
|---|---|---|---|---|
| D25 | `manifest.jsonl` 无轮转；`sessions/` 无保留策略（ADR-0004 提过"仅保留最近 N 天派生文件"，未实现） | S3 | 数据目录 > 5 GB | 3h |
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
| 3 | **D18 + D19 + D21** | 一组低成本工程卫生，做 CI 的前置 | 一次 0.5d 打包 |
| 4 | **D20 CI** | 没有门禁，回归全靠自觉 | D21 之后 |
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

## 八、维护约定

- 修一条就把对应行的级别改成 `~~S?~~ ✅已还` 并挪到第七节，**不要删行**（保留判断依据）。
- 新欠债必须先写"触发偿付条件"，否则不入表。
- ADR-0001/0006/0007 已覆盖的能力缺口（音素级 GOP、韵律、100% Rust 迁移）**不在本表重复登记**，只引用。


## 2026-11-22 状态更新（GOP / 素材库 / 数据集工作后）

| 债务 | 变化 |
|---|---|
| D09 词级时间戳 ±0.5 s | **部分偿还**：音素级 GOP 已可用（`gop.phones[].t0/t1`），但 CTC spike 使每个音素只有 1–2 帧，仍不能用于"元音时长" |
| D10 音形编码不区分元音长短 | **部分偿还**：`gop.phones[].heard` 能直接看出 /ɪ/ vs /iː/（本机实测 seat/sit 区分成功），但 GOP 分数本身仍不区分 |
| D12 无韵律（F0/重音/升降调） | 未偿还；ADR-0007 第一项仍开放 |
| D21 回归用例 | **已还**：9 条（含 r08 英美变体 / r09 用户确认真值）；仍缺合成 fixture 供 CI 无音频跑 |
| D27 learner.json 无 schema | 未偿还（字段已增至 history/error_stats/drill_queue/gop_words） |
| D38 分块边界切词 | 未偿还；实测 30.8 s TTS 仍丢 `hometown`/`universities`（`docs/phoneme-gop.md`） |
| **新增 D42** | 蓝牙耳机麦克风增益自动漂移（同一小时内 peak −11.8 ~ −33.5 dBFS）→ 已加绝对电平闸门与 rec 即时提示，但**无法根治**（用户侧设备行为） |
| **新增 D43** | 音素级精确率受基率锁死（上限 0.45）；下一步方向是"可疑词定向重录"降基率，而不是继续训更大的模型 |
| **新增 D44** | `bank` 的 tag 是关键词启发式，召回偏低（`theta_to_s` 只命中 237/4 997）；已用 so762 统计数据做权重，但覆盖仍靠人工维护变体表 |
