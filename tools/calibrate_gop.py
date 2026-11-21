#!/usr/bin/env python3
"""GOP 标定/评估（S1 第 ⑥ 步）：用 speechocean762 的人工标注选算法 + 定阈值。

评什么
------
1. **句子级相关性**：我们的 mean-GOP 与人工 `accuracy` 分的 Spearman ρ
   （ρ 太低说明这个分数没法用，无论看起来多合理）。
2. **音素级判别力**：拿人工 `mispronunciations` 当金标，算 AUROC（越接近 0.5 越等于瞎猜）。

比什么
------
三种 GOP 统计口径，用数据选，不拍脑袋：
- A `span`    ：CTC-Viterbi 分配到该音素的帧（本模型是 spike 型，往往只有 1–2 帧）
- B `span±K`  ：把区间向两侧各扩 K 帧（对 spike 型模型的抖动更稳）
- C `soft`    ：区间内 `log(Σ p(c) / Σ max_q p(q))`，用概率质量代替单帧最大值

输出
----
`docs/gop-calibration.md` 里的表格 + 建议阈值（FP ≤ 10%）。

用法
  python tools/calibrate_gop.py --n 80
"""
from __future__ import annotations

import argparse
import io
import json
import wave
from pathlib import Path

import numpy as np

MODEL = r"C:/voice_models/phoneme-ctc/model.onnx"
VOCAB = r"C:/voice_models/phoneme-ctc/vocab.json"
DATASET = r"D:/voice_data/datasets/so762"

# ARPAbet → 可接受 espeak 符号（与 crates/voice-core/src/gop/phones.rs 保持一致）
CAND = {
    "AA": ["ɑ", "ʌ", "a", "ə"], "AE": ["æ", "a", "ɛ"], "AH": ["ʌ", "ə", "a"],
    "AO": ["ɔ", "o", "ɑ", "ʌ"], "AW": ["au", "aʊ"], "AY": ["ai", "aɪ"],
    "EH": ["ɛ", "e", "æ"], "ER": ["ə", "ɜ", "ər"], "EY": ["ei", "eɪ", "e"],
    "IH": ["ɪ", "i", "ə"], "IY": ["i", "ɪ"], "OW": ["o", "ou", "oʊ"],
    "OY": ["ɔi", "ɔɪ"], "UH": ["ʊ", "u", "ə"], "UW": ["u", "ʊ", "ə"],
    "B": ["b"], "CH": ["tʃ", "ʃ"], "D": ["d", "t"], "DH": ["ð", "d", "z"],
    "F": ["f"], "G": ["g", "k"], "HH": ["h"], "JH": ["dʒ", "ʒ", "tʃ"],
    "K": ["k", "g"], "L": ["l"], "M": ["m"], "N": ["n", "ŋ"], "NG": ["ŋ", "n"],
    "P": ["p", "b"], "R": ["r", "ə", "ɜ"], "S": ["s", "z", "ʃ"], "SH": ["ʃ", "s"],
    "T": ["t", "d", "s"], "TH": ["θ", "f", "s", "t"], "V": ["v", "w", "f"],
    "W": ["w", "v"], "Y": ["j", "i"], "Z": ["z", "s", "ʒ"], "ZH": ["ʒ", "ʃ"],
}


# 窄候选：只有真正的同位异读（元音弱化/儿化），辅音 1:1；
# 中候选：元音宽容 + 少数塞音弱化；
# 宽候选：当前实现（含 θ/s、v/w 等混淆对）—— 会把真错读吃掉
NARROW = {k: v[:1] for k, v in CAND.items()}
NARROW.update({
    "AA": ["ɑ", "ʌ"], "AH": ["ʌ", "ə"], "ER": ["ə", "ɜ"], "IH": ["ɪ", "ə"],
    "UW": ["u", "ə"], "UH": ["ʊ", "ə"], "IY": ["i", "ɪ"], "EY": ["ei", "eɪ"],
    "OW": ["o", "oʊ"], "AY": ["ai", "aɪ"], "AW": ["au", "aʊ"], "OY": ["ɔi", "ɔɪ"],
})
MEDIUM = {k: list(v) for k, v in NARROW.items()}
MEDIUM.update({"T": ["t", "d"], "D": ["d", "t"], "G": ["g", "k"], "B": ["b", "p"]})


def norm(sym: str) -> str:
    out = []
    for c in sym:
        if c in "ˈˌːˑʰ":
            continue
        if c == "ɡ":
            c = "g"
        elif c == "ɹ":
            c = "r"
        elif c in "ɾʔ":
            c = "t"
        elif c == "ɐ":
            c = "ə"
        out.append(c)
    return "".join(out)


class Model:
    def __init__(self):
        import onnxruntime as ort

        self.vocab = json.loads(Path(VOCAB).read_text(encoding="utf-8"))
        self.inv = {v: k for k, v in self.vocab.items()}
        self.norm_to_id = {}
        for sym, i in self.vocab.items():
            self.norm_to_id.setdefault(norm(sym), i)
        self.sess = ort.InferenceSession(MODEL, providers=["CPUExecutionProvider"])
        self.blank = self.vocab.get("<pad>", 0)

    def log_probs(self, pcm: np.ndarray) -> np.ndarray:
        x = (pcm - pcm.mean()) / np.sqrt(pcm.var() + 1e-7)
        logits = self.sess.run(None, {"input_values": x[None, :]})[0][0]
        m = logits.max(1, keepdims=True)
        return logits - (m + np.log(np.exp(logits - m).sum(1, keepdims=True)))

    def cands(self, phone: str, table=None) -> list[int]:
        key = "".join(c for c in phone if c.isalpha()).upper()
        tbl = table or CAND
        return [self.norm_to_id[norm(s)] for s in tbl.get(key, []) if norm(s) in self.norm_to_id]


def viterbi(lp: np.ndarray, refs: list[list[int]], blank: int) -> list[tuple[int, int]]:
    """CTC 状态对齐，返回每个音素的 [t0, t1)（无 blank 惩罚）。"""
    T = lp.shape[0]
    L = len(refs)
    S = 2 * L + 1
    NEG = -1e9
    dp = np.full((T, S), NEG)
    frm = np.zeros((T, S), dtype=np.int8)

    def sc(s: int, t: int) -> float:
        return lp[t, blank] if s % 2 == 0 else float(lp[t, refs[s // 2]].max())

    dp[0, 0] = sc(0, 0)
    if S > 1:
        dp[0, 1] = sc(1, 0)
    for t in range(1, T):
        for s in range(S):
            best, how = dp[t - 1, s], 0
            if s >= 1 and dp[t - 1, s - 1] > best:
                best, how = dp[t - 1, s - 1], 1
            if s >= 3 and s % 2 == 1:
                i = s // 2
                if refs[i] and refs[i - 1] and refs[i][0] == refs[i - 1][0] and dp[t - 1, s - 2] > best:
                    best, how = dp[t - 1, s - 2], 2
            dp[t, s] = best + sc(s, t)
            frm[t, s] = how
    s = S - 1 if dp[-1, S - 1] >= dp[-1, S - 2] else S - 2
    spans = [(T, 0) for _ in range(L)]
    for t in range(T - 1, -1, -1):
        if s % 2 == 1:
            i = s // 2
            spans[i] = (min(spans[i][0], t), max(spans[i][1], t + 1))
        s -= int(frm[t, s])
    return spans


def gop_span(lp, refs, spans, blank, pad=0, soft=False):
    out = []
    T = lp.shape[0]
    for (t0, t1), cands in zip(spans, refs):
        a, b = max(0, t0 - pad), min(T, t1 + pad)
        if a >= b:
            out.append(-20.0)
            continue
        seg = lp[a:b]
        num = seg[:, cands]
        if soft:
            num = np.log(np.exp(num).sum(1))
        else:
            num = num.max(1)
        mask = np.ones(seg.shape[1], dtype=bool)
        mask[cands] = False
        mask[blank] = False
        den = seg[:, mask].max(1) if mask.any() else np.zeros(len(seg))
        out.append(float((num - den).mean()))
    return out


def spearman(a: np.ndarray, b: np.ndarray) -> float:
    def rank(v):
        order = v.argsort()
        r = np.empty_like(order, dtype=float)
        r[order] = np.arange(len(v))
        return r

    ra, rb = rank(a), rank(b)
    ra -= ra.mean()
    rb -= rb.mean()
    return float((ra * rb).sum() / (np.sqrt((ra**2).sum() * (rb**2).sum()) + 1e-12))


def auc(scores: list[float], labels: list[int]) -> float:
    """二分类 AUROC（rank 法），labels: 1 = 错读。"""
    pairs = sorted(zip(scores, labels))
    ranks = np.empty(len(pairs))
    i = 0
    while i < len(pairs):
        j = i
        while j + 1 < len(pairs) and pairs[j + 1][0] == pairs[i][0]:
            j += 1
        avg = (i + j) / 2 + 1
        for k in range(i, j + 1):
            ranks[k] = avg
        i = j + 1
    pos = [r for r, (_, l) in zip(ranks, pairs) if l == 1]
    neg = [r for r, (_, l) in zip(ranks, pairs) if l == 0]
    if not pos or not neg:
        return float("nan")
    return float((sum(pos) - len(pos) * (len(pos) + 1) / 2) / (len(pos) * len(neg)))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=80)
    args = ap.parse_args()

    import pyarrow.parquet as pq

    model = Model()
    files = sorted(Path(DATASET).glob("*.parquet"))
    rows = []
    for p in files:
        d = pq.read_table(p).to_pydict()
        rows.extend(
            {
                "audio": d["audio"][i]["bytes"],
                "text": d["text"][i],
                "acc": d["accuracy"][i],
                "words": d["words"][i],
            }
            for i in range(len(d["text"]))
        )
    # 分层取样：低 / 中 / 高人工分各三分之一
    rows.sort(key=lambda r: r["acc"])
    per = max(1, args.n // 3)
    sample = rows[:per] + rows[len(rows) // 2 - per // 2 : len(rows) // 2 + per // 2] + rows[-per:]

    variants = {"span": dict(pad=0, soft=False), "soft±2": dict(pad=2, soft=True)}
    tables = {"narrow": NARROW, "medium": MEDIUM, "wide": CAND}
    keys = [f"{t}/{v}" for t in tables for v in variants]
    sent = {k: [] for k in keys}
    labels, scores = [], {k: [] for k in keys}
    accs = []

    for r in sample:
        with wave.open(io.BytesIO(r["audio"])) as w:
            pcm = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32) / 32768
        lp = model.log_probs(pcm)
        ok = True
        for tname, tbl in tables.items():
            refs, bad = [], []
            for w in r["words"] or []:
                mp = {m["index"] for m in (w.get("mispronunciations") or [])}
                for i, ph in enumerate(w["phones"]):
                    c = model.cands(ph, tbl)
                    if not c:
                        continue
                    refs.append(c)
                    bad.append(1 if i in mp else 0)
            if len(refs) < 3:
                ok = False
                break
            spans = viterbi(lp, refs, model.blank)
            for vname, cfg in variants.items():
                k = f"{tname}/{vname}"
                g = np.array(gop_span(lp, refs, spans, model.blank, **cfg))
                med = np.median(g)
                z = (g - med) / (1.4826 * np.median(np.abs(g - med)) + 1e-3)
                sent[k].append(float(g.mean()))
                scores[k].extend(z.tolist())
            if tname == "narrow":
                labels.extend(bad)
        if not ok:
            continue
        accs.append(r["acc"])

    print(f"句子数 {len(accs)} | 音素样本 {len(labels)} | 错读 {sum(labels)}")
    print(
        f"{'候选/统计':<18} {'句级ρ':>7} {'AUROC*':>7} {'τ(FP10%)':>9} "
        f"{'精确率':>7} {'召回':>6}"
    )
    print("* AUROC：错读为正类、分数取负；>0.5 才说明方向对")
    for k in keys:
        rho = spearman(np.array(sent[k]), np.array(accs))
        # 我们的 z 越负越可疑 → 取负号让"分数越高越可能错"
        a = auc([-v for v in scores[k]], labels)
        neg = [s for s, l in zip(scores[k], labels) if l == 0]
        pos = [s for s, l in zip(scores[k], labels) if l == 1]
        tau = float(np.percentile(neg, 10)) if neg else 0.0
        tp = sum(1 for s in pos if s <= tau)
        fp = sum(1 for s in neg if s <= tau)
        prec = tp / max(1, tp + fp)
        rec = tp / max(1, len(pos))
        print(f"{k:<18} {rho:7.3f} {a:7.3f} {tau:9.2f} {prec:7.3f} {rec:6.3f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
