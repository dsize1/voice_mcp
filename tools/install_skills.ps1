# 已废弃：技能现在由 voice-mcp 通过 MCP 提供（SEP-2640），见 docs/adr/0012-skills-over-mcp.md。
# 本脚本会把 skills/ 复制/junction 到用户级目录，制造会滞后的副本；保留仅作断网/服务异常时的临时兜底。
# 临时用法：pi --skill <仓库>/skills/<name>（不要装到 ~/.pi/agent/skills）
#
# 把仓库内 skills/ 安装到 Pi 的用户级 skills 目录（跨项目可用）
# 用法: powershell -ExecutionPolicy Bypass -File tools/install_skills.ps1 [-Uninstall]
param([switch]$Uninstall)

Write-Warning "install_skills.ps1 已废弃：技能由 voice-mcp 通过 MCP 提供（ADR-0012）。安装副本会导致技能滞后。"

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$src  = Join-Path $repo "skills"
$dest = Join-Path $env:USERPROFILE ".pi\agent\skills"
New-Item -ItemType Directory -Force -Path $dest | Out-Null

Get-ChildItem $src -Directory | ForEach-Object {
    $target = Join-Path $dest $_.Name
    if ($Uninstall) {
        if (Test-Path $target) { Remove-Item -Recurse -Force $target; Write-Host "removed  $($_.Name)" }
        return
    }
    if (Test-Path $target) { Remove-Item -Recurse -Force $target }
    # 优先用目录联接（免管理员、仍指向仓库，改代码立即生效）
    $linked = $false
    try {
        cmd /c mklink /J "`"$target`"" "`"$($_.FullName)`"" | Out-Null
        $linked = $true
    } catch { }
    if (-not $linked) { Copy-Item -Recurse -Force $_.FullName $target }
    Write-Host ("{0}  {1}" -f $(if ($linked) { "junction" } else { "copy    " }), $_.Name)
}
Write-Host "-> $dest"
