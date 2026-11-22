# MDD 微调实验（B 阶段）

目标：把音素级 GOP 的判别力从"只能用来看排序"提升到"能报单个音素"。
判据（与 S1 标定同口径）：so762 音素级 AUROC，以及 FP=10% 处的精确率/召回。

## 结论先说

**有效，但还没到"可以逐个音素断言"的程度。**

| 配置 | AUROC | P@FP10% | 召回 |
|---|---|---|---|
| 基线 `wav2vec2-lv-60-espeak-cv-ft` | 0.690 | 0.132 | 0.187 |
| 微调 3 epoch / 最后 4 层 / lr 3e-4 | 0.757 | **0.198** | **0.304** |
| 微调 6 epoch / 最后 8 层 / lr 2e-4 | **0.769** | 0.170 | 0.252 |

- AUROC 相对基线 **+0.067 ~ +0.079**（约 +10%）
- FP 固定 10% 时，精确率 0.132 → 0.17–0.20（**报 5 个可疑音素，真错 1 个**）
- 训练代价极低：**3 epoch ≈ 47 s/epoch，6 epoch ≈ 65 s/epoch**（RTX 5070 Ti，4000 句）

### 因此设计不变

GOP 仍然只能：① 句级趋势、② 句内排序、③ 与词级 diff 交叉验证后说"可能"。
**仍然禁止**"你读错了 /θ/"。但排序质量提升约 40–50%（相对值），top-1 命中的概率明显变高。

## 协议（不可省）

| 项 | 做法 |
|---|---|
| 切分 | **按说话人**：200 训 / 25 验 / 25 测，同一人只出现在一个 split（so762 官方 train/test 已按人切，这里再打散重划） |
| 标签 | 音素序列来自 CMUdict（ARPAbet）→ 映射到模型 espeak 符号；**人工 `mispronunciations` 只用于评估，不进训练** |
| 训练目标 | CTC loss（模型本身就是音素 CTC），冻结 CNN 特征提取器，只训最后 N 层 transformer + 分类头 |
| 评估 | 训练没见过的 25 位说话人，分层取低/中/高人工分各 50 句（共 150 句、3 051 个音素、230 个错读） |
| GOP 算法 | 与生产代码同一套（`tools/calibrate_gop.py` 的 Viterbi + GOP + 说话人内 z） |

> 为什么要分层：随机抽 200 句只有 17 个错读（0.4%），AUROC 会剧烈波动（实测同配置在随机样本上
> 是 0.849 → 0.865，看着漂亮但没有统计意义）。

## 复现

```bash
.cache/venv-train/Scripts/python.exe tools/mdd_train.py dump
.cache/venv-train/Scripts/python.exe tools/mdd_train.py train --epochs 6 --layers 8 --lr 2e-4
.cache/venv-train/Scripts/python.exe tools/mdd_train.py export
.cache/venv-train/Scripts/python.exe tools/mdd_train.py eval --strat 150
```

产物：`C:/voice_models/phoneme-ctc-ft/`（`model.onnx` 1 205 MB + `vocab.json` + `metadata.json`，
metadata 里记录训练配置、评估数字与 sha256）。

## 环境（一次性）

GPU 栈单独放 venv，不动全局 CPU torch：

```
python -m venv --system-site-packages .cache/venv-train
pip install --no-deps https://mirrors.aliyun.com/pytorch-wheels/cu128/torch-2.10.0%2Bcu128-cp313-cp313-win_amd64.whl
```

- `download.pytorch.org` 本机不可达（000），阿里云镜像可用（16.6 MB/s）
- 实测：`torch 2.10.0+cu128` / CUDA 12.8 / RTX 5070 Ti cc(12,0) / 15.9 GB
- matmul 3748 GFLOP/s（fp32），SDPA / Conv1d / LayerNorm / bf16 均正常

## 跨域抽查（两个模型，同一段音频）

| 音频 | 基线 worst | 微调 worst |
|---|---|---|
| TTS 干净音（`I think this is the third time.`） | ER −11.1（离群） | DH −3.5（ER 回到 −0.8） |
| 真机 48 kHz（`We're going to spend three days…`） | R −4.3 / UW −3.7 / IY −3.5 | R −6.2 / R −6.0 / R −5.7 |

微调后离群值被压平（TTS 的 ER 从 −11 回到 −0.8），说明分布更稳；真实录音里 `R` 仍是最可疑项，与弱读一致。

## 下一步可选项

1. **切换默认模型**（`VOICE_PHONEME_MODEL=C:/voice_models/phoneme-ctc-ft`）并在真机上 A/B 10 轮；
2. 继续提升：数据增广（加噪/变速）、解冻全部层、用 dev loss 选 epoch（3ep 的 P/R 更好，说明 6ep 有过拟合迹象）；
3. 换更大音素模型（Conformer）→ 收益未知，成本高；
4. 停止 B，把精力放回产品链路（真机训练 + 雅思素材）。
