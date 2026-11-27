# TTS 文本前端改造：开放词表 G2P → phoneme_codepoints（2026-11-27）

> 触发事故：`Day 12 复习块` 的 r1/r3 缺词（`client's`、`Where's` 被整词吞掉）。
> 根因不是声学模型，是**文本前端闭集**：sherpa-onnx v2.0.0 起删除 espeak-ng 兜底
> （二进制里写死 `espeak-ng is no longer used`），Kokoro 只剩 `lexicon` 查表，
> 词表外 token 被静默跳过。

## 一、改了什么

| 层 | 变更 |
| :--- | :--- |
| 文本前端 | 新增 `crates/voice-core/src/engine/g2p_ipa.rs`：`G2pBackend` trait + `CmuBackend`（CMUdict + 形态规则 + 数字 + overlay 覆盖层）+ `SidecarBackend`（接 misaki / espeak-ng / piper_phonemize） |
| 注入方式 | 英文**一律**走 `GenerationConfig::phoneme_codepoints`，不再查引擎词表；`KokoroTokenMap` 负责 IPA → 码点 + 合成前字符集校验 |
| 引擎词表 | 只剩 `lexicon-zh.txt`（中文路径不变）+ overlay；英文 lexicon 降级为 G2P 的**高优先级覆盖层**（命中保音质，未命中走规则保覆盖） |
| 失败语义 | G2P 覆盖不了 → **直接报错**（`oov` 列出词），不再输出缺词音频；`oov_words()` 对英文改为返回 G2P 的 OOV |
| 证据 | `Synthesized.phonemes` 记录实际送进模型的音素串；`TtsEngine::phonemize_preview()` 供不合成时的静态断言 |
| 门禁 | 新增 `voice-cli tts-regress <corpus.jsonl>`：逐句合成 → 双 ASR 回读 → 逐词比对，漏词即退出码 2 |

中文：`lexicon-zh` 路径保留（Bopomofo 需要 jieba/pypinyin，属后续工作），走 `SidecarBackend` 可以立刻接 misaki-zh。

## 二、实测（`cases/tts-word-loss.jsonl`，11 条，`--speed 0.55`）

旧路径（`lexicon`）与新前端的对照，同一音色 `am_michael`、同一脚本、同一双 ASR：

| 句 | 旧路径（闭集 lexicon） | 新前端（G2P → 码点） |
| :--- | :--- | :--- |
| `The client's report isn't ready…` | 丢 `client's`（`phone_min −4.99`） | ✅ 逐字一致 |
| `Where's the log file…` | 丢 `Where's`（−3.97） | ✅ |
| `The teacher's book…` | 丢 `teacher's`（−4.99） | ✅ |
| `Apple's stock fell again.` | 丢 `Apple's`（−5.03） | ✅ |
| `What's going on?` | 丢 `What's`（−3.97） | ✅ |
| `today's meeting…` | 丢 `today's`（−4.11） | ✅ |
| `Sarah's laptop isn't working.` | 丢 `Sarah's`（−5.99） | ✅ |
| `Zorbly's report is late.`（生造专名） | 丢 2 词 | 明确报 OOV（`expect=oov` 用例） |

`tts-regress` 现状：**13/13 pass，退出码 0**（11 条英文判别句 + 2 条中英混说结构用例）。

### 中英混说（新增边界）

英文强制码点化、中文走 `lexicon-zh`，两套逻辑在同一个 `TtsEngine` 里，
所以按语言**分段**合成再拼接（`g2p_ipa::split_segments`）：中文段不进 G2P，英文段不进词表。

- 用例 `mx01`（`这个 API 返回 404 error，我们明天再查。`）、`mx02`（`Sarah's laptop 上那个 build 失败了。`）
- 门禁口径 `check=structure`：只锁结构（英文段必须拿到音素、中文不得漏进 G2P、合成不报错）。
- **为什么不判混说音质**：Kokoro 没有双语音色——英文 voice 读中文、中文 voice 读英文都会跑调
  （实测 `zf_xiaobei` 读 `Sarah's laptop` → “exse laptop…”）。这是 voice 选型问题，
  与本次前端改造无关；要判混说音质需接 misaki-zh。

## 三、踩出来的四条硬规则（都写进代码注释了）

1. **双元音必须映射成 Kokoro 的单字母符号**：`aɪ→I`、`eɪ→A`、`oʊ→O`、`aʊ→W`、`ɔɪ→Y`。
   拆成两个 IPA 字符会被当成两个音素（`Friday` → "fra-ee-di" → ASR 听成 "fried"）。
2. **词内不能有空格**：按 lexicon 那种“每个音素一个空格”喂码点会崩
   （`Apple stock fee El again`，丢词 3–4 个）。
3. **空格也不能作为 token 发出去**：`tokens.txt` 里 `  16` 是符号但不是静音，
   发出去会把相邻词糊成一个（`and they`→`then they`、`isn't ready`→`tizen dready`）。
   词边界交给音素序列本身。（这一步让门禁从 10/11 变成 11/11）
4. **lexicon 覆盖层要美音优先**：先 `lexicon-us-en` 后 `lexicon-gb-en`（后者只补空缺）。
   反了会把 `last` 读成 `/lɑːst/`（听成 "lost"）、`on` 读成 `/ɒn/`，与 `am_michael` 不匹配。
5. 央元音按重音分：`AH0→ə`、`AH1/2→ʌ`；`ER0→əɹ`、`ER1/2→ɜɹ`。全映射成 ʌ 会让
   `the`→ðʌ、`client's`→klˈaɪʌnts、`aren't`→ˈɑɹʌnt 全部读歪。

## 四、CI 接入

**结构层（无需模型，随 `cargo test` 进 CI）**：`g2p_ipa` 有 15 条单测，
覆盖“所有格/wh-clitic 不得消失”“词数守恒（每个词要么有音素要么进 OOV）”
“未登录词根必须报 OOV”“`'s` 清浊规则”“双元音映射”“lexicon 覆盖层”“码点不是 token id”。

**行为层（需模型，本地/有模型的环境跑）**：

```powershell
voice-cli tts-regress cases/tts-word-loss.jsonl --speed 0.55 --out report.jsonl
# 任何一句两个引擎同时丢词 → 退出码 2；单引擎丢只报告（SenseVoice 会自己听错）
```

判定口径：
- `g2p_oov` 非空 → 失败（有词没拿到音素）；
- `lost_both`（两引擎同时丢）非空 → 失败；
- `phone_min_gop` 仅作报告：新前端沿用 lexicon 的发音惯例（`ði`、`ᵊ`、闪音 `ɾ`、次重音 `ˌ`），
  与 GOP 参考（CMUdict ARPAbet）不是同一套标注，负分不等于读错。

## 五、未完成 / 已知问题

1. **Piper A/B 第三臂仍缺权重**：本机无外网（`curl/pip` 全超时），`C:\voice_models` 下无 Piper/VITS 模型，
   也没有 espeak-ng 可执行。`tts-ab --engine piper --model-dir <目录> --mode g2p` 已就绪，
   模型到位即可跑（需要 `model.onnx` + `tokens.txt`，有 `lexicon.txt` 更好）。
   ⚠️ 前提认知：v2.0.1 里 Piper 同样是 closed-lexicon 前端，**换引擎不换前端不解决问题**。
2. **语速标定散布**：`synthesize_at_wpm` 在个别短句上落到 96–190 wpm（越界）。
   与新前端无关（旧路径同样存在），但会让“140 wpm”这个承诺不可靠 → 建议单独一轮修。
3. **生造专名仍需 LTS**：`Zorbly's` 这类词根不在 CMUdict 的词，现在会**报错**（不会静默丢），
   要真正读出来需要 espeak/piper_phonemize 兜底（`SidecarBackend` 已留口）。
4. **SenseVoice 噪声**：`client's report`→`lions report Tan`、`sarah's laptop` 整句重复，
   单引擎丢词不作为门禁失败依据（判定用“双引擎同错”）。
5. **`tts-audit` 的 OOV 列语义变了**：英文现在报 G2P 的 OOV（生造词会 reject）。
   **已加版本标记**：每行与汇总都有 `frontend` 字段
   （`g2p-ipa/1.0[cmudict+overlay]+lexicon-zh`），跨版本曲线不可直接比较，
   但不会再被误读成“OOV 暴增”。
6. **Piper 已从评测计划移出**：v2.x 里 Piper 同样是 closed-lexicon 前端，
   不能替代本次前端改造；`tts-ab --engine piper --mode g2p` 代码保留，可随时跑纯声学对比。


---

## 六、听测与标定诊断（2026-11-27 晚，按 `MCP 重启前听测验收清单` 执行）

### 6.1 材料（都已播放试听）

| 标签 | 文本 | 音色 | speed | 时长 |
| :--- | :--- | :--- | ---: | ---: |
| mx01_zh | 这个 API 返回 404 error，我们明天再查。 | zf_xiaobei | 0.55 | 9.18 s |
| mx01_en | 同上 | am_michael | 0.55 | 11.03 s |
| mx02_zh | Sarah's laptop 上那个 build 失败了。 | zf_xiaobei | 0.55 | 5.57 s |
| mx02_en | 同上 | am_michael | 0.55 | 8.09 s |
| reg_client | The client's report isn't ready, but we'll send it tomorrow. | am_michael | 0.55 | 5.56 s |
| reg_where | Where's the log file that we saw last Friday? | am_michael | 0.55 | 4.60 s |

### 6.2 客观指标（拼接点）

- mx01_zh：内部静音 8 段，最长 520 ms；拼接点电平跳变 +10.2 / +19.3 / +7.0 dB。
- mx02_zh：内部静音 6 段，最长 320 ms；跳变 +5.4 / +6.6 / +9.6 dB。
- mx02_en：内部静音 9 段，最长 250 ms；跳变最大 +14.0 dB。
- 纯英文回归样本（reg_client / reg_where）：内部静音 ≤ 290 ms，跳变 ≤ ±11 dB，与单段合成同量级（属正常词间停顿）。

**结论**：混说句的拼接点确有可闻的停顿（0.2–0.5 s）与电平跳变；纯英文路径没有新增异常。

**处置（按听测清单的三分支）**：混说落在“拼接点不可懂”这一档 →
**产品入口改为默认拒答**（`bail!` + 提示拆成两次调用），内部实验用
`VOICE_ALLOW_MIXED=1` / `voice-cli say --allow-mixed` 放行；
回归用例 `mx01/mx02` 改成 `expect=reject_mixed`（必须拒答，且英文段音素仍可用）。
理由：英语教学材料本身不用混说，但 MCP 是产品入口，不该输出已知降级的音频。

### 6.3 数字读法（清单里的 404 项）

已修：`read_number()` 按上下文分三种读法——
`404 error` → four oh four（逐位，0 读 oh）、`2026` → twenty twenty six（年份成对）、`140 wpm` → one hundred forty（基数）。
门禁侧用 `number_candidates()` 做多候选匹配，避免“读法不同”被误判成漏词。

### 6.4 D54 诊断：不是尺子错，是选候选的键错

同一文本扫 speed 0.45→1.0（wl04：71.6 → 190.5 wpm）**单调、无跳变**，说明 `length_scale` 正常。
问题在 `synthesize_at_wpm` 的 `rank()`：

- 旧键 `(off_band, artifact, dist)`，`off_band` 是 0/1 →
  `190 wpm` 与 `128 wpm` **并列**（都算越界），第二键“无起始伪影”把 190 wpm 抬成最优 →
  循环提前收敛在越界候选上。
- 新键 `(excess, artifact, dist)`，`excess = max(0, |wpm−target| − target×6%)` 是连续量。
- 修复后：wl06 142.0 / wl08 136.1 / wl01 139.9 / wl10 140.5（wl04 残留 124.4，见 D54 残留项）。

另一个**确实**是尺子错的地方：混说句 wpm 只数 ASCII 词 →
`这个 API 返回 404 error，我们明天再查。` 曾算出 20.7 wpm。已加 `spoken_units()`（中文按字），同一句现在 146.0。


## 七、人耳验收记录

| 项 | 值 |
| :--- | :--- |
| 日期 | 2026-11-27 深夜（MCP 重启后） |
| 版本 | `main @ 355241a`，MCP/CLI 同一份 `TtsEngine` |
| 前端标识 | `g2p-ipa/1.0[cmudict+overlay]+lexicon-zh` |
| 音色 / speed | `am_michael` / 140 wpm 校准（`norm`） |
| 试听文件 | `2026-11-28/000039_tts/…001_smoke_mcp.ref_norm.wav`（`client's`）<br>`2026-11-28/000042_tts/…001_smoke_mcp.ref_norm.wav`（`Where's`）<br>对照：`private/2026-11-27/review/r1.wav`、`r3.wav`（旧路径事故音频） |
| 结论 | **通过**（用户确认）。A/B 对照下 `client's` 的 /ts/ 尾音清楚、`Where's` 起句与后续衔接自然，无二次起音/截断；语速与音质较旧路径无退化 |
| 附带确认 | 混说 `这个 API 返回 404 error。` → MCP 返回 `ok:false`（拒答生效），不再产出降级音频 |

遗留（不阻塞本次收口）：D54 残留（`Where's` 句 128.6 wpm，略低于 130 带下沿）、D55（Sidecar 超时/自愈，接 misaki 前补）、
D56（混说音质，需 misaki-zh）、LTS 兜底（`Zorbly's` 这类未见词根）。


## 八、D54 / D55 收口（2026-11-27 深夜，同一轮）

### 8.1 D54 语速标定：已还

- **改法**：`rank()` 第一键由 `off_band: 0/1` 改为**连续带宽距离** `excess = max(0, |wpm−target| − target×6%)`；
  重试条件由“仅当有起始伪影”改为“**只要还没进带宽**就继续换速度”，候选上限 9 点（含去重与 clamp）。
- **复测**（`voice-cli say` 默认 140 wpm 校准，am_michael）：

| case | 改前 wpm | 改后 wpm | onset |
| :--- | ---: | ---: | ---: |
| wl04 Apple's stock fell again. | 190.5 | **136.4** | None |
| wl06 today's meeting was cancelled. | 89.9 | **142.0** | None |
| wl08 He's already sent the invoice… | 168.2 | **136.1** | None |
| wl01 The client's report isn't ready… | 139.9 | **140.2** | None |
| wl02 Where's the log file… | 128.6 | **138.1** | None |
| wl10 It's not finished, and they aren't ready. | 140.5 | **139.5** | 20 ms |

全部落在雅思 130–160 带内；单次合成 430–930 ms（重试没有明显增加延迟）。

### 8.2 D55 Sidecar 生命周期：已还

- `check_program()`：启动即验程序可达（绝对路径 / PATH+PATHEXT），错配立刻失败。
- 单次调用**超时**（默认 5 s，`VOICE_G2P_TIMEOUT_MS` 可调），超时 `kill` 子进程并报错。
- stdin / stdout / stderr **各独立线程**：子进程不读 stdin 也不会卡住调用方。
- 失败**一次重启重试**（新进程＝自愈）；连续失败 3 次**熔断**快速失败；任何失败都**不退回闭集词表**。
- 6 条单测（用本机 python 当假 sidecar）：成功 / 行数不符 / 非零退出 / 超时被杀（300 ms 内返回）/ 程序不存在 / 熔断。

> 这层是接 misaki（`VOICE_G2P_CMD='python -m misaki --ipa -'`）的前置条件；接口与失败语义已就绪，
> 缺的只是网络与权重。
