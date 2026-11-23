# 英语水平与训练就绪等级（Levels & Readiness）

> 先澄清：**雅思没有“学习资格”门槛**，任何水平都可以学。本文定义的是
> **训练就绪度（readiness）**——什么时候适合从跟读进入半自由、再进入无稿自由说。
> 目的是避免在发音/连读还不稳时做无稿独白，得到无法解释的流利度数据。

## 1. 系统里现有的“等级”字段

| 字段 | 取值 | 含义 | 谁维护 |
|---|---|---|---|
| `learner.level.band_estimate` | 6.0（区间 5.5–6.5） | 雅思分数量级估计；**跟读/朗读下界**，不是官方成绩 | Skill 复评时更新 |
| `learner.level.confidence` | low / medium / high | 证据强度（不是水平） | Skill |
| `learner.teaching_lang` | `zh` / `zh-en` / `en-heavy` / `en` | **讲解语言档**（L1–L4），不是英语水平 | Skill |
| `learner.readiness.stage` | L0–L4 | **训练就绪等级**（本文） | Skill / 用户确认 |
| `bank.level` | 1–4 | 素材难度（NCE），由 band 映射 | `bank plan` |
| CEFR 近似 | A2 / B1 / B2 / C1 | 仅近似，不对标证书 | 文档 |

`bank.level` 映射（`plan::level_from_band`）：

```
band <5.5 → 1     band <6.5 → 2     band <7.0 → 3     band ≥7.0 → 4
```

当前 band 6.0 → **bank level 2**。

## 2. 就绪等级 L0–L4

| 等级 | 名称 | 判据（全部满足） | 训练形式 | 雅思近似 | CEFR 近似 |
|---|---|---|---|---|---|
| L0 | 起步 | 无法稳定成句；acc <0.5 | 单词/短词组 | 4.0– | A2 |
| L1 | 基础 | 短句 acc 0.5–0.75；语速 <90 | 短句跟读 | 4.0–5.0 | A2 |
| L2 | 跟读稳定 | 长句 acc ≥0.80；语速 90–160；pause ≤8/30s | 整句跟读 | 5.0–6.0 | B1 |
| **L3** | **半自由** | **连续 3 条 acc ≥0.90**；语速 120–160；pause ≤6/30s；核心音素（θ/v/词尾）无回归 | 30–45 s 有稿/提纲独白 | **6.0–6.5** | **B2** |
| L4 | 自由说 | L3 + 45–60 s 无稿独白：pause ≤8/45s、repetitions ≤1、prosody reliable；用户同意 | 无稿自由说 | 6.5–7.0 | B2+/C1 |

> “核心音素无回归”当前指：`theta_to_s`、`v_to_w`、`final_consonant_drop`（含 `they'll/he'll` 的词尾 /l/）
> 在最近 3 条同难度素材里没有重复出错。

## 3. 自由说 gate（代码行为）

`learner.json`：

```json
"readiness": {
  "stage": "L2+",
  "name": "跟读稳定（半自由未满）",
  "free_speech_enabled": false,
  "ielts_band": 6.0,
  "cefr_approx": "B2",
  "updated": "2026-11-24",
  "gate": {
    "min_consecutive_acc": 0.90,
    "consecutive_n": 3,
    "rate_wpm": [120, 160],
    "pause_per_30s_max": 6,
    "free_speech_pause_per_45s_max": 8,
    "free_speech_repetitions_max": 1
  },
  "missing": ["…"]
}
```

`bank plan` 的行为：

- 无显式参数 → 读 `readiness.free_speech_enabled`：
  - `false` → **省略** 60 s 自由说槽位，并在 notes 写明缺什么；
  - `true` → 包含槽位（`pseudo_gop: disabled (ADR-0010)`）。
- `--free-speech` → 强制包含（用于用户明确要求试一次）。
- `--no-free-speech` → 强制省略。

## 4. 当前状态（2026-11-24）

| 项 | 值 |
|---|---|
| stage | **L2+** |
| band | 6.0（5.5–6.5），confidence medium（跟读下界） |
| CEFR 近似 | B2 |
| free speech | **关闭** |
| 已达标 | 语速 144 wpm；/θ/（thank/south）与 /v/ 单点稳定；`south` 词尾 /θ/ 修正；`live` /v/ 正确 |
| 未达标 | 连续 3 条 acc≥0.90（1.00 / 0.80 / 0.875）；`they'll/he'll` 词尾 /l/；208 wpm 时 /θ/ 失守；51.8 s 自由说 pause 10.5/30s |

## 5. 升级流程

1. 每轮训练后把结果写进 `learner.json.history`（accuracy / rate / pause / 核心音素）。
2. 每 5 次训练或用户要求时，Skill 重新计算最近 3 条：
   - 连续 3 条 acc ≥0.90、rate 120–160、pause ≤6/30s、核心音素无回归 → **L3 达标**。
3. L3 达标后，做一次 30–45 s 有稿/提纲独白；pause ≤8/45s、repetitions ≤1 → **L4 就绪**。
4. 用户确认后，把 `readiness.free_speech_enabled` 置 `true`（或本轮用 `--free-speech` 试一次）。
5. 任何一次出现 ≥2 个核心音素回归或 pause >10/30s → 退回 L2，关闭自由说槽位。

## 6. 注意事项

- **不要把 band_estimate 当官方雅思成绩**，也不要用它报名/对外承诺。
- CEFR 映射只是近似：IELTS 6.0 通常落在 B2，但听说读写四项不均衡时不能直接换算。
- 朗读/跟读不能证明“无稿流利度”；L4 的自由说数据必须标注 `scene=free`，不能与朗读混算。
- `teaching_lang`（L1–L4）和 `readiness.stage`（L0–L4）是两套东西：前者决定讲解语言，后者决定训练形式。
