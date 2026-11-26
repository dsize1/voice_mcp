# 目标与进度（近期 / 远期）

> 更新：2026-11-25 · 状态图例：✅ 完成 · 🟡 进行中 · ⛔ 阻塞 · ⏳ 未开始 · 🅿️ 挂起
>
> 相关：[STATE.md](STATE.md)（交接）· [levels.md](levels.md)（等级与自由说 gate）·
> [ADR-0008](adr/0008-technical-debt.md)（技术债务）· [ADR-0009](adr/0009-drill-library.md)（素材库）·
> [ADR-0010](adr/0010-free-speech-pseudo-gop.md)（Route A，挂起）

## 0. 当前快照（2026-11-24）

| 项 | 值 |
|---|---|
| 阶段 | P0–P4 完成；P5 进行中（`bank plan` 已上线） |
| 英语水平 | band **6.0**（区间 5.5–6.5），CEFR 近似 **B2**；**跟读/朗读下界**，非官方成绩 |
| 就绪等级 | **L2+**（跟读稳定，半自由未满） |
| 自由说 | **关闭**（`readiness.free_speech_enabled=false`）；L3 后做 30–45 s 提纲，L4 才做 60 s 无稿 |
| 素材库 | **5 007 条**（NCE 4 997 + IELTS prompt 10），可直接练 3 378 |
| 诊断能力 | 词级 diff + flow + 词级 GOP（**真人原声优先**基线/混淆对）+ 韵律（F0/升降/平调/重音候选/DTW，参考音标 native/tts） |
| TTS | 默认 **am_michael（男声）+ 140 wpm**（130–160），slow=110；WPM 按**语音跨度**；起始伪影检测 + 自动换速度/换音色；OOV 预检 + `tts-verify` + `bank refs` 批量验证；verified 听力集/10 条 prompt 见 STATE |
| 债务 | D01–D52 已登记；**2026-11-25 还 D20/D47/D49/D51，D04/D52 部分偿还** |
| 近期训练 | 计划 **7/7 项**完成；`/θ/` 词首/词尾修正；144 wpm 下 `/θ/`、`/v/` 守住 |
| 本地提交 | 2026-11-25 已推送（本轮 6 个 commit 全部推送） |

---

## 1. 近期目标（本轮迭代 / 约 1–2 周）

### A. 让测量可信（最高优先）

| ID | 目标 | 状态 | 验收 | 依赖 |
|---|---|---|---|---|
| **N1** | D04：whisper 路径回归（token→词、标点、无时间戳降级） | 🟡 | routing 单测 ✅ + 3 个 fixture ✅；**端到端 whisper 音频回归仍缺** | 无 |
| **N2** | D49：文本可用性闸门（声学过闸但 ASR 不可用） | ✅ | `assess.text_agreement`（LCS 一致率）<0.7 → `degraded` + 只出 flow/prosody；9 条录音回归无误杀 | N1 同批 |
| **N3** | D47：短语音（<2 s）SNR 误杀单词 drill | ✅ | `snr_verdict`：语音跨度 <3 s 且 peak ≥−25 → 不降级（只记 limitation），GOP 照跑 | 无 |

### B. 让反馈正确

| ID | 目标 | 状态 | 验收 | 依赖 |
|---|---|---|---|---|
| **N4** | D46：G2P 同形异音（live/read/use） | ⏳ | `I live` 的 live 不再误低；补 3 条回归 | 无 |
| **N5** | D50：韵律 12 条人工标注验收 | ⏳ | 升降/平/重音一致率 ≥80% | 人工标注 2 h |
| **N13** | D52：TTS 不再当“标准/基线”（真人原声优先） | 🟡 | `assess(bank_id)` → `gop_baseline_source=native`、`prosody.reference.source` 标注；基线加音素自检。残留：IELTS prompt 无真人音、历史 Δ 需重跑 | D46 |

### C. 让训练循环自动

| ID | 目标 | 状态 | 验收 | 依赖 |
|---|---|---|---|---|
| **N6** | D48：readiness 自动评估（从 history 算 stage/缺项） | ⏳ | `voice-cli learner readiness` 输出并可写回 | 无 |
| **N7** | `bank plan` 排除“最近练过”的素材 | ⏳ | 连续两次计划重复率 <20% | N6 同批 |
| **N8** | L3 达标：连续 3 条 acc≥0.90 | 🟡 | 最近 1.00 / 0.80 / 0.875 → 需补 2 条 | 训练 |

### D. 素材与录音

| ID | 目标 | 状态 | 验收 | 依赖 / 阻塞 |
|---|---|---|---|---|
| **N9** | 雅思素材入库（先 1 本 / Part 1） | ⛔ | 至少 200 条 IELTS 可练素材 | **缺源音频 + LRC/字幕** |
| **N10** | 外部录音（手机/系统录音机）可用性 | 🟡 | 3 条录音双引擎一致率 ≥0.8，能出词级结论 | 与 N2 联动 |
| **N11** | TTS 参考音验证（D51）：OOV 预检 + 双 ASR/音素/WPM + 基线自检 | ✅ | `tts-verify` 3/3；OOV 0；`gop_baseline_valid` 生效；**`bank refs` 批量接入**；**10/10 IELTS prompt verified 3/3（含音素）** | 无 |
| **N12** | Day 9 听力改用 `chunks-listening-verified/` 并更新答案键/训练记录 | ✅ | `day-9.md` 材料表 + `chunks-recall(.md/-key.md)` + 会话记录全部指向 verified；原文件未动；复测口径已写清（只有 s5 = 168 wpm 越界） | — |

### E. 运维收尾

| 目标 | 状态 | 说明 |
|---|---|---|
| 推送本地 5 个 commit | ⏳ | `main` 领先 `origin/main` 3 个（2026-11-25：语速/伪影+bank refs、s5 复测、默认男声） |
| 重启 MCP 进程 | ✅ | 2026-11-25：杀子进程 → 重建 `voice-mcp.exe` → Pi 自动重连；实测 `synthesize` 返回 `am_michael` + `speech_duration` + `onset_gap_ms` |
| CI 首跑 | ✅ | 绿：`530e668`（2026-11-24）+ `3cf8736`（2026-11-25），`windows-latest` 上 `fmt → clippy -D warnings → test` 全过（run 45643953511） |

---

## 2. 中期目标（约 1–2 个月）

| ID | 目标 | 状态 | 说明 / 依赖 |
|---|---|---|---|
| **M1** | P5：慢速示范批量生成（`ref_slow`） | ⏳ | 按 `drill_queue` 预生成 110 wpm，减少每轮 TTS 等待 |
| **M2** | P5：按音素定向组卷 | ⏳ | 依赖 D46/D50 与更稳的 GOP |
| **M3** | Part 2/3 长独白训练 | ⏳ | 需 D38 分块切词 + D14 长句 accuracy 口径 |
| **M4** | 自由说 L4 + Route A 评估 | 🅿️ | ADR-0010；前置 L4、D04、D49 |
| **M5** | 中文发音诊断（拼音层） | ⏳ | D13；当前中文只做字级 diff |
| **M6** | 分发：Pi package / 预编译 / 跨平台 | ⏳ | D29/D30/D31；Windows-only 假设 |
| **M7** | CI + 合成 fixture + 数据保留策略 | ⏳ | D20/D21/D25/D26 |
| **M8** | `learner.json` schema 校验与迁移 | ⏳ | D27；现有 `readiness` 等字段只靠散文 |

---

## 3. 远期目标（3 个月+）

| ID | 目标 | 状态 | 说明 |
|---|---|---|---|
| **L1** | P4 产品形态：热键听写 daemon / HTTP 模式 / 流式 TTS / 音色复刻 | ⏳ | 当前只有 MCP stdio + CLI |
| **L2** | 雅思全题库 + Part 1/2/3 课程化 | ⏳ | 依赖 N9 素材入库与长独白支持 |
| **L3** | 数据飞轮：真实训练 → `error_stats` → 定向组卷 → 再标定 | ⏳ | 先用单用户跑通，再考虑第二用户 |
| **L4** | 跨平台与发布（macOS/Linux、安装包、Release） | ⏳ | 与 M6 合并推进 |
| **L5** | 运行时性能/GPU EP（`VOICE_PROVIDER=cuda`） | ⏳ | 当前 CPU 已够；GPU EP 未打通 |
| **L6** | 100% Rust 运行时（无 Python 训练依赖） | 🟡 | 运行时已纯 Rust；训练/标定仍用 Python |

---

## 4. 进度总览

| 阶段 | 内容 | 状态 |
|---|---|---|
| P0 | 环境验证（解码/原生库/ASR/TTS/供应链） | ✅ |
| P1 | 音频层 + 8 个 MCP 工具 + CLI + watch | ✅ |
| P2 | 评测层：词级 diff + flow + **prosody** | ✅ 4/4 |
| P2.5 | 音素级 GOP（词级口径、TTS 基线、混淆对） | ✅ 2026-11-22 |
| P3 | IELTS Coach / Dictation Skill + 档案 | ✅ |
| P4 | 跟读素材库（NCE 4 997 + IELTS prompt 10） | ✅ |
| P4.5 | MDD 微调（音素 AUROC 0.690→0.769） | ✅ |
| P5 | 定向训练计划 `bank plan` | ✅ 2026-11-24 |
| P5 | 慢速示范批量生成 / 按音素组卷 | ⏳ |
| P6 | 产品化（热键/HTTP/流式/分发） | ⏳ |
| — | Route A 伪参考 GOP | 🅿️ Deferred（ADR-0010） |
| — | 自由说 L3 / L4 | 🟡 L2+ / ⛔ L4 |

---

## 5. 最近完成（便于回溯）

| 日期 | 内容 | 证据 |
|---|---|---|
| 2026-11-22 | 词级 GOP、TTS 基线、混淆对、MDD 微调、素材库 5 007 | ADR-0007 / `docs/mdd.md` / ADR-0009 |
| 2026-11-23 | 韵律分析（F0/升降/平调/重音/DTW）；自由说 51.8 s；`bank plan` | `f0f0171` / `e08f51d` |
| 2026-11-24 | readiness gate + L0–L4；7/7 训练；`/θ/` 修正；D46–D50 | `7394b48` / `87e03de` |
| 2026-11-24 | TTS 默认语速校准：140 wpm（130–160），slow=110；短文本/中文不校准 | 本次提交 |
| 2026-11-24 | TTS OOV 预检 + overlay + `tts-verify` + `gop_baseline_valid` | 本次提交 |
| 2026-11-24 | TTS 资产审计：38 项静态 + 10 prompts + 6 baseline 抽样 | [audit/tts-2026-11-24.md](audit/tts-2026-11-24.md) |
| 2026-11-24 | s1–s8 verified 集；s5 af_heart 0.7×（旧口径 159 / 复测 168 wpm）；s8 error 人耳确认 | `chunks-listening-verified/README.md` |
| 2026-11-25 | TTS 语速口径改为语音跨度 + 最优候选校正 + 起始伪影检测/规避；`bank refs` 批量验证（换语速/换音色回退）；10/10 IELTS prompt 3/3 | [audit/tts-2026-11-24.md](audit/tts-2026-11-24.md) §12 |
| 2026-11-25 | D47 短语音 SNR 豁免；D49 双引擎文本闸门（`text_agreement`）；D04 routing + 3 fixture 单测；会话目录同秒防撞 | `cargo test` 135 单测 + 9 录音回归全绿 |
| 2026-11-25 | N12：Day 9 听辨/复测口径切到 `chunks-listening-verified/`（原 C1 记录不动） | `weeks/week-02/day-9.md` |
| 2026-11-25 | 默认音色改为 **am_michael（男声）**（MCP/CLI/GOP 基线三处统一，`DEFAULT_VOICE` 常量 + 单测） | `080e8aa` |
| 2026-11-25 | MCP 端换新二进制（杀子进程 → 重建 → Pi 自动重连），`synthesize` 实测返回 `am_michael`/`speech_duration`/`onset_gap_ms` | `97d7793` |
| 2026-11-25 | CI 首跑确认绿（D20 部分偿还→偿还）：run [45643953511](https://github.com/dsize1/voice_mcp/actions/runs/45643953511) | 本次推送 |
| 2026-11-25 | D52：TTS 不再当基线 —— `bank_id` 用真人原声切片当 GOP 基线/韵律参考，基线加音素自检，来源打标 | 本次提交 |

---

## 6. 维护规则

1. 每次训练/迭代后更新本页的 **状态** 与 **当前快照**；完成的近期项移到“最近完成”。
2. 技术债务变化写 [ADR-0008](adr/0008-technical-debt.md)，不在这里重复细节。
3. 等级/自由说 gate 变化写 [levels.md](levels.md) 与 `learner.json.readiness`。
4. 新目标必须先写**验收标准**；没有验收标准的不进本页（与 ADR-0008 的债务登记规则一致）。
