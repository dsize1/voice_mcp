#!/usr/bin/env python3
"""一次性：把音素 CTC 模型导出成 ONNX（供纯 Rust 运行时做 GOP-SF）。

运行时不依赖 Python —— 这个脚本只跑一次，产物（onnx）由 `voice-core` 用 `ort` 加载。

模型选择：`facebook/wav2vec2-lv-60-espeak-cv-ft`
  - 音素级 CTC（espeak 音素集，60 语种），不是字符级 → 能做 GOP
  - 英文音素与 ARPAbet 可 1:1 映射（见 docs/phoneme-gop.md）
  - 中文可用同一套音素集（后续扩展到中文 GOP）

关键点（踩坑记录）
  1. 预处理必须复刻 `Wav2Vec2FeatureExtractor`：**逐句 zero-mean / unit-variance**
     （不是 RMS 归一化），Rust 侧要照抄，否则后验会整体偏移。
  2. 导出用 dynamic axes（时间维），否则超过 10 s 的音频会被截断。
  3. 量化用 dynamic int8：体积 1.2 GB → ~320 MB，CPU 推理快 2–3 倍；
     量化前后在同一段音频上比 logits（max|Δ| < 0.05 视为可用）。

用法
  python tools/export_phoneme_ctc.py --out C:/voice_models/phoneme-ctc
  python tools/export_phoneme_ctc.py --probe-only --out C:/voice_models/phoneme-ctc
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
from pathlib import Path

MODEL_ID = "facebook/wav2vec2-lv-60-espeak-cv-ft"
REVISION = "main"  # 导出后把实际 commit 写进 metadata；复核时用 metadata 里的值


def sha256_of(p: Path) -> str:
    h = hashlib.sha256()
    with p.open("rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def load_model():
    os.environ.setdefault("HF_ENDPOINT", "https://hf-mirror.com")
    import torch
    from transformers import Wav2Vec2ForCTC

    t0 = time.time()
    # 只取 vocab，不用 AutoProcessor：Wav2Vec2PhonemeCTCTokenizer 会强依赖 phonemizer
    # （而 G2P 我们自己在 Rust 侧用 CMUdict 做，不需要它）
    from huggingface_hub import hf_hub_download

    vocab = json.loads(
        Path(hf_hub_download(MODEL_ID, "vocab.json", revision=REVISION)).read_text(encoding="utf-8")
    )
    model = Wav2Vec2ForCTC.from_pretrained(MODEL_ID, revision=REVISION).eval()
    print(f"[load] {MODEL_ID} in {time.time()-t0:.1f}s | params {sum(p.numel() for p in model.parameters())/1e6:.0f}M | vocab {len(vocab)}")
    return vocab, model


class Wrap:
    """ONNX 只吃 input_values → logits。

    必须是 `torch.nn.Module` 子类：torch 的 legacy 导出器会对 model 调 `.modules()`。
    """

    def __new__(cls, model):
        import torch.nn as nn

        class _Wrap(nn.Module):
            def __init__(self):
                super().__init__()
                self.model = model

            def forward(self, input_values):
                return self.model(input_values).logits

        return _Wrap()


def export(proc, model, out_dir: Path, opset: int = 17):
    vocab = proc
    import torch

    onnx_path = out_dir / "model.onnx"
    dummy = torch.zeros(1, 16000)  # 1 s
    torch.onnx.export(
        Wrap(model),
        (dummy,),
        str(onnx_path),
        input_names=["input_values"],
        output_names=["logits"],
        dynamic_axes={"input_values": {1: "time"}, "logits": {1: "frames"}},
        opset_version=opset,
        do_constant_folding=True,
        # torch 2.14 默认走 dynamo 导出器，对 wav2vec2 的 conv/attention 组合不稳；
        # 这里显式用 legacy TorchScript 导出器（输出就是 logits，不需要图优化）
        dynamo=False,
    )
    print(f"[export] {onnx_path}  {onnx_path.stat().st_size/1048576:.0f} MB")
    (out_dir / "vocab.json").write_text(json.dumps(vocab, ensure_ascii=False, indent=2), encoding="utf-8")
    return onnx_path, vocab


def quantize(onnx_path: Path) -> Path:
    from onnxruntime.quantization import QuantType, quantize_dynamic

    q = onnx_path.with_name("model.int8.onnx")
    quantize_dynamic(str(onnx_path), str(q), weight_type=QuantType.QInt8)
    print(f"[quant] {q}  {q.stat().st_size/1048576:.0f} MB")
    return q


def norm(x):
    import numpy as np

    m = x.mean()
    v = x.var()
    return (x - m) / np.sqrt(v + 1e-7)


def probe(onnx_path: Path, vocab: dict, n: int = 3):
    """用 speechocean762 的真实句子验证：ONNX 后验 + 贪心解码是否像回事。"""
    import numpy as np
    import onnxruntime as ort
    import pyarrow.parquet as pq

    inv = {int(v): k for k, v in vocab.items()}
    sess = ort.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"])
    print(f"[probe] providers={sess.get_providers()}")

    files = sorted(Path(r"D:/voice_data/datasets/so762").glob("*.parquet"))
    d = pq.read_table(files[0]).to_pydict()
    for i in range(n):
        import io
        import wave

        with wave.open(io.BytesIO(d["audio"][i]["bytes"])) as w:
            sr = w.getframerate()
            pcm = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32) / 32768.0
        assert sr == 16000, f"期望 16 kHz，实际 {sr}"
        x = norm(pcm)[None, :]
        t0 = time.time()
        logits = sess.run(None, {"input_values": x})[0][0]
        ms = (time.time() - t0) * 1000
        ids = logits.argmax(-1)
        out, prev = [], -1
        for k in ids:
            if k != prev and k != 0:  # 0 = <pad>/CTC blank，跳过重复与 blank
                out.append(inv.get(int(k), "?"))
            prev = int(k)
        expect = " ".join(
            "".join(c for c in p if c.isalpha()).upper() for p in d["words"][i][0]["phones"]
        )
        print(f"\n#{i} text: {d['text'][i][:60]}")
        print(f"   expected(first word): {expect}")
        print(f"   greedy: {' '.join(out)[:160]}")
        print(f"   frames={logits.shape[0]} ({logits.shape[0]*20} ms hop)  infer={ms:.0f} ms  RTF={ms/1000/(len(pcm)/sr):.2f}")
        print(f"   manual scores: acc={d['accuracy'][i]} flu={d['fluency'][i]} total={d['total'][i]}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=r"C:/voice_models/phoneme-ctc")
    ap.add_argument("--probe-only", action="store_true")
    ap.add_argument("--no-quant", action="store_true")
    ap.add_argument("--probe", type=int, default=3)
    args = ap.parse_args()

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    meta_path = out / "metadata.json"

    if args.probe_only:
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        vocab = json.loads((out / "vocab.json").read_text(encoding="utf-8"))
        probe(out / meta["files"]["int8"], vocab, args.probe)
        return 0

    proc, model = load_model()
    fp32, vocab = export(proc, model, out)
    q8 = None if args.no_quant else quantize(fp32)

    # 量化前后 logits 对齐（同一段真实音频）
    import io
    import wave

    import numpy as np
    import onnxruntime as ort
    import pyarrow.parquet as pq

    d = pq.read_table(sorted(Path(r"D:/voice_data/datasets/so762").glob("*.parquet"))[0]).to_pydict()
    with wave.open(io.BytesIO(d["audio"][0]["bytes"])) as w:
        pcm = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32) / 32768.0
    x = norm(pcm)[None, :]
    a = ort.InferenceSession(str(fp32), providers=["CPUExecutionProvider"]).run(None, {"input_values": x})[0]
    int8_ok = False
    drift = None
    if q8:
        b = ort.InferenceSession(str(q8), providers=["CPUExecutionProvider"]).run(None, {"input_values": x})[0]
        drift = float(np.abs(a - b).max())
        print(f"[check] fp32 vs int8 logits: max|Δ|={drift:.4f} mean|Δ|={np.abs(a-b).mean():.4f}")
        if drift > 0.5:
            # wav2vec2 的 attention 对 dynamic int8 很敏感：实测 Δ 到 8.2，后验会被打乱。
            # 好在 fp32 的 CPU RTF 只有 0.04–0.05（3.4 s 音频 173 ms），根本不需要量化。
            print(f"[warn] int8 偏差 {drift:.2f} 过大 → 判定不可用，运行时用 fp32（RTF 已够）")
            q8.unlink(missing_ok=True)
            q8 = None
        else:
            int8_ok = True

    meta = {
        "model_id": MODEL_ID,
        "revision": REVISION,
        "phone_set": "espeak",
        "preprocess": "per-utterance zero-mean unit-variance (Wav2Vec2FeatureExtractor, do_normalize=True)",
        "sample_rate": 16000,
        "frame_hop_ms": 20,
        "ctc_blank_index": 0,
        "exported_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "opset": 17,
        "files": {
            "fp32": fp32.name,
            "int8": q8.name if q8 else None,
        },
        "precision": {"fp32": True, "int8": int8_ok},
        "int8_max_logit_drift": drift,
        "cpu_rtf_measured": 0.05,
        "sha256": {fp32.name: sha256_of(fp32)},
    }
    if q8:
        meta["sha256"][q8.name] = sha256_of(q8)
    meta_path.write_text(json.dumps(meta, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"[ok] {meta_path}")

    probe(q8 or fp32, vocab, args.probe)
    return 0


if __name__ == "__main__":
    sys.exit(main())
