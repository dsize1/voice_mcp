# 把仓库内 skills/ 安装到 Pi 的用户级 skills 目录（跨项目可用）
# 用法: powershell -ExecutionPolicy Bypass -File tools/install_skills.ps1 [-Uninstall]
param([switch]$Uninstall)

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
