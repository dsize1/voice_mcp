# D52 基线审计（只读生成）

> 数据目录：`D:\voice_data`；生成工具：`tools/audit_baseline.py`

## 口径

| 分类 | 含义 | Δ 能否跨期比较 |
| :--- | :--- | :--- |
| `native` | 真人原声基线（`gop_baseline_source=native`） | ✅ 可以（仍需 `gop_baseline_valid=true`） |
| `tts` | TTS 合成基线（2026-11-25 前的默认） | ❌ 只能当“与合成示范的差异” |
| `absent` | 有基线但没写来源（字段是后加的） | ❌ 同上，且无法确认来源 |
| `no-baseline` | 本次没跑基线 | — |

## 计数

| 分类 | 条数 |
| :--- | ---: |
| `native` | 5 |
| `tts` | 6 |
| `absent` | 35 |
| `no-baseline` | 234 |

按月：

| 月份 | native | tts | absent | 无基线 |
| :--- | ---: | ---: | ---: | ---: |
| 2026-11 | 5 | 6 | 35 | 234 |

## Δ 不可比的记录（41 条，引用时请避开）

| 文件 | 分类 | 基线自检 |
| :--- | :--- | :--- |
| `sessions/2026-11-22/155944_drill/takes/001_bank.metrics.json` | `absent` | None |
| `sessions/2026-11-22/160239_drill/takes/001_word.metrics.json` | `absent` | None |
| `sessions/2026-11-22/160606_drill/takes/001_retry.metrics.json` | `absent` | None |
| `sessions/2026-11-22/160730_drill/takes/001_retry.metrics.json` | `absent` | None |
| `sessions/2026-11-22/160912_drill/takes/001_word.metrics.json` | `absent` | None |
| `sessions/2026-11-22/161401_drill/takes/001_word.metrics.json` | `absent` | None |
| `sessions/2026-11-22/162029_drill/takes/001_word.metrics.json` | `absent` | None |
| `sessions/2026-11-22/162135_drill/takes/001_word.metrics.json` | `absent` | None |
| `sessions/2026-11-22/162245_drill/takes/001_bank.metrics.json` | `absent` | None |
| `sessions/2026-11-22/220057_drill/takes/001_bank.metrics.json` | `absent` | None |
| `sessions/2026-11-22/220223_drill/takes/001_bank.metrics.json` | `absent` | None |
| `sessions/2026-11-22/220254_assess/takes/001_assess.metrics.json` | `absent` | None |
| `sessions/2026-11-22/220324_drill/takes/001_retry.metrics.json` | `absent` | None |
| `sessions/2026-11-22/220424_drill/takes/001_word.metrics.json` | `absent` | None |
| `sessions/2026-11-22/220521_drill/takes/001_word.metrics.json` | `absent` | None |
| `sessions/2026-11-22/220805_drill/takes/001_word.metrics.json` | `absent` | None |
| `sessions/2026-11-22/220836_drill/takes/001_word.metrics.json` | `absent` | None |
| `sessions/2026-11-23/213732_drill/takes/001_plan_village.metrics.json` | `absent` | None |
| `sessions/2026-11-23/213856_drill/takes/001_plan_village_retry2.metrics.json` | `absent` | None |
| `sessions/2026-11-23/213939_drill/takes/001_plan_village_word2.metrics.json` | `absent` | None |
| `sessions/2026-11-23/214210_drill/takes/001_plan_umbrella.metrics.json` | `absent` | True |
| `sessions/2026-11-23/214441_drill/takes/001_plan_umbrella_retry2.metrics.json` | `absent` | None |
| `sessions/2026-11-23/214808_drill/takes/001_plan_thank_slow2.metrics.json` | `absent` | None |
| `sessions/2026-11-24/000321_drill/takes/001_plan_country_retry2.metrics.json` | `absent` | None |
| `sessions/2026-11-24/000409_drill/takes/001_plan_brought.metrics.json` | `absent` | True |
| `sessions/2026-11-24/000543_drill/takes/001_plan_tomorrow.metrics.json` | `absent` | None |
| `sessions/2026-11-24/001436_assess/takes/001_assess.metrics.json` | `absent` | None |
| `sessions/2026-11-24/001735_assess/takes/001_assess.metrics.json` | `absent` | None |
| `sessions/2026-11-24/001754_assess/takes/001_assess.metrics.json` | `absent` | None |
| `sessions/2026-11-24/001851_drill/takes/001_plan_south_drill.metrics.json` | `absent` | None |
| `sessions/2026-11-24/001922_drill/takes/001_plan_mouth_drill.metrics.json` | `absent` | None |
| `sessions/2026-11-24/001951_drill/takes/001_plan_south_sentence.metrics.json` | `absent` | None |
| `sessions/2026-11-24/002036_drill/takes/001_plan_south_natural.metrics.json` | `absent` | None |
| `sessions/2026-11-24/002157_drill/takes/001_plan_south_140.metrics.json` | `absent` | None |
| `sessions/2026-11-24/201337_assess/takes/001_assess.metrics.json` | `absent` | None |
| `sessions/2026-11-26/212650_assess/takes/001_assess.metrics.json` | `tts` | True |
| `sessions/2026-11-26/212709_assess/takes/001_assess.metrics.json` | `tts` | True |
| `sessions/2026-11-26/212724_assess/takes/001_assess.metrics.json` | `tts` | True |
| `sessions/2026-11-26/212739_assess/takes/001_assess.metrics.json` | `tts` | True |
| `sessions/2026-11-26/212750_assess/takes/001_assess.metrics.json` | `tts` | True |
| `sessions/2026-11-26/212804_assess/takes/001_assess.metrics.json` | `tts` | True |

## 结论 / 处置

1. 趋势与“迁移”结论**只允许**用 `native` + `gop_baseline_valid=true` 的记录；
2. 需要重跑的历史点：凡是曾被用来下结论且来源为 `tts`/`absent` 的，用 `assess --bank-id <id>`
   （有真人切片）重跑一次，形成新记录，**不要**改写旧 `metrics.json`（它是留证）；
3. IELTS prompt（10 条）目前没有真人参考音，其 Δ 一律视为不可比；
   要用于趋势，需补录真人示范或改用 `bank` 里有真人切片的素材。

## 本次验证（2026-11-28）

**新字段**：`Assessment.delta_comparable` —— 只有「基线存在 + `gop_baseline_source=native` + `gop_baseline_valid=true`」才 `true`；
落盘进 `metrics.json`，同时出现在 CLI `assess` 与 MCP `assess` 的输出里。

**实跑（`assess --bank-id`，真人切片当基线）**：

| 素材 | source | valid | delta_comparable |
| :--- | :--- | :--- | :--- |
| `nce2/01/002`（Last week I went to the theatre.） | `native` | `true` | **`true`** |
| `nce2/01/001`（Why did the writer complain…） | `native` | `true` | **`true`**（旧二进制输出无该字段，故 `None`） |

**IELTS prompt 现状**：`bank/index.jsonl` 里 `corpus=ielts` 共 24 条 —— `native` 14 条（真人听力切片）、
`tts` 10 条（prompt，无真人示范）。这 10 条的 Δ 按本报告口径**一律不可比**。

**重跑原则**：
1. 旧 `metrics.json` **不改写**（它是留证）；需要新结论就重跑，产生新记录；
2. 重跑时优先 `--bank-id`（有真人切片）→ `source=native`；
3. 趋势/迁移类结论只引用 `delta_comparable=true` 的记录。

**待办（不阻塞）**：IELTS prompt 补录真人示范音（10 条），补录后这批才能进入趋势统计。
