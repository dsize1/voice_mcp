# Voice MCP Service — 文档索引

> 本地语音 I/O + 发音评测能力，以 MCP 形式暴露给 Pi，配合 Skill 实现英语口语（雅思 7 分目标）训练。
> 核心设计原则：**MCP 只做"测量"，Skill + LLM 做"教学"。**

## 1. 文档地图

| 文档 | 内容 | 状态 |
|---|---|---|
| [DESIGN.md](DESIGN.md) | 总体架构、分层、数据流 | 待编写（P2 前） |
| [CONTRACT.md](CONTRACT.md) | MCP 工具清单与 JSON 出入参契约 | ✅ 已对齐实现 |
| [adr/0001-runtime-strategy.md](adr/0001-runtime-strategy.md) | 运行时路线：当前 Rust 主干 + Python 评分 sidecar；长期 100% Rust | Accepted |
| [adr/0002-asr-routing.md](adr/0002-asr-routing.md) | ASR 双引擎与语言路由（含中英混说、教学语言分级） | Accepted |
| [adr/0003-model-selection.md](adr/0003-model-selection.md) | 模型选型、显存预算、放置路径 | Accepted |
| [adr/0004-audio-io-and-evidence.md](adr/0004-audio-io-and-evidence.md) | 音频输入/输出契约与留证机制 | Accepted |
| [adr/0005-deps-and-model-acquisition.md](adr/0005-deps-and-model-acquisition.md) | 第三方依赖与模型获取策略（网络/工具链实测与替代通路） | Accepted |
| [adr/0006-assessment-scope.md](adr/0006-assessment-scope.md) | 评测层能力边界（P2 词级诊断的三态推断） | Accepted |
| [adr/0008-technical-debt.md](adr/0008-technical-debt.md) | **技术欠债登记**（33 条债 + 触发条件 + 偿付顺序） | Living |
| [adr/0009-drill-library.md](adr/0009-drill-library.md) | 跟读素材库（Drill Library）与 NCE 语料接入 | Proposed (P4) |
| [security-supply-chain.md](security-supply-chain.md) | 模型与原生库供应链校验（工具+结论） | ✅ |
| [security-hashes.txt](security-hashes.txt) | 已知良好基线哈希 | ✅ |

## 2. 已确认决策速查

| # | 决策 | 说明 |
|---|---|---|
| D1 | 先跑通「Rust 主干 + Python 评分 sidecar」 | 100% Rust 进 ADR-0001 的后期路线 |
| D2 | ASR 双引擎 | 英文/中英混说 → whisper-large-v3-turbo；中文 → SenseVoice-Small |
| D3 | 教学语言随水平分级 | 薄弱用户中文讲解 + 英文示范；高阶全英文 |
| D4 | TTS 先只上 Kokoro | 音色复刻（CosyVoice2 / GPT-SoVITS）非必须，P4 可选 |
| D5 | 首训做基线录制 | 3 句诊断句 + 1 分钟自由说，写入 learner.json |
| D6 | 模型放 `C:/voice_models/` | C 盘固态，常驻加载 |
| D7 | 音频 I/O 以「文件优先」 | 支持 Win11 录音机 .m4a 直入；所有输出留音频 + 文字证据（见 ADR-0004） |

## 3. 路径约定

```
代码    : D:/code/voice_mcp_service/
模型    : C:/voice_models/<model-id>/          # 只读，不带 git
数据    : C:/voice_data/                       # 录音留证、档案、索引（可用 VOICE_HOME 覆盖）
  ├── sessions/YYYY-MM-DD/...                  # 每次训练一个目录
  ├── learner.json                             # 学习者档案（水平、易错音素、趋势）
  └── manifest.jsonl                           # 全局追加索引，可 grep 追溯
```

## 4. 阶段与当前进度

| 阶段 | 内容 | 验收 | 状态 |
|---|---|---|---|
| P0 | 环境验证：**`.m4a` 解码** ✅ / **sherpa-onnx 原生库** ✅ / **SenseVoice 真机转写** ✅ / **Kokoro 出声** ✅ / whisper 评估 / **供应链校验** ✅ | 4 个 spike 脚本跑通 | ✅ 完成 |
| P1 | `voice-core` 7 模块 + MCP 7 工具 + `voice-cli doctor/stt/say/play/rec/watch/tts-probe` | Pi 里说话→文字→出声回环 | ✅ 完成（`record` 待麦克风） |
| P2 | 评测层：**词级 diff** ✅ / **flow** ✅ / prosody / GOP scorer | 能定位到具体音素错误 | ⏳ 2/4 完成 |
| P3 | `ielts-speaking-coach` Skill + learner.json + 雅思题库 | 15 分钟完整训练闭环 | 未开始 |
| P4 | 热键听写、`record`(cpal+VAD)、CosyVoice2、HTTP、流式 | 按需 | 未开始 |
| P4 | **跟读素材库**（[ADR-0009](adr/0009-drill-library.md)）：NCE1–4 共 276 课 → 逐句切片 + 质量门槛 | 5 项全部通过（4 997 条） | ✅ 完成 |

## 5. 硬件基线（实测）

```
CPU  : Core Ultra 5 245K (Arrow Lake-S, 6P+8E)  — 桌面版无 NPU，方案不依赖 NPU
RAM  : 48 GB
GPU  : RTX 5070 Ti 16 GB, sm_120, driver 610.88  — 推理栈需 CUDA ≥12.8 编译
工具 : rustc 1.99.0 msvc ✅ / CUDA 13.3 ✅ / CMake ✅ / VS2026 cl.exe ✅
       ffmpeg ❌（不引入外部二进制依赖）/ Python 3.13（sidecar 另开 3.12 venv）
显存 : 常驻全部模型 ≈ 4.4 GB（余量充足，无需模型换入换出）
```

## 6. P0 实测结论（2026-11-18）

| 项 | 结果 | 证据 |
|---|---|---|
| `.m4a`(Win11 录音机) 解码 | ✅ 纯 Rust（symphonia），1.8 ms / 6.08 s 音频 | `crates/spike/src/bin/probe_audio.rs` |
| 输入规格 | AAC-LC, **16 kHz, mono, ~39 kbps**, 6.08 s, peak -0.2 dBFS | `test.m4a` 容器解析 |
| 声学质量 | 4–6 kHz 摩擦音带占 20% 能量，无硬低通；6–8 kHz 仅 1% → `band_limited=true` | 频谱分析 |
| sherpa-onnx 原生库 | ✅ v2.0.1 链接+运行，无 C++ 编译、无 CUDA 链接 | `sherpa-hello` |
| SenseVoice int8 真机 ASR | ✅ 81 ms 推理，RTF 0.013，**带 token 时间戳** | `asr-sensevoice` |
| 网络通道 | hf-mirror 8 路并发 ~1.1 MB/s；ModelScope 11–22 MB/s；GitHub 直连不可用；PyPI 镜像 2 s/17 MB | ADR-0005 |
| 待完成 | whisper-turbo（下载中 989 MB）、Kokoro 出声（369 MB 排队）、GPU EP（已降级为非阻塞） | – |

### P0 收尾补充（2026-11-18 深夜）

| 项 | 结果 |
|---|---|
| 模型下载 | 4 套全部完成，共 **1.58 GB**，367 个文件 **sha256 全对上上游清单** |
| Kokoro TTS | ✅ 24 kHz，54 个音色，CPU RTF **0.21**（2.16 s 音频用 469 ms）；慢速/常速双版落盘 |
| whisper-turbo | ✅ 可跑但 **CPU RTF 0.145**（6 s 音频 883 ms）；`enable_token_timestamps` 不可用（模型未导出 cross-attention），且本例文本不如 SenseVoice → 降为副通道 |
| espeak-ng | **sherpa-onnx ≥2.0 已不再使用**（语言由 `lexicon-*.txt` 决定），下载清单剔除 375 个 espeak 文件 |
| 供应链校验 | ✅ 完整性/ONNX 结构/数据文件/PE 静态扫描/PyPI 双镜像哈希全通过，详见 [security-supply-chain.md](security-supply-chain.md) |

**首次真机转写**（`test.m4a`）：

```
text: "I think this is the straight time."
timestamps: [1.86, 2.28, 3.24, 3.66, 4.08, 4.50, 5.04, 5.94]
```

若原句为 "I think this is the **third** time"，则 `third → straight` 正是典型的
/θ/ 被读成 /s/+t 的错误 —— 这就是 `check_pronunciation` 要自动捕捉的那类问题（词级 diff + 音素级定位）。
