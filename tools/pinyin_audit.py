#!/usr/bin/env python
"""中文拼音层（D13）真机审计：TTS 朗读 → ASR 转写 → `assess` 拼音层。

为什么要这个脚本：拼音层是**纯文本**结论，它的价值必须用"读对的音频"证明——
如果 TTS 明明读得标准，拼音层却报出一堆发音错误，那就是误报（会直接毁掉信任）。

流程（每句）：`voice-cli say` → `voice-cli assess --lang zh --reference` →
读 `text_diff.pinyin` 与 `text_diff.issues`，汇总成 markdown 报告。

用法：
    python tools/pinyin_audit.py                 # 用内置 10 句，写 docs/audit/pinyin-<date>.md
    python tools/pinyin_audit.py --out out.md    # 指定报告路径
    python tools/pinyin_audit.py --cli target/release/voice-cli.exe

判读口径（写进报告）：
* `accuracy_char < 1.0` 而 `accuracy_pinyin == 1.0` → 差异全是**同音字**（ASR 选字，不是发音）
* `accuracy_pinyin < 1.0` → 真有非同类差异；TTS 读对的前提下，这是 ASR 听错/选字，仍不是发音错
* 因此本审计的通过线是：**没有任何一条被标成 `likely_pronunciation`**
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 覆盖：平翘舌 / 前后鼻音 / 多音字 / 轻声 / 数字
CORPUS = [
    "今天天气真好，我们一起去公园散步吧。",
    "他跑得很快，我都追不上他。",
    "我在北京的银行工作。",
    "四是四，十是十，十四是十四。",
    "你吃饭了吗？我还没吃呢。",
    "这个价格便宜一点可以吗？",
    "重庆的长城很长。",
    "学习语言需要每天练习。",
    "他把空调开到二十六度。",
    "银行行长正在开会。",
]


def run(cmd: list[str]) -> dict:
    out = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8")
    if out.returncode != 0:
        raise SystemExit(
            f"命令失败({out.returncode}): {' '.join(cmd)}\n{out.stderr[-2000:]}"
        )
    return json.loads(out.stdout)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cli", default=str(ROOT / "target" / "release" / "voice-cli.exe"))
    ap.add_argument("--out", default=None)
    ap.add_argument("--limit", type=int, default=len(CORPUS))
    args = ap.parse_args()

    rows = []
    for sent in CORPUS[: args.limit]:
        said = run([args.cli, "say", sent, "--scene", "d13-pinyin"])
        wav = f"D:/voice_data/sessions/{said['session']}/takes/{said['prefix']}.ref_norm.wav"
        a = run(
            [
                args.cli,
                "assess",
                wav,
                "--reference",
                sent,
                "--lang",
                "zh",
                "--scene",
                "d13-pinyin",
            ]
        )
        p = (a.get("text_diff") or {}).get("pinyin") or {}
        issues = (a.get("text_diff") or {}).get("issues") or []
        flagged = [i for i in issues if i.get("hint") == "likely_pronunciation"]
        rows.append(
            {
                "ref": sent,
                "heard": a["transcript"]["text"],
                "acc_char": p.get("accuracy_char"),
                "acc_pinyin": p.get("accuracy_pinyin"),
                "homophones": p.get("homophones", 0),
                "tone_only": p.get("tone_only", 0),
                "initial_only": p.get("initial_only", 0),
                "rime_only": p.get("rime_only", 0),
                "mixed": p.get("mixed", 0),
                "confusions": p.get("confusions", []),
                "flagged": [(i["reference"], i["said"], i["note"]) for i in flagged],
                "table": p.get("table"),
                "degraded": a.get("degraded"),
            }
        )
        print(f"[{len(rows):02d}] char={rows[-1]['acc_char']} pinyin={rows[-1]['acc_pinyin']}", flush=True)

    n = len(rows)
    mean_char = sum(r["acc_char"] or 0 for r in rows) / max(n, 1)
    mean_py = sum(r["acc_pinyin"] or 0 for r in rows) / max(n, 1)
    homophones = sum(r["homophones"] for r in rows)
    flagged = sum(len(r["flagged"]) for r in rows)
    tags: dict[str, int] = {}
    for r in rows:
        for c in r["confusions"]:
            tags[c["tag"]] = tags.get(c["tag"], 0) + c["count"]

    lines = [
        f"# 中文拼音层真机审计 · {date.today().isoformat()}",
        "",
        "生成：`python tools/pinyin_audit.py`（TTS `zf_xiaoni` 朗读 → SenseVoice `zh` → `assess` 拼音层）",
        "",
        f"- 句数：**{n}**；拼音表：`{rows[0]['table'] if rows else 'n/a'}`",
        f"- 平均 **字级 accuracy {mean_char:.3f}** / **拼音层 accuracy {mean_py:.3f}**",
        f"- 同音不同字（不算发音问题）：**{homophones}** 处",
        f"- 被判成 `likely_pronunciation` 的：**{flagged}** 处（通过线是 0）",
        "",
        "## 逐句",
        "",
        "| # | 字级 | 拼音层 | 同音 | 声调 | 声母 | 韵母 | 参考 | ASR |",
        "|---|---|---|---|---|---|---|---|---|",
    ]
    for i, r in enumerate(rows, 1):
        lines.append(
            f"| {i} | {r['acc_char']:.3f} | {r['acc_pinyin']:.3f} | {r['homophones']} | "
            f"{r['tone_only']} | {r['initial_only']} | {r['rime_only']} | {r['ref']} | {r['heard']} |"
        )
    lines += ["", "## 混淆统计", ""]
    if tags:
        for tag, c in sorted(tags.items(), key=lambda kv: -kv[1]):
            lines.append(f"- `{tag}` × {c}")
    else:
        lines.append("- （无）")
    lines += ["", "## 被判成「发音偏差」的条目", ""]
    if flagged:
        for r in rows:
            for ref, said, note in r["flagged"]:
                lines.append(f"- {r['ref']}：{ref}→{said} — {note}")
    else:
        lines.append("- （无。TTS 读对的音频上，拼音层没有误报发音问题）")

    report = "\n".join(lines) + "\n"
    out = Path(args.out) if args.out else ROOT / "docs" / "audit" / f"pinyin-{date.today().isoformat()}.md"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(report, encoding="utf-8", newline="\n")
    print(f"\n[audit] 已写入 {out}")
    print(f"[audit] 字级 {mean_char:.3f} / 拼音层 {mean_py:.3f} / 同音 {homophones} / 误报 {flagged}")
    return 0 if flagged == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
