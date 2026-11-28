# 当前状态与下一步（交接用）

> 写于 2026-11-19，最后更新 **2026-11-28**（**D13/M5 中文拼音层上线** + 真人素材入库 + 语料排除 + D52/D53）。新会话先读 [GOALS.md](GOALS.md)（近期/远期目标与进度），
> 再读这一页、README / CONTRACT / ADR。

## 一句话现状

**P3 闭环 + P4 素材库（5 028 条：NCE 4 997 / IELTS prompt 10 / IELTS 听力真人 14 / Peppa 7）+ 音素级 GOP + 韵律分析 + **中文拼音层**均已上线并在真机验证。**
评测侧的“参考音”已从 TTS 改为**真人原声优先**（D52）；真人素材有两条入库通道（有字幕 `--srt` / 无字幕 ASR 双引擎复核）。
`crates` 12k+ 行 Rust，**209 个单测 + TTS 前端 golden 集成测试 + 9 条录音回归**全绿；`fmt` / `clippy -D warnings` / `test`
已由 `tools/check.ps1` 与 GitHub Actions 守住（D20/D45）。
**优先级（2026-11-28 调整）**：分发（M6/L4、D29–D31、D33）**调低**——单用户本地跑通优先；
**已收口（2026-11-28 这一轮，可直接引用）**：
- TTS 前端换代：开放词表 G2P → `phoneme_codepoints`（lexicon 降为覆盖层）+ 规则 LTS + 漏词门禁 `tts-regress`；
  中文默认 **misaki-zh**（多音字 7/8 vs 1/8）+ 默认音色 **`zf_xiaoni`**；中英混说**默认拒答**
- 评测精度：**D46** 同形异音按音频选变体（`gop.words[].variant`）；**D37** 功能词 `omitted` whisper 交叉复核
  （`cross_check`）+ 补齐 `CONTRACTIONS` 20 条；**D53** `en`/`zh` 走语言锁定 SenseVoice（佩奇 CJK 30→0）；
  **D52** `delta_comparable` 字段 + 基线审计（280 条里仅 5 条 Δ 可跨期比）
- 中文评测：**D13/M5 拼音层**（`text_diff.pinyin`）：声母/韵母/声调三层诊断 + 同音不同字不算错
  （字级 accuracy 0.922 vs 拼音层 0.933，10 句真机审计 0 误报）+ 中文数字归一（`26`→`二十六`）
- 工程：**D18** `tools/build.ps1`（DLL 占用不再 panic）；**D21** 前端 golden 进 CI（无需模型）
下一步（按优先级）：**M8**（learner schema 校验/迁移，D27）→ **N5**（韵律 12 条人工标注验收）→
**M1/M2**（ref_slow 批量、按音素组卷）；**IELTS prompt 10 条补录真人示范**（补完 D52 才完全收口，需要人录音）。

> **材料可信度（2026-11-28 登记，暂不偿还；ADR-0008 D57/D58）**：**TTS 生成物不作听力素材**。
> 实测：美音 lexicon 无弱读（`the`=/ði/、`to`≈300 ms、`and`≈460 ms）＋ 声学层丢词尾（`files` 的 `-z` 在 110/140 wpm 都缺）；
> 且 ASR 会**补** `-ed` / **吞** `-z`，词级回读不能证明音频正确。分级：**L-A 真人原声**（唯一可信答案键）
> \> **L-B TTS＋已过音素门禁** > **L-C 未审 TTS**（禁止进训练计划/复习块/听力题）。
> 已写入 MCP `synthesize`/`assess` 工具说明与 `docs/CONTRACT.md §synthesize`；`synthesize` 返回值新增
> `material_tier`/`allowed_use`/`not_for` 字段。后续实验：多段 `phoneme_codepoints`+`silence_scale`、`blank_penalty`（见 D58）。

门禁一句话：`powershell -File tools/check.ps1`（fmt+clippy+209 单测+golden+pinyin 表 `--check`）＋ `voice-cli tts-regress cases/tts-word-loss.jsonl --speed 0.55`（需模型）。
TTS 侧已收口：前端开放词表 G2P + misaki-zh + LTS + 漏词门禁（见 `docs/audit/tts-2026-11-27-frontend.md`）。

## 已实测的关键数字（别再猜）

| 项 | 数值 |
|---|---|
| ASR SenseVoice int8 | 6 s 音频 82 ms（RTF 0.013）；81 s 音频分块后 2.3 s（RTF 0.028） |
| TTS Kokoro 82M | 默认音色按语言：英文 `am_michael`、中文 **`zf_xiaoni`**（`zf_xiaobei` 实测 0.515，已弃用）；RTF 0.21，24 kHz，54 音色；**默认 140 wpm**（130–160），slow=110；短文本/中文不按 WPM 校准；WPM 按**语音跨度**算（单位＝英文词＋中文字） |
| TTS 文本前端（2026-11-27） | 英文走**开放词表 G2P** → `phoneme_codepoints`（CMUdict + 形态规则 + 数字 + 规则 LTS 兜底），lexicon 降为**覆盖层**；未见词根走 LTS 并打 `lts_words` 标记；中文走 `lexicon-zh`；**中英混说默认拒答**；标识 `g2p-ipa/1.0[cmudict+overlay]+lexicon-zh` |
| TTS 中文前端（2026-11-28） | **默认 misaki-zh**（`tools/g2p_misaki.py`，自动探测，`VOICE_G2P_CMD_ZH=off` 可关）；失败回退 `lexicon-zh`。多音字：lexicon 逐字查表错 7/8（银行/重庆/长城/音乐/空调/便宜/行长），misaki 只错 1（还钱）。普通句 ASR 字符准确率两者都 1.000（speed 1.0）。原记录：可选 misaki-zh sidecar（`tools/g2p_misaki.py`，`pip install "misaki[zh]"`）。5 句实测字符准确率 misaki 0.939 vs lexicon 0.903（`zf_xiaoni`）；`zf_xiaobei` 在码点路径仅 0.515 → 中文示范音优先 `zf_xiaoni` |
| ASR 语言（D53） | `en`/`zh` 走**语言锁定**的 SenseVoice 实例（懒加载，每份 ~230 MB）；实测佩奇 90 s：CJK 乱码 30 → 0；`VOICE_SENSE_LANG_LOCK=0` 可关 |
| TTS 回归门禁 | `cargo test`（209，含 G2P/LTS/sidecar/拼音层）＋ `voice-cli tts-regress cases/tts-word-loss.jsonl`（13 条：漏词=词数减少、`expect=reject_mixed`、`expect=lts`、wpm 带内断言） |
| 引擎冷启动 | 1.7 s（ASR 与 TTS 并行加载） |
| `.m4a`/mp3 解码 | 纯 Rust（symphonia），6 s 音频 1.8 ms |
| 麦克风（EDIFIER W820NB 蓝牙 HFP 端点） | 48 kHz 单声道 F32 可用；底噪 p50 −54.7 / p99 −46 dBFS（**偏高，AGC 强**） |
| 长音频 | **必须分块**（>30 s 单次解码会静默截断 + 时间戳压到 0.77 倍） |
| 教材对齐精度 | NCE2 L2：19/19 行文本相似度 1.00，行起始偏差 +0.07~+0.29 s，LRC 拟合 scale 0.999 |
| **音素 CTC（wav2vec2-espeak）** | ONNX 1 205 MB，CPU **RTF 0.04–0.05**；int8 量化漂移 8.21 → 弃用 |
| **GOP 判别力（so762 未见说话人）** | 句级 ρ 0.67；**词级 mean-GOP：AUROC 0.887、P@FP10 0.457**；音素级 0.769/0.170（基率上限 0.45） |
| **MDD 微调（6ep/8L）** | 词级 AUROC 0.883→0.887、音素级 0.690→0.769；训练 65 s/epoch（RTX 5070 Ti cu128） |
| **混淆对信号（TH→s）** | 命中精确率 0.50、召回 0.67（仅 TH 系列可对用户说） |
| **韵律（YIN，纯 Rust）** | 51.8 s 真人：F0 中位 116.3 Hz、起伏 3.31 st、音域 12.61 st、句末 −0.38 st→flat；TTS 同文本 DTW 0.11 st |
| **readiness 自动评估（N6/D48）** | `voice-cli learner readiness [--write]`：三态判据（acc/语速/停顿），`unknown` 不当通过；真机 `learner.json` 复算 L2+（missing 2 条 + unknown 3 条） |
| **计划去重（N7）** | `RECENT_PRACTICED_N=12` 窗口；队列项排队尾、素材不够兜底复用并写 note；单测验收重叠 0/6 |
| **中文拼音层（D13）** | 表 **26 703 汉字 / 7 310 多音字**（pypinyin 0.55 派生，287 KiB 内置二进制，runtime 无依赖）；10 句 TTS→ASR 审计：字级 0.922 / 拼音层 0.933、同音 1 处、**误报 0** |
| 中文数字归一 | 中文参考下 `26`→`二十六`（逐字 3 单元）、`1998`/`007`/5 位以上逐位读；修掉「二十六度」被判 0.700 的假错 |
| 素材库 | **5 028 条 / 7.18 h**（NCE 4 997 + IELTS prompt 10 + IELTS 听力真人 14 + Peppa 7），quarantine 1 234；`peppa` 默认不进训练计划 |
| 素材库 ingest | 276 课 ≈ 10.5 min；`bank stats` 是唯一权威计数 |

## 环境

```
模型   C:/voice_models     （1.58 GB，9 个文件，已供应链校验）
数据   D:/voice_data       （config.json 里 mic="EDIFIER"，silence=2.2 s）
素材   D:/voice_data/cases_audio （回归用例音频，VOICE_CASES_DIR 指向它）
代码   D:/code/voice_mcp_service
Pi     ~/.pi/agent/mcp.json 里 voice = target/release/voice-mcp.exe（exposure=direct, timeout=180）
Skill  tools/install_skills.ps1 已 junction 到 ~/.pi/agent/skills/
```

## 常用命令

```bash
cargo build --release && cp .cache/sherpa/lib/*.dll target/release/
./tools/check.ps1                                # 门禁：fmt + clippy -D warnings + test（提交前必跑）
./tools/check.ps1 -Cases                         # 额外跑 9 条录音回归（需 VOICE_CASES_DIR）
./target/release/voice-cli.exe doctor --load     # 12 项自检
./target/release/voice-cli.exe mic --secs 3      # 麦克风能力 + 底噪 + 阈值建议
./target/release/voice-cli.exe rec --max-sec 20 --stt --lang en
./target/release/voice-cli.exe assess <audio|last> --reference "原句" --with-gop   # 词级+音素级 GOP + 基线（无 bank-id 时用 TTS）
./target/release/voice-cli.exe assess <audio|last> --bank-id nce2/02/003 --with-gop # 素材库：参考文本+真人原声切片当基线（推荐）
./target/release/voice-cli.exe assess <audio|last> --with-prosody --reference-audio <示范.wav>  # 韵律 + DTW
./target/release/voice-cli.exe learner readiness --write                           # 从 history 重算 readiness（写回留 .prev）
./target/release/voice-cli.exe learner readiness --confirm-free-speech                # L4 达标才真开自由说
./target/release/voice-cli.exe pinyin "银行行长正在开会"                            # 中文逐字读音（多音字全列）
./target/release/voice-cli.exe pinyin "我在北京银航工作" --reference "我在北京的银行工作"  # 拼音层诊断（纯文本）
./target/release/voice-cli.exe gop <audio> --text "原句"                          # 只跑 GOP（看 heard/confusion）
./target/release/voice-cli.exe bank plan --minutes 15 --limit 8                   # 定向训练计划（含 60 s 自由说槽位）
./target/release/voice-cli.exe bank plan --minutes 15 --limit 8 --no-free-speech # 强制省略自由说槽位
./target/release/voice-cli.exe bank plan --minutes 15 --limit 8 --free-speech    # 强制包含（试一次）
./target/release/voice-cli.exe bank pick --tag theta_to_s --level 2 --limit 3     # 按 tag 权重取材
./target/release/voice-cli.exe bank pick --exclude-corpus nce --limit 5           # 排除某语料（可重复）
./target/release/voice-cli.exe bank plan --exclude-corpus ielts --dry-run         # 计划里排除某语料
./target/release/voice-cli.exe bank drill nce1/019/003                            # 播原生音频
./target/release/voice-cli.exe lesson <mp3> --lrc <lrc> --verbose
./target/release/voice-cli.exe tts-audit --manifest <jsonl> --out <report.jsonl>  # 只读审计 TTS 资产
./target/release/voice-cli.exe bank refs --kind prompt --runs 3 --out <dir>       # 批量生成示范音 + 逐条 tts-verify（verified 才落盘）
./target/release/voice-cli.exe bank ingest-audio <mp3> --lesson <label>          # 单文件真人音频入库（无 LRC：VAD 切句 + 双引擎验证）
./target/release/voice-cli.exe bank refs ielts/custom/p1-weather --voice am_fenrir --runs 3 --out <dir>  # 指定素材/音色
VOICE_CASES_DIR=/d/voice_data/cases_audio ./target/release/voice-cli.exe cases
```

## 阻塞与前置条件

1. **蓝牙耳机必须处于连接状态**才能在系统里看到输入设备（休眠时 `mic` 会明确报错）。
   设备名会带 `2- ` 之类前缀且会变 → 配置用品牌子串 `EDIFIER`，匹配器已做"宽松匹配 + 宽带优先"。
2. Skill 需要 `/reload`（或重启 pi）才出现在会话里；`pi -p "列出可用 skills"` 可验证。
3. 没有麦克风时一切文件路径功能仍可用（`assess path=/stt`），只是不能 `record`。

## P3 真机训练闭环（2026-11-21 已实跑验收）

设备：EDIFIER W820NB（蓝牙 HFP，48 kHz 单声道）。留证：`sessions/2026-11-21/*_drill/takes/`。
会话小结：`sessions/2026-11-21/233336_drill/session.md`。建档：`D:/voice_data/learner.json`。

- [x] 1. `mcp__voice__*` 8 个工具与 2 个 Skill 都在（`pi -p "列出可用 skills"` 实测 3 个 skill，含 archify）
- [x] 2. 建档：3 句诊断（accuracy 1.00 / 1.00 / 1.00）+ retry → `learner.json`（band 6.0，confidence low：未做自由说）
- [x] 3. 单轮训练：`synthesize(slow_too, play)` → `record(silence_sec=2.2)` → `assess(reference=句)` → 只报 1 条（语速 214.7 wpm）→ `retry` 对比（192.7 wpm，断点 0.96 s）
- [x] 4. 留证完整：同一 take 目录内 `.raw48000.wav` + `.16k.wav` + `.asr.json` + `.metrics.json` + `.md`（+ `.capture.json`）；重复评测留 `.metrics.prev.json`
- [x] 5. Skill 铁律在实测中全部生效：`unclear` 不下结论、whisper（`timestamp_alignment=missing`）不报秒数、单轮只报 1 条
- [x] 6. 实跑暴露的问题已修（共 5 个）：`slow_too` 语速对、assess 证据分裂、中文 panic、16 kHz 派生污染 `band_limited`、英美拼写变体与 ASR 伪缩略误报

**无稿自由说改为 readiness gate（2026-11-24）**：当前 `readiness.stage = L2+`，槽位关闭；
`band_estimate` 6.0（5.5–6.5），confidence medium，只作**跟读/朗读下界**。
达标 L3（连续 3 条 acc≥0.90 等）后再做 30–45 s 提纲独白，L4 才开启 60 s 无稿自由说。
详见 [docs/levels.md](levels.md)。`bank plan` 默认按 `readiness.free_speech_enabled` 自动决定槽位。
（`record` 的 8 s 起始等待已可通过 `start_timeout_sec` 调整，D39 已还。）

## TTS 语速与伪影（2026-11-27 更新：D54 已收口）

语速标定的候选排序已改为“**离 130–160 带宽的连续距离**优先，起始伪影次之”，并改成
“只要还没进带宽就继续换速度”（上限 9 个候选）。复测 10 条判别句：136.1–145.6 wpm 全部在带内。
旧结论（“140 wpm 承诺不可靠”）已失效。

## TTS 语速与伪影（2026-11-25 旧记录）

- **WPM 口径**：按语音跨度（峰值 −40 dB 界定的首末 10 ms 帧）算，不按文件总时长。
  旧口径目标 140 会量到 128（历史 s 集 wpm 也是旧口径，复测后 7/8 仍在 130–160，s5 = 168）。
- **语速校正**：保留最优候选（先看目标 ±6%、再看有无起始伪影），不再“只取最后一次”
  （Kokoro 的 speed→wpm 不连续：0.78→132、0.815→153）。
- **起始伪影**：`onset_gap_ms` 非 null = 先出一小段音 → 停顿 → 正文；af_heart 会被双 ASR 听成多一个 “And”。
  规避 = 换速度/换音色；**不裁音频**。结构信号只降到 `review`，只有双 ASR 也多听出词才 `reject`。
- **批量生成**：`bank refs` 逐条验证，失败自动换目标语速（±4）→ 换音色（am_michael → am_fenrir）；
  只有 verified 进 `out/`。10 条 IELTS prompt 已 10/10 verified 3/3（含音素），
  产物 `D:/voice_data/bank/tts-refs/ielts-prompts/`。
- **词表**：`hometown` 的次重音会让 whisper 拆成 `home town`，已改成 `h ˈ O m t W n`（无次重音）。
- **默认音色 = `am_michael`（男声）**（2026-11-25 用户决定，MCP `synthesize` / CLI `say` / GOP 基线三处统一，
  常量 `voice_core::engine::DEFAULT_VOICE`）。理由：af_heart 在 140 wpm 附近有起始伪影（双 ASR 听成 “And”），
  am_michael 是 10 条 IELTS prompt 的主力已验证音色。需要女声时显式传 `--voice`。
  注意：GOP 基线音色同步改了 → **2026-11-25 前后的 Δ 不可严格横比**。

## 语料排除（`--exclude-corpus`，2026-11-26）

`bank pick` / `bank plan` 默认**排除** `peppa`（儿童动画：可听、但词汇层太低、卡通语调、
文本常是 ASR 派生，不是雅思训练素材）。规则：

- 不指定 `--corpus` → 自动追加 `default_exclude_corpora()`（当前只有 `peppa`）；
- 指定 `--corpus peppa` → 以用户为准，不再默认排除；
- 追加自定义排除：`--exclude-corpus <name>`（可重复）。

## 雅思真人素材入库（2026-11-26，N9 首次打通）

`bank ingest-audio` 补上了“只有一段真人录音、没有 LRC/字幕”这条路：

```bash
voice-cli bank ingest-audio <mp3> --lesson 2023-12-08 [--corpus ielts] [--book IELTS]     [--srt <subtitle.srt>] [--min-agree 0.7] [--max-sec 12]     [--start 20 --end 110] [--max-chunks 20] [--dry-run]
```

**有字幕就用字幕**（`--srt`，或把 `S01E01.srt` 放音频旁边自动找）：文本与时间都取字幕
（`text_source=srt`），ASR 只做时间轴复核（一致率 <0.7 的 cue 进 quarantine）。
没有字幕才退回 ASR 派生（`text_source=asr`）。

`--start/--end/--max-chunks` 用来只取“某一集里的某个片段”（长音频/不想全量入库时）。

流程：解码 → VAD 段 → **段内按真实停顿再切**（不用 ASR 词时间戳，CTC 会漂）→ 每片跑双引擎
（SenseVoice 出文本、whisper 独立复核）→ 一致率 <0.7 进 quarantine → 写 16 kHz 切片 + 索引
（`ref_audio=native`、`text_source=asr`、`quality.asr` 记一致率）。`--dry-run` 会打印逐句预览。

已入库：`ielts/2023-12-08/001–014`（**14 句**，来源 `ielts_2023_12_08_b9380f3a….mp3`，44.1 kHz 真人）。
首句 `Right, Judy,` 只有 2 个词（<3）被拒；`bank show` 会显示 `text_source: asr`。

**这段音频的性质（别搞错）**：它是 **IELTS 听力风格的导师独白**（"Right, Judy, this week is your
turn to bring us up to date on your dissertation…"），**不是那 10 条口语 prompt 的朗读**，
所以它补不上“prompt 缺真人音”那一项。另外实测语速 **≈214 wpm**（257 词 / 72.2 s 语音），
高于雅思 130–160 —— 当“快速听力挑战”可以，当“标准语速材料”要先用 ffmpeg 降到 ~0.75×。

### 佩奇（Peppa）音频评估（2026-11-26，结论：只当听力/跟读音，不当答案键）

`D:/code/NCE-main/NCE-main/peppa/ugc/` 有 52 集 m4a（每集 ~5 min，bilibili UGC，44.1 kHz，无字幕）。
实测 `S01E01 --start 20 --end 110`：**入库 8 句 / 拒 9 句**，且入库的句子仍有明显错字：

| ASR 给的 | 实际 |
|---|---|
| `Pepper loves jumping in muddy puddles.` | Peppa |
| `Mdy puddles.` / `puals` | muddy puddles / puddles |
| `HaAll right, run along you too.`（被拒） | All right… |

**加字幕之后（2026-11-26 实测）**：把 `S01E01.srt` 放到音频旁边再跑同一段
→ `text_source=srt`，入库 7 句全部是正确拼写（`Peppa` / `muddy puddles`），
字幕与音频错位的那条 cue 被一致率闸门拦下（拒 1）。**所以字幕是佩奇这类素材的关键路径。**

- **能用**：native 音频（真人、清楚、~130–150 wpm、level 1–2）、`ref_audio=native`、可做听力/跟读/韵律参考。
- **不能用**：`text_source=asr` 的文本当听写答案键（专有名词 + 卡通童声 + 背景音，双引擎也一起错）。
- 要当“标准文本”，必须有 **SRT/VTT**：`--srt` 已实现（放同名文件会自动发现）。
- **语速提醒**：实测佩奇 ≈180–200 wpm、那条雅思录音 ≈214 wpm，都**高于**雅思 130–160
  —— 两个 bilibili 源都像是被变速上传过（×1.25–1.4）。要标准语速材料，需先用 ffmpeg
  `-filter:a atempo=0.8`（本机暂无 ffmpeg）降速后再入库。
- 版权：仅本地个人学习，不入 git（`bank/` 在 `D:/voice_data`）；入库会写 `ATTRIBUTION.md`。

## 参考音与基线（2026-11-25 修正：TTS 不再当“标准”）

**之前的错**：跟读评测的 GOP 基线一律用 TTS 合成音，韵律 DTW 的参考音也大多是
`synthesize` 产出的 `.ref_norm/.ref_slow`。问题有三层：

1. TTS 自己会读错（审计：country/thank/tokyo 回读不一致），旧实现只做**内容**自检 → 词对就算过；
2. TTS 与 G2P 共用同一套音素变体（D46），同形异音词（live/read）基线跟着错，无法自救；
3. 合成音的语调不是人标准，拿它判“语调/重音不地道”没有依据。

**现在的口径（D52）**：

| 场景 | 基线/参考 | 标记 |
|---|---|---|
| 有 `bank_id` 且 `ref_audio=native`（NCE 4 997 条） | 素材库**真人原声切片** | `gop_baseline_source=native`、`prosody.reference.source=native` |
| 只有 prompt（`ref_audio=tts`，10 条）或普通跟读句 | TTS（am_michael 140 wpm） | `…=tts`，**不得判“语调”** |
| 外部参考音（自己传路径） | 原样使用 | `unknown`（除非它是 `.ref_norm/.ref_slow`） |

- 基线自检加了一条**音素**闸门：最差词级 GOP < −1.5 → `gop_baseline_valid=false`（只做内容自检会漏掉“词对但音读坏”）。
- CLI：`assess <录音> --bank-id <id> --with-gop`；MCP：`assess(bank_id=…, with_gop=true)`；`bank drill`/`bank plan` 会打印这条用法。
- **历史数据**：2026-11-25 之前 `metrics.json` 里的 Δ 都是 TTS 基线算的，**不可与新结果横比**。
- **native 基线的已知偏差**：NCE 切片是 16 kHz、用户录音 48 kHz，带宽不同 → 摩擦音的 GOP 会偏；
  Δ 比 TTS 基线可信，但**不要拿它判 /s/ /θ/ /f/ 的细微差别**（要严格比就把两边都降到 16 kHz）。

## 已知债务（详见 ADR-0008）

S1 级已全部偿还。剩余主要项：
D04 whisper 路径无回归 · D06 音色表未人耳校准 · D09/D11 时间戳与阈值（韵律已上线，D12 已还）
D15 全局锁串行 · D18 DLL 手工复制 · D21 CI 里的合成 fixture（**D20 已还：Actions 已绿**；仍缺音频 fixture）· D29 Skill 分发 · D35 蓝牙端点选择 · D36 AGC 强导致能量维度不可用于重音 · D38 分块边界可能切词
D40 教材导语不在 LRC 里 → 每课固定 10–12 条 `extra` 噪声 · D41 英美变体表需人工维护（只覆盖 50 对高频词）· D46 G2P 同形异音词只取首变体（live/read）· D48 readiness 未自动评估 · D50 韵律 12 条标注验收未做
D04 whisper 端到端音频回归仍缺（routing + 3 个 fixture 单测已补）· **D52 TTS 当基线（部分偿还）**：真人原声优先 + 音素自检已做；残留是 IELTS prompt 无真人音 + 历史 Δ 需重跑 · **D53 SenseVoice `lang` 不生效（部分偿还）**：语言守卫（请求 en/zh 却拿到另一种语言 → 换引擎重听）；彻底修需按语言分别加载识别器

已还（2026-11-28）：**D13**（中文拼音层）· **M5** 达成 · **D56**（录音回归断言改等价集合，9/9 复绿）

已还（2026-11-25）：**D47**（短语音 SNR 豁免）· **D49**（双引擎文本闸门 `text_agreement`）· **D51**（`bank refs` 批量验证 + 10/10 prompt 3/3 + 起始伪影检测）· **D20**（CI 首跑已绿）
新增通道（2026-11-26）：`bank ingest-audio`（`--srt` 优先 / 无字幕 ASR 双引擎）、`bank pick/plan --exclude-corpus`

已还：D19 rust-toolchain/MSRV · D39 `record.start_timeout_sec` · D45 构建门禁。（D18 只缓解：`check.ps1` 把 DLL 目录加进 PATH。）

## 下一批工作（用户已确认顺序）

1. ~~**P3 真机训练闭环**~~（2026-11-21 完成）
2. ~~**P4 跟读素材库**~~（2026-11-22 全量完成）：`bank ingest/list/show/drill/stats/retag`，NCE1–4 共 276 课 → 4 997 条（现 5 028：另加 IELTS prompt 10 + 真人音频 21）
3. ~~补一次自由说 30–60 s~~（2026-11-23 完成；**2026-11-24 改为 readiness gate**：L2+ 关闭，L3/L4 达标后开启）
4. ~~韵律分析（F0 / 句末升降 / 平调 / 重音候选 / DTW）~~（**完成** 2026-11-23，见下节）
5. **Route A（无参考自由说伪 GOP）**：已立 [ADR-0010](adr/0010-free-speech-pseudo-gop.md)，
   **Proposed / Deferred**，不在当前迭代；触发条件见 ADR。
6. **回到原定路线**：素材库定向训练（`bank plan`）→ 雅思素材入库（**通道已通**：`bank ingest-audio --srt`，仍缺整本题库音频）→ 按 `error_stats` 组卷
7. **2026-11-26 之后**：N1（whisper 端到端回归）→ ~~N6/N7（readiness 自动评估 + plan 去重）~~（2026-11-28 已还）→ N4（D46 同形异音）

## GOP / 诊断层现状（2026-11-22）

**能力**：`assess(with_gop=true)` 返回
`gop.words_worst[]`（词级，对外）、`gop.worst[]`（音素级，仅排序）、`gop_baseline`（对照基线，**真人原声优先 / TTS 兜底**，见 D52）、
`gop.worst[].confusion`（混淆对，如 `TH→s`，`confusion_reportable` 当前仅 TH 为 true）。

**硬规则（SKILL 4.0/4.1 已固化）**：
1. `degraded=true`（snr<13 / peak<−30 dBFS / 削波>0.1%）→ **不报任何发音结论**，只给重录建议；
2. 词级 vs 基线：`Δ ≤ −3 且基线 ≥ 4` 才可说"低于示范"；基线 <0.3 的词一律不报（模型偏好）；
3. 音素级禁止断言；只有 `confusion` 且 `TH→s` 允许说"模型听到的是 /s/ 而不是 /θ/"；
4. 整词替换靠词级 diff（GOP 会漏，见 `cases/r09_why_when.json`）。

**已知限制**：音素级精确率天花板 0.45（基率 7.5%）；CTC spike 导致每个音素只有 1–2 帧，
**不能用时长判断元音长短**；MDD 进一步提升空间 ≈0（词级已贴天花板），下一步方向是"降基率"
（可疑词定向重录复核）。

## 韵律分析现状（2026-11-23）

`assess(with_prosody=true)` 或 `reference_audio=<示范音>` 返回 `prosody`：
F0 中位/标准差/音域、句末斜率与升降方向、平调判定、重音候选 top5、节奏、与示范音的 F0 形状 DTW。

算法与验证见 [ADR-0007](adr/0007-prosody-and-gop.md)；**只给数值+位置，不下"语调不好"的结论**。
SKILL 4.2 已固化措辞规则（先看句型再解读升降、`stress_top` 只是候选、`energy_z` 不可用于重音）。

已知边界：
- **未做 12 条人工标注验收**（ADR 验收项：升降/平/重音一致率 ≥80%），当前只有 8 条单测 + 1 条真机 + DTW 端到端；
- 重音候选的时长来自 ASR 词时间戳（±0.5 s），只能说"第几个词"；
- 蓝牙 AGC 让 `energy_z` 失真（D36），只作第 3 特征；
- `voiced_ratio` 含静音帧，不要与 `flow.speech_ratio` 混用。

**已生效（2026-11-25）**：`voice-mcp.exe` 已重编译并在 MCP 端实测（`synthesize` 返回 `voice=am_michael`、
`speech_duration`、`onset_gap_ms`）。MCP 是 Pi 的 stdio 子进程：换二进制必须先让旧进程退出
（`/mcp` 里 disable/reconnect，或杀掉 PID），再 `cargo build --release -p voice-mcp`，
否则 Windows 文件锁会让构建静默复用旧 exe。

## 中文拼音层（2026-11-28，D13/M5 已还）

详细口径见 **[docs/pinyin.md](pinyin.md)**，这里只记"是什么/数字/边界"。

**做了什么**：参考文本含汉字时 `assess` 自动产出 `text_diff.pinyin`，把字级 diff 升级成
**声母 / 韵母 / 声调**三层诊断。三个变化：

1. **同音不同字不算错**（`的/得`、`在/再`、`银行/银航`）→ `issues[].hint = likely_homophone`、
   severity `low`；中文真实发音口径看 `accuracy_pinyin`（字级 `accuracy` 口径不变，保持可纵向比）。
2. **汉字替换的音形相似度不再恒为 0**：过去英文骨架编码对汉字返回空 key，导致所有汉字替换都被推成
   `likely_different_word`（"说成了别的词"）。现在按 声母 .45 / 韵母 .35 / 声调 .20 加权。
3. **已知混淆对可点名**：平翘舌（s/sh）、前后鼻音（-n/-ng）、n/l、尖团音… → `confusions[].tag`；
   表外的差异只统计、不结论。声调一律 `unclear/medium`（文本层分不清"声调错"与"ASR 选字"）。

**数据**：`crates/voice-core/data/pinyin-zh.txt`（`tools/gen_pinyin_table.py` 由 pypinyin 0.55 生成，
**26 703 汉字 / 7 310 多音字**，287 KiB，`include_str!` 内置 → **运行时不依赖模型目录，CI 可跑**）。
多音字取"让两边最接近的那一读"（同 D46：银行/长城/音乐/空调/重庆/便宜 全部判对）。

**附带修复**：中文数字归一 —— SenseVoice 把「二十六」写成 `26`，旧口径按英文展开成 `twenty six`
→ 「他把空调开到二十六度」字级准确率被拖到 0.700；现在两侧都归一成汉字（`26`→`二十六`，
5 位以上/年份逐位）。修完该句 1.000。

**实测**（`python tools/pinyin_audit.py`，10 句 TTS `zf_xiaoni` → SenseVoice `zh`）：
字级 **0.922** / 拼音层 **0.933**、同音 1 处（`银行`→`银航`）、**被判成发音偏差的 0 处**。
报告：[audit/pinyin-2026-11-28.md](audit/pinyin-2026-11-28.md)。

**语言不匹配守卫（2026-11-28 真机踩到后补）**：参考是汉字、转写里却没有任何汉字
（`lang="zh"` 配英文录音 → `X Ste par loved books to Shanghai last week.`）→ `degraded=true` +
`limitations` 说明、`pinyin=null`，避免把"每个汉字都被替换成英文单词"当成发音结论。
判据 `assess::chinese_ref_without_hanzi`（1 个单测）。

**边界**：纯文本层（中文**没有**音素级 GOP，`with_gop` 会跳过并在 `limitations` 写明）；
声调结论最弱；多读音会让错误被低估（宁漏不误报）；中文韵律未标定，不得据 `prosody` 判声调。

**验收**：单测 **+21**（pinyin 13 / diff 中文 5 / 数字归一 2 / 语言守卫 1），全仓 **196 条**；
`check.ps1` 增加 `gen_pinyin_table.py --check`（缺 pypinyin 只提示不阻断）。

**录音回归现状（2026-11-28）**：**9/9 全绿**。本轮一度是 7/9（`r01_third_16k` / `r06_nce2_l2_native`），
用 `git stash` 对比确认与 D13 无关，是 ASR 输出漂移（`third`→`three`、导语 `Lesson2`→`lesson`+`two`）。
已按 **D56** 收口：断言支持**等价集合**（`cases::OneOrMany`，`said`/`hint`/`severity` 可写数组，命中任一即过），
不再用单字符串锁死 ASR 的偶然写法。

## readiness 自动评估 + 计划去重（2026-11-28，D48/N6/N7 已还）

**问题**：`levels.md` 的 L2/L3/L4 判据早就写好，但代码只**消费** `readiness.free_speech_enabled`，
`stage`/`missing` 全靠手写 → 与真实历史漂移（真机档案里 missing 还写着"最近 1.00 / 0.80 / 0.875"，
其实 history 末尾三条是 0.800 / 0.875 / 0.667）。

**N6（`voice-core::readiness`）**：从 `history` 复算，三态判据：

| 判据 | 口径 | 缺数据 |
|---|---|---|
| `consecutive_acc` | 最近 `gate.consecutive_n`（3）条 acc ≥0.90 | `not_met` |
| `rate_band` | 语速 120–160 wpm | `unknown`（全缺时） |
| `pause_density` | pause_count ÷ (speech_duration/30) ≤ 6 | `unknown` |
| `core_phonemes` | θ / v / 词尾无回归 | **固定 `unknown`**（交 Skill 判读） |
| `free_speech` | 最近一条 `scene=free` 的停顿/重复 | `unknown` |

- `scene=word`（单词 drill）与 `scene=free` **不参与**"连续 N 条"（单词 acc 与句子不可比，无稿没有 accuracy）。
- CLI：`learner readiness [--write] [--confirm-free-speech]`；写回只动 `readiness`，保留人工写的 evidence
  （`theta`/`v`/`contractions` 等），写前留 `learner.json.prev`；**开自由说必须 L4 达标 + 显式确认**。
- 真机复算（2026-11-28）：stage **L2+**，missing 由 4 条手写 → **2 条明确未达标**（连续 acc / 语速带）
  + **3 条 unknown**（停顿缺字段、核心音素、自由说）。

**N7（`plan::RECENT_PRACTICED_N`）**：`bank plan` 把 `history` 末尾 12 条里练过的素材**降级**：
`error_stats`/`fill` 两步跳过它们，`drill_queue` 里刚练过的排到队尾（不丢弃）；素材不够时兜底复用并在
`notes` 写明。验收线"连续两次计划重复率 <20%"由单测 `consecutive_plans_do_not_repeat_material` 锁住
（实测重叠 0/6）。真机 `bank plan --limit 6` 已带 `已把最近练过的 5 条素材降级` 提示。

## Route A 立项（2026-11-23，暂不实现）

无参考自由说的伪参考 GOP 已写入 [ADR-0010](adr/0010-free-speech-pseudo-gop.md)：
可行性实测（51.8 s 样本：一致词覆盖率 **84.1%**、共识词 CMUdict 覆盖 100%、端到端 ≈22 s/60 s、
共识里存在 `i mistake` 这类双引擎同错）、设计、R1–R9 风险、V1–V10 验收线都已固化。

**决策：先不开工。** 当前迭代继续原定路线；A 的触发条件是 D04 whisper 回归已还，
且用户要求自由说纠音——**自由说已于 2026-11-24 改为 readiness gate（L2+ 关闭，L4 才开启）**，
L4 之前该触发条件不会成立；A 仅保留为未来选项。

为了以后能接上，当前代码保持：`Transcript.words/tokens` 双保留、`normalize`/`phonetic`
公开且确定性、`assess` 单入口 + feature flag、伪参考结果只新增字段不合并进 `gop`。

## 真机训练记录（2026-11-22 夜）

`D:/voice_data/sessions/2026-11-22/`，`learner.json` history 14 条（含 2026-11-23 三条自由说）。已修：
`why→when`（Δ −4.27→−0.39）、`thank` 的 /θ/（TH→s → heard θ，Δ −0.05）、`valley`（acc 0.83→1.00）。
待练：`short_long_vowel`（/ɪ/ vs /iː/，seat 第 1 次仍读成 sɪt）、`ae_to_e`（sit→set）、`village` 的 /ɪlɪ/。

**唯一的硬件障碍**：EDIFIER W820NB 的麦克风增益在同小时内漂移 −11.8 ~ −33.5 dBFS，
开练前先 `rec` 说句 test 确认 `peak` 在 −20 ~ −12 dBFS。

## 门禁与文档对齐（2026-11-22 晚）

**为什么做**：`4f57a36` 在 `format!` 里写了 `{peak_dbfs:.1f}`（Rust 只接受 `{:.1}`），
`voice-mcp` 直接编译失败，但后面 3 个提交都没人发现 —— 因为没有门禁。

已落地：

- 修掉那 1 处编译错误；`cargo fmt --all` 全仓格式化；清掉全部 36 条 clippy 警告
  （26 条 collapsible_if 自动修，4 条手工：needless_range_loop / field_reassign ×2 / too_many_arguments 加注）。
- `tools/check.ps1`：`fmt --check` → `clippy --all-targets -D warnings` → `test`，
  可选 `-Cases` 跑录音回归。本地实测：**148 单测 + 9 用例全绿**（2026-11-26）。
- `.github/workflows/ci.yml`：windows-latest 上跑同一套脚本，先 `fetch_deps.ps1` 取原生库。
  **GitHub Actions 已跑绿**：`530e668`（2026-11-24）/ `3cf8736`（2026-11-25），
  `windows-latest` 上 `fmt → clippy -D warnings → test`（148 单测）全过。
  仍缺的是**合成 fixture**（D21）：CI 里跑不了带音频的 decode/flow 用例。
- `rust-toolchain.toml` + `Cargo.toml` 的 `rust-version = "1.99"`（实测版本；edition 2024 的理论下限 1.85 未验证）。

同批对齐的文档漂移：

| 文档 | 旧 | 新 |
|---|---|---|
| CONTRACT | 工具清单“7 个” | 8 个（transcribe/synthesize/record/play/prepare_audio/voices/status/assess） |
| CONTRACT | `record.silence_sec` 默认 1.0；无 `start_timeout_sec` | 默认 2.0；已加 `start_timeout_sec`（默认 8） |
| CONTRACT | GOP “句级 ρ 0.67；音素级 AUROC 0.63–0.66 / P 0.15–0.24” | 补词级 AUROC 0.887 / P@FP10 0.457；音素级 0.769 / 0.170；补 `gop.words_worst` / `gop.worst` / `confusion` / `gop_baseline` 字段 |
| STATE | 素材 4 997 / ready 3 368 / 3 384（两个数） | 5 007 / ready 3 378（`bank stats` + 索引实测） |
| STATE | history 11 条 | 14 条（后补三条自由说） |
| STATE | D39 仍列为未还 | 已还（`record.start_timeout_sec`） |
| README / docs/README | 债务“33 条” | 45 条（D01–D45） |

## 自由说定档（2026-11-23）

录音条件：样本 A/B 为 EDIFIER W820NB（Hands-Free，48 kHz，`record` 自动停止）；样本 C 为系统「录音机」m4a。三种都走 `assess(reference=不传)`。

| 样本 | 语音/窗口 | speech_rate | articulation | pauses（最长） | speech_ratio | run | peak/SNR |
|---|---|---|---|---|---|---|---|
| C `2026-11-23/205337_assess` **主样本** | 32.3 / 48.8 s | 100.9 | 152.2 | 17（1.85 s） | 0.663 | 9 词 | −18.7 dBFS / 24.2 dB |
| A `2026-11-23/204916_drill` | 13.5 / 19.0 s | 100.9 | 142.2 | 9（0.89 s） | 0.709 | 10 词 | −12.4 dBFS / 24.4 dB |
| B `2026-11-23/204215_dictate` | 13.9 / 28.1 s | 76.9 | 155.5 | 8（**3.64 s**） | 0.494 | 9 词 | −19.1 dBFS / 27.1 dB |

结论：单段 51.8 s（语音 32.3 s）**已达标**。`band_estimate` 维持 **6.0**（区间 5.5–6.5），`confidence` **low → medium**。
三条都是工作汇报，**不是雅思题**；转写有 ASR 误听（frontend→front 等），无 reference 不据此判发音。

短板不在发音而在**独白连续性**：停顿 ~10.5 次/30 s、最长一口气 9 词、`word_boundary_breaks` 15/76 词。
`focus` 已改为“60 s 少停顿 + 意群连读”。下次同题自测目标：pause ≤ 8 次/45 s、longest_run ≥ 12 词。
教训：`record` 会被 3.5–5 s 的思考停顿切断（三次尝试 4.4 / 19.5 / 18.6 s）→ 长独白一律用系统录音机，见 SKILL 第 7 节。


## TTS 资产验证（2026-11-24）

- 默认语速 **140 wpm**（雅思 130–160），slow=110；`VOICE_TTS_WPM` / `config.json.tts_wpm` 可覆盖。
- **OOV 预检 + 项目词表**：`$VOICE_HOME/tts-lexicon.txt`（`iphone`/`export`/`hometown`/`makes`/`universities`/`influenced`/`that's`/`why`）；
  缺词直接报错，不再静默丢。
- **`voice-cli tts-verify`**：合成 N 次 → 双 ASR + WPM + 可选音素，3/3 才 `verified`；单引擎不一致/边界语速为 `review`。
- **`voice-cli tts-audit`**：只读审计现有 wav；首次报告见 [audit/tts-2026-11-24.md](audit/tts-2026-11-24.md)。
- **`gop_baseline_valid`**：TTS 基线先 ASR 回读；不一致时禁止 Δ（country/thank/tokyo 已被拦）。
- verified 听力素材：`D:/code/up-main/up-main/.memory/private/2026-11-24/chunks-listening-verified/`
  - s1–s7 为 `tts-verify` 3/3；s8 人耳确认 `error` 正常（SenseVoice 偏差）。
  - s5 用 **af_heart 0.7×** 避开 140 wpm 附近的前置 schwa 伪影；**不采用裁切**。
    旧口径 159 wpm，按语音跨度复测 **168 wpm**（唯一越界的一条，其余 7 条 139–149）。
- 仍待：批量材料生成接入 `tts-verify`、10 条 IELTS prompt 的 3/3 + phones 复测、TTS “And…” 起始问题。
