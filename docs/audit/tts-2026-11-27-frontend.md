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
