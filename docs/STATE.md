# 当前状态与下一步（交接用）

> 写于 2026-11-19，最后更新 2026-11-22（GOP/词级诊断 + 真机训练夜）。新会话先读这一页，再读 README / CONTRACT / ADR。

## 一句话现状

**P3 闭环 + P4 素材库（4 997 条 NCE）+ 音素级 GOP（词级口径）均已上线并在真机验证。**
`crates` 7700+ 行 Rust，**97 个单测 + 9 个录音回归用例**全绿，零编译警告。
下一步：雅思素材入库 + 按 `error_stats` 做定向训练。

## 已实测的关键数字（别再猜）

| 项 | 数值 |
|---|---|
| ASR SenseVoice int8 | 6 s 音频 82 ms（RTF 0.013）；81 s 音频分块后 2.3 s（RTF 0.028） |
| TTS Kokoro 82M | RTF 0.21，24 kHz，54 音色（`af_heart`=sid 3 已验证） |
| 引擎冷启动 | 1.7 s（ASR 与 TTS 并行加载） |
| `.m4a`/mp3 解码 | 纯 Rust（symphonia），6 s 音频 1.8 ms |
| 麦克风（EDIFIER W820NB 蓝牙 HFP 端点） | 48 kHz 单声道 F32 可用；底噪 p50 −54.7 / p99 −46 dBFS（**偏高，AGC 强**） |
| 长音频 | **必须分块**（>30 s 单次解码会静默截断 + 时间戳压到 0.77 倍） |
| 教材对齐精度 | NCE2 L2：19/19 行文本相似度 1.00，行起始偏差 +0.07~+0.29 s，LRC 拟合 scale 0.999 |
| **音素 CTC（wav2vec2-espeak）** | ONNX 1 205 MB，CPU **RTF 0.04–0.05**；int8 量化漂移 8.21 → 弃用 |
| **GOP 判别力（so762 未见说话人）** | 句级 ρ 0.67；**词级 mean-GOP：AUROC 0.887、P@FP10 0.457**；音素级 0.769/0.170（基率上限 0.45） |
| **MDD 微调（6ep/8L）** | 词级 AUROC 0.883→0.887、音素级 0.690→0.769；训练 65 s/epoch（RTX 5070 Ti cu128） |
| **混淆对信号（TH→s）** | 命中精确率 0.50、召回 0.67（仅 TH 系列可对用户说） |
| 素材库 | 4 997 条 / 7.1 h，quarantine 1 221，可直接练 3 368 条 |
| 素材库（P4，全量） | **4 997 条 / 7.1 h**，quarantine 1 221，可直接练 3 384；ingest 276 课 ≈ 10.5 min |

## 环境

```
模型   C:/voice_models     （1.58 GB，9 个文件，已供应链校验）
数据   D:/voice_data       （config.json 里 mic="EDIFIER"，silence=2.2 s）
素材   D:/voice_data/cases_audio （回归用例音频，VOICE_CASES_DIR 指向它）
代码   D:/code/voice_mcp_service
Pi     ~/.pi/agent/mcp.json 里 voice = target/release/voice-mcp.exe（exposure=direct, timeout=180）
Skill  tools/install_skills.ps1 已 junction 到 ~/.pi/agent/skills/
```

## 常用命令

```bash
cargo build --release && cp .cache/sherpa/lib/*.dll target/release/
./target/release/voice-cli.exe doctor --load     # 12 项自检
./target/release/voice-cli.exe mic --secs 3      # 麦克风能力 + 底噪 + 阈值建议
./target/release/voice-cli.exe rec --max-sec 20 --stt --lang en
./target/release/voice-cli.exe assess <audio|last> --reference "原句" --with-gop   # 词级+音素级 GOP + TTS 基线
./target/release/voice-cli.exe gop <audio> --text "原句"                          # 只跑 GOP（看 heard/confusion）
./target/release/voice-cli.exe bank pick --tag theta_to_s --level 2 --limit 3     # 按 tag 权重取材
./target/release/voice-cli.exe bank drill nce1/019/003                            # 播原生音频
./target/release/voice-cli.exe lesson <mp3> --lrc <lrc> --verbose
VOICE_CASES_DIR=/d/voice_data/cases_audio ./target/release/voice-cli.exe cases
```

## 阻塞与前置条件

1. **蓝牙耳机必须处于连接状态**才能在系统里看到输入设备（休眠时 `mic` 会明确报错）。
   设备名会带 `2- ` 之类前缀且会变 → 配置用品牌子串 `EDIFIER`，匹配器已做"宽松匹配 + 宽带优先"。
2. Skill 需要 `/reload`（或重启 pi）才出现在会话里；`pi -p "列出可用 skills"` 可验证。
3. 没有麦克风时一切文件路径功能仍可用（`assess path=/stt`），只是不能 `record`。

## P3 真机训练闭环（2026-11-21 已实跑验收）

设备：EDIFIER W820NB（蓝牙 HFP，48 kHz 单声道）。留证：`sessions/2026-11-21/*_drill/takes/`。
会话小结：`sessions/2026-11-21/233336_drill/session.md`。建档：`D:/voice_data/learner.json`。

- [x] 1. `mcp__voice__*` 8 个工具与 2 个 Skill 都在（`pi -p "列出可用 skills"` 实测 3 个 skill，含 archify）
- [x] 2. 建档：3 句诊断（accuracy 1.00 / 1.00 / 1.00）+ retry → `learner.json`（band 6.0，confidence low：未做自由说）
- [x] 3. 单轮训练：`synthesize(slow_too, play)` → `record(silence_sec=2.2)` → `assess(reference=句)` → 只报 1 条（语速 214.7 wpm）→ `retry` 对比（192.7 wpm，断点 0.96 s）
- [x] 4. 留证完整：同一 take 目录内 `.raw48000.wav` + `.16k.wav` + `.asr.json` + `.metrics.json` + `.md`（+ `.capture.json`）；重复评测留 `.metrics.prev.json`
- [x] 5. Skill 铁律在实测中全部生效：`unclear` 不下结论、whisper（`timestamp_alignment=missing`）不报秒数、单轮只报 1 条
- [x] 6. 实跑暴露的问题已修（共 5 个）：`slow_too` 语速对、assess 证据分裂、中文 panic、16 kHz 派生污染 `band_limited`、英美拼写变体与 ASR 伪缩略误报

**仍缺**：自由说 30–60 s（用户跳过）→ `band_estimate` 置信度仍是 low；`record` 的 8 s 起始等待不可配置（真机连说 4 次“开始”才进齐）。

## 已知债务（详见 ADR-0008）

S1 级已全部偿还。剩余主要项：
D04 whisper 路径无回归 · D06 音色表未人耳校准 · D09/D12 韵律与 GOP（ADR-0007，触发：≥10 次真实训练）
D15 全局锁串行 · D18 DLL 手工复制 · D20/D21 CI（有 JSON 用例但无合成 fixture）· D29 Skill 分发 · D35 蓝牙端点选择 · D36 AGC 强导致能量维度不可用于重音 · D38 分块边界可能切词
**新增** D39 `record` 起始等待（8 s）不可配置 → 真机常见“说了但没开始”误报 · D40 教材导语不在 LRC 里 → 每课固定 10–12 条 `extra` 噪声 · D41 英美变体表需人工维护（只覆盖 50 对高频词）

## 下一批工作（用户已确认顺序）

1. ~~**P3 真机训练闭环**~~（2026-11-21 完成）
2. ~~**P4 跟读素材库**~~（2026-11-22 全量完成）：`bank ingest/list/show/drill/stats/retag`，NCE1–4 共 276 课 → 4 997 条
3. 补一次自由说 30–60 s（把 `learner.json` 的 band 置信度从 low 拉起来）

## GOP / 诊断层现状（2026-11-22）

**能力**：`assess(with_gop=true)` 返回
`gop.words[]`（词级，对外）、`gop.phones[]`（音素级，仅排序）、`gop_baseline`（TTS 标准发音对照）、
`gop.phones[].confusion`（混淆对，如 `TH→s`）。

**硬规则（SKILL 4.0/4.1 已固化）**：
1. `degraded=true`（snr<13 / peak<−30 dBFS / 削波>0.1%）→ **不报任何发音结论**，只给重录建议；
2. 词级 vs 基线：`Δ ≤ −3 且基线 ≥ 4` 才可说"低于示范"；基线 <0.3 的词一律不报（模型偏好）；
3. 音素级禁止断言；只有 `confusion` 且 `TH→s` 允许说"模型听到的是 /s/ 而不是 /θ/"；
4. 整词替换靠词级 diff（GOP 会漏，见 `cases/r09_why_when.json`）。

**已知限制**：音素级精确率天花板 0.45（基率 7.5%）；CTC spike 导致每个音素只有 1–2 帧，
**不能用时长判断元音长短**；MDD 进一步提升空间 ≈0（词级已贴天花板），下一步方向是"降基率"
（可疑词定向重录复核）。

## 真机训练记录（2026-11-22 夜）

`D:/voice_data/sessions/2026-11-22/`，`learner.json` history 11 条。已修：
`why→when`（Δ −4.27→−0.39）、`thank` 的 /θ/（TH→s → heard θ，Δ −0.05）、`valley`（acc 0.83→1.00）。
待练：`short_long_vowel`（/ɪ/ vs /iː/，seat 第 1 次仍读成 sɪt）、`ae_to_e`（sit→set）、`village` 的 /ɪlɪ/。

**唯一的硬件障碍**：EDIFIER W820NB 的麦克风增益在同小时内漂移 −11.8 ~ −33.5 dBFS，
开练前先 `rec` 说句 test 确认 `peak` 在 −20 ~ −12 dBFS。
