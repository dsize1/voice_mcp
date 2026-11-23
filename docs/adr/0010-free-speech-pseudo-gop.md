# ADR-0010: 无参考自由说的伪参考 GOP（Route A）—— 功能立项，暂不实现

- 状态：**Proposed / Deferred**（2026-11-23 立项，明确不进入当前迭代）
- 日期：2026-11-23
- 相关：ADR-0006（评测边界）、ADR-0007（GOP/韵律）、ADR-0008（D04/D09/D37/D43）、
  `docs/phoneme-gop.md`、`docs/STATE.md`
- 决策：**记录设计与验收标准，先不开工**；当前迭代回到原定路线（素材库定向训练）。
  等触发条件满足后，按本文实现，且实现时保持现有接口不被破坏。

## 一、背景

自由说（无 reference）现在只有：

```
ASR 文本 + flow（语速/停顿/填充词）+ prosody（F0/升降/平调）
```

没有发音维度。现有 GOP 的启动条件是：

```rust
gop::should_run(with_gop, has_reference, band_limited)
```

无参考文本 → `gop: null`，只写 limitation。这是 ADR-0006 的刻意边界，不是 bug。

Route A 的想法：**用双 ASR 的一致词拼出“伪参考文本”，再跑现有 GOP**。它不能消除
“自由说没有目标”的根本问题，但能把自由说从“完全不敢谈发音”推进到**有覆盖率的筛查**。

## 二、已实测的可行性（2026-11-23，1 条 51.8 s 样本）

| 项 | 结果 |
|---|---|
| SenseVoice / whisper 词数 | 82 / 83 |
| 完全一致词（LCS） | 69 词 → **覆盖率 84.1%**（下界，未做音形相似合并） |
| 一致词在 CMUdict 的覆盖 | **100%**（69/69） |
| SenseVoice OOV | `diarrow`、`networker`（都是 ASR 误听） |
| whisper 延迟（热态，51.8 s） | **19 s** |
| 端到端预估（双 ASR + GOP） | **≈22 s / 60 s 音频** |
| 共识中的错误例 | `i mistake`、`i for a problem`（两引擎同错） |

结论：英文自由说的 G2P 覆盖不是瓶颈；**双引擎同错是真实风险**，不是理论问题。

## 三、决策

1. **暂不实现**。当前迭代优先原定路线（素材库/定向训练/雅思素材），A 只留设计。
2. 触发条件（任一）：
   - D04（whisper 回归）已偿还；
   - 有 ≥10 条英文自由说样本、且用户明确要求“自由说也能纠音”；
   - 跟读/素材路径的发音提升进入平台期（词级 GOP 已贴天花板）。
3. 实现时：
   - 默认关闭，参数 `free_speech_gop: bool = false`；
   - 输出**独立字段** `gop_pseudo`，绝不与 `gop` 合并、不覆盖 `accuracy/issues`；
   - 只支持 `lang=en`、源采样率 ≥24 kHz、通过现有 SNR/电平/削波闸门；
   - 措辞固定为“**筛查，不是诊断**”，一次只报 top-1/2。

## 四、设计（实现时按此落地）

```
assess(path, reference=null, free_speech_gop=true)
  ├─ asr_sv   = transcribe_long(engine="sense-voice", lang="en")
  ├─ asr_wh   = transcribe_long(engine="whisper",     lang="en")
  ├─ tokens   = normalize::words(both)
  ├─ align    = LCS/Levenshtein + phonetic::similarity ≥ 0.9
  ├─ consensus= 仅保留一一对应且相似的词（保序）
  ├─ coverage = consensus_words / max(sv_words, wh_words)
  ├─ gop      = gop::score_text(consensus_text)     # 现有实现
  └─ output   = gop_pseudo { coverage, words[], unverified[], engines }
```

### 输出契约（草案）

```json
{"gop_pseudo": {
  "version": "gop-pseudo-0.1",
  "reference_source": "asr_consensus",
  "engines": ["sense-voice", "whisper-turbo"],
  "coverage": 0.841,
  "scored_words": 69,
  "words": [{"word": "think", "gop_mean": 0.42, "t0_ms": 4120, "t1_ms": 4510,
             "agreement": "exact", "worst_phone": "TH"}],
  "unverified": [{"sv": "front", "wh": "front end", "reason": "mismatch"}],
  "limitations": ["pseudo_reference", "asr_consensus_blind_spot"]
}}
```

- 只输出词级；音素级仍只做句内排序，禁止断言。
- `coverage < 0.5` → 不输出 `words`，只输出 coverage + limitation。
- whisper 不可用/失败 → 不输出 `gop_pseudo`，追加 limitation，绝不退回单引擎。

### 算法要点

1. 两个引擎使用**同一套 VAD 分块**（现有 `transcribe_long`），避免块边界不同造成假分歧；
   若实现时发现边界抖动，改为“各自全文 + 全局对齐”。
2. 一致性判定：归一化 `normalize::words` → 小写去标点 → 完全相同，或
   `phonetic::similarity ≥ 0.9`（避免 login/logging 这类形态差异被误杀）。
3. 伪参考文本用**一致词的原文**拼回；OOV 词保留但 G2P 会跳过，计入 coverage 缺口。
4. 复用现有 `gop::score_text`（CMUdict/G2P/CTC-Viterbi 都不变）。
5. 阈值：**先不要用真参考的 `1.30/0.30/−0.70`**；伪参考先只报句内百分位（top 10%），
   等 V9 标定后再决定绝对阈值。

## 五、风险与边界

| # | 风险 | 处理 |
|---|---|---|
| R1 | 误报（自发语流弱读/吞音被判错） | 只报 top-1/2；必须给“单独重读该词再确认”；V5 量化 |
| R2 | 双引擎同错 → 拿错词当参考 | 输出 `unverified` 与 `agreement`；不承诺严重替换召回；与词级 diff 交叉 |
| R3 | 域偏移（gonna/wanna/flap t） | V5 误报实验；必要时 reductions 白名单 |
| R4 | 覆盖率不稳定（专名/数字/技术词） | coverage 门槛 0.5；低覆盖不产出 |
| R5 | 延迟 ≈22 s/60 s + 全局锁（D15） | 只在自由说结束/复评时跑，不逐轮跑 |
| R6 | whisper 不可复现（D04） | 先还 D04；同文件 3 次一致性 ≥0.95 |
| R7 | 阈值漂移 | 百分位优先；V9 重标定 |
| R8 | 中文/混说不可用 | `lang=en` 硬限制；文档与 Skill 明确 |
| R9 | 产品预期被抬高 | 字段名 `gop_pseudo` + `reference_source` + “screening” |

**明确做不到**：两引擎同错的替换、OOV 词、中文、16 kHz、低 SNR；不提高音素级 0.45 上限；
不替代跟读 GOP；不产出 band 分。

## 六、验收（实现前先跑，Go/No-Go）

| 实验 | 方法 | 通过线 |
|---|---|---|
| V1 复现性 | 同一 51.8 s 跑 3 次 | 共识词 Jaccard ≥0.95 |
| V2 覆盖率 | 8–10 条不同话题英文自由说 | 中位 coverage ≥80%（音形合并后 ≥85%）；content OOV <10% |
| V3 替身一致性 | 9 条回归用例去掉 reference，比较伪 GOP 与真 GOP/diff | top-3 重合 ≥2/3 或 Spearman ≥0.6；flagged precision ≥0.5 |
| V4 真人精度 | 10–20 条自由说人工标注 | top-2 precision ≥0.6；<0.4 不上线 |
| V5 连读误报 | gonna/wanna/kinda/flap t/elision 各 2 句 | 正常连读 FP <20% |
| V6 严重替换 | 自由说里 why→when、thank→sack | 只测量召回（预期低），写进限制 |
| V7 延迟 | 60 s，冷/热各 3 次 | p95 <25 s |
| V8 降级 | whisper 失败/16 k/低 SNR/coverage<50% | 无 `gop_pseudo`，只 limitation，不崩 |
| V9 标定 | 伪 vs 真参考的 word GOP 分布 | 均值/方差差 ≤0.5，否则用百分位 |
| V10 回归 | 现有 105 单测 + 9 用例 | 全绿，跟读路径不变 |

## 七、当前迭代必须保持的接口（为了 A 以后能接上）

即使现在不实现，以下约束从现在起必须遵守：

1. `Transcript` 保留 `words[]`（词级时间戳）与 `tokens[]`，不要只存整句文本。
2. `normalize::{tokens,words}` 与 `phonetic::similarity` 保持公开、确定性；不要改成依赖上下文的实现。
3. `assess` 继续是**单一入口 + 可选 feature flag**，不要为“无参考”另起一套工具/流水线。
4. 伪参考结果只允许新增字段，**不得合并进 `gop` / `accuracy` / `issues`**。
5. 证据目录在启用 A 后必须同时留 `asr.sv.json` 与 `asr.wh.json`；现在不要提前写死单引擎文件名。
6. 素材库/训练计划条目必须携带 `text`、`tags`、`level`、`ref_audio`、词级时间信息，
   这样 A 的筛查结果可以直接挂到同一训练条目上。
7. 任何“自由说评分”话术都不得暗示已做参考无关的发音诊断。

## 八、工作量

| 阶段 | 内容 | 预估 |
|---|---|---|
| 前置 | D04 whisper 回归 + fixture | 0.5d |
| 实现 | 双 ASR/共识/ coverage/ `gop_pseudo` | 1d |
| 替身评测 | V1–V3（9 用例，无人工） | 0.5d |
| 人工评测 | 6 条 Pilot（≈1 h）+ 20 条正式（≈6 h） | 1d |
| 标定 | V9 + 阈值/百分位 | 0.5d |

**总计 ≈3.5 人日 + 6 人时标注**；不通过 Go/No-Go 就只保留内部信号，不接 Skill。
