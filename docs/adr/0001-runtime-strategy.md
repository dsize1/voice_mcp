# ADR-0001: 运行时路线 —— 当前混合（Rust 主干 + Python 评分 sidecar），目标 100% Rust

- 状态：Accepted（当前阶段 = 路线 B；路线 A 标记为 Planned，不进 P0–P3）
- 日期：2026-11-18
- 相关：ADR-0003（模型选型）、ADR-0004（I/O 契约）

## 背景

需求要求「基于 Rust 开发 MCP 服务，包括客户端服务」。但系统真正的难点不是 ASR/TTS（都有成熟 ONNX 模型，Rust 侧 `ort` / `sherpa-onnx` 可直接调用），
而是**发音评测**：音素级强制对齐 + GOP 打分 + 韵律（F0/能量/时长）分析。

生态现状：

| 能力 | 成熟 Python 方案 | 成熟 Rust 方案 |
|---|---|---|
| 音素强制对齐 | torchaudio `forced_align` + MMS_FA；MFA | 无现成 crate（需自己实现 CTC Viterbi） |
| 音素后验 / GOP | wav2vec2 CTC 后验直接可得 | 需自写特征前处理 + 对齐 + 归一化 |
| 韵律分析 | Parselmouth（Praat 引擎） | 只有 YIN/PYIN 类基频检测，无 Praat 级配套 |
| ASR | 多 | `ort` / `sherpa-onnx` / `whisper-rs` ✅ |
| TTS | 多 | `ort`（Kokoro）/ `sherpa-onnx` ✅ |

即：**ASR/TTS 有 Rust 路径，评测算法没有。**

## 决策

### 当前（P0–P3）：路线 B —— Rust 主干 + Python 评分 sidecar

```
voice-mcp (Rust, 常驻)  ──stdio/json──►  voice_scorer (Python, 惰性启动)
  ├ audio  : cpal / rodio / rubato / hound / symphonia
  ├ asr    : ort 或 sherpa-onnx
  ├ tts    : ort (Kokoro)
  ├ vad    : ort (Silero)
  └ score  : 词级 diff、flow、基础 prosody（纯 Rust，永远可用）
```

约束：

1. **Python 只做"算法"，不做协议、不做 I/O、不做状态管理。** sidecar 是一个无状态函数服务器：进音频+参考文本，出 metrics JSON。
2. **必须优雅降级。** scorer 未安装 / 崩溃 / 超时（>3s）时，`check_pronunciation` 退化为"词级 diff + whisper 置信度"，工具契约与 JSON schema 不变，只把 `phonemes` 字段置空并带 `degraded: true`。
3. **sidecar 通过本地进程 + JSON-RPC over stdio 通信**（不用 HTTP，避免端口与防火墙问题）；启动超时 8s，失败则本次会话不再重试。
4. Python 环境独立：`scorer/.venv`（Python 3.12，torch cu12x）。

### 目标（P4 之后）：路线 A —— 100% Rust

迁移触发条件（满足任一即启动）：

- sidecar 的冷启动/Python 依赖成为部署痛点（换机器、离线分发、CI）；
- GOP 算法在 sidecar 阶段已被验证稳定（分数与人工听感一致性 >85%），数值定型，只需移植；
- 需要单二进制分发。

路线 A 需要自研的三块（**已记录，勿在 P0–P3 提前投入**）：

| 模块 | 工作内容 | 预估 |
|---|---|---|
| A1 CTC 强制对齐 | wav2vec2/MMS_FA 导出 ONNX（含降采样因子、帧移、blank 处理）→ Rust 实现 Viterbi 强制对齐（窄带状态剪枝） | 3–4d |
| A2 GOP | 帧级 log-softmax 后验 → 音素段归一化打分（比例/对数/几何三种口径取与 sidecar 对齐的那个） | 1–2d |
| A3 韵律 | 迁移到 Rust：YIN/自相关基频 + 能量包络 + 时长统计；Praat 特有的抖动/谐噪比若不必要则砍掉 | 2–3d |

验收等价性：对 sidecar 阶段保存的 200 条已标注样本，路线 A 输出的音素分数与 sidecar 的**排序一致性 ≥0.9**（Spearman）。

## 被否决的方案

- **路线 C：Python 写 MCP，Rust 只写客户端** —— 违背需求，且 Python 在 Windows 上的 CUDA/依赖坑会在换机器时反复爆炸。
- **一步到位路线 A** —— 在算法尚未验证的阶段先重写算法，等于把"调分数"和"调对齐"的 bug 混在一起，P2 工期会从 5–7d 涨到 14d+。

## 后果

- ✅ P1 能立刻闭环（不依赖 Python），P2 精度可独立迭代。
- ⚠️ 需要维护两套环境；用 `voice-cli doctor --scorer` 显式检查 sidecar 状态，避免"静默降级"被误当成"发音变好了"。
- ⚠️ JSON schema 必须从第一天就按"最终形态"定（含 `phonemes` / `degraded` 字段），否则迁移时契约破坏。
