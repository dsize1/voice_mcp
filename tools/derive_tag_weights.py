#!/usr/bin/env python3
"""从 speechocean762 的人工标注推导 **tag 权重**（S0b）。

思路
----
`bank` 里的 tags 是关键词启发式（文本含 think 就标 `theta_to_s`），
排序原来看"命中几个 tag"——等于假设所有音素难点一样重要。
但中文母语者的真实错读率差得很远（TH 7.70% vs S 2.77%）。

这里把 so762 的音素级 `mispronunciations` 聚合到我们的 tag 上：
  tag_rate = Σ(bad_phone) / Σ(occurrences)   （成员音素按出现次数加权）

刻意**只为有音素证据的 tag 生成权重**；`linking_missing` / `question_rise` /
`tone_flat` / `fragment` 这类无法从音素错读率推导的 tag 不参与加权（权重 0），
宁可少算，也不编造依据。

输出
----
  docs/tag-weights.json          —— 溯源用（含 so762 revision + 每个音素的计数）
  标准输出                        —— 可直接粘进 Rust 的常量表

用法
----
  python tools/derive_tag_weights.py
"""
from __future__ import annotations

import argparse
import collections
import json
import statistics
import sys
from pathlib import Path

# ARPAbet（去掉重音数字）→ 我们的 tag id（见 skills/ielts-speaking-coach/references/phoneme-map.md）
PHONE_TAGS: dict[str, list[str]] = {
    "TH": ["theta_to_s"],
    "DH": ["eth_to_z"],
    "V": ["v_to_w"],
    "W": ["w_to_v"],
    "R": ["r_to_l"],
    "L": ["r_to_l", "l_dark"],
    "N": ["n_to_ng"],
    "NG": ["n_to_ng"],
    "AE": ["ae_to_e"],
    "EH": ["ae_to_e"],
    "IY": ["short_long_vowel"],
    "IH": ["short_long_vowel"],
    "UH": ["short_long_vowel"],
    "UW": ["short_long_vowel"],
    "EY": ["short_long_vowel"],
    "OW": ["short_long_vowel"],
    "AW": ["short_long_vowel"],
    "AY": ["short_long_vowel"],
    "OY": ["short_long_vowel"],
    "S": ["s_ending"],
    "Z": ["s_ending"],
    "T": ["ed_ending", "final_consonant_drop"],
    "D": ["ed_ending", "final_consonant_drop"],
    "P": ["final_consonant_drop"],
    "B": ["final_consonant_drop"],
    "K": ["final_consonant_drop", "cluster_simplify"],
    "G": ["final_consonant_drop"],
    "F": ["final_consonant_drop"],
    "SH": ["cluster_simplify"],
    "CH": ["cluster_simplify"],
    "JH": ["cluster_simplify"],
}

# tag 的"可理解度影响"（来自 references/phoneme-map.md 的分级，人工给定，不来自数据集）。
# 为什么必须乘上去：so762 统计的是**错读频率**，不是"错了有多严重"。
# 例如 /v/→/w/ 在中文母语者里错得不算多（2.5%），但一旦错会把 very 说成 wery —— 影响是"高"。
IMPACT = {
    "theta_to_s": 1.0,
    "eth_to_z": 0.8,
    "v_to_w": 1.0,
    "w_to_v": 1.0,
    "r_to_l": 1.0,
    "l_dark": 0.6,
    "final_consonant_drop": 1.0,
    "ed_ending": 0.6,
    "s_ending": 0.6,
    "ae_to_e": 0.6,
    "short_long_vowel": 0.6,
    "n_to_ng": 0.45,
    "cluster_simplify": 0.6,
    "linking_missing": 0.6,
    "schwa_missing": 0.6,
    "stress_misplace": 1.0,
    "tone_flat": 0.6,
    "question_rise": 0.6,
}

# 不参与加权的 tag（无音素级证据）
NO_EVIDENCE = [
    "linking_missing",
    "schwa_missing",
    "stress_misplace",
    "tone_flat",
    "question_rise",
    "fragment",
    "lesson_header",
    "instruction",
]


def collect(parquet_files: list[Path]):
    import pyarrow.parquet as pq

    occ: collections.Counter = collections.Counter()
    bad: collections.Counter = collections.Counter()
    rows = 0
    for p in parquet_files:
        d = pq.read_table(p).to_pydict()
        rows += len(d["text"])
        for ws in d["words"]:
            for w in ws or []:
                mp = {m["index"] for m in (w.get("mispronunciations") or [])}
                for i, ph in enumerate(w.get("phones") or []):
                    key = "".join(c for c in str(ph) if c.isalpha()).upper()
                    if not key:
                        continue
                    occ[key] += 1
                    if i in mp:
                        bad[key] += 1
    return rows, occ, bad


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--datasets", default=r"D:/voice_data/datasets/so762")
    ap.add_argument("--out", default="docs/tag-weights.json")
    args = ap.parse_args()

    files = sorted(Path(args.datasets).glob("*.parquet"))
    if not files:
        print(f"[FAIL] 找不到 parquet: {args.datasets}", file=sys.stderr)
        return 2
    rows, occ, bad = collect(files)

    total_occ = sum(occ.values())
    total_bad = sum(bad.values())
    if total_occ == 0:
        print("[FAIL] 音素 token 为 0，数据可疑", file=sys.stderr)
        return 2
    base_rate = total_bad / total_occ

    # 聚合到 tag
    tag_occ: collections.Counter = collections.Counter()
    tag_bad: collections.Counter = collections.Counter()
    tag_phones: dict[str, list[str]] = collections.defaultdict(list)
    for ph, n in occ.items():
        for tag in PHONE_TAGS.get(ph, []):
            tag_occ[tag] += n
            tag_bad[tag] += bad[ph]
            tag_phones[tag].append(ph)

    tags = {}
    for tag in sorted(tag_occ):
        rate = tag_bad[tag] / tag_occ[tag]
        impact = IMPACT.get(tag, 0.6)
        tags[tag] = {
            "rate": round(rate, 4),
            "impact": impact,
            # pick 实际使用的权重 = 错读频率 × 可理解度影响
            "weight": round(rate * impact, 4),
            "tokens": tag_occ[tag],
            "mispronounced": tag_bad[tag],
            "phones": sorted(tag_phones[tag]),
            # 相对基线的倍数：>1 表示比"平均音素"更容易错
            "lift": round(rate / base_rate, 2),
        }
    for tag in NO_EVIDENCE:
        tags.setdefault(tag, {"rate": 0.0, "tokens": 0, "mispronounced": 0, "phones": [], "lift": 0.0, "no_evidence": True})

    out = {
        "source": {
            "dataset": "mispeech/speechocean762",
            "revision": "06385584fad212b26134c656fdd3ccf9f093f33e",
            "rows": rows,
            "phone_tokens": total_occ,
            "mispronounced": total_bad,
            "baseline_rate": round(base_rate, 4),
            "note": "weight = rate(本数据集实测错读率) × impact(phoneme-map 的可理解度分级)；"
                    "rate 用成员音素按出现次数加权；无音素级证据的 tag 权重为 0，不参与排序",
        },
        "tags": tags,
    }
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(json.dumps(out, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    print(f"[ok] {args.out}  基线错读率 {base_rate*100:.2f}%  (tokens={total_occ}, bad={total_bad})")
    print("\n按权重排序（可直接粘进 Rust）：")
    printed = sorted(
        ((t, v) for t, v in tags.items() if v["tokens"] > 0),
        key=lambda kv: -kv[1]["weight"],
    )
    total = sum(v["rate"] for _, v in printed) or 1.0
    print("pub const TAG_WEIGHTS: &[(&str, f32, u32)] = &[")
    for t, v in printed:
        print(
            f'    ("{t}", {v["weight"]:.4}, {v["tokens"]}), '
            f'// rate {v["rate"]*100:.2f}% × impact {v["impact"]} (lift {v["lift"]}x)'
        )
    print("];")
    print(f"\n权重和 = {total:.4f}（未归一化，pick 里直接用原始 rate）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
