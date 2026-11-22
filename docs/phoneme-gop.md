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
| ④ `ort` 接入（Rust 侧推理） | ✅ `gop/model.rs`（load-dynamic + sha256 校验 + blank 校验） |
| ⑤ G2P（CMUdict：文本 → ARPAbet） | ✅ `gop/g2p.rs`（135 166 词，OOV 显式报出） |
| ⑥ so762 标定 | ✅ 见下节（结论：**只能当排序信号，不能当逐音素判决**） |

## 6.1 标定结果（2026-11-22，so762，120 句 / 2 015 个音素 / 223 个错读）

口径：`speechocean762` 的 `mispronunciations` 当金标；句级用人工 `accuracy` 分。

| 候选集 / 统计口径 | 句级 Spearman ρ | 音素 AUROC | τ(FP=10%) | 精确率 | 召回 |
|---|---|---|---|---|---|
| narrow / span | **0.659** | 0.653 | −1.63 | 0.171 | 0.166 |
| narrow / soft±2 | 0.564 | **0.659** | −1.08 | 0.227 | 0.238 |
| medium / span | **0.674** | 0.647 | −1.57 | 0.151 | 0.143 |
| medium / soft±2 | 0.580 | 0.656 | −1.06 | 0.231 | 0.242 |
| wide / span | 0.646 | 0.629 | −1.65 | 0.171 | 0.166 |
| wide / soft±2 | 0.565 | 0.638 | −1.14 | **0.241** | 0.256 |

### 三条硬结论

1. **句子级可用**：mean-GOP 与人工分 ρ≈0.67，方向与量级都对（人工分 0–5 组 mean GOP −1.71，
   9–10 组 +2.66）。→ 可以进趋势对比。
2. **音素级不够当判决**：AUROC 只有 0.63–0.66；在"误报 ≤10%"的保守操作点，
   精确率 **0.15–0.24** —— 也就是**报出的 4–6 个可疑音素里只有 1 个是真的**。
   这达不到我原先设的 "F1 ≥ 0.55" 目标，**不能对用户说"这个音素你读错了"**。
3. **加宽候选集并不能救精度**：wide（含 θ/s、v/w 混淆对）只是把召回从 0.166 抬到 0.256，
   精确率没本质变化 → 说明误差主要来自**对齐/模型判别力**，不是等价类宽度。

### 因此的设计决定（写进 Skill 与 CONTRACT）

- GOP 只用于 **① 句级趋势**、**② 句内排序（top-1/2 提示）**、**③ 与词级 diff 交叉验证**：
  只有"词级 diff 也标了这个词"**或**"GOP z 极端（< −2.5）且该音素属高影响类（θ/v/w/r-l/词尾辅音）"
  时才允许说"可能是发音问题"，且必须给"重录/对照示范音"的验证动作。
- `limitations` 必须带：`gop_precision_low`，措辞里不能出现"你读错了 /θ/"这类断言。
- 下一步要提升精度的话，按性价比排：① 用 so762 微调音素分类头（MDD）；
  ② 换更强的音素 CTC（Conformer/更大的 wav2vec2）；③ 用 MMS_FA 做参考对齐、GOP 只在已对齐区间算。

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

其它运行时依赖（同样有指纹）：

```
C:/voice_models/onnxruntime/onnxruntime.dll                 sha256 417bed96…（来自 pip onnxruntime 1.31.0，与 ort rc.15 匹配）
C:/voice_models/onnxruntime/onnxruntime_providers_shared.dll sha256 b5bf6295…
C:/voice_models/g2p/cmudict.dict  sha256 81917843c7f44ce2b094ac63873c2c7a4cf802040792c455ba3ca406891c3d22  135 166 词
```

`ort` 用 `load-dynamic`：不下载二进制、不参与链接，运行时按
`ORT_DYLIB_PATH` → `C:/voice_models/onnxruntime/onnxruntime.dll` 查找，找不到就报错降级。

## 混淆对信号（`confusion`）：绕开等价类的盲区

等价类把 `/θ/`→`/s/` 当"可接受"（当初为了压误报），结果是**显式的 θ→s 错误看不出来**。
但模型自己知道它听到了什么 —— `Aligned.best` 对应的符号（`heard`）就是证据。

所以在 so762 未见说话人上单独标定了"混淆对命中"（`heard` 落在该音素的常见混淆集里）：

| 音素 | n | 真错率 | 混淆命中 | **命中精确率** | 召回占比 |
|---|---|---|---|---|---|
| **TH** | 28 | 21% | 8 | **0.50** | **0.67** |
| R | 90 | 10% | 7 | 0.29 | 0.22 |
| Z | 78 | 14% | 10 | 0.20 | 0.18 |
| V / W / L / S / DH | 44/112/107/140/101 | 2–5% | 1–4 | **0.00** | 0.00 |

- 只有 **TH 系列（θ→s/f/t）** 达到可用精度（0.50，且能抓住 2/3 的真错），因此只有它允许对用户说；
- 其余混谱对只作为内部标记（`confusion` 字段有值，但 `confusion_reportable=false`）。

真机验证（同一人三次录音）：

| 音频 | 文本 | `heard` | confusion |
|---|---|---|---|
| 161152 | Thank you very much. | s | **TH→s** |
| 160606 | Why do the children thank… | s | **TH→s** |
| 160912 | Thank you. | s | **TH→s** |
| TTS 干净音 | I think this is the third time. | θ | 无 |

即：该学员的 /θ/ 在**单词和句子中都被听成 /s/**，与 `learner.json` 里 `theta_to_s` 的档案一致；
而 ASR 仍能把 `thank` 转对（ASR 对这类音变鲁棒），所以**只有声学层能抓到它**。
