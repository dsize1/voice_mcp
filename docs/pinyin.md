# 中文拼音层（D13 / M5）

> 状态：**已上线**（2026-11-28）。`assess` 在参考文本含汉字时自动产出 `text_diff.pinyin`，
> `voice-cli pinyin` 可离线核对。真机审计见 [audit/pinyin-2026-11-28.md](audit/pinyin-2026-11-28.md)。

## 1. 一句话

把中文的**字级 diff** 升级成**声母 / 韵母 / 声调**三层诊断，并且**同音不同字不再算发音错误**。

## 2. 为什么需要（旧口径的三个坑）

中文在 `normalize` 里是逐字比较的，所以旧实现只会说「少了一个字」。三个具体问题：

| 旧行为 | 后果 | 实测例子 |
|---|---|---|
| `的/得`、`在/再`、`他/她` 拼音完全相同，仍算替换错误 | 字级 accuracy 被系统性压低，教练被迫挑"不是错"的错 | 「我觉**得**」被听成「我觉**的**」，accuracy 0.857，实际发音 1.00 |
| 汉字替换走英文音形编码 `phonetic::encode`（只保留 ASCII 字母） | 任何汉字对都得到 `phonetic_similarity = 0` → 一律推断成 `likely_different_word`（"说成了别的词"） | 「四」→「十」被说成"用词问题"，其实是平翘舌+声调 |
| 没有声学级中文音素打分（GOP 模型面向英文/espeak） | 想说"哪个音错了"时无话可说 | 现在至少能说"声母 s→sh、声调 4→2" |

## 3. 数据源与生成

拼音表是**生成物**，随源码固化，运行时纯 Rust 解析（`include_str!`），
**不依赖模型目录、不需要 Python、CI 里也能跑**：

```text
crates/voice-core/data/pinyin-zh.txt     # 26 703 汉字 / 7 310 个多音字 / 287 KiB
tools/gen_pinyin_table.py                # 生成 + --check 校验
```

- 数据源：`pypinyin` 0.55.0（MIT）的 `pinyin-data`，`Style.TONE3` + `heteronym=True` + `ü → v`
  → 每行 `字<TAB>读1,读2,...`，声调 5 = 轻声。
- 重新生成：`pip install pypinyin && python tools/gen_pinyin_table.py`；
  校验：`python tools/gen_pinyin_table.py --check`（`tools/check.ps1` 已加这一步，缺 pypinyin 时只提示不阻断）。
- **为什么不复用 TTS 的 `lexicon-zh.txt`**：那份是音素串（`银 i ↗ n`）且逐字只有一个读音，
  实测 8 个多音字错 7 个（银行/重庆/长城/音乐/空调/便宜/行长/还钱）。拿它做评测会直接把「行 háng 读对」
  判成「声母 x→h + 韵母 ing→ang」。TTS 侧的多音字问题由 misaki-zh 解决，评测侧用本表的多读音候选。

## 4. 判定口径

### 4.1 三层差异

一个读音拆成 `声母 + 韵母 + 声调`，只比较**音系化**后的形式：

- `y`/`w` 不算声母（拼写规则），`yi → i`、`wu → u`、`yu → v`（ü 记作 `v`）；
- `iu→iou`、`ui→uei`、`un→uen`、`j/q/x + u → v`（`jun1` 的韵母是 `vn`）；
- 零声母在话术里写作 `∅`。

于是每对字得到 `layers ⊆ {initial, rime, tone}`，并归成一个 `kind`：

| `kind` | 含义 | 例子 | `hint` | severity |
|---|---|---|---|---|
| `homophone` | 三层全同 → **不算发音问题** | 得/的 (de5) | `likely_homophone` | low |
| `tone_only` | 只有声调不同 | 买 mai3 / 卖 mai4 | `unclear` | medium |
| `initial_only` | 只有声母不同 | 私 si1 / 诗 shi1 | `likely_pronunciation`（命中已知对） | high |
| `rime_only` | 只有韵母不同 | 新 xin1 / 星 xing1 | 同上 | high |
| `mixed` | 两层不同 | 四 si4 / 十 shi2（s/sh + 4/2） | 命中已知对 → `likely_pronunciation`，否则 `unclear` | high/medium |
| `different` | 三层全不同 → 更像另一个词 | 十 shi2 / 不 bu2 | `unclear` | medium |

**声调为什么只给 medium**：拼音层是**文本层**结论，输入是 ASR 输出的汉字。声调差异既可能是用户真读错，
也可能只是 ASR 选了另一个调的字——文本无法区分，所以措辞必须是「声调 3→4」而不是「你声调错了」。

### 4.2 多音字：取"让两边最接近"的那一读

不做单点猜测（与 D46 英文 live/read 选变体同一条道理）：对 `行`(xíng/háng/héng) × `航`(háng)
的所有读音组合求距离，取层数最少的那一对 → 判同音。否则「银行」被听成「银航」会报 3 条假错误。

单测 `polyphone_strategy_matches_real_word_readings` 固定了这批：
行/航 (háng)、长/常 (cháng)、乐/月 (yuè)、调/条 (tiáo)、重/虫 (chóng)、宜/姨 (yí)。

### 4.3 已知混淆对（可以点名说的只有这些）

| 声母 | 标签 | 韵母 | 标签 |
|---|---|---|---|
| zh/z、ch/c、sh/s | `initial.zh_z` / `ch_c` / `sh_s`（平翘舌） | an/ang、en/eng、in/ing、ian/iang、uan/uang | `rime.an_ang` …（前后鼻音） |
| n/l | `initial.n_l`（鼻音/边音） | u/ü | `rime.u_v`（圆唇） |
| f/h、r/l | `initial.f_h` / `r_l` | ai/ei、ao/ou、e/o、uo/o、iu/u、i/ei、ün/uen、üe/ie | `rime.…` |
| j/z、q/c、x/s | `initial.j_z` / `q_c` / `x_s`（尖团音） | | |

命中表里的对 → 可以当"发音偏差"说；表外的差异（`initial.sh_b`）只写进 `confusions` 统计，
**不得**下发音结论（`hint` 保持 `unclear`）。

音形相似度（写进 `issues[].phonetic_similarity`）：声母 0.45 / 韵母 0.35 / 声调 0.20 加权，
已知混淆对的那一层记 0.5 而不是 0。→ 同音 1.0、单声调差 0.80、平翘舌 0.775、前后鼻音 0.825。

## 5. 输出字段

`assess`（MCP 的 `detail.pinyin`、CLI 的 `metrics.json`）新增：

```json
{
  "text_diff": {
    "accuracy": 0.857,             // 字级（口径未变，保持可纵向比）
    "pinyin": {
      "table": "pinyin-zh/0.55.0",
      "chars_total": 7, "chars_compared": 7, "chars_lookup_missing": 0,
      "equal": 6, "homophones": 1,
      "tone_only": 0, "initial_only": 0, "rime_only": 0, "mixed": 0, "different": 0,
      "accuracy_char": 0.857,
      "accuracy_pinyin": 1.0,      // 同音不同字不算错 ← 中文的真实发音口径
      "confusions": [{"tag": "initial.sh_s", "count": 2, "examples": ["四→十 (si4→shi2)"]}],
      "chars": [{"reference":"得","said":"的","ref_pinyin":"de5","said_pinyin":"de5",
                 "kind":"homophone","layers":[],"known":false,"similarity":1.0,
                 "note":"参考「得」= de5，识别「的」= de5：**同音不同字**…"}]
    },
    "issues": [{ "...": "…", "hint": "likely_homophone", "pinyin": { "kind": "homophone", "...": "…" } }]
  }
}
```

- `text_diff.accuracy` **不变**（字级），`accuracy_pinyin` 是新增的中文口径，两者一起看：
  `accuracy < accuracy_pinyin` 的差额就是"同音字噪声"，不是发音问题。
- `chars_lookup_missing > 0` → 生僻字不在表里，**不进分母**、不计入准确率（`limitations` 会写明）。
- 英文/纯数字参考 → `pinyin` 字段整体缺失（不产出空壳）。

## 6. 中文数字归一（D13 附带修复）

SenseVoice 中文会把「二十六」写成 `26`、把「十四」写成 `14`，而参考文本是汉字。
旧口径按英文展开成 `twenty six` → 与逐字比较完全对不上（实测「他把空调开到二十六度」字级准确率 0.700）。

现在的口径（`normalize::units_zh`，仅在参考文本含汉字时启用）：

| 输入 | 归一 |
|---|---|
| `26` | `二十六`（逐字成为 3 个比较单元） |
| `105` | `一百零五` |
| `1998` / `007` / 5 位以上 | **逐位**：`一九九八` / `零零七`（年份、编号的中文读法） |
| `3.5` | `3` + `5`（小数点被切分，读作「三、五」） |

因为两侧用同一套规则，写法一致时不会互相误伤；只有"参考写汉字、ASR 写数字的年份"（一九九八 vs 1998）
会不同步——这属于写法差异，拼音层不据此判发音。

## 7. 真机审计（2026-11-28）

`python tools/pinyin_audit.py`：TTS `zf_xiaoni` 朗读 10 句 → SenseVoice `zh` → `assess`。

| 指标 | 值 |
|---|---|
| 平均字级 accuracy | **0.922** |
| 平均拼音层 accuracy | **0.933** |
| 同音不同字（不算错） | 1 处（`银行` → `银航`，yín háng 完全同音） |
| 被判成 `likely_pronunciation` 的 | **0 处**（通过线 = 0：TTS 读对的音频上不许误报发音问题） |

结论：拼音层在"读对的音频"上不误报；字级与拼音层的差额全部来自 ASR 选字/漏词（`的`、`我` 被吞、数字写法）。

## 8. 边界（必须承认）

1. **纯文本层**：结论来自 ASR 的汉字，不是声学信号。中文**没有**音素级 GOP
   （现有 CTC 模型面向英文/espeak，中文未标定 → `with_gop` 时直接跳过并写 `limitations`）。
2. **声调结论最弱**：文本层区分不了"声调真错"与"ASR 选字"，故 `tone_only` 一律 `unclear/medium`。
3. **多读音会让错误被低估**：候选读音越多，"同音"命中越容易 → 宁漏不误报（这是刻意的方向）。
4. **不做词级/句级韵律**：中文 F0 与声调的映射不同于英文语调，`prosody` 的中文结论尚未标定，
   现有 `prosody` 字段在中文下只作参考，不得据此判"声调/语调不对"。
5. **不覆盖非汉字**：英文、数字、标点、生僻字不进拼音层（`chars_total` 只数汉字）。
6. **不能替代声学复核**：`likely_pronunciation` 也只是"这一层不一样"，仍要求用户重录那 1–2 个字确认。
7. **语言不匹配会直接降级**（2026-11-28 真机踩到）：参考是汉字、转写里却**一个汉字都没有**
   （例如 `lang="zh"` 配了一条英文录音 → `X Ste par loved books to Shanghai last week.`），
   `assess` 会置 `degraded=true` 并写明原因，`pinyin` 为 `null`。
   否则字级 diff 会把每个汉字都报成"被替换成英文单词"，看起来像"用户把中文读成了英文"。
   判据见 `assess::chinese_ref_without_hanzi`（空转写不算，交给"没检测到语音"那条）。

## 9. 怎么用

```bash
# 只看一段中文的读音（多音字会列出全部候选）
voice-cli pinyin "银行行长正在开会"

# 与参考文本做拼音层诊断（纯文本，不需要音频和模型）
voice-cli pinyin "我在北京银航工作" --reference "我在北京的银行工作"

# 正常录音评测：中文参考自动带拼音层
voice-cli assess last --reference "四是四，十是十" --lang zh
```

MCP：`assess(reference="四是四", lang="zh")` → `detail.pinyin` + `detail.issues[].pinyin`。

教练话术（SKILL 4.3）：

- 同音：**不说**，只在用户问"为什么 accuracy 掉了"时解释"同音不同字，不算错"。
- 声调：`"「买」你读的是 mài（4 声），参考是 mǎi（3 声）—— 不过这句也可能是识别选字，先重读一次确认"`
- 声母/韵母：`"这句里「四」听成了「十」：声母 s → sh（平翘舌），声调也从 4 掉到 2。先只练 s/sh 这一对"`

## 10. 验收

- 新增单测 **20 条**：`assess::pinyin` 13 + 中文 diff 集成 5（`assess::diff`）+ 数字归一 2（`assess::normalize`）；
- 全仓 `cargo test` **196 条**全绿；`check.ps1` 增加拼音表 `--check` 门禁；
- 真机审计 10 句 0 误报（第 7 节）。
