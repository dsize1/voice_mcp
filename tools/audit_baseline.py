#!/usr/bin/env python
"""D52 基线审计（**只读**）：清点历史 metrics 的 GOP 基线来源与 Δ 可比性。

背景：2026-11-25 之前，跟读/朗读的 GOP 基线**一律是 TTS 合成音**，
所以历史 `metrics.json` 里的"Δ（用户 − 基线）"与改用真人原声后的 Δ **不可横比**。
更早的记录连 `gop_baseline_source` 字段都没有。

用法：
    python tools/audit_baseline.py                 # 默认读 %VOICE_HOME% 或 D:\\voice_data
    python tools/audit_baseline.py --home D:\\voice_data --out docs/audit/d52-baseline-audit.md

输出：
    - 控制台摘要（各类计数）
    - 可选 markdown 报告（含“Δ 不可比”的文件清单，便于往后引用时避开）
"""

import argparse
import glob
import json
import os
import re
from collections import Counter, defaultdict


def find_metrics(home: str):
    return sorted(glob.glob(os.path.join(home, "sessions", "**", "*.metrics.json"), recursive=True))


def classify(doc: dict) -> str:
    """native / tts / absent（有基线但没标来源）/ no-baseline（本次没跑基线）"""
    if not doc.get("gop_baseline"):
        return "no-baseline"
    src = doc.get("gop_baseline_source")
    if src in ("native", "tts"):
        return src
    return "absent"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--home", default=os.environ.get("VOICE_HOME", r"D:\voice_data"))
    ap.add_argument("--out", default="")
    args = ap.parse_args()

    files = find_metrics(args.home)
    buckets = Counter()
    by_month = defaultdict(Counter)
    unusable = []  # (rel_path, kind, baseline_valid)

    for p in files:
        try:
            doc = json.load(open(p, encoding="utf-8"))
        except Exception:
            continue
        kind = classify(doc)
        buckets[kind] += 1
        m = re.search(r"sessions[/\\](\d{4}-\d{2})", p)
        if m:
            by_month[m.group(1)][kind] += 1
        if kind in ("tts", "absent"):
            unusable.append((os.path.relpath(p, args.home), kind, doc.get("gop_baseline_valid")))

    total = sum(buckets.values())
    print(f"metrics 总数: {total}  ← {args.home}")
    for k in ("native", "tts", "absent", "no-baseline"):
        print(f"  {k:<12}{buckets.get(k, 0)}")
    print("按月:")
    for month in sorted(by_month):
        c = by_month[month]
        print(
            f"  {month}  native={c.get('native',0):<3} tts={c.get('tts',0):<3} "
            f"absent={c.get('absent',0):<3} 无基线={c.get('no-baseline',0)}"
        )

    delta_usable = total - len(unusable) - buckets.get("no-baseline", 0)
    print(f"\nΔ 可用于跨期比较的记录: {delta_usable} / {total}（其余只能当“与合成示范的差异”）")

    if args.out:
        lines = [
            "# D52 基线审计（只读生成）",
            "",
            f"> 数据目录：`{args.home}`；生成工具：`tools/audit_baseline.py`",
            "",
            "## 口径",
            "",
            "| 分类 | 含义 | Δ 能否跨期比较 |",
            "| :--- | :--- | :--- |",
            "| `native` | 真人原声基线（`gop_baseline_source=native`） | ✅ 可以（仍需 `gop_baseline_valid=true`） |",
            "| `tts` | TTS 合成基线（2026-11-25 前的默认） | ❌ 只能当“与合成示范的差异” |",
            "| `absent` | 有基线但没写来源（字段是后加的） | ❌ 同上，且无法确认来源 |",
            "| `no-baseline` | 本次没跑基线 | — |",
            "",
            "## 计数",
            "",
            "| 分类 | 条数 |",
            "| :--- | ---: |",
        ]
        for k in ("native", "tts", "absent", "no-baseline"):
            lines.append(f"| `{k}` | {buckets.get(k, 0)} |")
        lines.append("")
        lines.append("按月：")
        lines.append("")
        lines.append("| 月份 | native | tts | absent | 无基线 |")
        lines.append("| :--- | ---: | ---: | ---: | ---: |")
        for month in sorted(by_month):
            c = by_month[month]
            lines.append(
                f"| {month} | {c.get('native',0)} | {c.get('tts',0)} | "
                f"{c.get('absent',0)} | {c.get('no-baseline',0)} |"
            )
        lines += [
            "",
            f"## Δ 不可比的记录（{len(unusable)} 条，引用时请避开）",
            "",
            "| 文件 | 分类 | 基线自检 |",
            "| :--- | :--- | :--- |",
        ]
        for rel, kind, valid in unusable[:200]:
            lines.append(f"| `{rel.replace(os.sep, '/')}` | `{kind}` | {valid} |")
        if len(unusable) > 200:
            lines.append(f"| …（其余 {len(unusable) - 200} 条省略） | | |")
        lines += [
            "",
            "## 结论 / 处置",
            "",
            "1. 趋势与“迁移”结论**只允许**用 `native` + `gop_baseline_valid=true` 的记录；",
            "2. 需要重跑的历史点：凡是曾被用来下结论且来源为 `tts`/`absent` 的，用 `assess --bank-id <id>`",
            "   （有真人切片）重跑一次，形成新记录，**不要**改写旧 `metrics.json`（它是留证）；",
            "3. IELTS prompt（10 条）目前没有真人参考音，其 Δ 一律视为不可比；",
            "   要用于趋势，需补录真人示范或改用 `bank` 里有真人切片的素材。",
            "",
        ]
        os.makedirs(os.path.dirname(args.out), exist_ok=True)
        open(args.out, "w", encoding="utf-8").write("\n".join(lines))
        print(f"\n报告已写入 {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
