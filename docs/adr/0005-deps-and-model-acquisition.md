# ADR-0005: 第三方依赖与模型获取策略（中国网络环境）

- 状态：Accepted
- 日期：2026-11-18
- 相关：ADR-0001（运行时）、ADR-0003（模型选型）

## 背景：P0 实测的网络/工具链事实

| 通道 | 实测 | 结论 |
|---|---|---|
| `github.com` 直连 | 超时 / reset | ❌ 不可用 |
| `gh-proxy.com` 代理 GitHub | ~88 KB/s（15 分钟只下到 33/122 MB） | ⚠️ 仅小文件可用 |
| `huggingface.co` 直连 | 10s 超时 | ❌ 不可用 |
| `hf-mirror.com` 单连接 | 78–200 KB/s | ⚠️ 勉强 |
| `hf-mirror.com` **8 路分段并发** | **~1.1 MB/s** | ✅ 模型主通道 |
| `modelscope.cn` 直连 | **11–22 MB/s** | ✅ 最快，但缺 sherpa-onnx 格式模型包 |
| PyPI 清华镜像 | 17 MB 轮子 ~2 s | ✅ 二进制依赖主通道 |
| `crates.io` | 已配置 rsproxy 镜像 | ✅ 正常 |
| CUDA Toolkit | 已装 13.3 | ⚠️ 与部分 `-sys` crate 期望版本不符 |
| MSVC | VS2026 `cl.exe` / `lib.exe` / `dumpbin` 可用 | ✅ 可做导入库生成 |

## 决策

### D1. 原生推理库：sherpa-onnx 共享库，来自 PyPI 轮子（不编译 C++、不链 CUDA）

`sherpa-onnx-sys` 默认从 GitHub Releases 拉 122 MB 预编译包——本机不可用。
改为：

```
tools/fetch_deps.ps1
  └─ pip download sherpa-onnx-core==2.0.1 (PyPI 清华镜像, 17 MB)
     └─ 解压 sherpa_onnx/lib/{sherpa-onnx-c-api.dll,.lib, onnxruntime.dll}
        └─ dumpbin -exports + lib /def  → 生成 onnxruntime.lib（轮子里缺 import lib）
           └─ .cargo/config.toml: SHERPA_ONNX_LIB_DIR=.cache/sherpa/lib
              └─ sherpa-onnx crate 使用 shared 特性链接
```

实测结果：`sherpa_onnx::version() == 2.0.1`，链接与运行成功，全程无 C++ 工具链参与、无 CUDA 链接。

**副作用**：该 `onnxruntime.dll` 是 **CPU** 版（17.7 MB）。GPU 化是 P0 的遗留问题：

- 方案 G1：`sherpa-onnx-core` 的 GPU 变体（k2-fsa 自建索引 / 替换为 GPU 版 onnxruntime.dll，需 ORT 主版本一致）。
- 方案 G2：nvidia 提供的 `onnxruntime-gpu` pip 轮子里的 `onnxruntime.dll` 直接替换（同一 ORT 主版本时 ABI 兼容）。
- 方案 G3：CPU 就够 → SenseVoice int8 ~70 ms/10 s、Kokoro RTF ≪1，只有 whisper 需要 GPU。
- **决策：先按 G3 把 P1 跑通**（普通跟读/听写全部 CPU 达标），P2 再验证 G1/G2；whisper 若 CPU 不达标，先用 `distil-whisper` / SenseVoice-for-English 过渡。

### D2. 模型只从 hf-mirror 多连接拉取，格式统一用 sherpa-onnx 包

`tools/fetch_models.ps1`：读 HF tree API 拿文件清单 → 按 `Include` 正则裁剪 → 8 MB 分片、8 路并发、断点续传、大小校验。
只拉最小集合（int8 优先，跳过 `.weights` / `test_wavs`）：合计约 **1.6 GB**。

| 集合 | 仓库 | 需要的文件 | 大小 |
|---|---|---|---|
| `vad` | `onnx-community/silero-vad` | `onnx/model.onnx` | 2 MB |
| `sensevoice` | `csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17` | `model.int8.onnx` + `tokens.txt` | 228 MB |
| `whisper` | `csukuangfj/sherpa-onnx-whisper-turbo` | `turbo-{encoder,decoder}.int8.onnx` + tokens | 989 MB |
| `kokoro` | `csukuangfj/kokoro-multi-lang-v1_0` | `model.onnx` + `voices.bin` + `tokens.txt` + `espeak-ng-data/` | 369 MB |

### D3. ModelScope 作为备选加速通道，但不用它的 Kokoro

ModelScope 上 `onnx-community/Kokoro-82M-v1.0-ONNX` 可 22 MB/s 拉取，但 **没有 sherpa-onnx 需要的 `voices.bin` / `tokens.txt` / `espeak-ng-data` 布局**（是 HF transformers 布局）。
自建转换有格式风险，收益（省 6 分钟）与风险不成比例 → 不用。若将来换 ONNX 自研路径（路线 A），再启用该通道。

### D4. 依赖可重放

`fetch_deps.ps1` / `fetch_models.ps1` 均为幂等脚本（已存在且大小正确则跳过），`.cache/` 与模型目录不入 git。
换机器只需两条脚本 + `cargo build`。

## 后果

- ✅ 在当前网络环境下没有"不可完成"的外部依赖，全部有实测通路的替代方案。
- ⚠️ GPU EP 尚未打通，`provider` 字段在配置里保留（`cpu` / `cuda` / `coreml`），P2 补。
- ⚠️ 依赖 PyPI 轮子的二进制布局（`sherpa_onnx/lib`），升级版本时需同步 `SHERPA_ONNX_LIB_DIR` 里的 dll 与 crate 版本。`voice-cli doctor` 会校验 `sherpa_onnx::version()` 与 Cargo 版本一致。
