# MCP 工具契约（voice server）

> 服务名 `voice`，工具全名 `mcp__voice__<tool>`。
> 全部工具**所有返回都是同一个信封**，失败不抛协议错误，便于 Agent 统一处理：

```json
{ "ok": true,  "detail": { ... }, "error": null }
{ "ok": false, "detail": null,    "error": "人类可读的错误原因" }
```

工具清单（8 个）：

| 工具 | 用途 | 副作用 |
|---|---|---|
| [`assess`](#assess) | **评测**：转写 + 语流 + 词级差异 | 留证：asr.json + metrics.json + feedback.md |
| [`transcribe`](#transcribe) | 语音转文字（词级时间戳） | 留证：原始音频 + 16k 派生 + asr.json |
| [`synthesize`](#synthesize) | 文字转语音（示范音，可服务端播放） | 留证：wav + ref.json |
| [`record`](#record) | 麦克风录音（静音自动停） | 留证：16k wav + capture.json |
| [`play`](#play) | 播放 wav | 无 |
| [`prepare_audio`](#prepare_audio) | 音频体检/去静音 | 留证：16k wav |
| [`voices`](#voices) | 列出音色 | 无 |
| [`status`](#status) | 自检 | 无 |

---

## assess

口语训练的核心工具。一次调用完成：解码 → ASR → 语流分析 → 与参考文本的词级差异。

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `path` | string | ✅ | 绝对路径或 `"last"` |
| `reference` | string | | 参考文本（跟读目标句）。为空则只做语流分析 |
| `lang` | string | | `auto`/`en`/`zh`/`mixed` |
| `engine` | string | | 强制 `sense-voice`/`whisper` |
| `with_gop` | bool | | 同时跑音素级 GOP（需 `reference`；首次调用加载 1.2 GB 模型，约 1–2 s） |
| `baseline_tts` | bool | | 用 TTS 合成参考文本再跑一遍同一套 GOP，作为**对照基线**（区分"用户读错"与"模型分辨不出"） |
| `with_prosody` | bool | | 同时跑韵律：F0 轮廓 / 句末升降 / 平调 / 音域 / 重音候选 / 节奏 |
| `reference_audio` | string | | 参考音路径（TTS/真人示范）。提供时**自动开启韵律**，并返回 F0 形状 DTW 对比 |
| `bank_id` | string | | 素材库 id（如 `nce2/02/003`）：自动填 `reference`；`ref_audio=native` 时用**真人原声切片**当 GOP 基线与韵律参考（**优先于 TTS**） |
| `scene` | string | | 场景标签（仅外部文件新建会话时生效；本机 take 沿用原前缀） |

**留证位置（重要）**：`path` 指向本机 `sessions/<date>/<id>/takes/` 下的文件时，
`assess` **就地在那个 take 上写回**，不新建会话、不复制音频：

```text
sessions/<date>/<hhmmss>_drill/takes/
├── 001_drill.raw48000.wav   record 写的原始证据
├── 001_drill.16k.wav        派生（assess 优先吃这一份，不会覆写）
├── 001_drill.asr.json       assess 写
├── 001_drill.metrics.json   assess 写（重复评测时上一份留成 .metrics.prev.json）
└── 001_drill.md             assess 写的反馈骨架（已存在则不覆盖）
```

外部文件（录音机目录、`cases_audio`）仍走“新建 `HHMMSS_assess` 会话 + 导入副本”的老路径。
反查规则：`Store::session_for_take()` + `prefix_of_take_file()`（前缀 = 第一个点之前，且须以数字段开头）。
副作用：`last_take` 不再被 assess 推到副本上（仍指向 record 的 `.raw48000.wav`）。

返回 `detail` 关键字段：

```json
{
  "summary": {
    "text": "I think this is the straight time.",
    "accuracy": 0.857,
    "substitutions": 1, "deletions": 0, "insertions": 0,
    "speech_rate": 69.1, "articulation_rate": 161.3, "rate_unit": "wpm",
    "pause_count": 2, "pause_longest": 0.54,
    "word_boundary_breaks": 0, "filler_rate": 0.0, "repetitions": 0,
    "degraded": false, "issue_count": 1,
    "timestamp_alignment": "exact"
  },
  "issues": [{
    "kind": "substitution", "reference": "third", "reference_surface": "third", "said": "straight",
    "t0": 4.5, "t1": 5.04, "severity": "high",
    "similarity": 0.25, "phonetic_similarity": 0.40,
    "hint": "unclear",
    "note": "参考词 "third" → 识别为 "straight"（音形相似 40%、拼写相似 25%）：从文本无法判断是发音偏差还是 ASR 误听…"
  }],
  "flow": { "duration": 6.08, "speech_duration": 2.60, "speech_ratio": 0.43,
            "pauses": [{"t0":2.92,"t1":3.46,"dur":0.54}],
            "word_gaps": [], "fillers": [], "longest_run_words": 7 },
  "words": [{"w":"I","t0":1.86,"t1":2.28}],
  "text_agreement": 1.0,
  "limitations": ["源音频采样率 ≤16 kHz…", "SenseVoice…token 时间戳 ±0.5 s…", "尚无音素级对齐…"],
  "scorer_version": "p2.0-wordlevel",
  "evidence": { "src": "…", "norm_wav": "…", "metrics_json": "…", "feedback_md": "…" },
  "session": "2026-11-19/003545_assess", "prefix": "001_read_third"
}
```

**Agent 必须遵守的解读规则**（细节见 ADR-0006）：

1. `issues[].kind` 三态：`substitution`（替换，high）/ `omitted`（漏读，high）/ `extra`（多读，medium）。
2. `hint=likely_asr_miss`（出现在 `kind=omitted` 上）表示**被漏掉的词是短功能词**
   （is/a/the/of/to…），而 ASR（尤其 CTC 解码）吞掉这类弱读词非常常见。
   实测：同一段 48 kHz 录音，SenseVoice 漏掉 `is`，whisper 能听到。
   **此时绝对不能判用户漏读**，必须让用户只重读该短语确认，或 `engine="whisper"` 复核一次。
   该类型 severity 固定为 `medium`。
3. `summary.timestamp_alignment` 是时间戳质量，决定你能不能报秒数：
   - `exact` → 按 token 序号精确对齐，可以说"第 4.5 秒那个词"（但仍是 CTC token 级，±0.5 s）
   - `estimated` → 识别分词与规范化分词不一致（如 `rock'n'roll`、连字符），位置按比例估算，
     **只能说"第几个词"，不要报秒数**
   - `missing` → 无时间戳，只能报词
4. `issues[].reference_surface` 是参考文本里的**原始写法**（`I'm`、`3rd`）。
   反馈时优先用它——同一缩写展开出的多个词只会报 1 条问题，不会出现"漏读 i"+"漏读 am"两条。
5. `hint` 是**四态推断**，不是结论：
   - `likely_pronunciation` → 可以说"这里听起来是两个近音词，大概率发错了"
   - `likely_different_word` → 别说成发音问题，更像用词/记忆问题
   - `unclear` → **必须**说明"可能是发音问题，也可能是识别误差"，并提出重录或对照示范音
6. `limitations` 里出现的每一条都要反映到措辞上（例如 `band_limited` 时不要断言 /θ/ vs /s/）。
7. `accuracy` 只用于**纵向趋势对比**，不要换算成雅思分数对外承诺。
8. **规范化会吃掉两类“假错误”**（ADR-0006 补充）：
   - **英美拼写变体**：`theatre↔theater`、`colour↔color`、`realise↔realize` 等（显式表 `US_UK_VARIANTS`，按同词处理）。
     教材文本常是英式、音轨是美音，不归并就会把标准发音报成“读错了”。
   - **ASR 伪缩略**：`did't`/`could't` 这类不存在的词形（识别引擎把 `did not` 糊成一个 token），
     自动展开成 `did + not`，不得报成“漏读 + 替换”两条错误。合法缩略仍走缩写表。
   所以：**看到 `accuracy` 下降时，先看 `issues` 是否集中在导语/拼写变体/功能词**，再决定要不要当真。
9. **停顿要看 `flow.pauses`（VAD，以音频为真），不是 `flow.word_gaps`（ASR 时间戳）。**
   两者矛盾时（例：`pause_count 0` 但 `word_gaps` 出现 2.3 s 空隙），说明 **ASR 漏词造成了假空隙**，
   不得说“你停了 2.3 秒”；只能提示“这段识别有漏词”。`word_gaps` 仅用于比较词界粘连的相对趋势。

---

## transcribe

语音转文字。**输入永远是文件路径**（`"last"` 表示最近一次 take），服务内部统一成 16 kHz mono。

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `path` | string | ✅ | 绝对路径，或 `"last"`。支持 `wav/m4a/mp3/flac/ogg/aac`（Win11 录音机 `.m4a` 直读） |
| `lang` | string | | `auto`(默认) / `en` / `zh` / `mixed` |
| `engine` | string | | 强制 `sense-voice` / `whisper`；默认按 `lang` 路由 |
| `scene` | string | | 场景标签，进入留证文件名与 manifest，如 `read_third` |

返回 `detail`：

```json
{
  "text": "I think this is the straight time.",
  "lang": "en", "engine": "sense-voice", "duration": 6.08,
  "infer_ms": 82.3, "degraded": false,
  "words":  [{ "w": "third", "t0": 4.5,  "t1": 5.04 }],
  "tokens": [{ "t": "third", "t0": 4.5 }],
  "audio": {
    "src": "...001_read_third.m4a", "norm_wav": "...001_read_third.16k.wav",
    "source_sr": 16000, "band_limited": true,
    "peak_dbfs": -0.25, "silent_ratio": 0.64
  },
  "session": "2026-11-19/002508_dictate", "prefix": "001_read_third"
}
```

**给 Agent 的解读要点**

- `lang` 为 `auto` 时本次会回填实际判定结果（`zh`/`en`/`mixed`）。
- `degraded: true` 表示时间戳缺失或引擎回退 → **不要做音素级结论**，只能谈整句/词级。
- `audio.band_limited: true` 表示源音频采样率 ≤16 kHz（如 Win11 录音机），6–8 kHz 摩擦音细节有限，
  判断 /θ/ vs /s/ 这类区分时置信度要打折。
- `silent_ratio` 很高（>0.6）说明录音里留白多，属正常；若 >0.95 基本是空录音。

---

## synthesize

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `text` | string | ✅ | 待合成文本。**一段文本只能一种语言**：中英混说会被**拒答**（`ok:false`，提示拆成两次调用） |
| `voice` | string | | 音色名（`am_michael`/`am_fenrir`/`bf_emma`/`af_heart`/`zf_xiaobei`…）或 sid 数字，**默认 `am_michael`（男声）** |
| `speed` | number | | **显式倍率**（高级用法）；不传时按目标 **140 wpm**（雅思区间 130–160）自动校准。传了就以倍率为准 |
| `play` | bool | | `true` 时由**服务端**播放（Pi 终端放不出声音，必须用这个） |
| `slow_too` | bool | | 同时生成一对：`label=slow`(**110 wpm**) + `label=norm`(**140 wpm**)；显式 `speed` 时 slow 取 `speed×0.75` |
| `scene` | string | | 场景标签 |

**文本前端（2026-11-27 起，ADR-0011）**：英文走**开放词表 G2P**（`cmudict+形态规则+数字`，
项目 lexicon 只作**高优先级覆盖层**），音素经 `phoneme_codepoints` 注入，**不再查引擎 closed lexicon**
（sherpa-onnx 2.0.x 已移除 espeak-ng，闭集词表会静默丢词，见 `docs/audit/tts-2026-11-27-frontend.md`）。

- 覆盖不了（未见词根、生造专名）→ `ok:false` 并列出词，**不输出缺词音频**；
  给该词补发音：`$VOICE_HOME/tts-lexicon.txt` 写 `word 音素…`。
- 中文走 `lexicon-zh`（原路径）；中英混说**默认拒答**（单音色必然降调 + 拼接点停顿），
  内部实验：`VOICE_ALLOW_MIXED=1` / `voice-cli say --allow-mixed`。
- 前端标识（审计/留证用）：`g2p-ipa/1.0[cmudict+overlay]+lexicon-zh`；`tts-audit` 每行带 `frontend` 字段，
  **跨版本 OOV/通过率曲线不可直接比较**。

返回 `detail`：`voice` / `sid` / `sample_rate`(24000) /
`files[]{label,speed,wpm,target_wpm,path,duration,speech_duration,onset_gap_ms,gen_ms,rtf}` / `session` / `prefix` /
`phonemes`（本次实际送进模型的音素串，留证/回归用；中文路径为 `null`）

- `wpm` 按**语音跨度**算（去掉首尾静音），不再按文件总时长（后者系统性低估，目标 140 会量到 128）；
  单位是“英文词 + 中文字”（混说句只数 ASCII 词会把 20.7 当 146，已修）
- **已知（D54）**：语速校准的候选排序已改为“离目标带宽的连续距离优先”，实测 140 目标落在
  124–143 wpm；个别短句仍可能掉到 130 下沿以下（如 `Apple's stock fell again.`）。**不要把 `target_wpm` 当保证**
- `onset_gap_ms`：`null` 表示没有起始伪影；非 `null`（如 `40`）表示“先出一小段音 → 停顿 → 才进正文”，
  实测 af_heart 在某些速度点会被双 ASR 听成开头多一个 “And”。这种音频**不要直接当示范音/听力材料**

合成逻辑集中在 `voice_core::engine::demo_targets` + `TtsEngine::synthesize_at_wpm`（MCP 与 CLI 共用，有单测）：
- 默认：`slow_too=false` → `[norm @ 140 wpm]`；`slow_too=true` → `[(slow,110), (norm,140)]`
- 校准：先按 1.0 生成测速，再算 `speed = target/measured`；偏差 >3% 继续校正，**保留最优候选**而不是最后一次
  （Kokoro 的 speed→wpm 不连续：实测 0.78→132 wpm、0.815→153 wpm，只取最后一次会来回震荡）
- 起始伪影规避：若最优候选仍有 `onset_gap_ms`，自动换速度（×1.04/0.96/1.08/0.92）重试；**不裁音频**（audit §11）
- 短文本（<3 词）与中文不做 WPM 校准（WPM 不可靠），返回 `target_wpm=null`
- `speed=1.0` 的 Kokoro 实测约 **220 wpm**，不要当“常速”；旧 `0.75` 也约 **160–200 wpm**
- `play=true` 只播 `files[0]`（即 slow 版）；要听常速请用 `play(path=<label=norm 的 path>)`

**规则（重要）**：中文讲解与英文示范**必须拆成两次调用**（Kokoro 是单语模型）。
中文讲解默认只出文字、不必合成音频。

---

## record

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `max_sec` | number | | 录音上限，默认 30 |
| `silence_sec` | number | | 静音多久自动停止，默认 **2.0**（配置 `record_silence_sec`） |
| `start_timeout_sec` | number | | 开始前等说话的超时，默认 8，最长建议 20（自由说/长独白） |
| `transcribe` | bool | | 录完直接转写 |
| `lang` | string | | 转写语言 |
| `scene` | string | | 场景标签 |
| `device` | string | | 输入设备名子串（蓝牙耳机用 `"Hands-Free"`）；不填用配置 `mic` 或系统默认 |

返回 `detail`：`wav`（16k 派生）/ `src`（设备采样率原始证据，如 `.raw48000.wav`）/ `duration` / `auto_stopped` / `device` / `device_sr` / `transcript` / `session` / `prefix`

> 自动停止阈值**开场自适应**（0.4 s 噪声 p95 + 8 dB）。`auto_stopped=false` 且时长≈max_sec 时说明一直没停顿；一直没说话会直接返回 `ok:false`。

**失败时**（采到稀疏零值/过静音）会返回明确诊断，而不是把假音频当正常录音：

```json
{ "ok": false, "error": "设备 \"耳机 (…)\" 采到的音频接近静音（峰值 -40 dBFS），不是真实声音。
常见原因与处理：1) 蓝牙耳机…选 Hands-Free 端点…" }
```

> 前提：系统必须有可用输入设备。`status.mic` 为 `null` 时此工具会明确报错。
> 长独白（>60 s，如雅思 Part 2）建议改用录音机 + `voice-cli watch`，不要走这个阻塞式工具。

---

## play

| 参数 | 类型 | 必填 |
|---|---|---|
| `path` | string | ✅ 绝对路径或 `"last"` |

返回 `{"played": "<路径>"}`。

---

## prepare_audio

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `path` | string | ✅ | 绝对路径或 `"last"` |
| `trim_silence` | bool | | 默认 `true`，用 Silero VAD 去首尾静音 |

返回 `detail`：`src` / `norm_wav` / `trimmed_offset` / `info{source_sr,source_channels,duration,peak_dbfs,rms_dbfs,clipped,silent_ratio,band_limited}` / `session`

---

## voices

返回 `{count: 54, sample_rate: 24000, default_sid, voices:[{sid,name,lang,gender}]}`。
音色顺序取自 hexgrad 官方 `voices` 字典序（已用 `af_heart → sid=3` 验证）。
`voice-cli tts-probe` 可逐个人耳校准。

---

## status

返回：版本、sherpa-onnx 版本、模型是否齐全、**引擎是否已加载**、`mic`（麦克风名，`null` 表示没有）、
监视目录是否存在、manifest 条数、`last_take`。

**引擎是惰性加载**：第一次调用 `transcribe`/`synthesize`/`record` 时才加载（约 2.5 s），
之后常驻。`engines_loaded: false` 不代表故障。

---

## 留证布局（每次调用都会写）

```text
$VOICE_HOME (默认 D:/voice_data)
├── manifest.jsonl        # 追加索引：ts/session/seq/scene/src/lang/engine/dur/text/degraded/feedback_ref
├── last_take             # 最近一次 take 的原始音频路径（path:"last" 指向这里）
├── watch_state.json      # 目录监听的已处理文件（大小指纹）
└── sessions/<YYYY-MM-DD>/<HHMMSS>_<kind>/
    ├── session.json
    └── takes/
        ├── 001_<scene>.m4a           ← 原始输入，永不修改
        ├── 001_<scene>.raw48000.wav  ← 设备采样率的原始证据（record）
        ├── 001_<scene>.raw24000.wav  ← 非 16 kHz 的 wav 输入被 align 成这个形状（assess/prepare）
        ├── 001_<scene>.16k.wav       ← 派生（可删可重建）
        ├── 001_<scene>.asr.json      ← 转写全文 + words[] + tokens[]
        ├── 001_<scene>.audio.json    ← 解码后的声学指标
        ├── 001_<scene>.capture.json  ← 采集元信息 + 能量包络（record）
        ├── 001_<scene>.metrics.json  ← 评测指标（assess；重复评测时上一份为 .metrics.prev.json）
        ├── 001_<scene>.md            ← 反馈骨架（assess 建，Agent 填）
        ├── 001_<scene>.ref_norm.wav  ← 示范音（默认 140 wpm）
        ├── 001_<scene>.ref_slow.wav  ← 示范音（默认 110 wpm）
        └── 001_<scene>.ref_*.json    ← text/voice/speed/wpm/target_wpm/model
```

`manifest.jsonl` 可直接 `grep`，也是 `progress` 趋势分析的数据源。

## assess: `with_gop=true` 的返回

返回 `gop` 字段（首屏只回最可疑的 3 个词 + 5 个音素，避免诱导 LLM 逐个下结论）：

```json
{"gop": {
  "version": "gop-sf-0.1", "phones_total": 18, "oov": [], "unmapped": [],
  "words_worst": [{"word": "their", "gop_mean": -0.60, "t0_ms": 1170, "t1_ms": 1430,
                   "worst_phone": "DH", "worst_z": -1.42}],
  "worst": [{"phone": "DH", "heard": "d", "t0_ms": 1200, "t1_ms": 1220,
             "gop": -2.89, "z": -1.60,
             "confusion": "TH→s", "confusion_reportable": true}]
}}
```

- `words_worst[]` 是**对外唯一允许下结论的粒度**（词级 mean-GOP）。完整词表不随 `assess` 返回；需要全量请用 CLI `gop`。
- `worst[]` 只是**句内排序**，音素级禁止断言。
- `confusion` 形如 `TH→s`；`confusion_reportable` 当前只对 TH 系列为 `true`（标定 P 0.50 / R 0.67），其余（`R→l` / `Z→s`）只作内部标记。

**精度事实（必须落到措辞）**：speechocean762 未见说话人标定——
句级 ρ≈0.67；**词级 AUROC 0.887、FP=10% 处精确率 0.457**；
音素级 AUROC 0.769、精确率 0.170（基率 7.5% ⇒ 上限 0.45）。
因此 `assess` 会在 `limitations` 追加 `gop_precision_low`；Skill 只能把音素级当排序/趋势信号。
带宽不足（`band_limited`）或缺 `reference` 时不跑 GOP，并明确写进 `limitations`。

## 音频质量闸门（`snr_db` / `peak_dbfs` / `degraded`）

`assess` 返回 `snr_db`（20 ms 帧 RMS 的 **p90−p10**），`record` 返回 `snr_db` / `peak_dbfs` / `level_warning`。

- 阈值 `SNR_MIN_DB = 13.0`（实测分界：可用录音 14.3–19.5 dB，ASR 出碎片的录音 11.0–11.5 dB）
- 另一个方向也会坏：**输入过载（削波）**。`clipped / 总采样 > 0.1%`（或峰值 > −1 dBFS）→ `degraded`，
  同样不跑 GOP。实测：用户把输入音量调高后取到 1.67% 削波、峰值 0 dBFS，ASR 仍全对但音素后验已不可信
- 第三个方向：**绝对电平过低**。`peak_dbfs < −30 dBFS` → `degraded`。
  实测同一句：peak −24.6 → ASR acc 0.83；peak −33.5 → ASR 只输出 "The."（acc 0.00），而 SNR 仍有 17.9 dB
- 短录音（<4 s）的 SNR 口径不同：用 `top-5% 帧均值 − p10`（单词 drill 里语音只占 10–25% 帧）
- **短语音豁免（D47）**：VAD 语音跨度 < 3 s 且 `peak_dbfs ≥ −25` 时，`snr_db` 只写进 `limitations`
  （“SNR 会被低估，仅供参考”），**不置 `degraded`、照常跑 GOP**——单词 drill 的静音帧太少，
  p10 会落在轻声语音上（实测 1.8 s `word` drill 只有 12.6 dB 却被整条判死）
- `< 13 dB` → `degraded = true`，`limitations` 追加明确原因，**不跑 GOP**

## 文本可用性闸门（`text_agreement`，D49）

声学过闸（电平/SNR/削波都 OK）不等于 ASR 听清了。`assess` 在**给了 `reference` 且非中文**时会用另一个引擎
再转一遍，返回 `text_agreement`（词级 LCS 一致率 `2·|LCS|/(|a|+|b|)`，0–1）：

- `≥ 0.7`：正常出词级 / 音素级结论；
- `< 0.7`：`degraded = true`，`limitations` 写明“ASR 自身没听清”，本条**只保留语流与韵律**；
- 第二引擎不可用（未下载模型等）不降级，只追加 limitation。
- 为什么用 p90 而不是 p95：蓝牙 AGC 的尖峰帧会把 p95 抬高，两条作废录音会算出 13.0/15.4 dB 的假高 SNR
- 实测梯度（TTS + 噪声）：名义 30/20/12/8/4 dB → 测得 30.1/23.8/16.9/14.0/11.3 dB，
  最后两档 ASR 仍全对、只有 4 dB 档被拦截 —— 说明该闸门卡在"ASR 开始失效"的位置

## GOP 对照基线（`baseline_tts` / `bank_id`）

**基线优先级（2026-11-25 修正，D52）**：

| 情况 | 基线 | `gop_baseline_source` |
|---|---|---|
| 传了 `bank_id` 且该素材是 `ref_audio=native` | 素材库的**真人原声切片** | `native` |
| 只有 `baseline_tts=true`（或 `ref_audio=tts` 的 prompt） | TTS 合成（默认 am_michael / 140 wpm） | `tts` |
| 都没传 | 无基线 | 字段缺失 |

返回字段：

| 字段 | 含义 |
|---|---|
| `gop_baseline` | 基线的 GOP 结果（`version` + 全量 `words[]`） |
| `gop_baseline_valid` | 自检是否可用：① 参考音回读词序列必须与文本一致；② **基线自身音素不能读坏**（最差词级 GOP ≥ −1.5） |
| `gop_baseline_source` | `native` / `tts` |

- `valid=true`：可以算 `用户 − 基线`；
- `valid=false`：**禁止** Δ，`limitations` 写明是内容不一致还是音素读坏（D51/D52）；
- **`source=tts` 不得用于“语调/重音是否地道”的结论**：合成音的语调不是人标准，
  只能说“与这段合成示范的差异”；只有 `source=native` 才能说“和真人原声的差异”。
- 旧实现（2026-11-25 之前）一律用 TTS 当基线且只做内容自检 → 音素读坏但词读对的基线会被当标准。
- **已知偏差（native 基线）**：素材库原生切片是 16 kHz（教材 MP3 带宽），用户录音通常是 48 kHz；
  基线与用户音频的**带宽不同**，摩擦音（/s/ /θ/ /f/）的 GOP 会系统性偏低或偏高。
  Δ 仍比 TTS 基线可信（内容与音素都是真人），但**不要用它判摩擦音的细微差别**；
  要严格对比时，把用户音频也降采样到 16 kHz 再跑一次（`--engine`/重采样由调用方负责）。

动机：GOP 绝对值带模型偏好。实测同一个音素 CTC 模型读 `their` 也只有 −0.36，
所以"用户 −基线"才是可解释的量。

实测（`Why do the children thank their mother?`）：

| 词 | 用户 | TTS 基线 | Δ |
|---|---|---|---|
| why | 2.24 | 6.51 | **−4.27** |
| children | 0.58 | 4.18 | −3.60 |
| their | −0.60 | −0.36 | −0.24（都是低分 → 模型偏好，不怪用户） |

规则：基线 < 0.30 的词直接不报；Δ ≤ −3 且基线 ≥ 4 才算"低于示范"；其余不报。
声音可用 `VOICE_GOP_BASELINE_VOICE` 指定，**默认 `am_michael`**（2026-11-25 由 `af_heart` 改来：
af_heart 在 140 wpm 附近有起始伪影，会让基线自检误判；改后 Δ 与旧记录不可严格横比）。

## 韵律（`with_prosody=true` / `reference_audio`）

纯 Rust（YIN + 自相关），不引入 sidecar。默认关闭；提供 `reference_audio` 时自动开启。

```json
{"prosody": {
  "version": "prosody-0.1", "voiced_frames": 1364, "voiced_ratio": 0.263,
  "f0_median_hz": 116.3,
  "f0_std_st": 3.31, "f0_p10_st": -2.41, "f0_p90_st": 1.72, "range_st": 12.61,
  "flat": false, "unreliable": false,
  "final_slope_st_per_s": -0.06, "final_delta_st": -0.38, "final_direction": "flat",
  "rhythm": {"word_duration_cv": 0.43, "stress_ratio": 3.11, "mean_word_dur": 0.366},
  "word_count": 82,
  "stress_top": [{"word": "management.", "t0": 20.97, "t1": 21.81, "dur": 0.84,
                  "f0_peak_st": 6.22, "f0_mean_st": -0.54, "dur_z": 2.98,
                  "energy_z": -0.92, "salience": 1.62}],
  "reference": {"source": "native", "f0_median_hz": 205.8, "dtw_cost_st": 0.11, "points": 50}
}}
```

口径：
- 半音一律相对**说话人自己的** F0 中位数（`12*log2(f/f0_median)`），因此男/女、高/低音可比；
  `f0_median_hz` 只是记录基线，不是分数。
- `final_direction` 只按 `final_delta_st` 判：`≥ +1.0 st` rise，`≤ −1.0 st` fall，其余 flat。
- `flat=true` ⇔ `f0_std_st < 1.5`（全句起伏过小）。
- `unreliable=true` ⇔ `f0_std_st > 6` 或 `range_st > 18`（常见于八度跳变/噪声）：
  此时 `final_direction` 只能作参考，**不得**当结论说给用户。
- `stress_top` 是**重音候选排序**：`salience = 0.5*dur_z + 0.3*(f0_peak_st/6) + 0.2*energy_z`。
  词时长来自 ASR（±0.5 s 量级），只能当"大概第几个词"；**不是"你重音放错了"的判决**。
- `energy_z` 在蓝牙 AGC 设备上不可信（D36），只作第 3 特征；反馈中不要拿它说"重音太轻"。
- `reference.source` 是参考音来源：`native`（真人原声，`bank_id` 自动带来）、`tts`
  （`synthesize` 产出的 `.ref_norm/.ref_slow`，按文件名推断）、`unknown`（外部文件）。
  **`tts` 参考只能说“与这段合成示范的形状差多少”，不能说“语调不对”**（D52）。
- `reference.dtw_cost_st` 是**时间归一化后**的半音形状 DTW 平均代价（越小越像）。
  用户与参考各自减掉自己的中位数，所以音高/性别/参考音时长差异都被消掉。
- `degraded=true`、`snr_db < 13`、浊音帧不足 → 不返回 `prosody`，`limitations` 说明原因。
- 无词级时间戳 → 只给句级指标，`stress_top` 为空并追加 limitation。

**给 Agent 的措辞规则**：只报数值 + 位置（"末段下降了 3.9 个半音"），
不说"语调不好 / 重音错了"；音域窄可以说"起伏偏小"，但要先给 `range_st` 数字。
