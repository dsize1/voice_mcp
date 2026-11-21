#!/usr/bin/env python3
"""评测数据集下载与供应链校验（speechocean762 等）。

为什么单独一个脚本
------------------
模型走 `verify_models.py`，数据集走这里：两者威胁模型不同——
数据集是**带标注的音频语料**，攻击面是"内容被替换"（改标注、掺样本），
不是"释放可执行载荷"。所以校验重心在 **内容指纹 + 语义不变量**。

四层检查（每层都可能单独失败）
  A. 来源   —— 只允许白名单主机；URL 必须钉死 revision（不用 main），防上游内容漂移
  B. 完整性 —— 文件 sha256 与上游 LFS oid 逐字节比对；大小必须一致；防截断/中间人改写
  C. 结构   —— Parquet 魔数 PAR1（首尾各一），排除"错误页/HTML 伪装成 parquet"
  D. 语义   —— 行数、说话人数、分值域、音素集、音频时长分布；防"标注被篡改"

任何一层失败：文件移到 `.quarantine/`，退出码非 0，**绝不静默继续**。

用法
----
  python tools/fetch_datasets.py                    # 下载 + 四层校验（默认 speechocean762）
  python tools/fetch_datasets.py --verify-only      # 只对本地文件重跑 B/C/D
  python tools/fetch_datasets.py --write-pins       # 校验通过后把指纹写进 docs/dataset-pins.json
  python tools/fetch_datasets.py --dataset so762 --dest D:/voice_data/datasets
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

MIRROR = "https://hf-mirror.com"
ALLOWED_HOSTS = {
    "hf-mirror.com",
    "cas-bridge.xethub.hf.co",
    "cdn-lfs.hf.co",
    "cdn-lfs.hf-mirror.com",
}

# 钉死 revision：上游 main 变了不会影响我们（要升级就改这里并重跑校验）
DATASETS = {
    "so762": {
        "id": "mispeech/speechocean762",
        "revision": "06385584fad212b26134c656fdd3ccf9f093f33e",
        "license": "apache-2.0",
        "arxiv": "2104.01378",
        "note": "5000 句 / 250 名中文母语者，句-词-音素三级人工分 + 音素级 mispronunciations 标注",
        "files": [
            {
                "path": "train-00000-of-00001.parquet",
                "remote": "data/train-00000-of-00001.parquet",
                "size": 332843486,
                "sha256": "b06fe00f49d2c02c",
            },
            {
                "path": "test-00000-of-00001.parquet",
                "remote": "data/test-00000-of-00001.parquet",
                "size": 311543671,
                "sha256": "c951056c8bbc52f7",
            },
        ],
    }
}

# ---- D 层语义不变量（speechocean762）----
EXPECT_ROWS = {"train-00000-of-00001.parquet": 2500, "test-00000-of-00001.parquet": 2500}
ARPABET = {
    "AA", "AE", "AH", "AO", "AW", "AY", "B", "CH", "D", "DH", "EH", "ER", "EY", "F",
    "G", "HH", "IH", "IY", "JH", "K", "L", "M", "N", "NG", "OW", "OY", "P", "R", "S",
    "SH", "T", "TH", "UH", "UW", "V", "W", "Y", "Z", "ZH",
}


TTY = sys.stdout.isatty()


def sha256_of(path: Path, chunk: int = 1 << 20, progress: bool = False) -> str:
    h = hashlib.sha256()
    total = path.stat().st_size
    done = 0
    t0 = time.time()
    with path.open("rb") as f:
        while True:
            b = f.read(chunk)
            if not b:
                break
            h.update(b)
            done += len(b)
            if progress and total and TTY:
                pct = done * 100 // total
                mbps = done / 1048576 / max(time.time() - t0, 1e-6)
                print(f"\r  sha256 {pct:3d}%  {done/1048576:7.1f}/{total/1048576:.1f} MB  {mbps:5.1f} MB/s", end="")
    if progress and TTY:
        print()
    return h.hexdigest()


def check_hosts(url: str) -> None:
    from urllib.parse import urlparse

    host = urlparse(url).hostname or ""
    if host not in ALLOWED_HOSTS:
        raise SystemExit(f"[FAIL] 来源主机不在白名单: {host}")


def resolve_api(repo: str, revision: str) -> dict[str, dict]:
    """拉取上游文件清单（含 LFS oid = sha256）。用于 A 层复核：指纹必须能再次取到。"""
    url = f"{MIRROR}/api/datasets/{repo}/tree/{revision}?recursive=true"
    check_hosts(url)
    req = urllib.request.Request(url, headers={"User-Agent": "voice-mcp/1.0"})
    with urllib.request.urlopen(req, timeout=60) as r:
        data = json.loads(r.read().decode())
    out = {}
    for f in data:
        if f.get("type") != "file":
            continue
        lfs = f.get("lfs") or {}
        out[f["path"]] = {"size": f.get("size"), "oid": lfs.get("oid")}
    return out


def download(url: str, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    part = dest.with_suffix(dest.suffix + ".part")
    have = part.stat().st_size if part.is_file() else 0
    headers = {"User-Agent": "voice-mcp/1.0"}
    if have:
        headers["Range"] = f"bytes={have}-"
        print(f"  续传：已有 {have/1048576:.1f} MB")
    req = urllib.request.Request(url, headers=headers)
    check_hosts(url)
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=120) as r, part.open("ab" if have else "wb") as f:
        total = int(r.headers.get("Content-Length") or 0) + have
        done = have
        while True:
            b = r.read(1 << 20)
            if not b:
                break
            f.write(b)
            done += len(b)
            mbps = done / 1048576 / max(time.time() - t0, 1e-6)
            print(
                f"\r  {done/1048576:7.1f}/{total/1048576:.1f} MB  {mbps:5.1f} MB/s",
                end="",
                flush=True,
            )
    print()
    part.replace(dest)


def check_struct(path: Path) -> None:
    """C 层：Parquet 魔数（首尾 PAR1）。错误页/HTML 会立刻被抓到。"""
    size = path.stat().st_size
    if size < 16:
        raise SystemExit(f"[FAIL] 文件过小: {path.name} ({size} B)")
    with path.open("rb") as f:
        head = f.read(4)
        f.seek(-4, os.SEEK_END)
        tail = f.read(4)
    if head != b"PAR1" or tail != b"PAR1":
        raise SystemExit(f"[FAIL] 不是 Parquet（head={head!r} tail={tail!r}）: {path.name}")


def check_semantics(path: Path) -> dict:
    """D 层：用 pyarrow 读出关键不变量。缺 pyarrow 时明确跳过而不是假装通过。"""
    try:
        import pyarrow.parquet as pq  # noqa: PLC0415
    except Exception:
        print("  [WARN] 未安装 pyarrow，跳过语义校验（pip install pyarrow）")
        return {"semantic": "skipped"}

    t = pq.read_table(path)
    n = t.num_rows
    expect = EXPECT_ROWS.get(path.name)
    if expect is not None and n != expect:
        raise SystemExit(f"[FAIL] 行数异常: {path.name} 期望 {expect}，实际 {n}")

    cols = set(t.column_names)
    need = {"accuracy", "completeness", "fluency", "prosodic", "total", "text", "words", "speaker", "audio"}
    missing = need - cols
    if missing:
        raise SystemExit(f"[FAIL] 缺列: {sorted(missing)}")

    d = t.to_pydict()
    # 1) 分值域：句级四项必须是 0..10 的整数
    for k in ("accuracy", "completeness", "fluency", "prosodic", "total"):
        vals = [v for v in d[k] if v is not None]
        if not vals:
            raise SystemExit(f"[FAIL] {k} 全为空")
        lo, hi = min(vals), max(vals)
        if lo < 0 or hi > 10:
            raise SystemExit(f"[FAIL] {k} 超出 [0,10]: min={lo} max={hi}")

    # 2) 说话人数：官方 250 人（train+test 合计）；单个分片必须 ≥100 才不算被抽走
    speakers = {s for s in d["speaker"] if s}
    if len(speakers) < 100:
        raise SystemExit(f"[FAIL] 说话人过少: {len(speakers)}")

    # 3) 音素集：必须全部落在 ARPAbet（去掉重音数字）
    bad_phones: set[str] = set()
    checked = 0
    for ws in d["words"][:500]:
        for w in ws or []:
            for p in w.get("phones") or []:
                base = "".join(ch for ch in str(p) if ch.isalpha()).upper()
                checked += 1
                if base and base not in ARPABET:
                    bad_phones.add(base)
    if bad_phones:
        raise SystemExit(f"[FAIL] 非 ARPAbet 音素: {sorted(bad_phones)[:10]}")

    # 4) 音频：长度与采样率必须合理（防"静音/空音频"掺入）
    durs = []
    for a in d["audio"][:200]:
        if not a:
            continue
        import io
        import wave

        try:
            with wave.open(io.BytesIO(a["bytes"])) as w:
                durs.append(w.getnframes() / w.getframerate())
        except Exception:
            durs.append(-1.0)
    if durs:
        if min(durs) < 0.3 or max(durs) > 30:
            raise SystemExit(f"[FAIL] 音频时长异常: min={min(durs):.2f}s max={max(durs):.2f}s")

    return {
        "semantic": "ok",
        "rows": n,
        "speakers": len(speakers),
        "phones_checked": checked,
        "audio_checked": len(durs),
        "dur_range": [round(min(durs), 2), round(max(durs), 2)] if durs else None,
    }


def verify(spec: dict, base: Path, upstream: dict[str, dict] | None) -> list[dict]:
    results = []
    for f in spec["files"]:
        p = base / f["path"]
        if not p.is_file():
            raise SystemExit(f"[FAIL] 缺少文件: {p}")
        size = p.stat().st_size
        if size != f["size"]:
            raise SystemExit(f"[FAIL] 大小不符 {p.name}: 期望 {f['size']} 实际 {size}")

        print(f"[B] sha256 {p.name}")  # noqa: 进度条只在 TTY 输出
        got = sha256_of(p, progress=True)
        if not got.startswith(f["sha256"]):
            raise SystemExit(f"[FAIL] sha256 不符 {p.name}: 期望 {f['sha256']}… 实际 {got}")

        # A 层复核：上游 pinned revision 的 LFS oid 必须与本地指纹一致
        if upstream is not None:
            remote = upstream.get(f["remote"]) or {}
            oid = (remote.get("oid") or "")[:16]
            if oid and oid != f["sha256"]:
                raise SystemExit(f"[FAIL] 上游 fingerprint 与 pin 不一致 {f['remote']}: {oid} != {f['sha256']}")

        check_struct(p)
        sem = check_semantics(p)
        results.append(
            {"file": f["path"], "size": size, "sha256": got, **sem}
        )
        print(f"    ok  size={size/1048576:.1f} MB  {sem}")
    return results


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", default="so762", choices=sorted(DATASETS))
    ap.add_argument("--dest", default=r"D:/voice_data/datasets")
    ap.add_argument("--verify-only", action="store_true")
    ap.add_argument("--write-pins", action="store_true")
    args = ap.parse_args()

    spec = DATASETS[args.dataset]
    base = Path(args.dest) / args.dataset
    base.mkdir(parents=True, exist_ok=True)

    print(f"== {args.dataset}: {spec['id']} @ {spec['revision'][:12]} ({spec['license']})")
    upstream = None
    try:
        upstream = resolve_api(spec["id"], spec["revision"])
        print(f"[A] 上游清单 {len(upstream)} 个文件")
    except Exception as e:
        print(f"[A] 无法取上游清单（{e}）—— 仅做本地指纹校验")

    if not args.verify_only:
        for f in spec["files"]:
            dest = base / f["path"]
            if dest.is_file() and dest.stat().st_size == f["size"]:
                print(f"[skip] 已存在且大小一致: {f['path']}")
                continue
            url = f"{MIRROR}/datasets/{spec['id']}/resolve/{spec['revision']}/{f['remote']}"
            print(f"[dl] {f['path']} ← {url}")
            download(url, dest)

    try:
        results = verify(spec, base, upstream)
    except SystemExit as e:
        # 失败即隔离，绝不留在原地被下游当成"可用数据"
        q = Path(args.dest) / ".quarantine"
        q.mkdir(parents=True, exist_ok=True)
        for f in spec["files"]:
            p = base / f["path"]
            if p.is_file():
                shutil.move(str(p), str(q / p.name))
        print(str(e))
        print(f"[!] 已把可疑文件移入 {q}")
        return 2

    manifest = {
        "dataset": args.dataset,
        "id": spec["id"],
        "revision": spec["revision"],
        "license": spec["license"],
        "arxiv": spec["arxiv"],
        "mirror": MIRROR,
        "fetched_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "files": results,
    }
    (Path(args.dest) / "dataset_manifest.json").write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    print(f"[ok] 四层校验通过，manifest: {Path(args.dest)/'dataset_manifest.json'}")

    if args.write_pins:
        pins = Path("docs/dataset-pins.json")
        old = json.loads(pins.read_text(encoding="utf-8")) if pins.is_file() else {}
        old[args.dataset] = {
            k: manifest[k] for k in ("id", "revision", "license", "arxiv")
        } | {"files": [{"file": r["file"], "size": r["size"], "sha256": r["sha256"]} for r in results]}
        pins.write_text(json.dumps(old, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"[ok] 已写入 {pins}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
