#!/usr/bin/env python
"""misaki-zh 中文 G2P sidecar（Kokoro 的官方前端，可选）。

协议（与 Rust 侧 `SidecarBackend` 一致）：
  stdin  : 每行一句（**只传纯中文段**；英文/数字由 Rust 侧的 G2P 负责）
  stdout : 每行一个音素串（Bopomofo 风格 IPA + 声调箭头 ↓↗↘→，与 Kokoro tokens.txt 同一套符号）
  stderr : 加载日志与错误信息（Rust 侧只看 stdout 行数）

为什么是 sidecar 而不是 Rust 依赖：
  - crates.io 上**没有** misaki crate（2026-11-27 核实：`crate 'misaki' does not exist`）；
  - misaki 只有 Python 版（0.7.4），中文需要 pypinyin + jieba + cn2an。
默认仍用引擎的 `lexicon-zh`（零外部依赖），本脚本只在显式配置时启用。

启用：
  pip install "misaki[zh]"
  set VOICE_G2P_CMD_ZH=python D:\\code\\voice_mcp_service\\tools\\g2p_misaki.py
  set VOICE_G2P_TIMEOUT_MS=8000        # 首次调用要加载 jieba 词典，放宽超时

已验证的坑（都在这段代码里处理掉）：
  1. jieba 首次加载会往 **stdout** 打印 "Building prefix dict..." →
     把 fd 1 重定向到 stderr，只用一个 dup 出来的"真 stdout"写音素行；
  2. Windows 默认 GBK：写 IPA 会抛 `gbk codec can't encode` → stdout/stdin 显式 UTF-8；
  3. 数字 + 空格 + 量词（"2026 年"）misaki 内部 cn2an 会抛 ValueError →
     数字交给 Rust 侧读，本脚本只吃纯中文；
  4. 单行失败不能拖垮整批 → 输出空行，Rust 侧对空行回退 lexicon-zh。
"""

import logging
import os
import sys


def main() -> int:
    # 1) 先把 fd 1 指向 stderr，再用 dup 出来的真 stdout 只写音素行
    real_fd = os.dup(1)
    os.dup2(2, 1)
    out = os.fdopen(real_fd, "w", encoding="utf-8", newline="\n", buffering=1)
    # 2) UTF-8 显式化（Windows 默认 GBK）
    sys.stdin = os.fdopen(sys.stdin.fileno(), "r", encoding="utf-8", newline="")
    try:
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass

    try:
        import jieba
        from misaki import zh

        jieba.setLogLevel(logging.ERROR)
    except Exception as exc:  # pragma: no cover - 环境问题
        sys.stderr.write(f'misaki 未安装：{exc}\n先执行 pip install "misaki[zh]"\n')
        return 2

    g2p = zh.ZHG2P()
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            out.write("\n")
            continue
        try:
            out.write(g2p(line).replace(" ", "") + "\n")
        except Exception as exc:  # 单行失败：空行，让 Rust 侧回退 lexicon-zh
            sys.stderr.write(f"g2p 失败: {line!r}: {exc}\n")
            out.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
