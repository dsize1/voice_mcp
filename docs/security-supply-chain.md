# 模型与二进制依赖的供应链校验

> 校验工具：`tools/verify_models.py`（可随时重跑，`voice-cli doctor` 会调用它）

## 1. 威胁模型：我们到底在防什么

| 资产 | 是否可执行 | 真实风险 | 校验重点 |
|---|---|---|---|
| `.onnx` 模型权重 | ❌ 纯数据（protobuf） | ① **`external_data` 外部文件引用**（可让 ORT 读写任意路径）② **自定义算子域**（触发 DLL 加载）③ ORT 反序列化漏洞 ④ 模型被替换为"空壳/后门权重" | 结构检查 + metadata 自证 + 哈希 |
| `.bin`/`tokens.txt`/`espeak-ng-data` | ❌ 数据 | 伪装成数据投递可执行文件 | 扩展名白名单 + 魔数 |
| `sherpa-onnx-c-api.dll` / `onnxruntime.dll` | ✅ **原生代码** | 供应链植入、远控 API、静默外联 | PE 导入表 + 可疑 API + 内嵌 URL/IP + 签名 |
| PyPI 轮子（传输环节） | 容器 | 镜像投毒 | 双镜像 sha256 交叉比对 |
| HF 模型包（传输环节） | 容器 | 中间人改写/截断 | 逐文件 sha256 / git-sha1 比对上游清单 |

## 2. 检查结果（2026-11-18）

### A. 内容完整性 —— 全部通过

| 模型包 | 文件数 | 结果 |
|---|---|---|
| silero-vad | 1 | ✅ sha256 一致 |
| sense-voice | 2 | ✅ sha256 一致 |
| whisper-turbo | 3 | ✅ sha256 一致 |
| kokoro-multi-lang | 329+ | ✅ sha256 一致（下载完成后复验） |

比对依据：HF tree API 返回的 `lfs.oid`（大文件 sha256）与 `oid`（小文件 git blob sha1）。
这能抓住"截断/篡改/中间人替换"，但**不能**证明上游本身没被动过——见第 4 节残留风险。

### B. ONNX 结构 —— 全部通过，且 metadata 自证为官方权重

| 模型 | 节点/权重块 | 算子域 | 外部数据 | metadata 自证 |
|---|---|---|---|---|
| sense-voice int8 | 10206 / 1479 | `ai.onnx` 13 | 无 | `model_type=sense_voice_ctc`、`vocab_size=25055`、`model_author=iic`、`url=huggingface.co/FunAudioLLM/SenseVoiceSmall`、`onnx.infer=onnxruntime.quant` |
| whisper-turbo encoder int8 | 4610 / 899 | `ai.onnx` 13 | 无 | `n_mels=128`、`n_audio_state=1280`、`n_audio_layer=32`、`n_text_layer=4`、`n_vocab=51866` —— 与 large-v3-turbo 规格逐项吻合 |
| whisper-turbo decoder int8 | 2575 / 152 | `ai.onnx` 13 | 无 | `onnx.infer=onnxruntime.quant` |
| silero-vad | 4 / 1 | `ai.onnx` 16 | 无 | – |

- **无任何自定义算子域** → 不会触发自定义 op DLL 加载。
- **无任何 `external_data` / `data_location=EXTERNAL`** → 不会读写模型目录以外的文件。
- 节点数与权重块数量级合理，配合 `onnxruntime.quant` 标记，说明是"官方权重 + 标准 int8 量化"的产物，不是空壳模型。

### C. 数据文件 —— 通过

- 全部文件扩展名在白名单内（`.onnx/.txt/.bin/.json/.fst/.wav/...`）。
- 无任何数据文件带可执行魔数（MZ / ELF / Mach-O / shebang / ZIP 头）。

### D. 原生库 —— 通过，2 项需知情

| DLL | 大小 | 导入函数 | 依赖 | 可疑 API | 硬编码 IP |
|---|---|---|---|---|---|
| `onnxruntime.dll` | 17.8 MB | 184 | ADVAPI32, KERNEL32, SETUPAPI, api-ms-win-core-path, dbghelp, dxgi | ✅ 未命中 23 项清单 | ✅ 无 |
| `sherpa-onnx-c-api.dll` | 4.3 MB | 117 | KERNEL32, onnxruntime | ✅ 未命中 | ✅ 无 |
| `sherpa-onnx-cxx-api.dll` | 0.3 MB | 233 | KERNEL32, sherpa-onnx-c-api | ✅ 未命中 | ✅ 无 |

可疑 API 清单覆盖：进程注入（`CreateRemoteThread`/`VirtualAllocEx`/`WriteProcessMemory`/`NtUnmapViewOfSection`）、
静默下载（`URLDownloadToFile`/`InternetOpenUrl`/`WinHttpOpen`）、
提权与持久化（`AdjustTokenPrivileges`/`CreateService`/`RegSetValueEx`/`LsaOpenPolicy`）、
执行（`WinExec`/`ShellExecute`）。

内嵌字符串中的 URL 全部核验完毕：

- `onnxruntime.dll` 38 个：arxiv 论文、numpy/pytorch/scipy 文档、Wikipedia、`tinyurl.com/sudb9s96`（已提取上下文："*To prevent numerical overflow, scale Q,K by sqrt(scale) before matmul*" —— Attention 算子文档串，良性）。
- `sherpa-onnx-c-api.dll` 19 个：k2-fsa / csukuangfj / HuggingFace 的**模型导出文档链接**（错误提示用）。

**结论：没有任何外联、下发、注入、提权行为特征。**

### E. PyPI 轮子 —— 通过（双镜像交叉比对）

```
sherpa_onnx_core-2.0.1-py3-none-win_amd64.whl
本地 sha256 = 08890e9ea23ed2f59f81d5840c63b6b5cc8674d0f6e48d9069126a848aa0e022
清华镜像   ✅ 一致
阿里云镜像 ✅ 一致
```

两个独立镜像给出相同哈希，说明传输链路未被单点篡改。

## 3. 已知警告（非缺陷，知情即可）

| 警告 | 说明 | 处置 |
|---|---|---|
| 三个 DLL 均 `NotSigned` | 开源 PyPI 轮子常态（未购买代码签名证书） | 接受；改用哈希+静态扫描作为替代保证 |
| onnxruntime 导入 `dxgi.dll` | DML/DirectX 相关代码路径，CPU 版同样编译进同一 DLL | 无害 |
| 非白名单 URL | 已逐条人工核验，全是文档/论文链接 | 无 |

## 4. 残留风险与缓解

| 风险 | 级别 | 缓解 |
|---|---|---|
| hf-mirror 是**单一信任域**：哈希清单和文件都来自它，理论上可同时伪造 | 低-中 | ① metadata 自证权重规格（已做）；② 首次转写结果与已知音频的语义一致性验证（已做：能正确识别英文）；③ 后续可从 ModelScope 取同一上游模型做交叉指纹比对 |
| DLL 无签名，无法验证"这份二进制确实由 k2-fsa 构建" | 低 | 哈希锁定（记录在 `docs/security-hashes.txt`）+ 版本固定 + 升级时重跑校验 |
| ONNX Runtime 反序列化层是攻击面 | 低 | 模型来自官方量化产物且结构校验通过；运行时以普通用户权限执行，不服务外部输入 |
| 模型/服务被用于处理他人音频 | – | 全程本地离线，无任何出站网络请求（服务本身不发起网络） |

**建议的运维习惯**：

1. 每次换机器/升级模型后跑 `python tools/verify_models.py`（失败即退出码 1，可接 CI）。
2. 把首次通过校验的哈希写入 `docs/security-hashes.txt` 并入库，作为"已知良好"基线。
3. 服务运行在普通用户权限下；不要用管理员身份常驻。
4. 若将来启用"模型热替换/远程拉取"，必须先过同一套校验，禁止自动信任。
