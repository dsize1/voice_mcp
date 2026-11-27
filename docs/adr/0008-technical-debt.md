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
| D46 | G2P 同形异音（`live`/`read`/`use`/`record`/`tomorrow`…）只取第一变体 → 读对也可能被判错 | 长期存在；2026-11-28 收口 | S2 | 所有跟读/朗读评分 | **已还（2026-11-28）**：`Gop::word_variants()` + `score_span()`——对多发音词用它自己的帧区间把每个变体各对齐一次、取平均 GOP 最高者，换变体时写 `gop.words[].variant`（证据）。实测 `tomorrow` 自动选中 `T UW M AA R OW`。单测 `score_span_prefers_the_variant_that_was_actually_spoken`（合成 log-prob，无模型） |
| D47 | 短语音（<2 s）SNR 估计误杀单词 drill | **已还（2026-11-25）**：`snr_verdict(snr, speech_duration, peak)` 纯函数 + 4 个单测。VAD 语音跨度 <3 s 且 peak ≥−25 dBFS → `SnrVerdict::LowButShort`：**不置 degraded、照跑 GOP**，只在 `limitations` 写明“SNR 会被低估”。历史失败样本（1.8 s word drill、SNR 12.6）已被覆盖 | ~~S2~~ ✅已还 | – | – |
| D48 | **readiness/free-speech gate 只读 JSON 标志，未自动从 history 计算**：`readiness.stage`/`missing` 目前手动写，升/降级没有代码判定 | 2026-11-24 gate 上线；`plan` 只消费 `free_speech_enabled` | S2 | 下一次升/降级或第二个学习者 | 1d：`voice-core::readiness` 评估器 + CLI `learner readiness`，写回 history 证据 |
| D49 | 声学闸门通过 ≠ 文本可用 | **已还（2026-11-25）**：`assess.text_agreement`（词级 LCS 一致率 `2·|LCS|/(|a|+|b|)`）。给了 `reference` 且非中文时用另一个引擎复核，`<0.7` → `degraded=true` + limitation（只出 flow/prosody）；第二引擎不可用只记 limitation。3 个单测 + 9 条录音回归零误杀（clean case agreement=1.0） | ~~S2~~ ✅已还 | – | – |
| D50 | 韵律的 12 条人工标注验收未做（升降/平/重音一致率 ≥80%）；短句窗口与 `unreliable` 已加，但无真值标定 | ADR-0007 验收项；目前只有 10 单测 + 少量真机 | S2 | 下一次语调专项训练 | 0.5d + 标注 2h |
| D51 | TTS 参考音未验证就当基准 | **已还（2026-11-24/25）**：OOV 预检 + 项目 overlay + `tts-verify`（双 ASR/音素/WPM/3 次）+ `gop_baseline_valid`；**`bank refs` 把同一套验证接进批量材料生成**（失败自动换目标语速 → 换音色，只有 verified 落盘）；10/10 IELTS prompt verified 3/3（含音素）。另修：WPM 改按语音跨度、语速校正保留最优候选、`onset_gap_ms` 起始伪影检测。见 `docs/audit/tts-2026-11-24.md` §12 | ~~S1~~ ✅已还 | – | – |
| D52 | **TTS 产出被当成“标准/基线”**：① `assess(baseline_tts=true)` 的 GOP 基线一律是合成音，且只做**内容**回读自检（词对就算过），音素读坏的基线会被当标准；② 韵律 DTW 的参考音大量是 `synthesize` 产出的 `.ref_norm/.ref_slow`，等于拿合成音的语调当人标准；③ `bank` 里的 `ref_audio=tts` 只在 plan 文本里提一句，代码不区分；④ 素材库 4 997 条 NCE 明明带**真人原声切片**，却没人用它当基线 | 2026-11-25 复盘 TTS 审计：TTS 自己会读错（country/thank/tokyo 回读不一致）、与 G2P 共用同一套音素变体（同形异音无法自救，D46）、合成语调不是人标准 | S1 | 任何“比标准示范低 X 分”的结论（已发生） | **部分偿还（2026-11-28 补）**：①`Assessment.delta_comparable` 字段——只有「基线存在 + 来源 `native` + 自检通过」才 `true`，落盘进 `metrics.json`、也进 MCP/CLI 输出；②`tools/audit_baseline.py`（只读）清点历史：280 条 metrics 里 `native` 5 / `tts` 6 / `absent` 35 / 无基线 234 → **Δ 可跨期比较的只有 5 条**，报告见 `docs/audit/d52-baseline-audit.md`；③用 `assess --bank-id`（NCE 真人切片）实跑验证：`source=native / valid=true / delta_comparable=true`。**残留**：IELTS prompt 10 条仍无真人参考音（`ref_audio=tts`）→ 其 Δ 一律不可比，需补录真人示范；历史 TTS 基线记录按需重跑（不改旧文件）。原记录（2026-11-25）：`assess(bank_id=…)` 真人原声优先（`gop_baseline_source=native`），韵律 `reference.source` 标注 native/tts/unknown，基线**加音素自检**（最差词级 GOP < −1.5 → invalid），CLI `--bank-id` + MCP `bank_id` + SKILL 口径。残留：IELTS prompt（10 条）没有真人音；历史 `metrics.json` 里的 Δ 是 TTS 基线算的，**不可与新结果横比**，需重跑；**新偏差**：NCE 原生切片 16 kHz vs 用户录音 48 kHz 的带宽不一致，摩擦音 GOP 会偏（要严格比需两侧同带宽） || D52 | **TTS 产出被当成“标准/基线”**：① `assess(baseline_tts=true)` 的 GOP 基线一律是合成音，且只做**内容**回读自检（词对就算过），音素读坏的基线会被当标准；② 韵律 DTW 的参考音大量是 `synthesize` 产出的 `.ref_norm/.ref_slow`，等于拿合成音的语调当人标准；③ `bank` 里的 `ref_audio=tts` 只在 plan 文本里提一句，代码不区分；④ 素材库 4 997 条 NCE 明明带**真人原声切片**，却没人用它当基线 | 2026-11-25 复盘 TTS 审计：TTS 自己会读错（country/thank/tokyo 回读不一致）、与 G2P 共用同一套音素变体（同形异音无法自救，D46）、合成语调不是人标准 | S1 | 任何“比标准示范低 X 分”的结论（已发生） | **部分偿还（2026-11-25）**：`assess(bank_id=…)` 真人原声优先（`gop_baseline_source=native`），韵律 `reference.source` 标注 native/tts/unknown，基线**加音素自检**（最差词级 GOP < −1.5 → invalid），CLI `--bank-id` + MCP `bank_id` + SKILL 口径。残留：IELTS prompt（10 条）没有真人音；历史 `metrics.json` 里的 Δ 是 TTS 基线算的，**不可与新结果横比**，需重跑；**新偏差**：NCE 原生切片 16 kHz vs 用户录音 48 kHz 的带宽不一致，摩擦音 GOP 会偏（要严格比需两侧同带宽） |