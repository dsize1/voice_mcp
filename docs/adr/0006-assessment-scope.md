# ADR-0006: 评测层的能力边界（P2 词级诊断）

- 状态：Accepted
- 日期：2026-11-19
- 相关：ADR-0001（运行时）、ADR-0002（ASR）、CONTRACT.md

## 背景

P2 要在**没有音素级对齐**的前提下，先给出对教学有用的诊断。核心问题是：
**从「参考文本 + ASR 转写」究竟能推断出什么、不能推断出什么？**

设计过程中有两个实测发现直接改变了方案：

### 发现 1：拼写相似度不能区分「发音问题」与「说成别的词」

用归一化 Levenshtein 算拼写相似度：

| 词对 | 拼写相似度 | 实际关系 |
|---|---|---|
| think → thing | 0.80 | 发音问题（-k/-g） |
| **third → straight** | **0.25** | **发音问题（/θ/→/s/ + 插入 /r/）** |
| cat → elephant | 0.25 | 完全不同的词 |

`third→straight` 和 `cat→elephant` 的拼写相似度**完全一样**，说明这个指标对真实的口音混淆几乎无信息量。

### 发现 2：加一层音形编码后也只能做三态判断

自研 `assess::phonetic` 把英文压成粗略音形骨架（`th→0`、`sh→S`、`voicing` 归并、元音压缩）：

| 词对 | 音形 key | 音形相似度 | 判定 |
|---|---|---|---|
| think / thing | `0ANK` / `0AN` | 0.75 | 发音问题（可信） |
| thin / sin | `0AN` / `SAN` | 0.67 | 发音问题（θ/s，正是目标场景） |
| three / tree | `0RA` / `TRA` | 0.67 | 发音问题 |
| world / word | `WARLT` / `WART` | 0.80 | 发音问题（吞 /l/） |
| **third / straight** | `0ART` / `STRAT` | **0.40** | **无法判定** |
| cat / elephant | `KAT` / `ALAFANT` | 0.17 | 说成别的词 |

## 决策

### D1. 替换项一律输出**三态推断**，不假装确定

```
hint = likely_pronunciation   (音形≥0.6 或 拼写≥0.6)
     | likely_different_word  (音形≤0.35 且 拼写≤0.45)
     | unclear                (其余，含 third→straight)
```

`unclear` 时 `note` 明确写"从文本无法判断是发音偏差还是 ASR 误听，可让用户重录该词或对照示范音确认"。
**不允许**把它悄悄归到任一类——错误的归类会让 LLM 给出错误的纠音建议，比不给建议更糟。

### D2. 问题类型只保留三种：substitution / omitted / extra

打分与严重度：

| 类型 | 严重度 | 含义 |
|---|---|---|
| substitution | high | 意思变了，必须处理 |
| omitted | high | 漏读/吞音，通常是词尾辅音或弱读过度 |
| extra | medium | 插入音、口头禅、或多说的词 |

`accuracy = 1 - (S+D+I)/N_ref`，只作为**相对趋势**指标，不作为分数对外承诺。

### D3. 明确写进输出的 `limitations`（给 LLM 的免责声明）

每次评测都返回当前能力的边界，Agent 必须据此调整措辞：

1. `band_limited`（源音频 ≤16 kHz）→ /θ/ vs /s/ 一类判断置信度下降
2. SenseVoice 的 CTC token 时间戳精度约 ±0.5 s → 词界停顿时序仅供参考
3. 无音素级 GOP → **不能**给音素级发音分数
4. 音形编码**不区分元音长短**（bit/beat、bad/bed 编码相同）→ 元音长短必须等 GOP

### D4. 语流指标用 VAD，且必须分块喂

`sherpa-onnx` 的 Silero VAD 一次性喂整段音频时**只返回最后一段**（实测 6.08 s 音频只得到 0.22 s 语音，
导致 articulation rate 算出 1891 wpm 的荒谬值）。必须按 `window_size=512` 分块喂入，
修复后同一音频得到 2.60 s 语音 / 2 个停顿 / articulation 161 wpm，与听感一致。

这条已写进 `engine/vad.rs` 的注释，`crates/spike/src/bin/vad_probe.rs` 保留了对比证据。

## 后果

- ✅ 第一版诊断已经能覆盖用户最初提的目标：**漏读/多读/替换 + 语速/停顿/词界 + 填充词/重复**。
- ✅ 对 `third→straight` 这类案例，系统给出的是"这里有问题 + 时间位置 + 两种可能"，而不是错误的结论；
  P2 第 4 步（GOP）上线后，同一案例会被自动升级为音素级结论。
- ⚠️ 词级定位依赖 ASR token 时间戳精度；跟读场景下 ±0.5 s 足够让 Agent 说"第 5 秒那个词"，
  但不足以做"这个词的 /r/ 拖长了 80 ms"。
- ⚠️ 中文只做字级比较（无拼音层），中文发音诊断需等 GOP/拼音方案。

## 回归用例（P2 基线）

| 用例 | 输入 | 期望 |
|---|---|---|
| R1 | `test.m4a` + 参考 "I think this is the third time." | 1 个 substitution：third→straight，hint=unclear，t≈4.50–5.04 |
| R2 | 同上，语流 | speech_rate≈69 wpm，pause_count=2，longest≈0.54 s，speech_ratio≈0.43 |
| R3 | "I'm fine" vs 识别 "I am fine" | 0 问题（缩写展开后一致） |
| R4 | 中文 "我觉得非常好" vs "我觉得很好" | 总错误 2，accuracy<1 |
| R5 | 参考 "I'm fine." vs 识别 "I am fine"（3 词） | 0 问题，`timestamp_alignment=exact`，各词时间不被拉伸 |
| R6 | 参考 "I'm fine." vs 只识别到 "fine" | 1 条 omitted，`reference_surface="I'm"`（不重复报 2 条） |
| R9 | 真机 48 kHz 录音（句中停 1 s）| SenseVoice 输出 "I think this the third time"（漏 `is`），同一音频 whisper 输出 "I think it is the third time"。→ 短功能词漏读必须标 `hint=likely_asr_miss` + `severity=medium`，不允许当 high 指控用户 |
| R8 | **真机 48 kHz 麦克风**（Hands-Free，2.22 s 语音）| accuracy=1.0、0 问题、`exact`、`band_limited=false`、`analysis_window`≈1.92 s、speech_rate 218 wpm（快速朗读）。同一句用 16 kHz 录音机录制时曾识别为 "straight"——说明源音频质量直接决定能否捕捉 /θ/ |
| R7 | **TTS 音频当完美发音者**：`synthesize` 生成的句音频 → `assess(同一个 reference)` | accuracy=1.0、0 问题、`exact`、`band_limited=false`（24 kHz 源）。用于验证"纯净音频不会产生误报" |
