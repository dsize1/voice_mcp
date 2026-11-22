# 提交前门禁 —— 与 .github/workflows/ci.yml 跑同一套步骤
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File tools/check.ps1            # fmt + clippy + test
#   powershell -ExecutionPolicy Bypass -File tools/check.ps1 -SkipTests # 只查格式与 lint
#   powershell -ExecutionPolicy Bypass -File tools/check.ps1 -Cases     # 额外跑 9 条录音回归（需本机音频）
#
# 为什么需要它：HEAD 曾在 `format!` 里写了 `{:.1f}`，编译直接失败，
# 但连续 3 个提交没人发现（没有门禁）。见 ADR-0008 D45。

param(
    [switch]$SkipTests,
    [switch]$Cases
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
Push-Location $repo
try {
    # 原生 DLL 在 .cache 里，跑二进制（以及未来的集成测试）时需要它在 PATH 上
    $libDir = Join-Path $repo ".cache\sherpa\lib"
    if (Test-Path $libDir) { $env:PATH = "$libDir;$env:PATH" }

    function Invoke-Step([string]$Name, [scriptblock]$Body) {
        Write-Host "== $Name" -ForegroundColor Cyan
        & $Body
        if ($LASTEXITCODE -ne 0) {
            throw "$Name 失败（退出码 $LASTEXITCODE）"
        }
    }

    Invoke-Step "cargo fmt --all -- --check" { cargo fmt --all -- --check }
    Invoke-Step "cargo clippy --all-targets -- -D warnings" {
        cargo clippy --all-targets --release -- -D warnings
    }
    if (-not $SkipTests) {
        Invoke-Step "cargo test" { cargo test --release }
    }

    if ($Cases) {
        $casesDir = if ($env:VOICE_CASES_DIR) { $env:VOICE_CASES_DIR } else { "D:/voice_data/cases_audio" }
        if (-not (Test-Path $casesDir)) {
            throw "-Cases 需要录音回归音频：请设置 VOICE_CASES_DIR 或准备 $casesDir"
        }
        $env:VOICE_CASES_DIR = $casesDir
        Invoke-Step "voice-cli cases（录音回归）" {
            cargo run --release -p voice-cli -- cases
        }
    }

    Write-Host "门禁通过 ✅" -ForegroundColor Green
}
finally {
    Pop-Location
}
