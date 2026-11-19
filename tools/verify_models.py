#!/usr/bin/env python3
"""模型与二进制依赖的供应链校验。

四层检查：
  A. 完整性   —— 本地文件 sha256 / git-blob-sha1 与上游清单逐文件比对（防截断、防中间人改写）
  B. 结构体   —— ONNX 反序列化后检查算子域、外部数据引用（external_data 是已知的任意路径读写向量）
  C. 数据文件 —— 扩展名白名单 + 魔数检查（防止把可执行文件伪装成模型数据）
  D. 原生库   —— PE 导入表、可疑 Win32 API、内嵌 URL/IP、Authenticode 签名

用法：
  python tools/verify_models.py                 # 校验 C:/voice_models + .cache/sherpa/lib
  python tools/verify_models.py --models-dir D:/m --no-online   # 离线（跳过哈希比对）
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import urllib.request
import zipfile

MIRROR = "https://hf-mirror.com"
SHERPA_VERSION = "2.0.1"

SETS = {
    "silero-vad": "onnx-community/silero-vad",
    "sense-voice": "csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17",
    "whisper-turbo": "csukuangfj/sherpa-onnx-whisper-turbo",
    "kokoro-multi-lang": "csukuangfj/kokoro-multi-lang-v1_0",
}

# 数据文件允许的扩展名（其余一律可疑）
DATA_EXT_OK = {
    "", ".onnx", ".txt", ".bin", ".json", ".md", ".wav", ".yaml", ".yml",
    ".gitattributes", ".model", ".fst", ".int8", ".fp16", ".vocab", ".tsv",
}
EXEC_MAGIC = [b"MZ", b"\x7fELF", b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"#!",
              b"PK\x03\x04"]

# 原生库里绝对不该出现的导入（出现即人工复核）
SUSPICIOUS_IMPORTS = {
    "WinExec", "CreateRemoteThread", "CreateRemoteThreadEx", "VirtualAllocEx",
    "WriteProcessMemory", "SetWindowsHookExA", "SetWindowsHookExW",
    "URLDownloadToFileA", "URLDownloadToFileW", "InternetOpenA", "InternetOpenW",
    "InternetOpenUrlA", "InternetOpenUrlW", "WinHttpOpen", "ShellExecuteA",
    "ShellExecuteW", "RegSetValueExA", "RegSetValueExW", "NtUnmapViewOfSection",
    "AdjustTokenPrivileges", "LsaOpenPolicy", "CreateServiceA", "CreateServiceW",
}

ok, warn, bad = [], [], []


def log(level, msg):
    print({"ok": "  [OK]  ", "warn": "  [WARN]", "bad": "  [FAIL]"}[level] + " " + msg)
    {"ok": ok, "warn": warn, "bad": bad}[level].append(msg)


# ------------------------------------------------------------------ A. 完整性
def sha256_of(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def git_blob_sha1(path: str) -> str:
    size = os.path.getsize(path)
    h = hashlib.sha1(b"blob %d\0" % size)
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def fetch_tree(repo: str) -> dict:
    url = f"{MIRROR}/api/models/{repo}/tree/main?recursive=true"
    with urllib.request.urlopen(url, timeout=60) as r:
        data = json.load(r)
    return {e["path"]: e for e in data if isinstance(e, dict) and e.get("type") == "file"}


def check_integrity(models_dir: str, online: bool):
    print("\n== A. 内容完整性（与上游清单逐文件比对）")
    for sub, repo in SETS.items():
        root = os.path.join(models_dir, sub)
        if not os.path.isdir(root):
            log("warn", f"{sub}: 目录不存在，跳过")
            continue
        if not online:
            log("warn", f"{sub}: 离线模式，跳过哈希比对")
            continue
        try:
            tree = fetch_tree(repo)
        except Exception as e:
            log("bad", f"{sub}: 拉取上游清单失败 {e}")
            continue
        n_ok = n_bad = 0
        for dirpath, _, files in os.walk(root):
            for fn in files:
                full = os.path.join(dirpath, fn)
                rel = os.path.relpath(full, root).replace("\\", "/")
                ent = tree.get(rel)
                if ent is None:
                    log("warn", f"{sub}/{rel}: 上游清单中不存在（多余文件？）")
                    continue
                lfs = ent.get("lfs")
                if lfs:
                    expect, got = lfs["oid"], sha256_of(full)
                    algo = "sha256"
                else:
                    expect, got = ent["oid"], git_blob_sha1(full)
                    algo = "git-sha1"
                if expect == got:
                    n_ok += 1
                else:
                    n_bad += 1
                    log("bad", f"{sub}/{rel}: {algo} 不匹配 expect={expect[:16]}… got={got[:16]}…")
        (log("ok", f"{sub}: {n_ok} 个文件哈希一致") if n_bad == 0
         else log("bad", f"{sub}: {n_bad} 个文件哈希不一致"))


# ------------------------------------------------------------------ B. ONNX 结构
DANGEROUS_PATH_RE = re.compile(
    r"(\\\\|\.\./|[A-Za-z]:\\|/etc/|/home/|%[A-Za-z]+%|https?://|\\\\\.\\|cmd\.exe|powershell)",
    re.IGNORECASE,
)


def check_onnx(models_dir: str):
    print("\n== B. ONNX 结构检查（自定义算子 / 外部数据 / 可疑属性）")
    try:
        import onnx
        from onnx import AttributeProto
    except ImportError:
        log("warn", "未安装 onnx，跳过结构检查（pip install onnx）")
        return

    for dirpath, _, files in os.walk(models_dir):
        for fn in files:
            if not fn.endswith(".onnx"):
                continue
            path = os.path.join(dirpath, fn)
            rel = os.path.relpath(path, models_dir)
            size_mb = os.path.getsize(path) / 1e6
            try:
                m = onnx.load(path, load_external_data=False)
                onnx.checker.check_model(m, full_check=False)
            except Exception as e:
                log("bad", f"{rel}: 无法解析为合法 ONNX ({str(e)[:80]})")
                continue

            g = m.graph
            ops = {}
            for n in g.node:
                ops[n.op_type] = ops.get(n.op_type, 0) + 1
            std = set()
            for o in m.opset_import:
                std.add((o.domain or "ai.onnx", o.version))
            custom = sorted({n.domain for n in g.node if n.domain not in ("", "ai.onnx")})
            ext = [i.name for i in g.initializer if i.data_location == 1]
            ext += [i.name for i in g.initializer
                    if any(e.key == "location" for e in i.external_data)]
            weird = []
            for n in g.node:
                for a in n.attribute:
                    if a.type in (AttributeProto.STRING, AttributeProto.STRINGS):
                        vals = ([a.s] if a.type == AttributeProto.STRING else list(a.strings))
                        for v in vals:
                            try:
                                s = v.decode("utf-8", "ignore")
                            except Exception:
                                continue
                            if DANGEROUS_PATH_RE.search(s):
                                weird.append(f"{n.op_type}.{a.name}={s[:60]}")
            # 检查 metadata 里是否藏了可疑内容
            meta = {p.key: p.value[:80] for p in m.metadata_props}

            tag = f"{rel} ({size_mb:.0f} MB, {len(g.node)} 节点, {len(g.initializer)} 权重块)"
            if custom:
                log("bad", f"{tag}: 含自定义算子域 {custom}（可能触发 DLL 加载）")
            elif ext:
                log("bad", f"{tag}: 含外部数据引用 {ext[:3]}（可读写任意路径）")
            elif weird:
                log("warn", f"{tag}: 属性值含可疑路径/URL {weird[:3]}")
            else:
                log("ok", f"{tag}: 算子域={sorted(std)} 无一外部数据/自定义算子")
            if meta:
                print(f"          metadata: {meta}")
            top = sorted(ops.items(), key=lambda kv: -kv[1])[:6]
            print(f"          ops(top): {top}")


# ------------------------------------------------------------------ C. 数据文件体检
def check_data_files(models_dir: str):
    print("\n== C. 数据文件体检（扩展名白名单 + 魔数）")
    total = 0
    bad_ext, bad_magic = [], []
    for dirpath, _, files in os.walk(models_dir):
        for fn in files:
            path = os.path.join(dirpath, fn)
            rel = os.path.relpath(path, models_dir)
            if ".part" in fn or ".parts" in rel:
                continue  # 下载中的临时分片，不参与校验
            total += 1
            ext = os.path.splitext(fn)[1].lower()
            if ext not in DATA_EXT_OK:
                bad_ext.append(rel)
            try:
                with open(path, "rb") as f:
                    head = f.read(8)
            except OSError:
                continue
            if ext not in (".wav",) and any(head.startswith(m) for m in EXEC_MAGIC):
                bad_magic.append((rel, head[:4]))
    if bad_ext:
        log("bad", f"非白名单扩展名 {bad_ext[:5]}")
    else:
        log("ok", f"{total} 个文件扩展名全部在白名单内")
    if bad_magic:
        for rel, m in bad_magic[:5]:
            log("bad", f"{rel}: 数据文件带可执行魔数 {m!r}")
    else:
        log("ok", "未发现伪装成数据的可执行文件（无 MZ/ELF/Mach-O/shebang/ZIP 头）")


# ------------------------------------------------------------------ D. 原生库
def check_pe(lib_dir: str):
    print("\n== D. 原生库静态扫描（PE 导入表 / 可疑 API / 内嵌 URL / 签名）")
    try:
        import pefile
    except ImportError:
        log("warn", "未安装 pefile，跳过 PE 检查（pip install pefile）")
        return
    if not os.path.isdir(lib_dir):
        log("warn", f"原生库目录不存在: {lib_dir}")
        return

    for fn in sorted(os.listdir(lib_dir)):
        if not fn.lower().endswith(".dll"):
            continue
        path = os.path.join(lib_dir, fn)
        try:
            pe = pefile.PE(path, fast_load=True)
            pe.parse_data_directories(directories=[
                pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_IMPORT"],
                pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT"],
            ])
        except Exception as e:
            log("bad", f"{fn}: PE 解析失败 {e}")
            continue

        imports, dlls = set(), set()
        for attr in ("DIRECTORY_ENTRY_IMPORT", "DIRECTORY_ENTRY_DELAY_IMPORT"):
            for entry in getattr(pe, attr, []) or []:
                dlls.add(entry.dll.decode("latin1"))
                for imp in entry.imports:
                    if imp.name:
                        imports.add(imp.name.decode("latin1"))

        hits = sorted(imports & SUSPICIOUS_IMPORTS)
        size_mb = os.path.getsize(path) / 1e6
        print(f"\n  {fn}  ({size_mb:.1f} MB, {len(imports)} 个导入函数, 依赖 {len(dlls)} 个 DLL)")
        print(f"          依赖: {sorted(dlls)}")
        if hits:
            log("bad", f"{fn}: 命中可疑 API {hits}")
        else:
            log("ok", f"{fn}: 未命中 {len(SUSPICIOUS_IMPORTS)} 项可疑 API 清单")

        # 内嵌字符串里的 URL / IP
        with open(path, "rb") as f:
            blob = f.read()
        urls = set(re.findall(rb"https?://[A-Za-z0-9._/\-]{6,80}", blob))
        ips = set(re.findall(rb"\b(?:\d{1,3}\.){3}\d{1,3}\b", blob))
        urls = {u.decode("latin1") for u in urls}
        # 常见的许可证/规范链接属于正常
        benign = re.compile(r"(opensource\.org|apache\.org|mit-license|gnu\.org|w3\.org|"
                            r"github\.com/(microsoft|onnxruntime|k2-fsa|csukuangfj)|pytorch\.org)",
                            re.I)
        odd_urls = sorted(u for u in urls if not benign.search(u))
        odd_ips = sorted(i.decode() for i in ips if not i.decode().startswith(("0.", "127.", "255.")))
        print(f"          URL: {len(urls)} 个, 其中非白名单 {len(odd_urls)}: {odd_urls[:5]}")
        print(f"          IP : {odd_ips[:5]}")
        if odd_urls:
            log("warn", f"{fn}: 含非白名单 URL（需人工确认，多为错误上报/更新检查）")
        if odd_ips:
            log("warn", f"{fn}: 含硬编码 IP {odd_ips[:3]}")

        # Authenticode
        try:
            import subprocess
            out = subprocess.run(
                ["powershell", "-NoProfile", "-Command",
                 f"(Get-AuthenticodeSignature '{path}').Status"],
                capture_output=True, text=True, timeout=60).stdout.strip()
            (log("ok", f"{fn}: 签名状态 = {out or '未知'}")
             if out in ("Valid",) else
             log("warn", f"{fn}: 签名状态 = {out or '未知'}（开源轮子常见未签名）"))
        except Exception:
            log("warn", f"{fn}: 无法检查签名")


# ------------------------------------------------------------------ E. 轮子哈希
def check_wheel(wheel: str):
    print("\n== E. PyPI 轮子哈希（双镜像交叉比对）")
    if not os.path.exists(wheel):
        log("warn", f"未找到轮子 {wheel}")
        return
    name = os.path.basename(wheel)
    local = sha256_of(wheel)
    print(f"  {name}\n  本地 sha256 = {local}")
    pkg = "sherpa-onnx-core"
    for mirror, label in [
        ("https://pypi.tuna.tsinghua.edu.cn/simple", "清华"),
        ("https://mirrors.aliyun.com/pypi/simple", "阿里云"),
    ]:
        try:
            html = urllib.request.urlopen(f"{mirror}/{pkg}/", timeout=30).read().decode("utf-8", "ignore")
            m = re.search(r'href="[^"]*%s[^"]*#sha256=([0-9a-f]{64})' % re.escape(name), html)
            if not m:
                log("warn", f"{label}: 索引里没找到该文件")
                continue
            expect = m.group(1)
            (log("ok", f"{label}: sha256 一致 ({expect[:16]}…)") if expect == local
             else log("bad", f"{label}: sha256 不一致! 上游={expect[:16]}…"))
        except Exception as e:
            log("warn", f"{label}: 查询失败 {e}")


def write_baseline(models_dir: str, lib_dir: str, wheel: str, out: str):
    """把当前通过校验的哈希全量落盘，作为"已知良好"基线（D28）。"""
    lines = [
        "# 已知良好基线 —— 由 tools/verify_models.py --write-baseline 生成",
        "# 用法：升级模型/依赖后重跑校验，再把新结果覆盖本文件并提交",
        "",
    ]
    n = 0
    for root, _, files in os.walk(models_dir):
        for fn in sorted(files):
            if ".part" in fn:
                continue
            full = os.path.join(root, fn)
            rel = os.path.relpath(full, models_dir).replace("\\", "/")
            if not (fn.endswith((".onnx", ".bin", ".txt", ".fst", ".model", ".json"))):
                continue
            lines.append(f"{sha256_of(full)}  models/{rel}  ({os.path.getsize(full)/1e6:.1f} MB)")
            n += 1
    if os.path.isdir(lib_dir):
        for fn in sorted(os.listdir(lib_dir)):
            if fn.lower().endswith((".dll", ".lib")):
                full = os.path.join(lib_dir, fn)
                lines.append(f"{sha256_of(full)}  native/{fn}  ({os.path.getsize(full)/1e6:.1f} MB)")
                n += 1
    if os.path.exists(wheel):
        lines.append(f"{sha256_of(wheel)}  pypi/{os.path.basename(wheel)}  ({os.path.getsize(wheel)/1e6:.1f} MB)")
        n += 1
    with open(out, "w", encoding="utf-8") as f:
        f.write(chr(10).join(lines) + chr(10))
    print(f"基线已写入 {out}（{n} 条）")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--models-dir", default="C:/voice_models")
    ap.add_argument("--lib-dir", default=".cache/sherpa/lib")
    ap.add_argument("--wheel", default=f".cache/sherpa/sherpa_onnx_core-{SHERPA_VERSION}-py3-none-win_amd64.whl")
    ap.add_argument("--no-online", action="store_true")
    ap.add_argument("--write-baseline", action="store_true",
                    help="校验通过后把全量哈希写入 docs/security-hashes.txt")
    a = ap.parse_args()

    print(f"模型目录 : {a.models_dir}")
    print(f"原生库   : {a.lib_dir}")
    check_integrity(a.models_dir, not a.no_online)
    check_onnx(a.models_dir)
    check_data_files(a.models_dir)
    check_pe(a.lib_dir)
    check_wheel(a.wheel)

    if a.write_baseline and not bad:
        write_baseline(a.models_dir, a.lib_dir, a.wheel,
                       os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                                    "docs", "security-hashes.txt"))

    print("\n" + "=" * 72)
    print(f"结论: OK={len(ok)}  WARN={len(warn)}  FAIL={len(bad)}")
    if bad:
        print("存在问题：")
        for b in bad:
            print("  - " + b)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
