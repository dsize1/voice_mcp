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
    # D13：拼音表是生成物（pypinyin 派生），必须与生成脚本一致。
    # 没装 python/pypinyin 时只提示、不阻断——CI 不装 Python 依赖也能绿。
    $py = Get-Command python -ErrorAction SilentlyContinue
    if ($py) {
        & python -c "import pypinyin" 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) {
            Invoke-Step "python tools/gen_pinyin_table.py --check" {
                python tools/gen_pinyin_table.py --check
            }
        }
        else {
            Write-Host "== 跳过拼音表校验（缺 pypinyin：pip install pypinyin）" -ForegroundColor Yellow
        }
    }
    # 桥接扩展（pi-extensions）的离线测试：假 MCP 服务，不需要 pi 或模型。
    $node = Get-Command node -ErrorAction SilentlyContinue
    if ($node) {
        Invoke-Step "node --test（pi-extensions/mcp-skills）" {
            node --test pi-extensions/mcp-skills/test/bridge.test.mjs pi-extensions/mcp-skills/test/http.test.mjs
        }
    }
    else {
        Write-Host "== 跳过 pi-extensions 测试（缺 node）" -ForegroundColor Yellow
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
