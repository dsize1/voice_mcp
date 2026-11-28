# ADR-0008: 技术欠债登记（Debt Register）

- 状态：Accepted（活文档，每还一条就更新；最后更新 **2026-11-28**：D12/D13/D19/D20/D39/D45/D46/D47/D48/D49/D51/D56 已还，D04/D52/D53 部分偿还；未还：D50 + D06/D09/D11/D15/D18/D21/D29/D35/D36/D38/D40/D41/D44；新登记：D55（中文无声学级音素打分）、**D57/D58（TTS 材料可信度 + 验证链路假通过，2026-11-28 登记、暂不偿还）**；D56 当日即还）
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
| D13 | 中文只做**字级** diff，无拼音层 | **已还（2026-11-28）**：`assess::pinyin` —— 汉字 → 拼音（`crates/voice-core/data/pinyin-zh.txt`，26 703 字 / 7 310 多音字，pypinyin 0.55 派生、内置二进制、runtime 无模型依赖）；按**声母/韵母/声调**三层诊断，多音字取"最接近的一读"；`hint=likely_homophone`（同音不同字不算错，`accuracy_pinyin` 单独给口径）；已知混淆对（平翘舌/前后鼻音/n-l/尖团音）进 `confusions[]`；中文 `with_gop` 明确跳过并写 limitation；附带修掉中文数字未归一（`26`→`二十六`）。实测 10 句 TTS→ASR：字级 0.922 / 拼音层 0.933、**误报 0**（`docs/audit/pinyin-2026-11-28.md`）。残留：中文**声学**级音素打分（中文 GOP）仍未做；声调结论只能到 `unclear/medium`。见 `docs/pinyin.md` | ~~S2~~ ✅已还 | – | – |
| D14 | `accuracy = 1-(S+D+I)/N_ref` 对长句不公平（分母大），且不区分错误类型权重 | S3 | 用户开始练长句/Part 2 时 | 0.5d：改用 WER + 关键错误加权 |
| D41 | 英美拼写/发音变体表 `US_UK_VARIANTS` 需人工维护，只覆盖 50 对高频词；遇到表外变体仍会误报 | 训练中 `theatre/theater` 类误报 | S3 | 出现新的表外变体误报 | 0.5d/次：按真实误报补表，并加回归用例 |
| D46 | G2P 同形异音（`live`/`read`/`use`/`record`/`tomorrow`…）只取第一变体 → 读对也可能被判错 | 长期存在；2026-11-28 收口 | S2 | 所有跟读/朗读评分 | **已还（2026-11-28）**：`Gop::word_variants()` + `score_span()`——对多发音词用它自己的帧区间把每个变体各对齐一次、取平均 GOP 最高者，换变体时写 `gop.words[].variant`（证据）。实测 `tomorrow` 自动选中 `T UW M AA R OW`。单测 `score_span_prefers_the_variant_that_was_actually_spoken`（合成 log-prob，无模型） |
| D47 | 短语音（<2 s）SNR 估计误杀单词 drill | **已还（2026-11-25）**：`snr_verdict(snr, speech_duration, peak)` 纯函数 + 4 个单测。VAD 语音跨度 <3 s 且 peak ≥−25 dBFS → `SnrVerdict::LowButShort`：**不置 degraded、照跑 GOP**，只在 `limitations` 写明“SNR 会被低估”。历史失败样本（1.8 s word drill、SNR 12.6）已被覆盖 | ~~S2~~ ✅已还 | – | – |
| D48 | **readiness/free-speech gate 只读 JSON 标志，未自动从 history 计算** | `stage`/`missing` 手写会与真实历史漂移，`bank plan` 的提示话术跟着失真 | **已还（2026-11-28）**：`voice-core::readiness` —— 从 `history` 算「连续 N 条 acc / 语速带 / 停顿密度」三态判据（`met`/`not_met`/**`unknown`**，缺数据不当通过），`word` 单词 drill 与 `free` 无稿不参与连续统计；CLI `voice-cli learner readiness [--write] [--confirm-free-speech]`：写回只动 `readiness`（保留人工写的 evidence，写前留 `.prev`），开自由说需 L4 达标 + 显式确认。核心音素回归仍标 `unknown` 交 Skill 判读（避免把 ASR 误听当发音错）。9 个单测；真机 `learner.json` 已复算（L2+，missing 由 4 条手写改为 2 条明确未达标 + 3 条 unknown） | ~~S2~~ ✅已还 | – | – |
| D49 | 声学闸门通过 ≠ 文本可用 | **已还（2026-11-25）**：`assess.text_agreement`（词级 LCS 一致率 `2·|LCS|/(|a|+|b|)`）。给了 `reference` 且非中文时用另一个引擎复核，`<0.7` → `degraded=true` + limitation（只出 flow/prosody）；第二引擎不可用只记 limitation。3 个单测 + 9 条录音回归零误杀（clean case agreement=1.0） | ~~S2~~ ✅已还 | – | – |
| D56 | **录音回归用例 2/9 失败**（`r01_third_16k` / `r06_nce2_l2_native`）| ASR 输出随模型/语言锁定/数字归一演进漂移：`third` 从 `straight` 变 `three`、导语 `Lesson2` 从一个粘连 token 变成 `lesson` + `two` | **已还（2026-11-28）**：`cases::OneOrMany` —— `said` / `hint` / `severity` 可写**等价集合**（命中任一即过，单字符串写法向后兼容），r01 改 `said:[straight,three]`、r06 导语改 `[lesson2,lesson]`；`voice-cli cases` **9/9 全绿**（复测 3 次稳定）+ 2 个单测。判据：集合里每项必须能用"同一现象"解释，不许为了绿而乱放宽 | ~~S2~~ ✅已还 | – | – |
| D55 | **中文没有声学级发音打分**：现有音素 CTC 模型是英/多语 espeak 音素，中文未标定 → 中文只能用**文本层**拼音诊断（D13），声调/音素的实际声学实现无法判断 | D13 上线时明确划出边界；`with_gop` + 中文参考会跳过并写 limitation | S2 | 中文纠音要求"具体哪个音不对"（不只是"哪一层不对"）时 | 3–5d：中文音素 CTC（espeak-ng zh 音素集）微调 + 少量标注；兜底方案是中文音节级 GOP（拼音串当参考序列） |
| D50 | 韵律的 12 条人工标注验收未做（升降/平/重音一致率 ≥80%）；短句窗口与 `unreliable` 已加，但无真值标定 | ADR-0007 验收项；目前只有 10 单测 + 少量真机 | S2 | 下一次语调专项训练 | 0.5d + 标注 2h |
| D51 | TTS 参考音未验证就当基准 | **已还（2026-11-24/25）**：OOV 预检 + 项目 overlay + `tts-verify`（双 ASR/音素/WPM/3 次）+ `gop_baseline_valid`；**`bank refs` 把同一套验证接进批量材料生成**（失败自动换目标语速 → 换音色，只有 verified 落盘）；10/10 IELTS prompt verified 3/3（含音素）。另修：WPM 改按语音跨度、语速校正保留最优候选、`onset_gap_ms` 起始伪影检测。见 `docs/audit/tts-2026-11-24.md` §12 | ~~S1~~ ✅已还 | – | – |
| D52 | **TTS 产出被当成“标准/基线”**：① `assess(baseline_tts=true)` 的 GOP 基线一律是合成音，且只做**内容**回读自检（词对就算过），音素读坏的基线会被当标准；② 韵律 DTW 的参考音大量是 `synthesize` 产出的 `.ref_norm/.ref_slow`，等于拿合成音的语调当人标准；③ `bank` 里的 `ref_audio=tts` 只在 plan 文本里提一句，代码不区分；④ 素材库 4 997 条 NCE 明明带**真人原声切片**，却没人用它当基线 | 2026-11-25 复盘 TTS 审计：TTS 自己会读错（country/thank/tokyo 回读不一致）、与 G2P 共用同一套音素变体（同形异音无法自救，D46）、合成语调不是人标准 | S1 | 任何“比标准示范低 X 分”的结论（已发生） | **部分偿还（2026-11-28 补）**：①`Assessment.delta_comparable` 字段——只有「基线存在 + 来源 `native` + 自检通过」才 `true`，落盘进 `metrics.json`、也进 MCP/CLI 输出；②`tools/audit_baseline.py`（只读）清点历史：280 条 metrics 里 `native` 5 / `tts` 6 / `absent` 35 / 无基线 234 → **Δ 可跨期比较的只有 5 条**，报告见 `docs/audit/d52-baseline-audit.md`；③用 `assess --bank-id`（NCE 真人切片）实跑验证：`source=native / valid=true / delta_comparable=true`。**残留**：IELTS prompt 10 条仍无真人参考音（`ref_audio=tts`）→ 其 Δ 一律不可比，需补录真人示范；历史 TTS 基线记录按需重跑（不改旧文件）。原记录（2026-11-25）：`assess(bank_id=…)` 真人原声优先（`gop_baseline_source=native`），韵律 `reference.source` 标注 native/tts/unknown，基线**加音素自检**（最差词级 GOP < −1.5 → invalid），CLI `--bank-id` + MCP `bank_id` + SKILL 口径。残留：IELTS prompt（10 条）没有真人音；历史 `metrics.json` 里的 Δ 是 TTS 基线算的，**不可与新结果横比**，需重跑；**新偏差**：NCE 原生切片 16 kHz vs 用户录音 48 kHz 的带宽不一致，摩擦音 GOP 会偏（要严格比需两侧同带宽） || D52 | **TTS 产出被当成“标准/基线”**：① `assess(baseline_tts=true)` 的 GOP 基线一律是合成音，且只做**内容**回读自检（词对就算过），音素读坏的基线会被当标准；② 韵律 DTW 的参考音大量是 `synthesize` 产出的 `.ref_norm/.ref_slow`，等于拿合成音的语调当人标准；③ `bank` 里的 `ref_audio=tts` 只在 plan 文本里提一句，代码不区分；④ 素材库 4 997 条 NCE 明明带**真人原声切片**，却没人用它当基线 | 2026-11-25 复盘 TTS 审计：TTS 自己会读错（country/thank/tokyo 回读不一致）、与 G2P 共用同一套音素变体（同形异音无法自救，D46）、合成语调不是人标准 | S1 | 任何“比标准示范低 X 分”的结论（已发生） | **部分偿还（2026-11-25）**：`assess(bank_id=…)` 真人原声优先（`gop_baseline_source=native`），韵律 `reference.source` 标注 native/tts/unknown，基线**加音素自检**（最差词级 GOP < −1.5 → invalid），CLI `--bank-id` + MCP `bank_id` + SKILL 口径。残留：IELTS prompt（10 条）没有真人音；历史 `metrics.json` 里的 Δ 是 TTS 基线算的，**不可与新结果横比**，需重跑；**新偏差**：NCE 原生切片 16 kHz vs 用户录音 48 kHz 的带宽不一致，摩擦音 GOP 会偏（要严格比需两侧同带宽） |
---

## 三、材料可信度与验证链路（2026-11-28 登记，**暂不偿还**）

> 起因：Day 13 复习块冷测（`up-main/.memory/private/2026-11-28/review/r1–r6`）出现 3 次「听到的内容 ≠ 答案键」：
> r1 开头听成 "dito"（实为 `The two`）、r2 `files` 词尾 `-z` 在音频里不存在、r3 `updated` 的 `-ed` 被怀疑丢失。
>
> 排查方法：**phoneme-CTC 自由解码**（无词表、无 LM）+ 4–8 kHz 擦音能量，与既有 GOP（forced alignment）交叉验证。
> 结论：r3 的 `-ed` **在**（`… ʌ p d eɪ t ɪ d b aɪ …`）；r2 的 `-z` **确实不在**（TTS 丢的，不是 ASR 吞的）；r1 的 `The` 被读成 **/ðiː/**（材料缺陷）。

| ID | 欠债 | 级别 | 触发偿付条件 | 预估 |
|---|---|---|---|---|
| **D57** | **TTS 生成物不能当听力素材**：前端无弱读、声学层丢词尾、难度与真人错位 | S1 | 把 TTS 产出放进 `bank`/复习块当听力材料；用于难度校准或估分；学员报告「听到不存在的词 / 听不清词尾」 | 见下 |
| **D58** | **ASR 词级结果不能证明 TTS 音频正确**：`tts-verify` 的 `usable` 是「任一引擎逐词对即过」，音素判据是词级均值 → 结构性假通过 | S1 | 每次生成/复核材料；任何「材料已验证」的声明；`bank refs` 批量落盘 | 见下 |

### D57 详情（证据）

1. **前端词表只有强读形式**（`C:/voice_models/kokoro-multi-lang/lexicon-us-en.txt`）：
   `the ð i`(L168882)、`to t u`(L169741)、`and æ n d`(L100468)、`a A`(= /eɪ/，L93395)、`for f ɔ ɹ`(L124319)、
   `him h ˌ ɪ m`(L129720，**带次重音**)。GB 词表有 `the ð ə`，但工程固定「美音优先」
   （`crates/voice-core/src/engine/tts.rs:276–284`），GB 只填空缺 → **所有含 the 的英文素材都是 /ði/**。
   实测：句首 `The` = /ðiː/（F1 245–290 Hz、F2 2100–2600 Hz、100–160 ms）；`to` ≈ 300 ms、`and` ≈ 460 ms（真人弱读 60–120 ms）。
2. **声学层吞词尾**：`Several files were missing…` 的 G2P 是 `fˈIlz`，音频里没有 /z/；
   110 wpm 与 140 wpm **都缺**（`files` 尾段 4–8 kHz 相对 0–1 kHz 最大值 **−23.6 / −23.7 dB**，同句句首 /s/ 对照 **+16.8 dB**）；
   CTC 自由解码两速都是 `… f aɪ l │ w ɜ …`。**慢速不补尾音**（否决「0.75×/0.5× 慢放」这条产品兜底）。
3. **难度错位**：TTS 140 wpm、干净单声道；入库 IELTS 听力真人 180–214 wpm、有弱化与连读。

**影响**：听力输入量不足、无法自审的学员会把材料缺陷当语言事实（把 `/ði/` 学成 the 的读法；把缺 -z 的 `files` 记成自己的词尾错误）；
且 TTS 的「清楚」会抬高听力正确率，换真人即崩。

**处置（暂不实施）**：材料三级 —— **L-A 真人原声**（唯一可信答案键）> **L-B TTS 且已过音素门禁**（自由解码 + 擦音/浊音能量）>
**L-C 未审 TTS**（只给自己练，禁止进训练计划/复习块/听力题）。词尾辨析题（`-ed/-s/-z/-t/-d`）只用 L-A/L-B。
可选修法（数据层，一行一类）：`tts-lexicon.txt` 加 `the ð ə`、`to t ə`、`a ə`、`and ə n`、`for f ɚ`、`him ɪ m`；
代价是元音前 "the" 也变 /ðə/（属轻微不标准，优于教错强读形式）。慢速只用于跟读、不进听力训练；难度校准只用真人集。

### D58 详情（证据）

**双向失效**（同一个门禁既假通过、也假拒绝）：

- 补音：孤立 `open`（G2P `ˈOpᵊn`，无 /d/；自由解码 `oʊ b ɛ n t`）→ whisper 输出 **"are opened."**；
  `He open the parcel…` / `…were very surprise…` → **双引擎**都补 `-ed`；r3 截到 1960 ms（只剩 `… ʌ p d eɪ`）→ whisper 仍输出 "updated."。
- 吞音：r2 `files` 的 /z/ → 双引擎都听成 "file"。
- 规律：`-ed` 独立成音节（/ɪd/，`updated`/`wanted`）双引擎判**对**；退化成单个 `/d,t/`（`open`/`surprise`/`walk`）双引擎都**补**。

**代码层根因**（`crates/voice-cli/src/main.rs::verify_synthesized`）：

- `usable = (sv_ok || wh_ok) && !wpm_extreme && !phone_suspect` —— **任一引擎逐词对即通过**；
- 音素判据 `min(word.gop_mean) < −1.5` 是**词级均值**：r2 的 `Z` 是整句最差音素（`gop −7.53 / z −6.22`），
  但被 `files` 的词均值 1.28 稀释，全句最小词均值仅 −0.19 → `phone_suspect=false`；
- **forced alignment 必塞满参考音素**：r2 没有 /z/，对齐器仍输出 `Z heard=z`（分数烂但音标在）→「对齐成功」≠「音存在」。

**处置（暂不实施，先做两个实验再定投入）**：

- 三层判据：**L0 前端音素串**（0 成本，直接看 `files[].phonemes` 抓强读 / `-ed` 规则 / 漏词）/
  **L1 音频**（自由解码「参考有、解码无」+ 擦音 4–8 kHz、`/d/` voice-bar 能量）/
  **L2 双 ASR**（只判整词漏 / 多 / 序）。
- 实验 A：多段 `phoneme_codepoints`（`Vec<Vec<i32>>`，每 inner 一句）+ `silence_scale` = **无 SSML 的 break**，看能否补出 /z/。
  已排除两条捷径：加逗号无效（`phonemes` 与时长逐位相同：4.051 s / 141.360992 wpm）、慢速无效（见 D57）。
- 实验 B：`blank_penalty` / `ctc_fst_decoder(graph)` 能否压低 SenseVoice 的 `-ed` 补偿。

### 记录：两套方案与取舍（2026-11-28，暂不实施）

外部评审提出的方案：①SSML `<break>/<phoneme>` 强制发音 ②关 / 降 ASR 语言模型权重 ③引 AED（音素强制对齐）④慢速 + UI 高亮词尾。
本仓库方案：L0/L1/L2 三层判据 + 材料分级 + 学员答案反审材料。

| 外部方案 | 在本栈的取舍 |
|---|---|
| SSML 强制发音 | sherpa-onnx **不解析 SSML**（绑定无该 API），但 `GenerationConfig.phoneme_codepoints`（每 inner 一句）+ `silence_scale` 可**等价实现** break；`<phoneme>` 等价物已经存在（我们就是 phonemizer）。属**实验 A** |
| 关 / 降 LM 权重 | whisper 配置**没有** LM/beam/temperature 旋钮；SenseVoice 是 **CTC，没有 LM**，照样补 `-ed` → 关 LM 治不了。存在且可用的旋钮是 `blank_penalty` / `decoding_method` / `ctc_fst_decoder_config{graph}`（= 传统 CTC+FST 解码入口）→ **实验 B** |
| 引入 AED | **已经有**（`crates/voice-core/src/gop`，AUROC 0.769，`GOP_LIMITATION` 已写明「只能排序、不能单独判定」）；且有「必塞满」盲点，必须配自由解码 |
| 慢速 / UI 高亮 | 慢速**已被实测证伪**（见 D57）；UI 高亮在听力题里等于替学员解码（反教学），且本仓库无 UI |

**关键取舍**：生成侧「修」与校验侧「审」正交 —— 但**没有度量之前无法判断「修好了」**（r2 就是活例：ASR 全对、音频是错的），
所以顺序是**先立 L0/L1，再谈修**。另：外部方案的「强制清晰 / 慢速」会进一步抬高正确率、
**恶化与真人语流的效度**，因此 TTS 的角色限定为「发音示范 / 跟读 / 听写提示音」，**不作为听力素材**。

**已同步**：MCP 工具说明（`synthesize` / `assess`）+ `docs/CONTRACT.md §synthesize` 已写入材料分级与「不要当听力素材」。

**证据文件**：`D:/voice_data/sessions/2026-11-28/` 下 `203452_tts`(110/140 wpm)、`203503_tts`(逗号无效)、
`202454_tts`(功能词句)、`202416_tts`(r1 复现)、`200332–200735_tts*`(open/opened、surprise(d)、want/wanted 等最小对)；
分析脚本 `C:/temp/r3check/greedy2.py`（仓库外，独立跑同一 phoneme-CTC 模型做自由解码，不改仓库代码）。
