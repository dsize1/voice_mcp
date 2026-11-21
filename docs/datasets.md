# 评测数据集（发音诊断标定/评估用）

> 数据集**不入 git**（音频体积 + 许可），落盘 `D:/voice_data/datasets/`。
> 仓库里只保留指纹：`docs/dataset-pins.json`。

## 已就位

| 数据集 | 用途 | 许可 | 规模 | 指纹 |
|---|---|---|---|---|
| `mispeech/speechocean762` (so762) | GOP 阈值标定、评分相关性、MDD 微调 | apache-2.0 | 5 000 句 / 250 名**中文母语**者 / 645 MB | `docs/dataset-pins.json` |

### 为什么先只要这一个

- **L1 完全匹配**目标用户（普通话母语者学英语）；
- 一句句都有 **句级 + 词级 + 音素级**人工分（5 位专家），且带 `mispronunciations[]`
  （`canonical-phone / index / pronounced-phone`）→ 既能算 P/R，也能算评分相关性；
- **音素集是 ARPAbet**（`M AA0 R K`），与 ADR-0007 选定的英文 MVP 音素集一致，不需要再做映射；
- train/test **按说话人切分**（实测交集 0），可直接做 speaker-disjoint 评估。

### L2-ARCTIC：暂缓

官方发布走 `psi.engr.tamu.edu` 的**研究用途申请表**，且不给校验和 → 无法做供应链指纹核验。
按"没有指纹就不入库"的原则先不引；若后续要与 6 种 L1 做泛化对比，再走正式申请并单独记指纹。

## 校验方法（`tools/fetch_datasets.py`，四层）

| 层 | 检查 | 失败处理 |
|---|---|---|
| A 来源 | 只允许白名单主机（hf-mirror / xet-bridge）；URL 钉死 **revision**（不用 `main`） | 退出码 2 |
| B 完整性 | 文件大小 + **sha256 == 上游 LFS oid**，并用 API 对 pinned revision 二次核对 | 移入 `.quarantine/` |
| C 结构 | Parquet 魔数 `PAR1`（首尾各一），排除错误页/HTML 伪装 | 同上 |
| D 语义 | 行数 2500、说话人 ≥100、句级分 ∈[0,10]、音素 ⊆ ARPAbet（38 个）、音频时长 0.3–30 s | 同上 |

```bash
python tools/fetch_datasets.py                       # 下载 + 四层校验
python tools/fetch_datasets.py --verify-only         # 只重跑 B/C/D（改代码后必跑）
python tools/fetch_datasets.py --write-pins          # 校验通过后更新 docs/dataset-pins.json
```

实测结果（2026-11-22）：

```
train-00000-of-00001.parquet   317.4 MB  sha256 b06fe00f49d2c02c…  2500 行 / 125 人 / 音频 1.59–10.72 s
test-00000-of-00001.parquet    297.1 MB  sha256 c951056c8bbc52f7…  2500 行 / 125 人 / 音频 1.94–6.62 s
speakers: train 125 / test 125 / 并集 250 / 交集 0
```

## 数据画像（TODO S0b 的输入）

- 句级均分（1–10）：accuracy 7.62 / 7.70、completeness 9.99、fluency 7.73、total 7.18（train/test 几乎一致）
- 音素 token 总数 **94 445**，其中被标错读 **3 403（3.60%）**
- **错读率最高的音素**（出现 ≥300 次）：

| 音素 | 出现 | 错读 | 错读率 | 对应我们的 tag |
|---|---|---|---|---|
| TH | 649 | 50 | **7.70%** | `theta_to_s` |
| EY | 1 605 | 118 | 7.35% | `short_long_vowel`（双元音） |
| ER | 1 327 | 91 | 6.86% | `r_to_l` |
| EH | 2 538 | 172 | 6.78% | `ae_to_e` |
| UH | 1 131 | 75 | 6.63% | `short_long_vowel` |
| AO | 1 497 | 91 | 6.08% | — |
| NG | 1 099 | 65 | 5.91% | `n_to_ng` |
| AW | 690 | 39 | 5.65% | — |
| SH | 699 | 37 | 5.29% | — |
| R | 2 445 | 119 | 4.87% | `r_to_l` |

对照：`S` 2.77%、`T` 2.90%、`W` 2.54% —— **/θ/ 的错读率是 /s/ 的 2.8 倍**，
与 `skills/ielts-speaking-coach/references/phoneme-map.md` 里"`theta_to_s` 可理解度影响最高"一致。
这张表就是 `tags` 权重（S0b）的数据依据，不再靠经验拍。
