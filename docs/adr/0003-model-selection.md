# ADR-0003: 模型选型与资源预算

- 状态：Accepted
- 日期：2026-11-18
- 相关：ADR-0001（运行时）、ADR-0002（ASR 路由）

## 硬件约束

| 项 | 值 | 影响 |
|---|---|---|
| GPU | RTX 5070 Ti 16 GB (sm_120, Blackwell) | 推理栈必须 CUDA ≥12.8 编译；**ONNX Runtime / PyTorch 轮子版本是第一风险项** |
| CPU | Core Ultra 5 245K (6P+8E) | 桌面 Arrow Lake-S **无 NPU**，不依赖 NPU；CPU 兜底跑 SenseVoice/Kokoro 足够 |
| RAM | 48 GB | 模型可全量常驻，不做卸载/换入换出 |
| 磁盘 | `C:/voice_models/`（固态） | 冷启动快；单模型 ≤3 GB |
| CUDA | 13.3（已装） | 部分 `-sys` crate 仍只认 12.x → 优先走 `ort` 预编译运行时 |

## 选型总表

| 角色 | 选定模型 | 体积/显存 | 许可 | 备注 |
|---|---|---|---|---|
| ASR 英文 / 中英混说 | `whisper-large-v3-turbo` (f16) | ~1.6 GB / ~2.5 GB | MIT | 词级时间戳、标点、数字规范化 |
| ASR 中文 / 快速通道 | `SenseVoice-Small` (ONNX int8 或 fp32) | ~0.25 GB / ~0.6 GB | Apache-2.0 | 非自回归，~70ms/10s；带语言/情绪标签 |
| VAD | `silero-vad` v5 (ONNX) | 2 MB | MIT | 自动断句、录音自动停止、停顿统计 |
| TTS 示范音 | `Kokoro-82M` (ONNX) | ~0.3 GB | Apache-2.0 | 24 kHz；英文 `af_heart`/`bf_emma`/`am_michael`，中文 `zf_xiaobei`/`zm_yunjian` |
| 音素对齐 + GOP（P2） | `MMS_FA` (wav2vec2 300M) | ~1.2 GB / ~1.5 GB | CC-BY-NC 4.0 ⚠️ | **仅个人学习用**；商用需换 `charsiu`/自制对齐模型 |
| 韵律分析（P2） | `Parselmouth`（Praat 引擎，Python） | 0 | GPL-3.0 ⚠️ | sidecar 内使用，独立进程不传染；若需闭源分发改自研 YIN |

**常驻合计 ≈ 4.4 GB / 16 GB**，余量留给 CosyVoice2（P4，~4 GB）。

## 被否决的候选

| 模型 | 否决原因 |
|---|---|
| `Qwen3-ASR-0.6B` | 官方推理栈为 PyTorch/Python，无稳定 ONNX/Rust 路径 → 不进主干（可作 sidecar 备选） |
| `Paraformer-zh` + CT-Transformer | 流式中文更强，但无英文能力，且我们需要的是"非流式 + 词级时间戳"，SenseVoice 已覆盖 |
| `distil-whisper-large-v3` | 仅英文，作为"纯英文极速通道"备选，P3 若延迟不满意再引入 |
| `Piper / VITS` 做示范音 | 韵律偏平、机械感强，不适合做"语调模仿"的参照 |
| `MFA`（Montreal Forced Aligner） | 依赖 Kaldi + conda，Windows 上维护成本过高 |
| `CosyVoice2` / `GPT-SoVITS` 进主干 | 只有 PyTorch 路径；音色复刻非必须（用户决策 D4）→ 推迟到 P4 可选 sidecar |
| 云端评测（Azure Pronunciation Assessment） | 隐私（口语录音）+ 离线要求；但可作为"分数校准"的对照基准（人工抽样） |

## 部署约定

```
C:/voice_models/
├── whisper-large-v3-turbo-{encoder,decoder}.onnx   # 或 ggml-*.bin（若走 whisper-rs 降级路线）
├── sensevoice-small/model.int8.onnx  tokens.txt
├── kokoro/{model.onnx, voices.bin, tokens.txt, espeak-ng-data/}
├── silero_vad.onnx
└── mms-fa/{model.onnx, vocab}          # P2
```

- 目录名即 `model_id`，`voice-cli doctor` 负责校验文件完整性（缺文件 → 明确报错，不静默降级到"半残"状态）。
- 权重不入 git；提供 `tools/fetch_models.ps1` 下载 + 校验 sha256。
- 所有模型**启动时一次性加载并常驻**（避免文档里"首次调用极慢"的问题）。
