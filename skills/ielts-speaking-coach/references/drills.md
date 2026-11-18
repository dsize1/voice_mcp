# 训练方法库

> 用途：当"重复跟读"效果不好时换方法。每种方法都给了**具体操作 + 用哪个工具验证**。
> 不要一次用三种；一次训练用**一种方法 + 一个目标音**。

## 1. 最小对立对辨别（minimal pair discrimination）

**适合**：`theta_to_s`、`v_to_w`、`r_to_l`、`æ_to_e`、`ɪ/iː`

1. 教练用 `synthesize` 播一个词（比如 `think`），不告诉用户是哪个
2. 用户回答"是 think 还是 sink"
3. 反过来：`record` 用户读的词，比较 `transcribe` 结果（ASR 听成哪个就是哪个）
4. 10 组全对 → 进入句子层面

**为什么有效**：先解决"听不出区别"，再解决"发不出区别"。很多人是听辨问题，不是发音器官问题。

## 2. 影子跟读（shadowing）

**适合**：`tone_flat`、`linking_missing`、整体节奏

1. `synthesize(text, speed=0.9, play=true)` 连续播 2–3 句
2. 用户**不等播完就跟着说**（延迟 0.5–1 秒，像影子）
3. 不看文本跟 2 遍，然后看文本再跟 1 遍
4. 用 `assess(reference=原句)` 只关注 `pause_count` 与 `word_boundary_breaks`

**注意**：影子跟读时不要求 accuracy 高（跟不上很正常），只看节奏。

## 3. 回音法（echo method）

**适合**：单词语音细节、`final_consonant_drop`

1. 播一个短句（≤8 词），用户**先听**、**在心里默念**、再**开口**
2. 每次只处理一个词：教练播单词 → 用户读 → `assess(reference=单词)`
3. 达标标准：`issues` 为空且 ASR 转写正确

## 4. 倒序链（back-chaining）

**适合**：`cluster_simplify`（street, asked, texts）、长单词

从最后一个音节往回加：

```
reet → treet → street
ast  → sked  → asked
```

每加一步都让用户读一遍，用 `transcribe` 检查 ASR 是否听对（听对 = 至少在词的层面可辨）。

## 5. 节拍器/拍手（stress & rhythm）

**适合**：`stress_misplace`、`schwa_missing`、节奏感差

1. 把句子按重音位置打拍子：`I THINK this is the THIRD time`（大写 = 拍手）
2. 用户边拍边说，重音处手拍重一点
3. 用 `assess` 看 `speech_rate` 是否变稳、`pause_count` 是否下降

## 6. 意群切分（chunking）

**适合**：`pause_count` 高、`repetitions` 高（其实是组句能力问题）

1. 把长句按意群切成 2–4 块，每块单独读顺
   `I think this is the third time / that I've asked you / to be quiet.`
2. 逐块跟读 → 两块连 → 整句连
3. 用 `assess` 看 `longest_run_words` 是否上升

## 7. 录音对比（A/B）

**适合**：所有问题，尤其"我不觉得自己有问题"

1. `play` 用户自己的录音（`evidence.norm_wav`）
2. `synthesize` 播示范音
3. 让用户指出"哪不一样"——用户自己发现的问题，改正率高得多
4. 再录一次，`assess(scene="retry")` 对比

## 8. 变速阶梯（speed ladder）

**适合**：新素材上手、语速过慢

```
0.7x（只求读对）→ 0.85x → 1.0x（正常）→ 1.15x（挑战）
```

每一档都要 `assess`，只有当前档 `issues` 为空才升级。
1.15x 只是挑战，不要求达标。

## 9. 注意力切换（attention shifting）

**适合**：顾了发音就忘了内容

三个焦点轮换，每遍只关注一件事：
1. 只听自己有没有把词尾说完整
2. 只关注句子有没有起伏
3. 只关注内容（说的意思对不对）

用 `assess` 逐次验证，通常第 3 遍指标最好——这是正常现象，要告诉用户。

## 10. 什么时候该停

| 信号 | 处理 |
|---|---|
| 同一句 3 次都没达标 | 换更短的句子，回到**更简单的目标**（不是同一句硬练） |
| 用户连续说"我知道但我做不到" | 换成第 1 节（听辨）或第 4 节（拆音），降低认知负荷 |
| `assess` 每次结果差异很大（accuracy 波动 > 0.2） | 录音环境/设备问题，先解决输入质量（换 48k 麦克风、关降噪） |
| 用户疲劳（反应变慢、开始敷衍） | 立刻收尾，布置 3 句作业，下次再来 |
