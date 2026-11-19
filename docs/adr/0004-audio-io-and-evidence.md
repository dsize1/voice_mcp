# ADR-0004: 音频输入输出契约与留证机制

- 状态：**Proposed**（待确认）
- 日期：2026-11-18
- 相关：ADR-0002（教学语言）、CONTRACT.md

## 背景

需求原文：

> 是否先使用 Win11 自带的录音工具录成 xxx.m4a 作为 MCP 的输入，任何输出也需要有录音和文字留证。

即三个诉求：**① 输入低成本启动 ② 输入是文件而不是"服务内录音" ③ 输出与输入都要可追溯**。

同时存在一个现实约束：MCP 是**请求-响应阻塞**模型，Pi 默认单次工具调用超时 60s。
一次 15 分钟训练会产生 20–40 条录音，若每条都要"手动打开录音机 → 录制 → 停止 → 找到文件 → 输入路径"，
摩擦会让训练无法坚持；而雅思 Part 2 的 2 分钟独白，阻塞式录音也不合适。

## 决策

### 1. 输入契约：**文件优先**（file-first）

`transcribe` / `check_*` 的入参统一是 `path`（或 `path: "last"` 指向最近一次 take）。
服务内部把一切输入规范化成 `16 kHz mono pcm_s16le WAV`（派生缓存），**原始文件永不被修改**。

支持格式（P1 必须全部支持）：

| 格式 | 解码方案 |
|---|---|
| `.wav` | `hound`（原生） |
| `.m4a` / `.aac`（Win11 录音机输出，AAC-LC in MP4） | `symphonia`（`symphonia-format-isomp4` + `symphonia-codec-aac`） |
| `.mp3` / `.flac` / `.ogg` | `symphonia` 对应解码器 |
| 其他/解码失败 | 可选外部 `ffmpeg.exe`（存在则用，不存在则报明确错误，不静默失败） |

> ⚠️ P0 必须先验证真实样本：Win11 录音机默认输出 AAC-LC、通常 44.1/48 kHz 单声道。
> 若 `symphonia` 对该样本解码异常，回退 Windows Media Foundation（`windows` crate）解码，不许引入 ffmpeg 硬依赖。

### 2. 消除"找文件"的摩擦：目录监听

`voice-cli watch`（以及 MCP 启动时的可选项 `--watch`）监视录音机的默认输出目录：

```
%USERPROFILE%\Documents\Sound Recordings\   (en-US)
%USERPROFILE%\Documents\录音\                (zh-CN，本机实测存在)
```

- 文件写入稳定（大小不再变化 ≥800ms）后自动导入，重命名为 `takes/<seq>_<scene>_<date>.m4a`，追加 `manifest.jsonl`。
- 导入即触发转写（可配置），结果落盘。用户视角：**"录完就等于交上去了"**，Agent 只需 `path: "last"`。
- 训练场景下 `record`（cpal + Silero VAD 自动停止）作为**补充**而非替代，P2 之后提供，用于 ≤30s 的单词/短句跟读；长独白仍走外部录音 + watch。

### 3. 留证机制（Evidence Ledger）

**每次调用都落盘，且是"音频 + 文字"双份**，永不覆盖、只追加：

```
C:/voice_data/
├── learner.json                          # 档案（水平、teaching_lang、易错音素榜）
├── manifest.jsonl                        # 全局追加索引（一行一个 take，可 grep/统计）
└── sessions/2026-11-18/
    ├── session.json                      # 本次会话：模式、目标、汇总指标、耗时
    ├── session.md                        # 人可读留证：Agent 追加的讲解与反馈原文
    └── takes/
        ├── 001_read_think.m4a            # 原始输入（不可变、原始格式、原始时间戳）
        ├── 001_drill.raw48000.wav        # 麦克风采集的原始证据（设备采样率原样）
        ├── 001_read_think.norm.wav       # 派生：16k mono，可删可重建
        ├── 001_read_think.asr.json       # 转写全文 + words[] 时间戳 + engine + degraded
        ├── 001_read_think.metrics.json   # 对齐/GOP/flow/prosody 全部数值
        ├── 001_read_think.md             # 该 take 的反馈文本（Agent 写的）
        ├── 001_read_think.ref_slow.wav   # TTS 示范音（0.75x）
        ├── 001_read_think.ref_norm.wav   # TTS 示范音（1.0x）
        └── 001_read_think.ref.txt        # 示范音对应的文本 + voice + speed 参数
```

`manifest.jsonl` 单行示例：

```json
{"ts":"2026-11-18T21:40:12+08:00","session":"2026-11-18","seq":1,"scene":"drill",
 "ref":"I think this is the third time.","src":"001_read_think.m4a","engine":"whisper-lv3-turbo",
 "lang":"en","dur":3.42,"wpm":118,"band_est":6.0,"top_issue":"θ→s","degraded":false}
```

追加字段 `feedback_ref`（指向 `.md`）保证"文字留证"可从索引直达。

### 4. 输出也必须留证

| 输出类型 | 留证形式 |
|---|---|
| TTS 示范音 | `.wav` 落盘 + `.ref.txt` 记录 `text / voice / speed / model_version` |
| 转写文本 | `.asr.json`（含原始输出与规范化后的文本，两者都存） |
| 反馈讲解 | `session.md` + 每个 take 的 `.md`；Agent 通过文件工具写入，MCP 只读不写 |
| 评分指标 | `.metrics.json`（含算法版本号，便于后期算法升级后做 A/B 与重算） |

`metrics.json` 里必须带 `scorer_version`，因为 P4 换 100% Rust 实现后要能回溯"这条分数当时是哪个算法算的"。

## 被否决/暂缓的方案

| 方案 | 判断 |
|---|---|
| 只做服务内 `record`（cpal），不接文件 | ❌ 长独白阻塞不友好；用户习惯的录音机没法用；一旦 VAD 失效就卡到超时 |
| 只做外部录音 + 手动传路径 | ❌ 摩擦太高，无法支撑"每天 15 分钟"的训练频率 → 已被"目录监听"解决 |
| 把音频以 base64 塞进 MCP 参数 | ❌ 大 payload、日志爆量；仅在 `synthesize(play=true)` 时允许服务端直接播放，不回传 base64 |
| 训练数据放 D 盘仓库目录 | ❌ 录音体积随时间增长，会把 git 仓库拖脏；改用 `C:/voice_data/`（可用 `VOICE_HOME` 覆盖） |

## 后果

- ✅ P1 零音频采集代码即可闭环，`.m4a` 直接是输入。
- ✅ "留证"天然支持 `progress` 趋势分析（读 `manifest.jsonl` 即可），也方便你事后自己复听。
- ⚠️ 需要 P0 验证 `.m4a` 解码 → **需要你提供 1 条真实样本**（拿 Win11 录音机随便录 10 秒英文）。
- ⚠️ `C:/voice_data/` 会持续增长（约 1–2 MB/分钟含示范音），P3 需要加一个"仅保留最近 N 天 norm.wav / ref.wav，原始 m4a 永久保留"的清理策略。

## 待确认

1. 是否接受"文件优先 + 目录监听 + P2 再补 `record`"这个顺序？
2. 留证目录用 `C:/voice_data/` 还是想放在 `D:/`（体积增长区）？
3. 原始录音是否允许在档案里长期保留（用于后续重算分数/模型 A/B）？
