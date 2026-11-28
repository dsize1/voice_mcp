#!/usr/bin/env python
"""生成 `crates/voice-core/data/pinyin-zh.txt`：汉字 → 拼音（带调号）表。

为什么要有这个数据文件（D13/M5）：

评测侧的「拼音层诊断」需要把**汉字对**（参考字 vs 识别字）翻译成
「声母/韵母/声调哪一层不一样」。英文那套用 CMUdict + 音形编码，
中文没有任何等价的运行时数据源，所以把表**预先算好、随源码固化**，
运行时纯 Rust 解析（`include_str!`），不引入 Python 依赖、不依赖模型目录。

数据来源与口径
--------------
* 权威源：`pypinyin`（MIT）的 `pinyin-data` / `phrase-pinyin-data`，即
  汉语拼音方案的标准读音表。这里用 `heteronym=True` 取**全部读音**：
  多音字必须保留多个候选，否则「行 háng 读对了却按 xíng 判错」。
* 风格：`Style.TONE3` —— 声调用数字后缀（`yin2`、`de5`），`ü` 写作 `v`
  （`nv3` = nǚ），零声母不加标点。这样 Rust 侧只需 `rsplit_once` 即可拆
  base / tone，不必处理变音符号。
* 声调 5 = 轻声。

为什么不用 `C:/voice_models/kokoro-multi-lang/lexicon-zh.txt`：
那份表是**TTS 前端**用的音素串（`银 i ↗ n`），逐字只有一个读音
（实测 8 个多音字错 7 个），拿它判"你把 háng 读成 xíng"会直接误判。
TTS 侧的多音字问题由 misaki-zh sidecar 解决，评测侧用本表的多读音候选。

用法
----
    pip install pypinyin
    python tools/gen_pinyin_table.py            # 覆盖写数据文件
    python tools/gen_pinyin_table.py --check    # 只校验，不写（CI/复核用）

`--check` 会在数据文件与当前 pypinyin 版本不一致时以非 0 退出，
这样「表是哪来的」这件事永远可复现（对齐 `docs/dataset-pins.json` 的做法）。
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "crates" / "voice-core" / "data" / "pinyin-zh.txt"

HEADER = """\
# 汉字 → 拼音（带调号，多读音用逗号分隔）· 评测侧拼音层诊断用（D13/M5）
# 生成：tools/gen_pinyin_table.py  （数据源 pypinyin {version}，MIT）
# 口径：Style.TONE3 + heteronym（多音字全部保留）+ ü→v；声调 5 = 轻声
# 只读数据：不要手改。改了拼音规则请改生成脚本并重跑，否则 --check 会失败。
"""


def char_range() -> list[str]:
    """基本区 + 扩展 A + 兼容区里常用的汉字范围（与 normalize::is_cjk_char 对齐）。"""
    out: list[str] = []
    for lo, hi in ((0x4E00, 0x9FFF), (0x3400, 0x4DBF)):
        for cp in range(lo, hi + 1):
            out.append(chr(cp))
    return out


def build() -> tuple[str, dict[str, list[str]]]:
    import pypinyin
    from pypinyin import Style, pinyin

    rows: dict[str, list[str]] = {}
    chars = char_range()
    # 逐字查询（pypinyin 对非汉字返回 errors 值，这里用空串过滤）
    for i in range(0, len(chars), 512):
        chunk = chars[i : i + 512]
        got = pinyin(
            chunk,
            style=Style.TONE3,
            heteronym=True,
            neutral_tone_with_five=True,
            errors=lambda _s: [""],
        )
        for ch, readings in zip(chunk, got):
            cleaned = [r for r in dict.fromkeys(readings) if r and r.isascii()]
            if cleaned:
                rows[ch] = cleaned
    lines = [HEADER.format(version=pypinyin.__version__).rstrip("\n")]
    for ch in sorted(rows):
        lines.append(f"{ch}\t{','.join(rows[ch])}")
    return "\n".join(lines) + "\n", rows


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true", help="只比对，不写文件")
    args = ap.parse_args()

    text, rows = build()
    multi = sum(1 for v in rows.values() if len(v) > 1)
    tones = {t: 0 for t in "12345"}
    for v in rows.values():
        for r in v:
            t = r[-1]
            if t in tones:
                tones[t] += 1
    print(
        f"[pinyin] 汉字 {len(rows)} 条（多读音 {multi} 条），"
        f"声调分布 1–4/轻声: {tones['1']}/{tones['2']}/{tones['3']}/{tones['4']}/{tones['5']}，"
        f"文件 {len(text.encode('utf-8')) / 1024:.0f} KiB"
    )

    if args.check:
        if not OUT.is_file():
            print(f"[pinyin] 缺少 {OUT}", file=sys.stderr)
            return 2
        cur = OUT.read_text(encoding="utf-8")
        # 版本行会随 pypinyin 升级而变，比对时忽略
        strip = lambda s: "\n".join(  # noqa: E731
            ln for ln in s.splitlines() if not ln.startswith("#")
        )
        if strip(cur) != strip(text):
            print("[pinyin] 数据文件与当前 pypinyin 不一致，请重跑生成脚本", file=sys.stderr)
            return 1
        print("[pinyin] 与数据文件一致")
        return 0

    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, encoding="utf-8", newline=chr(10))  # 统一 LF，避免跨平台 diff 噪声
    print(f"[pinyin] 已写入 {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
