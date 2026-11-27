# 构建包装脚本 —— 专门解决 D18：sherpa-onnx 的 DLL 被占用 / 没复制到输出目录
#
# 背景（2026-11-27 实测两次）：
#   `sherpa-onnx-sys` 的 build.rs 会把 prebuilt DLL 复制进 target/<profile>，
#   如果此时有 voice-mcp / voice-cli 进程正持有那些 DLL，构建立刻 panic：
#       "另一个程序正在使用此文件，进程无法访问。 (os error 32)"
#   报错长得像编译器错误，实际只是"先停进程"。这个脚本负责把它变成一句人话。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File tools/build.ps1                 # 需先停掉 MCP
#   powershell -ExecutionPolicy Bypass -File tools/build.ps1 -AltTarget      # 不动进程，构建到 %TEMP%
#   powershell -ExecutionPolicy Bypass -File tools/build.ps1 -Profile debug
#
# 顺序（重要）：停旧进程 → 构建 → 启动。不要在占用状态下覆盖二进制。

param(
    [ValidateSet("debug", "release")]
    [string]$Profile = "release",
    [switch]$AltTarget
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Push-Location $repo
try {
    $running = Get-Process -Name voice-mcp, voice-cli -ErrorAction SilentlyContinue
    if ($running -and -not $AltTarget) {
        $list = ($running | ForEach-Object { "$($_.ProcessName)#$($_.Id)" }) -join ", "
        Write-Host "✗ 检测到 voice 进程正在运行：$list" -ForegroundColor Yellow
        Write-Host "  sherpa-onnx 的 build.rs 需要写 target\$Profile\*.dll，被占用会直接 panic（os error 32）。"
        Write-Host "  二选一："
        Write-Host "    A) 先停掉再构建： Stop-Process -Id $($running[0].Id)    然后重启 MCP / pi"
        Write-Host "    B) 不动进程：     ./tools/build.ps1 -AltTarget        （构建到 %TEMP%，产品不受影响）"
        exit 2
    }

    if ($AltTarget) {
        $env:CARGO_TARGET_DIR = Join-Path $env:TEMP "voice_mcp_target"
        $env:SHERPA_ONNX_LIB_DIR = Join-Path $env:TEMP "voice_mcp_sherpa_lib"
        New-Item -ItemType Directory -Force -Path $env:SHERPA_ONNX_LIB_DIR | Out-Null
        Copy-Item "$repo\.cache\sherpa\lib\*" $env:SHERPA_ONNX_LIB_DIR -Force
        Write-Host "→ 临时 target: $env:CARGO_TARGET_DIR" -ForegroundColor Cyan
    }

    $cargoArgs = @("build")
    if ($Profile -eq "release") { $cargoArgs += "--release" }
    $cargoArgs += @("-p", "voice-cli", "-p", "voice-mcp")
    Write-Host "→ cargo $($cargoArgs -join ' ')" -ForegroundColor Cyan
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # 幂等补 DLL：build.rs 有时不复制（或复制失败），运行期会 "DLL 找不到"
    $out = if ($env:CARGO_TARGET_DIR) {
        Join-Path $env:CARGO_TARGET_DIR $Profile
    } else {
        Join-Path $repo "target\$Profile"
    }
    $copied = 0
    Get-ChildItem "$repo\.cache\sherpa\lib\*.dll" | ForEach-Object {
        $dst = Join-Path $out $_.Name
        try {
            Copy-Item $_.FullName $dst -Force
            $copied++
        } catch {
            Write-Host "! 复制 $($_.Name) 失败（被占用？停进程后重试）" -ForegroundColor Yellow
        }
    }
    Write-Host "→ DLL 已就位：$copied 个 → $out" -ForegroundColor Cyan

    foreach ($exe in @("voice-cli.exe", "voice-mcp.exe")) {
        $p = Join-Path $out $exe
        if (-not (Test-Path $p)) { Write-Host "✗ 缺少产物 $p" -ForegroundColor Red; exit 1 }
        $item = Get-Item $p
        Write-Host ("  {0,-16} {1,10:N0} B  {2:HH:mm:ss}" -f $exe, $item.Length, $item.LastWriteTime)
    }
    Write-Host "完成。要跑产品路径（MCP）请重启 pi；只跑 CLI 可直接用 $out\voice-cli.exe" -ForegroundColor Green
}
finally {
    Pop-Location
}
