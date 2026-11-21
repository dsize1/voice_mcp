# 音素级 GOP（S1 实现笔记）

> 目标：给"发音是否偏离"一个**声学测量**，LLM 只做解释（ADR-0006）。
> 这里记录选型、实测数字、以及**不许越界的边界**。

## 1. 选型：为什么是 espeak 音素 CTC

| 方案 | 结论 | 依据 |
|---|---|---|
| sherpa-onnx 统一承载 | ❌ | 只暴露 CTC **token** 时间戳（±0.5 s），不暴露 logits/后验，也没有音素对齐 API |
| Python sidecar（MMS_FA + torchaudio） | ⏸ 备选 | 对齐更准，但引入 Python 运行时 + 进程生命周期 |
| **音素 CTC ONNX + 自研 Viterbi**（采用） | ✅ | 运行时纯 Rust；GOP 只需要**后验**，不需要外部对齐器 |

模型：`facebook/wav2vec2-lv-60-espeak-cv-ft`（espeak 音素集，60 语种，英文与中文共用一套）
导出：`tools/export_phoneme_ctc.py`（一次性；之后运行时只读 onnx）

## 2. 实测数字（本机 CPU，Core Ultra 5 245K）

| 指标 | 值 |
|---|---|
| 模型体积 | fp32 **1 205 MB**（int8 303 MB，**弃用**） |
| int8 量化漂移 | 与 fp32 logits 的 `max|Δ| = 8.21` → 后验被打乱，判定不可用 |
| 单句推理 | 2.8–3.4 s 音频 **110–173 ms**，**RTF 0.04–0.05** |
| 帧率 | 20 ms/帧（wav2vec2 conv 下采样） |
| CTC blank | id 0（`<pad>`），词表 392 个符号 |

贪心解码抽查（so762 真实句子，中文母语者朗读）：

```
MARK IS GOING TO SEE ELEPHANT   →  m ʌ k ɪ z ɡ oʊ ɪ ŋ t ə s iː ɛ l ɪ f ə n t
TWO SIX FOUR EIGHT              →  t uː s ɪ k s f ɔːɹ ɹ eɪ t
```

注意 `MARK` 的 `AA` 被记成 `ʌ`、`TO` 的 `UW` 被弱化成 `ə` —— 这是**参考音素与模型音素集不一致**的直接证据，
所以映射必须用**等价类**（见下）。

## 3. 参考音素映射（`crates/voice-core/src/gop/phones.rs`）

- 参考来自 CMUdict（ARPAbet），模型输出 espeak IPA；
- 每个 ARPAbet 音素映射到**一组可接受符号**，GOP 取组内最大：
  `AA → {ɑ, ʌ, a, ə}`、`UW → {u, ʊ, ə}`、`TH → {θ, f, s, t}`、`R → {r, ə, ɜ}` …
- 规范化：去掉重音/长音/鼻化，`ɡ→g`、`ɹ→r`、`ɫ→l`、`ɾ/ʔ→t`、`ɐ→ə`；
- **等价类宁宽勿窄**：多收一个符号只会漏报（FN），少收一个会把标准发音判成错（FP），
  而 FP 会直接毁掉用户信任。

代价（写进 `limitations`）：等价类内部无法区分（例如 `AA` 与 `AH` 都落在 `ʌ/ɑ`），
所以**不对这类对比做断言**。

## 4. 算法（`crates/voice-core/src/gop/ctc.rs`）

```
log_softmax(每帧) → 单调 CTC 对齐（DP，帧数 T × 音素数 L）
                 → 每个音素的 GOP = mean_t [ log P(ref) − max_{q≠ref} log P(q) ]
                 → speaker_normalize（中位数 + 1.4826·MAD）
```

- 完美匹配 → GOP 为大正数；错读 → 显著负值（单测锁死这两个方向）；
- 等价类命中 → 不判错（单测 `candidate_classes_avoid_false_errors`）；
- 帧数少于音素数 → 直接放弃对齐（音频太短/参考异常），不硬算。

## 5. 与数据集的闭环

- **标定**：speechocean762 的音素级 `mispronunciations` 给"哪些音素被真人判错"，
  用它们定 `tau_cap`（目标：音素级 **误报率 ≤ 10%**）；
- **分级阈值**：`tau_eff(level) = clamp(tau_base + delta(level), tau_floor, tau_cap)`，
  低水平更宽松、高水平更严，但**任何档位都不许越过 `tau_cap`**；
- **验证分级是否有意义**：按人工分把人分低/中/高三组，检验 GOP 分布是否显著分离
  （Kruskal-Wallis，p<0.01）；不显著就只做说话人内归一化，不做档位调整。

## 6. 进度

| 步骤 | 状态 |
|---|---|
| ① 模型导出 + 精度/延迟实测 + int8 否决 | ✅（本文档） |
| ② 音素映射 + 规范化 | ✅ `gop/phones.rs`（含单测） |
| ③ CTC 对齐 + GOP + 说话人归一化 | ✅ `gop/ctc.rs`、`gop/mod.rs`（9 个单测） |
| ④ `ort` 接入（Rust 侧推理） | ⏳ 下一步 |
| ⑤ G2P（CMUdict：文本 → ARPAbet） | ⏳ 下一步 |
| ⑥ so762 阈值标定 + 与词级 diff 的 A/B | ⏳ |

## 7. 运行时依赖与指纹

```
模型目录  C:/voice_models/phoneme-ctc/
  model.onnx   sha256 e86bb3af1afef5455a48c7d81c615375ca853eb1fd62bb500dca3709cb67dd37  1 264 MB
  vocab.json   392 符号（index 0 = <pad> = CTC blank）
  metadata.json  预处理/帧率/精度/量化漂移记录
预处理   逐句 zero-mean / unit-variance（**不是 RMS 归一化**），16 kHz 单声道
```

**不变量**：`onnx` 的 sha256 必须与 `metadata.json` 一致；`vocab.json` 的 index 0 必须是 `<pad>`。
两者任一不符 → 拒绝启用 GOP，回退到词级 diff 并在 `limitations` 标注。
