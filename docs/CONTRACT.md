# MCP 工具契约（voice server）

> 服务名 `voice`，工具全名 `mcp__voice__<tool>`。
> 全部工具**所有返回都是同一个信封**，失败不抛协议错误，便于 Agent 统一处理：

```json
{ "ok": true,  "detail": { ... }, "error": null }
{ "ok": false, "detail": null,    "error": "人类可读的错误原因" }
```

工具清单（7 个）：

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
| `scene` | string | | 场景标签 |

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
  "limitations": ["源音频采样率 ≤16 kHz…", "SenseVoice…token 时间戳 ±0.5 s…", "尚无音素级对齐…"],
  "scorer_version": "p2.0-wordlevel",
  "evidence": { "src": "…", "norm_wav": "…", "metrics_json": "…", "feedback_md": "…" },
  "session": "2026-11-19/003545_assess", "prefix": "001_read_third"
}
```

**Agent 必须遵守的解读规则**（细节见 ADR-0006）：

1. `issues[].kind` 三态：`substitution`（替换，high）/ `omitted`（漏读，high）/ `extra`（多读，medium）。
2. `summary.timestamp_alignment` 是时间戳质量，决定你能不能报秒数：
   - `exact` → 按 token 序号精确对齐，可以说"第 4.5 秒那个词"（但仍是 CTC token 级，±0.5 s）
   - `estimated` → 识别分词与规范化分词不一致（如 `rock'n'roll`、连字符），位置按比例估算，
     **只能说"第几个词"，不要报秒数**
   - `missing` → 无时间戳，只能报词
3. `issues[].reference_surface` 是参考文本里的**原始写法**（`I'm`、`3rd`）。
   反馈时优先用它——同一缩写展开出的多个词只会报 1 条问题，不会出现"漏读 i"+"漏读 am"两条。
4. `hint` 是**三态推断**，不是结论：
   - `likely_pronunciation` → 可以说"这里听起来是两个近音词，大概率发错了"
   - `likely_different_word` → 别说成发音问题，更像用词/记忆问题
   - `unclear` → **必须**说明"可能是发音问题，也可能是识别误差"，并提出重录或对照示范音
5. `limitations` 里出现的每一条都要反映到措辞上（例如 `band_limited` 时不要断言 /θ/ vs /s/）。
6. `accuracy` 只用于**纵向趋势对比**，不要换算成雅思分数对外承诺。

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
| `text` | string | ✅ | 待合成文本。**一段文本只能一种语言** |
| `voice` | string | | 音色名（`af_heart`/`bf_emma`/`am_michael`/`zf_xiaobei`…）或 sid 数字，默认 `af_heart` |
| `speed` | number | | 语速，默认 1.0；跟读示范建议 0.75 |
| `play` | bool | | `true` 时由**服务端**播放（Pi 终端放不出声音，必须用这个） |
| `slow_too` | bool | | 同时生成 0.75x 慢速版 |
| `scene` | string | | 场景标签 |

返回 `detail`：`voice` / `sid` / `sample_rate`(24000) / `files[]{label,speed,path,duration,gen_ms,rtf}` / `session` / `prefix`

**规则（重要）**：中文讲解与英文示范**必须拆成两次调用**（Kokoro 是单语模型）。
中文讲解默认只出文字、不必合成音频。

---

## record

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `max_sec` | number | | 录音上限，默认 30 |
| `silence_sec` | number | | 静音多久自动停止，默认 1.0 |
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
        ├── 001_<scene>.16k.wav       ← 派生（可删可重建）
        ├── 001_<scene>.asr.json      ← 转写全文 + words[] + tokens[]
        ├── 001_<scene>.audio.json    ← 解码后的声学指标
        ├── 001_<scene>.capture.json  ← 采集元信息 + 能量包络（record）
        ├── 001_<scene>.ref_norm.wav  ← 示范音（1.0x）
        ├── 001_<scene>.ref_slow.wav  ← 示范音（0.75x）
        └── 001_<scene>.ref_*.json    ← text/voice/speed/model
```

`manifest.jsonl` 可直接 `grep`，也是 `progress` 趋势分析的数据源。
