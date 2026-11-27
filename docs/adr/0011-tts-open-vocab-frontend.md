# ADR-0011：TTS 文本前端改为开放词表 G2P（lexicon 降为覆盖层）

- 状态：已接受（2026-11-27）
- 关联：`docs/audit/tts-2026-11-27-frontend.md`、`docs/audit/tts-2026-11-24.md`、ADR-0003（模型选型）、ADR-0008（D51）

## 背景

2026-11-27 出题素材 r1/r3 出现**整词缺失**：`The client's report…` → `The report…`、`Where's the log file…` → `The log file…`。
双 ASR 独立复现 + 词级 GOP 出现 `omitted`，确认音频里真的没有这两个词。

根因不在声学模型，在**文本前端**：

- `sherpa-onnx v2.0.0` 起删除了 espeak-ng 兜底，二进制里写死
  `espeak-ng is no longer used`，`--kokoro-data-dir`/`--kokoro-lang` 被忽略；
- Kokoro 的文本前端因此只剩 `lexicon` 查表，词表外 token（所有格 `X's`、`wh's`、未见专名）被**整词静默跳过**；
- 本项目此前的 OOV 预检又会把 `X's` 当“`X` + `'s` 都认识”放行，于是缺口完全无声。

## 决策

1. **英文一律走开放词表 G2P → `GenerationConfig::phoneme_codepoints`**，不再查引擎词表。
   覆盖不了就 `bail!`（宁可失败，不出缺词音频）。
2. **引擎 lexicon 降为高优先级覆盖层**：`overlay(tts-lexicon.txt)` > `lexicon-us-en` > `lexicon-gb-en` > CMUdict > 形态规则 > 数字 > 报错。
   命中 lexicon 保音质（与声学模型训练分布一致），未命中由离线规则与 CMUdict 保覆盖。
3. **中文默认走引擎 `lexicon-zh`**；可选 **misaki-zh sidecar**（`VOICE_G2P_CMD_ZH`）接管，
   让中文也走 `phoneme_codepoints`（实测 0.939 vs lexicon 0.903，音色 `zf_xiaoni`）。
   注意 Kokoro 中文音色差异很大：`zf_xiaobei` 在 codepoint 路径只有 0.515，示范音优先 `zf_xiaoni`。
   **已定为产品默认**：`DEFAULT_ZH_VOICE = "zf_xiaoni"`，`default_voice_for(text)` 按语言自动选（英文 `am_michael`）。
   中英混说**默认拒答**：单音色下另一语言必然跑调，且分段拼接处实测有 0.2–0.5 s 停顿与
   +10…19 dB 电平跳变 → 产品入口不出已知降级的音频（契约见 ADR-0002“一段文本一种语言”）。
   内部实验放行：`VOICE_ALLOW_MIXED=1` 或 `voice-cli say --allow-mixed`（此时按语言分段合成后拼接，
   中文段不进 G2P、英文段不进词表）。
4. 前端可替换：`G2pBackend` trait；`CmuBackend`（默认，零外部依赖）与 `SidecarBackend`
   （`VOICE_G2P_CMD='python -m misaki --ipa -'` 等外部开放词表 G2P）。
5. 漏词纳入回归：`voice-cli tts-regress <corpus.jsonl>`（双 ASR 逐词比对，漏词退出码 2）+
   `g2p_ipa` 的单测（词数守恒/未登录词必须报 OOV）。

## 后果 / 硬性约束（实测得出，写进代码注释）

| 约束 | 违反后的实测症状 |
| :--- | :--- |
| 双元音必须映射成 Kokoro 单字母符号（`aɪ→I`、`eɪ→A`、`oʊ→O`、`aʊ→W`、`ɔɪ→Y`） | `Friday` → "fra-ee-di" → ASR 听成 "fried" |
| 词内不能有空格 | `Apple stock fell again` → `Apple stock fee El again`（丢 3–4 词） |
| 空格**不能**当 token 发出去（`  16` 不是静音） | `and they`→`then they`、`isn't ready`→`tizen dready` |
| lexicon 覆盖层必须**美音优先**（us-en 先于 gb-en） | `last`→`/lɑːst/`（听成 "lost"）、`on`→`/ɒn/` |
| 央元音按重音分：`AH0→ə`、`AH1/2→ʌ`；`ER0→əɹ`、`ER1/2→ɜɹ` | `the`→ðʌ、`client's`→klˈaɪʌnts、`aren't`→ˈɑɹʌnt |

## 已知问题（不阻塞本决策，单独修）

1. **`synthesize_at_wpm` 语速标定散布**：个别短句落到 96–190 wpm（越界）。
   新前端沿用 lexicon 的发音惯例后时长分布变化，使该问题更显性。
   **结论：“140 wpm” 这个承诺目前不可靠**，见 ADR-0008 D54；修法是单独一轮做标定。
2. ~~生造专名报错~~ **已还（2026-11-27）**：加规则 LTS 兜底（`lts_ipa`），
   读音**必带标记**（`lts_words`），材料侧可人工复听/拒收；`VOICE_LTS=0` 退回报错。见 D57。
   若要对齐真人读音，仍需外部 LTS（espeak-ng / piper_phonemize，`SidecarBackend` 已就绪）。
3. **中英混说（已改为默认拒答）**：检测到混说 → `bail!` 并提示拆成两次调用。
   回归用例改为 `expect=reject_mixed`（必须拒答，且英文段音素仍可用）。
   要真正支持混说需 misaki-zh 接管中文（两段走同一条 `phoneme_codepoints` 链路），见 D56。
4. `tts-audit` 的 `oov` 列语义随前端改变（英文现在报 G2P 的 OOV）。
   已在每行与汇总加 `frontend` 字段（如 `g2p-ipa/1.0[cmudict+overlay]+lexicon-zh`），
   跨版本曲线不可直接比较。

## 关于 Piper（评估结论）

- **Piper 不能替代本次前端改造**：在 sherpa-onnx v2.x 里 Piper 同样是 closed-lexicon 前端
  （VITS family，`data_dir` 同样被忽略），换引擎不换前端＝换一张词表，同一类 bug 会以另一种形式回来。
- 因此 **Piper 从当前评测计划移出**；`crates/spike/src/bin/tts_ab.rs` 的
  `--engine piper --mode g2p` 路径**保留**（同前端、同文本、同 ASR 校验，可随时跑纯声学对比），
  但不再作为前端修复的候选，也不再阻塞任何决策。
