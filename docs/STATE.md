# 当前状态与下一步（交接用）

> 写于 2026-11-19。新会话先读这一页，再读 README / CONTRACT / ADR。

## 一句话现状

**P0/P1/P2(1+2)/P3-Skill 已完成并在真机跑通；下一步是 P3 的"完整训练闭环"实跑。**
`crates` 5000+ 行 Rust，61 个单测 + 7 个录音回归用例全绿，零编译警告，已推送 origin/main。

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
./target/release/voice-cli.exe assess <audio|last> --reference "原句"
./target/release/voice-cli.exe lesson <mp3> --lrc <lrc> --verbose
VOICE_CASES_DIR=/d/voice_data/cases_audio ./target/release/voice-cli.exe cases
```

## 阻塞与前置条件

1. **蓝牙耳机必须处于连接状态**才能在系统里看到输入设备（休眠时 `mic` 会明确报错）。
   设备名会带 `2- ` 之类前缀且会变 → 配置用品牌子串 `EDIFIER`，匹配器已做"宽松匹配 + 宽带优先"。
2. Skill 需要 `/reload`（或重启 pi）才出现在会话里；`pi -p "列出可用 skills"` 可验证。
3. 没有麦克风时一切文件路径功能仍可用（`assess path=/stt`），只是不能 `record`。

## P3 待办（真机训练闭环）

- [ ] 1. `/reload` → 确认 `mcp__voice__*` 8 个工具与 2 个 Skill 都在
- [ ] 2. 建档：3 句诊断（`I think this is the third time.` / `He lives in a very small village.` / `Would you like a cup of tea?`）+ 30–60 s 自由说 → 写 `$VOICE_HOME/learner.json`
- [ ] 3. 单轮训练：`synthesize(slow_too, play)` → `record(silence_sec=2.2)` → `assess(reference=句)` → 只挑 1 个问题 → 重读 `scene=retry` 对比
- [ ] 4. 验证留证完整：`bank` 之外的 `sessions/<date>/<id>/takes/` 应同时有 `.raw48000.wav` + `.16k.wav` + `.asr.json` + `.metrics.json` + `.md`
- [ ] 5. 验证 Skill 遵守铁律：`hint=unclear` 不下结论、`likely_asr_miss` 不指责、一次只 1–2 个问题
- [ ] 6. 修掉实跑中暴露的问题（这是 P3 的主要产出）

## 已知债务（详见 ADR-0008）

S1 级已全部偿还。剩余主要项：
D04 whisper 路径无回归 · D06 音色表未人耳校准 · D09/D12 韵律与 GOP（ADR-0007，触发：≥10 次真实训练）
D15 全局锁串行 · D18 DLL 手工复制 · D20/D21 CI（有 JSON 用例但无合成 fixture）· D29 Skill 分发 · D35 蓝牙端点选择 · D36 AGC 强导致能量维度不可用于重音 · D38 分块边界可能切词

## 下一批工作（用户已确认顺序）

1. **P3 真机训练闭环**（本会话的下一步）
2. P4 跟读素材库（ADR-0009）：用户会先写脚本拉全 NCE1–4 资源，然后一起做 `bank ingest/list/show/drill`
