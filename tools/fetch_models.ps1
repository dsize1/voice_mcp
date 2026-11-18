# 模型获取脚本 — 多连接分段下载（中国网络环境优化）
#
# 背景（P0 实测）：
#   github.com / huggingface.co 直连超时；gh-proxy 约 88 KB/s；hf-mirror 单连接 ~100 KB/s，
#   8 路分段并发聚合 ~1.1 MB/s；modelscope.cn 直连可达 22 MB/s（但缺 sherpa-onnx 模型包）。
# 因此：优先多连接分段拉 hf-mirror，可用 ModelScope 时另走 fetch 命令。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File tools/fetch_models.ps1                 # 拉全部
#   powershell -ExecutionPolicy Bypass -File tools/fetch_models.ps1 -Sets vad,kokoro
#   powershell -ExecutionPolicy Bypass -File tools/fetch_models.ps1 -List            # 只看清单

param(
    [string]$OutRoot = "C:/voice_models",
    [string]$Mirror = "https://hf-mirror.com",
    [int]$Parallel = 8,
    [int]$ChunkMB = 8,
    [string[]]$Sets = @(),
    [switch]$List,
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$curl = "curl.exe"

# ---------------------------------------------------------------- 模型清单
# Include 为相对路径的正则；只拉需要的最小集合（int8 优先，跳过 .weights / test_wavs）
$Manifest = @(
    @{ Set = "vad";        Dir = "silero-vad";        Repo = "onnx-community/silero-vad"
       Include = '^onnx/model\.onnx$' }
    @{ Set = "sensevoice"; Dir = "sense-voice";        Repo = "csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17"
       Include = '^(model\.int8\.onnx|tokens\.txt|config\.json)$' }
    @{ Set = "whisper";    Dir = "whisper-turbo";      Repo = "csukuangfj/sherpa-onnx-whisper-turbo"
       Include = '^(turbo-encoder\.int8\.onnx|turbo-decoder\.int8\.onnx|turbo-tokens\.txt)$' }
    @{ Set = "kokoro";     Dir = "kokoro-multi-lang";  Repo = "csukuangfj/kokoro-multi-lang-v1_0"
       # 只要 4 样：model.onnx + voices.bin + tokens.txt + lexicon-*.txt
       # （sherpa-onnx >= 2.0 已不用 espeak-ng：语言由 lexicon 决定，实测警告 "espeak-ng is no longer used"）
       Include = '^(model\.onnx|voices\.bin|tokens\.txt|lexicon-[a-z-]+\.txt)$' }
)

if ($Sets.Count -eq 0) { $Sets = $Manifest | ForEach-Object { $_.Set } }

function Get-Tree($repo) {
    $url = "$Mirror/api/models/$repo/tree/main?recursive=true"
    $raw = & $curl -sS -m 60 $url
    if (-not $raw) { throw "拉取文件清单失败: $repo" }
    $json = $raw | ConvertFrom-Json
    if ($json -isnot [array]) { throw "仓库不存在或清单异常: $repo" }
    return $json | Where-Object { $_.type -eq 'file' }
}

function Encode-RepoPath($rel) {
    # 仓库里有含空格的文件名（如 espeak-ng-data/voices/!v/Mr serious），必须逐段百分号编码
    return (($rel -split '/') | ForEach-Object { [System.Uri]::EscapeDataString($_) }) -join '/'
}

function Download-Chunk($url, $out, $start, $end, $expect) {
    for ($attempt = 1; $attempt -le 6; $attempt++) {
        if (Test-Path $out) { Remove-Item $out -Force -EA SilentlyContinue }
        & $curl -sSL --globoff --retry 2 --retry-delay 2 -m 1800 -r "$start-$end" -o $out $url 2>$null
        if ((Test-Path $out) -and ((Get-Item $out).Length -eq $expect)) { return }
        Start-Sleep -Seconds (2 * $attempt)
    }
    throw "分片下载失败: $out"
}

function Fetch-File($repo, $rel, $size, $dest) {
    $url = "$Mirror/$repo/resolve/main/$(Encode-RepoPath $rel)"
    if ((Test-Path $dest) -and ((Get-Item $dest).Length -eq $size) -and -not $Force) {
        Write-Host "  [skip] $rel ($([math]::Round($size/1MB,1)) MB)" -ForegroundColor DarkGray
        return
    }
    New-Item -ItemType Directory -Force -Path (Split-Path $dest) | Out-Null
    $sw = [Diagnostics.Stopwatch]::StartNew()

    if ($size -le ($ChunkMB * 1MB)) {
        $tmp = "$dest.part"
        Download-Chunk $url $tmp 0 ($size - 1) $size
        Move-Item -Force $tmp $dest
    }
    else {
        $chunk = $ChunkMB * 1MB
        $parts = [System.Collections.Generic.List[object]]::new()
        $partsDir = "$dest.parts"
        New-Item -ItemType Directory -Force -Path $partsDir | Out-Null
        $idx = 0
        for ($off = 0; $off -lt $size; $off += $chunk) {
            $end = [Math]::Min($off + $chunk - 1, $size - 1)
            $parts.Add([pscustomobject]@{ Idx = $idx; Start = $off; End = $end; Path = (Join-Path $partsDir ("{0:D4}" -f $idx)) })
            $idx++
        }

        $queue = [System.Collections.Generic.Queue[object]]::new()
        foreach ($p in $parts) {
            if ((Test-Path $p.Path) -and ((Get-Item $p.Path).Length -eq ($p.End - $p.Start + 1))) { continue }
            $queue.Enqueue($p)
        }
        $running = [System.Collections.Generic.List[object]]::new()
        $done = 0; $total = $parts.Count
        while ($queue.Count -gt 0 -or $running.Count -gt 0) {
            while ($running.Count -lt $Parallel -and $queue.Count -gt 0) {
                $p = $queue.Dequeue()
                $proc = Start-Process -FilePath $curl -PassThru -NoNewWindow -ArgumentList @(
                    '-sSL', '--globoff', '--retry', '2', '--retry-delay', '2', '-m', '1800',
                    '-r', "$($p.Start)-$($p.End)", '-o', $p.Path, $url
                )
                $running.Add([pscustomobject]@{ Proc = $proc; Part = $p })
            }
            $finished = @()
            foreach ($r in $running) { if ($r.Proc.HasExited) { $finished += $r } }
            if ($finished.Count -eq 0) { Start-Sleep -Milliseconds 300; continue }
            foreach ($r in $finished) {
                $running.Remove($r) | Out-Null
                $p = $r.Part
                $expect = $p.End - $p.Start + 1
                if ((Test-Path $p.Path) -and ((Get-Item $p.Path).Length -eq $expect)) {
                    $done++
                    $mbps = if ($sw.Elapsed.TotalSeconds -gt 0) { ((($done * $chunk) / 1MB) / $sw.Elapsed.TotalSeconds) } else { 0 }
                    Write-Progress -Activity "下载 $rel" -Status "$done/$total 分片  ~$([math]::Round($mbps,2)) MB/s" -PercentComplete (100 * $done / $total)
                }
                else {
                    $queue.Enqueue($p)  # 重试
                }
            }
        }
        # 合并
        $fs = [System.IO.File]::Create($dest)
        foreach ($p in ($parts | Sort-Object Idx)) {
            $in = [System.IO.File]::OpenRead($p.Path)
            $in.CopyTo($fs)
            $in.Close()
        }
        $fs.Close()
        Remove-Item -Recurse -Force $partsDir
        $got = (Get-Item $dest).Length
        if ($got -ne $size) { throw "合并后大小不符 $rel : $got != $size" }
    }
    $sw.Stop()
    Write-Host ("  [ok]   {0}  {1:N1} MB  {2:N1}s" -f $rel, ($size / 1MB), $sw.Elapsed.TotalSeconds) -ForegroundColor Green
}

# ---------------------------------------------------------------- 主流程
$selected = $Manifest | Where-Object { $Sets -contains $_.Set }
if ($List) {
    foreach ($m in $selected) {
        $files = Get-Tree $m.Repo | Where-Object { $_.path -match $m.Include }
        $sum = ($files | Measure-Object -Property size -Sum).Sum
        Write-Host ("{0,-12} {1,-46} {2,5} files  {3,7:N0} MB" -f $m.Set, $m.Repo, $files.Count, ($sum / 1MB))
    }
    return
}

New-Item -ItemType Directory -Force -Path $OutRoot | Out-Null

# 先收集全部待下文件（避免串行小文件拖爆总时长），大文件优先
$jobs = [System.Collections.Generic.List[object]]::new()
foreach ($m in $selected) {
    Write-Host "== $($m.Set)  <- $($m.Repo)" -ForegroundColor Cyan
    $files = Get-Tree $m.Repo | Where-Object { $_.path -match $m.Include }
    foreach ($f in $files) {
        $jobs.Add([pscustomobject]@{
            Repo = $m.Repo; Rel = $f.path; Size = $f.size
            Dest = Join-Path (Join-Path $OutRoot $m.Dir) $f.path
        })
    }
}

$pending = $jobs | Where-Object {
    -not ((Test-Path $_.Dest) -and ((Get-Item $_.Dest).Length -eq $_.Size)) -or $Force
}
Write-Host ("待下 {0} 个文件, 共 {1:N0} MB" -f $pending.Count, (($pending | Measure-Object Size -Sum).Sum / 1MB)) -ForegroundColor Yellow

# 阶段 1：大文件（>= 512 KB）逐个下，用满 8 路连接
foreach ($j in ($pending | Where-Object { $_.Size -ge 512KB } | Sort-Object Size -Descending)) {
    Fetch-File $j.Repo $j.Rel $j.Size $j.Dest
}

# 阶段 2：小文件并行（每个一个 curl 进程）
$small = $pending | Where-Object { $_.Size -lt 512KB }
if ($small) {
    Write-Host ("  小文件并行: {0} 个" -f $small.Count) -ForegroundColor DarkGray
    $q = [System.Collections.Generic.Queue[object]]::new()
    foreach ($j in $small) { $q.Enqueue($j) }
    $running = [System.Collections.Generic.List[object]]::new()
    $n = 0
    while ($q.Count -gt 0 -or $running.Count -gt 0) {
        while ($running.Count -lt $Parallel -and $q.Count -gt 0) {
            $j = $q.Dequeue()
            New-Item -ItemType Directory -Force -Path (Split-Path $j.Dest) | Out-Null
            $url = "$Mirror/$($j.Repo)/resolve/main/$(Encode-RepoPath $j.Rel)"
            $tmp = "$($j.Dest).part"
            $p = Start-Process -FilePath $curl -PassThru -NoNewWindow -ArgumentList @(
                '-sSL', '--globoff', '--retry', '3', '--retry-delay', '1', '-m', '300', '-o', $tmp, $url)
            $running.Add([pscustomobject]@{ Proc = $p; Job = $j; Tmp = $tmp })
        }
        $fin = @($running | Where-Object { $_.Proc.HasExited })
        if ($fin.Count -eq 0) { Start-Sleep -Milliseconds 120; continue }
        foreach ($r in $fin) {
            $running.Remove($r) | Out-Null
            if ((Test-Path $r.Tmp) -and ((Get-Item $r.Tmp).Length -eq $r.Job.Size)) {
                Move-Item -Force $r.Tmp $r.Job.Dest
                $n++
            }
            else {
                Remove-Item $r.Tmp -Force -EA SilentlyContinue
                $q.Enqueue($r.Job)   # 重试
            }
        }
        Write-Progress -Activity "小文件" -Status "$n/$($small.Count)" -PercentComplete (100 * $n / $small.Count)
    }
}

Write-Host "全部完成 -> $OutRoot" -ForegroundColor Green
