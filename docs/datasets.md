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

## S0b：tag 权重（已落地）

`tools/derive_tag_weights.py` 把上面的音素统计聚合到我们的 tag 上，产出
`docs/tag-weights.json`，Rust 侧常量表在 `voice-core::bank::TAG_WEIGHTS`，
并有单测 `tag_weights_match_derived_artifact` 保证两侧不漂移。

```
weight = rate(本数据集实测错读率) × impact(phoneme-map 的可理解度分级)
```

| tag | rate | impact | **weight** | 样本量 |
|---|---|---|---|---|
| `theta_to_s` | 7.70% | 1.0 | **0.0770** | 649 |
| `r_to_l` | 4.11% | 1.0 | 0.0411 | 6 087 |
| `ae_to_e` | 4.90% | 0.6 | 0.0294 | 5 393 |
| `final_consonant_drop` | 2.59% | 1.0 | 0.0259 | 20 426 |
| `short_long_vowel` | 4.28% | 0.6 | 0.0257 | 19 895 |
| `w_to_v` / `v_to_w` | 2.54% / 2.47% | 1.0 | 0.0254 / 0.0247 | 3 148 / 1 617 |
| `eth_to_z` | 2.38% | 0.8 | 0.0190 | 3 027 |
| `s_ending` / `ed_ending` | 3.23% / 3.19% | 0.6 | 0.0194 / 0.0192 | 6 759 / 11 649 |
| `linking_missing` / `question_rise` / `tone_flat` / `fragment` | — | — | **0** | 无音素级证据 |

**为什么乘 impact 而不只用 rate**：so762 量的是"错了几次"，不是"错了有多严重"。
`/v/→/w/` 只占 2.47%，但 `very→wery` 会直接变词；只按 rate 排会把它排到最后。

**为什么无证据的 tag 权重为 0**：`linking_missing`、`question_rise` 这类无法从音素错读率推导，
给它们编一个权重就是编造依据。它们仍然可以被 `--tag` 显式选中，只是不参与加权排序。

### 用法

```bash
# 默认按权重排序（同一批 tag 命中的，优先高权重）
voice-cli bank pick --tag theta_to_s --tag s_ending --tag ed_ending --level 2 --limit 4
# 退回旧的“命中个数”排序（对比用）
voice-cli bank pick --tag theta_to_s --tag s_ending --tag ed_ending --level 2 --no-weighted
```

实测差异：旧排序第 4 条是 `nce1/079/008`（`eth_to_z+ed_ending+s_ending+linking_missing`，无 /θ/），
新排序换成 `nce1/087/010`（`theta_to_s+s_ending+linking_missing`）——
权重把"`linking_missing` 无证据 → 0 分"和"/θ/ 权重最高"都算进去了。
