# 原生依赖获取脚本 —— sherpa-onnx Windows 共享库
#
# 为什么这样做（P0 实测）：
#   - sherpa-onnx-sys 默认从 GitHub Releases 下载 122 MB 预编译包 → 本机直连超时，
#     gh-proxy 仅 ~88 KB/s（15 分钟只下到 33 MB）。
#   - GitHub 不可用、C++ 工具链未完全确认 → 不编译源码。
#   - 发现 PyPI 的 sherpa_onnx_core 轮子（17 MB）里就带 Windows 共享库 + import lib，
#     PyPI 清华镜像 ~2 s 下完 → 用轮子代替 GitHub 预编译包。
#
# 产物：
#   .cache/sherpa/lib/{sherpa-onnx-c-api.dll,.lib, onnxruntime.dll, onnxruntime.lib}
# 之后 .cargo/config.toml 里的 SHERPA_ONNX_LIB_DIR 指向该目录，sherpa-onnx 走 shared 特性链接。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File tools/fetch_deps.ps1 [-Version 2.0.1] [-Force]

param(
    [string]$Version = "2.0.1",
    [string]$LibDir = "",
    [string]$PypiMirror = "https://pypi.tuna.tsinghua.edu.cn/simple",
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
if (-not $LibDir) { $LibDir = Join-Path $repo ".cache/sherpa/lib" }
$cache = Join-Path $repo ".cache/sherpa"
New-Item -ItemType Directory -Force -Path $LibDir, $cache | Out-Null

function Find-MsvcTool($name) {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) { return $null }
    $root = & $vswhere -products * -property installationPath
    if (-not $root) { return $null }
    $hit = Get-ChildItem -Path "$root\VC\Tools\MSVC" -Directory -EA SilentlyContinue |
        Sort-Object Name -Descending |
        ForEach-Object { Join-Path $_.FullName "bin\Hostx64\x64\$name" } |
        Where-Object { Test-Path $_ } |
        Select-Object -First 1
    return $hit
}

# 1) 拉轮子
$whl = Get-ChildItem -Path $cache -Filter "sherpa_onnx_core-$Version-*.whl" -EA SilentlyContinue | Select-Object -First 1
if (-not $whl -or $Force) {
    Write-Host "== 下载 sherpa_onnx_core==$Version 轮子 (PyPI 镜像)" -ForegroundColor Cyan
    & python -m pip download "sherpa-onnx-core==$Version" --no-deps -d $cache --index-url $PypiMirror | Out-Null
    $whl = Get-ChildItem -Path $cache -Filter "sherpa_onnx_core-$Version-*.whl" | Select-Object -First 1
}
if (-not $whl) { throw "未能取得 sherpa_onnx_core 轮子" }
Write-Host "  轮子: $($whl.Name)" -ForegroundColor DarkGray

# 2) 解压出 sherpa_onnx/lib/*
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::OpenRead($whl.FullName)
try {
    foreach ($e in $zip.Entries) {
        if ($e.FullName -like "sherpa_onnx/lib/*" -and $e.Length -gt 0) {
            $out = Join-Path $LibDir ([System.IO.Path]::GetFileName($e.FullName))
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($e, $out, $true)
        }
    }
}
finally { $zip.Dispose() }

# 3) 生成 onnxruntime.lib（轮子里只有 dll，sherpa-onnx 的 link 指令要求 onnxruntime.lib）
$ortLib = Join-Path $LibDir "onnxruntime.lib"
if (-not (Test-Path $ortLib) -or $Force) {
    $ortDll = Join-Path $LibDir "onnxruntime.dll"
    if (-not (Test-Path $ortDll)) { throw "缺少 onnxruntime.dll" }
    $defPath = Join-Path $LibDir "onnxruntime.def"
    $dumpbin = Find-MsvcTool "dumpbin.exe"
    if ($dumpbin) {
        $out = & $dumpbin -nologo -exports $ortDll
        $names = $out | ForEach-Object {
            if ($_ -match '^\s+\d+\s+[0-9A-Fa-f]+\s+[0-9A-Fa-f]+\s+(\S+)\s*$') { $Matches[1] }
        }
        if ($names.Count -eq 0) { throw "dumpbin 未解析出导出符号" }
        Write-Host "  导出符号 $($names.Count) 个: $($names -join ', ')" -ForegroundColor DarkGray
    }
    else {
        # 工具链缺失时的兜底：onnxruntime 的 C API 只有这两个导出
        $names = @("OrtGetApiBase", "OrtSessionOptionsAppendExecutionProvider_CPU")
        Write-Host "  未找到 dumpbin，使用已知导出符号列表" -ForegroundColor Yellow
    }
    $lines = @("LIBRARY onnxruntime.dll", "EXPORTS") + ($names | ForEach-Object { "    $_" })
    Set-Content -Path $defPath -Value $lines -Encoding ASCII
    $libexe = Find-MsvcTool "lib.exe"
    if (-not $libexe) { throw "找不到 MSVC lib.exe，无法生成 onnxruntime.lib" }
    & $libexe -nologo -machine:x64 "-def:$defPath" "-out:$ortLib" | Out-Null
    Remove-Item $defPath -EA SilentlyContinue
}
Write-Host "== 完成. SHERPA_ONNX_LIB_DIR = $LibDir" -ForegroundColor Green
Get-ChildItem $LibDir | Format-Table Name, Length -AutoSize
