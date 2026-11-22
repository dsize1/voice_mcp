#!/usr/bin/env python3
"""MDD 微调实验（S1 精度提升 / B 阶段）。

目的
----
现有 GOP 的音素级精度不够（so762 上 AUROC 0.63–0.66，FP=10% 时精确率 0.15–0.24）。
这里用同一个数据集的**音素级人工标注**微调音素 CTC 模型，然后**用同一套 GOP 算法**
重新算 AUROC，看是否有实质提升 —— 否则不值得上线。

协议（不许作弊）
----------------
- **按说话人切分**：同一说话人的句子只会出现在一个 split（so762 官方 train/test 已按人切，但我们
  再按 speaker 打散重划，确保微调用的是 train speakers、评估用完全没见过的 speakers）
- 参考音素来自 CMUdict（ARPAbet）→ 映射到模型的 espeak 符号；人工 `mispronunciations` 只用于评估，
  **不参与训练**（否则模型会学"哪里错"，而不是"听到什么"）
- 评估口径与 `tools/calibrate_gop.py` 完全一致（同一函数），保证基线/微调可比

用法
  python tools/mdd_train.py dump                     # 导出 wav + 音素标签 + speaker 切分
  python tools/mdd_train.py train --epochs 2         # 微调（默认只训最后 4 层 + 分类头）
  python tools/mdd_train.py export                   # 导出 ONNX 到 C:/voice_models/phoneme-ctc-ft
"""
from __future__ import annotations

import argparse
import io
import json
import random
import time
import wave
from pathlib import Path

DATASET = Path(r"D:/voice_data/datasets/so762")
WORK = Path(r"D:/voice_data/mdd")
MODEL_ID = "facebook/wav2vec2-lv-60-espeak-cv-ft"
OUT_MODEL = Path(r"C:/voice_models/phoneme-ctc-ft")

# ARPAbet → 训练用的单一 espeak 符号（取 CAND 里模型最常吐的那个）
CANON = {
    "AA": "ɑ", "AE": "æ", "AH": "ʌ", "AO": "ɔ", "AW": "aʊ", "AY": "aɪ",
    "EH": "ɛ", "ER": "ɜ", "EY": "eɪ", "IH": "ɪ", "IY": "i", "OW": "oʊ",
    "OY": "ɔɪ", "UH": "ʊ", "UW": "u",
    "B": "b", "CH": "tʃ", "D": "d", "DH": "ð", "F": "f", "G": "g", "HH": "h",
    "JH": "dʒ", "K": "k", "L": "l", "M": "m", "N": "n", "NG": "ŋ", "P": "p",
    "R": "r", "S": "s", "SH": "ʃ", "T": "t", "TH": "θ", "V": "v", "W": "w",
    "Y": "j", "Z": "z", "ZH": "ʒ",
}


def load_rows() -> list[dict]:
    import pyarrow.parquet as pq

    out = []
    for p in sorted(DATASET.glob("*.parquet")):
        d = pq.read_table(p).to_pydict()
        for i in range(len(d["text"])):
            phones = []
            bad = []
            for w in d["words"][i] or []:
                mp = {m["index"] for m in (w.get("mispronunciations") or [])}
                for j, ph in enumerate(w["phones"]):
                    phones.append("".join(c for c in ph if c.isalpha()).upper())
                    bad.append(1 if j in mp else 0)
            out.append(
                {
                    "speaker": d["speaker"][i],
                    "text": d["text"][i],
                    "audio": d["audio"][i]["bytes"],
                    "phones": phones,
                    "bad": bad,
                    "acc": d["accuracy"][i],
                }
            )
    return out


def split_speakers(rows: list[dict], seed: int = 0) -> dict[str, list[str]]:
    spk = sorted({r["speaker"] for r in rows})
    random.Random(seed).shuffle(spk)
    n = len(spk)
    tr, dv = int(n * 0.8), int(n * 0.9)
    return {"train": spk[:tr], "dev": spk[tr:dv], "test": spk[dv:]}


def dump(args) -> int:
    rows = load_rows()
    parts = split_speakers(rows)
    WORK.mkdir(parents=True, exist_ok=True)
    audio_dir = WORK / "wav"
    audio_dir.mkdir(exist_ok=True)
    index = []
    for k, spk in enumerate(rows):
        spk_id = spk["speaker"]
        split = next((s for s, ids in parts.items() if spk_id in ids), "test")
        name = f"{split}_{k:05d}_{spk_id}.wav"
        p = audio_dir / name
        if not p.is_file():
            with wave.open(io.BytesIO(spk["audio"])) as w:
                sr = w.getframerate()
                frames = w.readframes(w.getnframes())
            assert sr == 16000, f"期望 16 kHz，实际 {sr}"
            with wave.open(str(p), "wb") as w:
                w.setnchannels(1)
                w.setsampwidth(2)
                w.setframerate(16000)
                w.writeframes(frames)
        index.append(
            {
                "wav": name,
                "split": split,
                "speaker": spk_id,
                "text": spk["text"],
                "phones": spk["phones"],
                "bad": spk["bad"],
                "acc": spk["acc"],
            }
        )
    (WORK / "index.jsonl").write_text(
        "\n".join(json.dumps(r, ensure_ascii=False) for r in index) + "\n", encoding="utf-8"
    )
    counts = {s: sum(1 for r in index if r["split"] == s) for s in ("train", "dev", "test")}
    print(f"[dump] {len(index)} 句 → {WORK}")
    print(f"[dump] splits {counts} | speakers { {s: len(v) for s, v in parts.items()} }")
    return 0


def ctc_train(args) -> int:
    import torch
    from torch.utils.data import DataLoader, Dataset
    from transformers import Wav2Vec2ForCTC

    torch.manual_seed(0)
    dev = "cuda" if torch.cuda.is_available() else "cpu"
    print(f"[train] device={dev}")

    rows = [json.loads(l) for l in (WORK / "index.jsonl").read_text(encoding="utf-8").splitlines()]
    vocab = json.loads(Path(r"C:/voice_models/phoneme-ctc/vocab.json").read_text(encoding="utf-8"))
    unk = vocab.get("<unk>", 3)

    class DS(Dataset):
        def __init__(self, items):
            self.items = items
            self.cache = {}

        def __len__(self):
            return len(self.items)

        def __getitem__(self, i):
            it = self.items[i]
            if it["wav"] not in self.cache:
                with wave.open(str(WORK / "wav" / it["wav"])) as w:
                    import numpy as np

                    pcm = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype("float32") / 32768.0
                self.cache[it["wav"]] = pcm
            labels = [vocab[CANON[p]] for p in it["phones"] if p in CANON and CANON[p] in vocab]
            return torch.from_numpy(self.cache[it["wav"]]), labels

    def collate(batch):
        import torch.nn.functional as F

        xs = [b[0] for b in batch]
        ys = [b[1] for b in batch]
        x = torch.nn.utils.rnn.pad_sequence(xs, batch_first=True)
        x = (x - x.mean(1, keepdim=True)) / torch.sqrt(x.var(1, keepdim=True) + 1e-7)
        y = torch.tensor([v for lab in ys for v in lab], dtype=torch.long)
        return x, torch.tensor([len(l) for l in ys]), y

    train = [r for r in rows if r["split"] == "train"]
    val = [r for r in rows if r["split"] == "dev"]
    if args.limit:
        train = train[: args.limit]
        val = val[: max(1, args.limit // 4)]

    model = Wav2Vec2ForCTC.from_pretrained(MODEL_ID, revision="main").to(dev)
    # 冻结 CNN 特征提取器（数据量小，避免过拟合并省显存）
    model.freeze_feature_encoder()
    trainable = list(model.lm_head.parameters())
    if args.layers > 0:
        for enc in model.wav2vec2.encoder.layers[-args.layers :]:
            trainable += list(enc.parameters())
    for p in model.parameters():
        p.requires_grad_(False)
    for p in trainable:
        p.requires_grad_(True)
    print(f"[train] 可训练参数 {sum(p.numel() for p in trainable)/1e6:.1f}M")

    opt = torch.optim.AdamW(trainable, lr=args.lr, weight_decay=0.01)
    dl = DataLoader(DS(train), batch_size=args.batch, shuffle=True, collate_fn=collate, num_workers=0)
    scaler = torch.amp.GradScaler("cuda", enabled=(dev == "cuda"))

    for ep in range(1, args.epochs + 1):
        t0 = time.time()
        model.train()
        tot, n = 0.0, 0
        for step, (x, ylen, y) in enumerate(dl):
            if ylen.sum() == 0:
                continue
            x, y = x.to(dev), y.to(dev)
            with torch.autocast("cuda", dtype=torch.bfloat16, enabled=(dev == "cuda")):
                logits = model(x).logits
            logp = torch.log_softmax(logits.float(), dim=-1)
            in_len = torch.full((x.shape[0],), logits.shape[1], dtype=torch.long, device=dev)
            loss = torch.nn.functional.ctc_loss(logp.transpose(0, 1), y, in_len, ylen, blank=0, zero_infinity=True)
            opt.zero_grad(set_to_none=True)
            loss.backward()
            torch.nn.utils.clip_grad_norm_(trainable, 1.0)
            opt.step()
            tot += loss.item()
            n += 1
            if step % 20 == 0:
                print(f"  ep{ep} step {step}/{len(dl)} loss {tot/max(n,1):.3f} ({time.time()-t0:.0f}s)")
        print(f"[epoch {ep}] train loss {tot/max(n,1):.3f} | {time.time()-t0:.0f}s")

        # dev：训练用 CTC loss 看趋势（不用人工标注，避免信息泄漏到调参）
        model.eval()
        vloss, vn = 0.0, 0
        vdl = DataLoader(DS(val), batch_size=args.batch, collate_fn=collate)
        with torch.no_grad():
            for x, ylen, y in vdl:
                if ylen.sum() == 0:
                    continue
                x, y = x.to(dev), y.to(dev)
                logits = model(x).logits
                logp = torch.log_softmax(logits.float(), dim=-1)
                in_len = torch.full((x.shape[0],), logits.shape[1], dtype=torch.long, device=dev)
                vloss += torch.nn.functional.ctc_loss(
                    logp.transpose(0, 1), y, in_len, ylen, blank=0, zero_infinity=True
                ).item()
                vn += 1
        print(f"[epoch {ep}] dev loss {vloss/max(vn,1):.3f}")
        OUT_MODEL.mkdir(parents=True, exist_ok=True)
        model.save_pretrained(OUT_MODEL)
        print(f"[epoch {ep}] saved → {OUT_MODEL}")
    return 0


def export(args) -> int:
    import torch
    from transformers import Wav2Vec2ForCTC

    out = OUT_MODEL
    model = Wav2Vec2ForCTC.from_pretrained(out).eval()

    class Wrap(torch.nn.Module):
        def __init__(self, m):
            super().__init__()
            self.m = m

        def forward(self, input_values):
            return self.m(input_values).logits

    dummy = torch.zeros(1, 16000)
    torch.onnx.export(
        Wrap(model), (dummy,), str(out / "model.onnx"),
        input_names=["input_values"], output_names=["logits"],
        dynamic_axes={"input_values": {1: "time"}, "logits": {1: "frames"}},
        opset_version=17, do_constant_folding=True, dynamo=False,
    )
    print(f"[export] {out/'model.onnx'}  {(out/'model.onnx').stat().st_size/2**20:.0f} MB")
    return 0


def evaluate(args) -> int:
    """在**没参与训练的说话人**上比较：基线模型 vs 微调模型（同一套 GOP 算法）。"""
    import sys

    import numpy as np
    import onnxruntime as ort

    sys.path.insert(0, str(Path(__file__).parent))
    import calibrate_gop as cal  # 复用同一套 GOP/对齐/AUROC 实现

    rows = [json.loads(l) for l in (WORK / "index.jsonl").read_text(encoding="utf-8").splitlines()]
    test = [r for r in rows if r["split"] == "test"]
    if args.strat:
        # 分层取样：低/中/高人工分各三分之一，否则随机抽样里错读只有 0.4%（评估没有统计意义）
        test.sort(key=lambda r: r["acc"])
        per = max(1, args.strat // 3)
        test = test[:per] + test[len(test) // 2 - per // 2 : len(test) // 2 + per // 2] + test[-per:]
    if args.limit:
        test = test[: args.limit]
    print(f"[eval] test 句数 {len(test)}（speakers {len({r['speaker'] for r in test})}，未参与训练）")

    vocab = json.loads(Path(r"C:/voice_models/phoneme-ctc/vocab.json").read_text(encoding="utf-8"))
    norm_to_id = {}
    for sym, i in vocab.items():
        norm_to_id.setdefault(cal.norm(sym), i)
    blank = vocab.get("<pad>", 0)

    def run(model_path: str, tag: str):
        sess = ort.InferenceSession(str(model_path), providers=["CPUExecutionProvider"])
        scores, labels = [], []
        for r in test:
            with wave.open(str(WORK / "wav" / r["wav"])) as w:
                pcm = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype("float32") / 32768.0
            x = (pcm - pcm.mean()) / np.sqrt(pcm.var() + 1e-7)
            logits = sess.run(None, {"input_values": x[None, :]})[0][0]
            m = logits.max(1, keepdims=True)
            lp = logits - (m + np.log(np.exp(logits - m).sum(1, keepdims=True)))
            refs, bad = [], []
            for ph, b in zip(r["phones"], r["bad"]):
                cands = [norm_to_id[cal.norm(s)] for s in cal.CAND.get(ph, []) if cal.norm(s) in norm_to_id]
                if not cands:
                    continue
                refs.append(cands)
                bad.append(b)
            if len(refs) < 3:
                continue
            spans = cal.viterbi(lp, refs, blank)
            g = np.array(cal.gop_span(lp, refs, spans, blank))
            med = np.median(g)
            z = (g - med) / (1.4826 * np.median(np.abs(g - med)) + 1e-3)
            scores.extend(z.tolist())
            labels.extend(bad)
        neg = [s for s, l in zip(scores, labels) if l == 0]
        pos = [s for s, l in zip(scores, labels) if l == 1]
        tau = float(np.percentile(neg, 10))
        tp = sum(1 for s in pos if s <= tau)
        fp = sum(1 for s in neg if s <= tau)
        a = cal.auc([-v for v in scores], labels)
        print(
            f"  {tag:<10} n={len(labels):5d} 错读={len(pos):4d} | AUROC {a:.3f} | "
            f"FP10% 精确率 {tp/max(1,tp+fp):.3f} 召回 {tp/max(1,len(pos)):.3f} | τ {tau:+.2f}"
        )
        return a

    base = run(r"C:/voice_models/phoneme-ctc/model.onnx", "baseline")
    ft_path = OUT_MODEL / "model.onnx"
    if ft_path.is_file():
        ft = run(str(ft_path), "fine-tuned")
        print(f"[eval] AUROC {base:.3f} → {ft:.3f}（Δ {ft-base:+.3f}）")
    else:
        print("[eval] 还没导出微调模型，先跑 export")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["dump", "train", "export", "eval"])
    ap.add_argument("--epochs", type=int, default=2)
    ap.add_argument("--batch", type=int, default=8)
    ap.add_argument("--lr", type=float, default=3e-4)
    ap.add_argument("--layers", type=int, default=4, help="微调最后 N 层 transformer（0 = 只训分类头）")
    ap.add_argument("--limit", type=int, default=0, help="只用前 N 句（冒烟测试）")
    ap.add_argument("--strat", type=int, default=0, help="分层取样句数（低/中/高人工分各 1/3）")
    args = ap.parse_args()
    if args.mode == "dump":
        return dump(args)
    if args.mode == "train":
        return ctc_train(args)
    if args.mode == "eval":
        return evaluate(args)
    return export(args)


if __name__ == "__main__":
    raise SystemExit(main())
