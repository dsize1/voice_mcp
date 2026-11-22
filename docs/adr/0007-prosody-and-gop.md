# ADR-0007: P2 收尾 —— 韵律分析与音素级 GOP（GOP 已实现，韵律待做）

- 状态：**部分完成**（2026-11-22）：音素级 GOP 已用**纯 Rust**落地（不做 Python sidecar）；
  韵律分析仍待做。实现与标定见 [../phoneme-gop.md](../phoneme-gop.md) / [../mdd.md](../mdd.md)。
- 日期：2026-11-19
- 相关：ADR-0001（运行时路线）、ADR-0006（评测层能力边界）、CONTRACT.md
- 决策背景：P2 的 1+2（词级 diff / flow）已上线并被 Skill 使用；3+4 需在 Skill 跑过若干真实训练后再启动，
  以便用真实数据校准阈值，而不是凭想象定参数。

## 一、待做项 3：韵律分析（纯 Rust，无需 sidecar）

### 目标指标

| 指标 | 算法 | 对应雅思 |
|---|---|---|
| F0 轮廓 | YIN/自相关，10 ms 帧移，60–400 Hz，浊音判定（周期性阈值 + 能量门限） | Pronunciation |
| 句末升降 | 末 30% 时长内的 F0 斜率（半音/秒）+ 与句首基线的差 | 陈述句降调 / 一般疑问句升调 |
| 重音落点 | 每词三元组显著性：时长 z-score、F0 峰值相对中位数的半音差、能量 z-score → 合成 salience | 句重音（nucleus placement） |
| 平调检测 | 全句 F0 标准差（半音）；< 1.5 st 判"平" | 单调、无起伏 |
| 音域 | F0 的 5%–95% 分位差（半音） | 语调宽度 |
| 节奏 | 相邻词间隔的时长方差、重读词与非重读词时长比 | 应力节拍（stress-timed） |
| 与示范音对比 | 用户 F0 与 TTS 参考音的 DTW 对齐（`dtw` crate，代价 = 半音差） | 模仿度 |

### 实现要点（踩坑预警）

1. **F0 必须在半音域（semitone）比较**：`12*log2(f/ref)`，`ref` 取该说话人语句中位数。
   直接比 Hz 会让男声/女声、高音/低音完全不可比。
2. **浊音判定比 F0 估计更容易出错**：清辅音段的 F0 是噪声，必须用"周期性 + 能量"双门限剔除，
   否则句末降调会被后缀清辅音的假 F0 拉平。
3. **0.75x 示范音的 F0 轮廓不可直接比**：时长拉伸会改变斜率。做法是先把两者的时间轴归一化
   （按词边界比例映射）再比，而不是比绝对斜率。
4. 输出**只给数值 + 位置**，不下"语调不好"这类结论（与 ADR-0006 的 D3 一致）。

### 接口

不新增工具，扩展现有 `assess`：

```
assess(path, reference, with_prosody: bool = false, reference_audio: Option<String>)
```

原因：教学循环是一轮一次调用，加一个需要单独调用的工具会让 Agent 每轮多两次往返。
新增字段 `prosody: { f0_median_st, final_slope_st_per_s, flat: bool, range_st, stress: [...], dtw_cost_vs_ref }`。

### 验收

- 对 12 条人工标注（升/降/平/重音位置）的录音，句末升降判定一致率 ≥ 80%。
- 对同一说话人重复录同一句，`f0_median_st` 波动 < 1.0 st。

---

## 二、待做项 4：音素级 GOP（Python sidecar，路线 B）

### 为什么必须用 sidecar

见 ADR-0001：音素强制对齐 + CTC 后验在 Python 有成熟实现（torchaudio `forced_align` + MMS_FA），
Rust 侧需自研 CTC Viterbi（ADR-0001 的 A1/A2 任务，3–5 天）。先验证算法，再决定是否移植。

### 设计

```
voice-mcp (Rust) --spawn--> python -m voice_scorer (持久进程, JSON over stdio)
                                  └─ MMS_FA (wav2vec2 300M, onnx 或 torch)
```

请求：`{audio_16k_b64, reference_text, lang}` → 响应：

```json
{ "phones": [{"p":"θ","word":"third","t0":0.42,"t1":0.51,"gop":0.31,"top_confusions":["s","f"]}],
  "scorer": "mms-fa-gop/0.1", "cold_start_ms": 0 }
```

### 关键实现约束

1. **对齐必须带词边界**：MMS_FA 用 uroman 罗马化，中英混说需要分句处理；先把 ASR 得到的词时间戳作为
   粗略边界，再在每个词窗口内做音素对齐，降低搜索空间并且更稳。
2. **GOP 归一化口径必须固定并写入版本号**：GOP 有三种常见定义（原始对数后验、按最大值归一、按比例归一），
   换口径等于换分数，`scorer_version` 必须体现。
3. **校准**：需人工听 20–30 条样本标注"对/错"，把 GOP 阈值调到与听感一致（目标：二分类一致率 ≥ 85%）。
   校准前的分数只用于**相对比较**（同一用户同一句的多次对比），不对外解释绝对值。
4. **优雅降级**：sidecar 未安装 / 冷启动超时（>8 s）/ 崩溃 → `assess` 自动退回词级结论，
   `degraded: true` 且 `limitations` 追加"音素级不可用"。契约不变（ADR-0001 决策 2）。
5. 显存：MMS_FA ~1.5 GB，与常驻模型合计仍 < 6 GB，可常驻。

### 迁移到纯 Rust（对应 ADR-0001 路线 A）

仅当 sidecar 分数与听感一致性 ≥ 85% 且算法稳定后才启动：
wav2vec2 导出 ONNX → Rust 实现 CTC 强制对齐（Viterbi、窄带剪枝）→ GOP → 用 200 条已标注样本验证
与 sidecar 的**排序一致性 ≥ 0.9（Spearman）**。

---

## 三、为什么现在不做

1. Skill 还没跑过真实训练，阈值（停顿 250 ms、词界 200 ms、语速区间）都还是估的；
   韵律分数的阈值同样需要真实数据。
2. `record` 因**本机没有麦克风**尚未实测，短句跟读的数据质量未知。
3. 音素级错误清单（θ/s、v/w、r/l、词尾辅音）在 Skill 的 `references/phoneme-map.md` 里已经可以
   作为**定性**提示使用，不阻塞教学闭环。

**触发条件**：完成 ≥ 10 次真实训练（`manifest.jsonl` ≥ 10 个 `scene=drill|retry` 条目）后启动。

## 实现结果（2026-11-22，覆盖本文第二节的 sidecar 方案）

原计划用 Python sidecar（torchaudio forced_align + MMS_FA）做 GOP。实际**不需要 sidecar**：
音素 CTC 模型（`wav2vec2-lv-60-espeak-cv-ft` → ONNX）直接给逐帧后验，
用 30 行 DP 做 CTC Viterbi 对齐即可同时得到边界与 GOP —— 运行时纯 Rust（`ort` + `load-dynamic`）。

| 项 | 实测 |
|---|---|
| 推理 | CPU RTF 0.04–0.05（3.4 s 音频 110–173 ms）；fp32 1 205 MB；int8 量化漂移 8.21 → 弃用 |
| 标定（so762 未见说话人） | 句级 ρ 0.67；**词级 AUROC 0.887 / P@FP10 0.457**；音素级 0.769 / 0.170 |
| MDD 微调 | 音素级 AUROC 0.690 → 0.769（6 epoch，65 s/epoch，RTX 5070 Ti cu128） |
| 混淆对 | `TH→s` 命中精确率 0.50 / 召回 0.67（其余混淆对精度为 0，不上线） |

**因此本 ADR 的第二项（音素级 GOP）关闭；第一项（韵律）保留**，触发条件仍是 ≥10 次真实训练。
