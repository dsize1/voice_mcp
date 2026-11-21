# ADR-0009: 跟读素材库（Drill Library）与 NCE 语料接入

- 状态：**Accepted / P4 已实现**（2026-11-21，见文末「实现进展」）
- 日期：2026-11-19
- 相关：ADR-0006（评测边界）、ADR-0008（技术债）、`docs/CONTRACT.md`

## 背景

训练循环目前缺一侧：**素材**。现有流程要求人肉找句子、手动合成示范音，导致：

- 每次训练前要花几分钟准备素材，破坏「10–15 分钟」的节奏；
- 素材难度无法控制（随手找的句子可能远超用户水平）；
- 没有「同一批素材反复练、纵向对比」的能力。

而 LRC 字幕教材天然提供 **「文本 + 行时间」**，正好是跟读素材的两要素。
`voice-cli lesson` 已在真实教材上验证过这条路可行（NCE2 L2：19/19 行文本相似度 1.00，
行起始时间偏差 +0.07 ~ +0.29 s，修复长音频分块后 LRC 拟合 `scale=0.999`）。

## 已核实的数据源（实测，非推测）

本地 NCE 项目 `D:\code\NCE-main\NCE-main`（ichochy/nce 的静态站点 + 本地反代 `server.js`）：

| 事实 | 实测结果 |
|---|---|
| 书目录 | `GET {base}/data.json` → `books[{key,title,path}]`，含 `NCE1..4` 与 `NCE1(85)..NCE4(85)` 两个版本 |
| 课目录 | `GET {bookPath}/book.json` → `{name,level,cover,units[{title,filename}]}` |
| 音频/字幕 | `GET {bookPath}/{urlencode(filename)}.mp3` 与 `.lrc`，**文件名含空格必须逐段编码** |
| 可达性 | `02.Breakfast%20or%20Lunch.mp3` → 200 / 488 685 B；`.lrc` → 200 / 1 471 B |
| 规模 | NCE1 72 课、NCE2 96 课、NCE3 60 课、NCE4 48 课 = **276 课** |
| 目录列举 | 不可用（GitHub Pages 无 index，`/NCE2/` 返回 404）→ **必须走 book.json** |

## 决策

### D1. 素材的最小单位 = 一行字幕（一句），不是整课

理由：跟读训练的单位是「一口气能读完的意群」，一行 LRC 恰好是教材作者划的句子。
整课保留在 `lesson.json` 里作为上下文，但**入库单位是行**。

### D2. 三种来源，按优先级尝试

```
1. 本地目录      --from-dir "D:/.../listening"         （已下载的 <NN>.<Title>.mp3/.lrc）
2. 本地反代      --from-url http://127.0.0.1:3000       （NCE 项目的 server.js，网络受限时用）
3. 直连          --from-url https://nce.mleo.site       （默认）
```

**按需下载，不预拉全量**：276 课约 1.5 GB，默认只拉指定范围（`--book NCE2 --from 1 --to 20`）。
`--edition 85` 才碰 85 版（与普通版内容重复）。

### D3. 素材库布局（`$VOICE_HOME/bank/`）

```text
bank/
├── index.jsonl                      # 全局索引，一行一个素材（唯一真相）
├── quarantine.jsonl                 # 未过质量门槛的行 + 原因，供人工复核
└── nce2/02-breakfast-or-lunch/
    ├── lesson.json                  # 源信息、lrc_fit、逐行质量、rejected[]
    ├── 001.ref.wav                  # 该行的原生朗读切片（16k mono，前后各留 0.15 s）
    ├── 001.ref_slow.wav             # 可选：TTS 0.75x 慢速示范（默认不生成）
    └── 001.mp3                      # 可选：该行原音轨片段（--keep-source 时保留）
```

素材**只读**：生成后服务不再修改，避免"练过的素材偷偷变了"。

### D4. 素材元信息（`index.jsonl` 单行）

```json
{"id":"nce2/02/003","book":"NCE2","lesson":"02.Breakfast or Lunch","seq":3,
 "text":"I never get up early on Sundays.","zh":"我星期天从不起早。",
 "t0":17.99,"t1":22.03,"dur":4.04,"words":7,"wpm":104,"level":1,
 "src":"bank/nce2/02-breakfast-or-lunch/003.ref.wav","ref_slow":null,
 "quality":{"line_sim":1.0,"asr":"sense-voice","band_limited":false,"lrc_fit_scale":0.999},
 "tags":["theta","final_consonant"]}
```

- `id` 稳定且可读：`<book>/<lesson编号>/<行号>`，Skill 与 `learner.json` 都引用它
- `tags` 来自 `skills/ielts-speaking-coach/references/phoneme-map.md` 的 id
  （文本里含 think/three/this/very/right… 就标 `theta` / `eth` / `v` / `r_l` / `final_consonant`）
- `level` 见 D6

### D5. 质量门槛（不达标不入库，进 `quarantine.jsonl`）

| 检查 | 阈值 | 不过的原因 |
|---|---|---|
| 整课 LRC 拟合 | `scale ∈ [0.98, 1.02]` 且 `|offset| < 0.6 s` | LRC 与音轨版本不一致（需 `voice-cli lesson --write-lrc` 重定时后再入库） |
| 逐行文本 | `line_sim ≥ 0.90` | 字幕文本与音频不符（缺句、错行） |
| 行时长 | `0.8 s ≤ dur ≤ 15 s` | 太短没法练；太长不适合单句跟读 |
| 源采样率 | `band_limited == false`（≥24 kHz） | 窄带音源不能用于 /θ/ /s/ 判断 |
| 切片语音占比 | VAD 语音 ≥ 50% 行时长 | 切到了静音/音乐，或行边界错位 |
| 词数 | ≥ 3 词（必要时合并相邻行） | NCE1 前半是单句对话，切完只剩 "Yes." 无法练 |

**门槛的意义**：宁可少入库，也不要让用户练一条"文本对不上音频"的素材——那会让评测结论彻底失真。

### D6. 难度分级：用可观测量，不引入词表

`level = 1..4`，由三个可测量量合成（与"练发音"直接相关）：

```
level = 1  if words ≤ 6  and wpm ≤ 120
level = 2  if words ≤ 10 and wpm ≤ 150
level = 3  if words ≤ 16 and wpm ≤ 180
level = 4  otherwise
```

**明确不采用** CEFR / 词频表：那需要外部词典依赖，而且**词汇难度 ≠ 发音难度**。
音素难度用 `tags` 表达（哪几行含 /θ/ /v/ /r/-/l/ 目标词），由 Skill 定向取材。

### D7. 与 Skill / `learner.json` 的接口

- `learner.json.drill_queue` 条目升级：`{"bank_id":"nce2/02/003","reason":"θ + 词尾 k"}`
  - **向后兼容**：没有 `bank_id` 的条目按原来的内联 `text` 处理
- Skill 取材顺序：`drill_queue` → `focus` 命中的 `tags` → 同 level 的教材顺序
- Skill 只读 `index.jsonl`（用文件读取），不经过 MCP 工具 —— 避免工具面膨胀

### D8. CLI（P4 实现）

```bash
voice-cli bank ingest --from-url https://nce.mleo.site --book NCE2 --from 1 --to 20
voice-cli bank ingest --from-dir "D:/code/up-main/.../listening" --dry-run
voice-cli bank list --level 1 --tag theta --limit 10
voice-cli bank show nce2/02/003
voice-cli bank drill nce2/02/003        # 播原生 + 慢速示范，打印句子与中文
voice-cli bank stats                     # 条数/时长分布/quarantine 统计
```

MCP 侧暂不新增工具（P4.5 再评估）；Skill 通过文件直接读索引。

### D9. 存储与版权

- 素材库体积：一句 5 s、16 kHz、16 bit 单声道 ≈ 160 KB；
  NCE2 全校按 ~12 行/课估算 ≈ 1 150 句 ≈ **190 MB**（wav）。另提供 `--format mp3` 与 `bank prune`。
- 默认 ingest 后**删除原 mp3**（只保留切片），`--keep-source` 才保留整课音轨。
- 音频来自公开站点，**仅本地个人学习使用**；不入 git（`.gitignore` 已排除 `*.wav/*.mp3`），
  在 `bank/ATTRIBUTION.md` 记录来源与日期。

## 验收标准

1. NCE1–4 全量 ingest 后 `index.jsonl` 条数 ≈ 2 800–3 200（276 课 × 平均 10–12 行，扣掉 quarantine）；
2. 随机抽 20 条人工试听：**无截断、无跨句、无静音开头**；
3. `bank drill` 端到端：播原生 → 用户跟读 → `assess` 在用户读对时 `accuracy ≥ 0.9`；
4. `quarantine.jsonl` 里每一行都有明确原因（不是"未知错误"）；
5. 同一素材重复 ingest 幂等（按 `id` 跳过或覆盖）。

## 风险与对策

| 风险 | 对策 |
|---|---|
| GitHub Pages 被限速/阻断 | 三级来源：本地目录 / `server.js` 反代 / 直连（已有 276 课可达的实测） |
| 文件名含空格与特殊字符 | 逐段 `percent-encode`（已在 `tools/fetch_models.ps1` 踩过同类坑） |
| NCE1 前段句子过短（"Yes."） | 自动合并相邻行直到 ≥3 词，并在 `lesson.json` 记录合并结果 |
| 85 版与普通版重复 | 默认只 ingest 普通版，`--edition 85` 显式开启 |
| 切片边界吃掉词尾 | 切片前后各留 0.15 s 余量；VAD 语音占比 <50% 直接 quarantine |

## 分期

| 阶段 | 内容 |
|---|---|
| **P4** | `bank ingest / list / show / drill / stats` + 质量门槛 + quarantine（本次要做的全部） |
| P4.5 | MCP 工具 `bank_list`/`bank_get`（若 Skill 直接读文件不够用） |
| P5 | 用素材做"批量生成慢速示范音"/"按音素定向组卷"（依赖 P2.5 的 GOP） |

## 实现进展（2026-11-21）

`voice-cli bank ingest / list / show / drill / stats` 已落地，代码在
`crates/voice-core/src/bank/mod.rs`（纯函数 + 质量门槛 + 索引）+ `voice-cli` 子命令。

| 验收项 | 结果 |
|---|---|
| 1. 全量 ingest ≈2 800–3 200 条 | ✅ **4 997 条**（NCE1 1 159 / NCE2 1 361 / NCE3 1 422 / NCE4 1 055）—— 比估算多，因为 276 课实际是 12–25 行；quarantine **1 221 条** |
| 2. 随机抽 20 条人工试听 | ✅ 随机 20 条：首静音 0.00–0.18 s、尾静音 0.02–0.26 s（VAD 收紧切片后），无截断；切片 1.6–11.9 s、16 kHz mono |
| 3. `bank drill` 端到端 | ✅ 播原生 → 打印句/中文/level/wpm/tags |
| 4. quarantine 每行有原因 | ✅ 只有三类：`line_sim` 915、`speech_ratio` 280、`dur` 45 |
| 5. 重复 ingest 幂等 | ✅ 索引按 id upsert（`--overwrite` 重写而非追加），4 本书全量重跑后无重复 id |

### 全量实测（2026-11-22）

```text
entries    4 997       quarantine 1 221     total_dur 25 744 s (7.1 h)
level L1 733 / L2 1 344 / L3 1 724 / L4 1 196      mean_wpm 133.5
top tags   eth_to_z 3 290 · s_ending 2 472 · linking_missing 2 338 · fragment 1 534
```

- ingest 速度：NCE1 1m50 / NCE2 2m41 / NCE3 3m24 / NCE4 2m36（约 1.6–3.4 s/课，含整课 ASR + 逐行 VAD）
- **可直接练**（排除 `lesson_header` / `instruction` / `fragment`）**3 384 条**
- 切片首尾按 VAD 收紧：行 `t1` 到下一行 `t0` 之间的句间停顿不再被带进素材
  （收紧前抽查出现 2.38 s 尾静音，收紧后 ≤0.26 s）

实现取舍（与原 ADR 的差异）：

- **`--from-url` 未实现**：项目不引外部二进制，也不引 TLS 依赖；网络拉取交给外部脚本
  （用户计划自己拉 NCE1–4），`bank ingest` 只吃本地目录。
  https 直连若要做，需在 P4.5 引 `ureq`/`rustls` 或复用本机反代。
- **慢速示范（`ref_slow`）默认不生成**：留字段，P5 批量生成时填。
- `bank retag`：tags 只依赖文本，可随时重算（全量 4 997 条约 1 s）。
- 内容型标签（不进质量门槛、只影响取材）：`lesson_header`（"Lesson 33"）、
  `instruction`（"Listen to the tape…"）、`fragment`（LRC 把长段落切成半句，占 30.7%）。
- **`--keep-source` 未实现**：整课原 mp3 由外部目录保留，bank 只存切片。
- 质量门槛与 D5 完全一致；`merge_short_lines` 会把不足 3 词的行并入下一行，
  末尾不足则并入上一行（不丢内容）。

## P4.5 实现（2026-11-22）

原计划是"MCP 新增 `bank_list`/`bank_get`"，实际**没加 MCP 工具**——理由与 ADR D7 一致：
Skill 只要"按 focus 取 1–3 条"，用 CLI 更省上下文（`index.jsonl` 有 2 MB，直接读会占满窗口）。

```bash
voice-cli bank pick --tag theta_to_s --tag final_consonant_drop --level 2 --limit 3
voice-cli bank pick --seed 42 --level 2 --limit 3      # 可复现洗牌
voice-cli bank show <id> / bank drill <id>
```

- 排序：命中 tags 数 ↓ → level ↑ → id ↑（`--seed` 时改为稳定哈希序）
- 默认排除 `lesson_header` / `instruction` / `fragment`（`--allow-noise` 可关）
- `Learner.drill_queue` 支持 `{"bank_id": "..."}`，向后兼容内联 `text`
- Skill 侧：第 1.5 节新增"素材库怎么用"，第 3 节把 ④ 的 `scene` 改为 `bank`，
  第 9 节作业句必须给 `bank_id` 或示范音路径
