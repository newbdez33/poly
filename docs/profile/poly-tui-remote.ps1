# =====================================================================
# poly — remote monitoring helpers (remote trader → local TUI)
#
# Paste into your PowerShell $PROFILE (or dot-source it). Edit the three
# variables below for your environment.
#
# Aliases:
#   poly-tunnel-up    start SSH tunnel localhost:16379 -> VM:6379
#   poly-tunnel-down  kill tunnel
#   poly-tunnel       status + event count
#   poly-tui-remote   ensure tunnel, launch TUI against VM Redis
# =====================================================================

$PolyVmHost     = 'ubuntu@<vm-ip>'                                                   # edit
$PolyTunnelPort = 16379
$PolyTuiExe     = 'C:\path\to\poly\target\release\poly-tui.exe'                       # edit
$PolyPidFile    = Join-Path $env:TEMP 'poly-tunnel.pid'

function Get-PolyTunnel {
    if (-not (Test-Path $PolyPidFile)) { return $null }
    $tunnelPid = Get-Content $PolyPidFile -ErrorAction SilentlyContinue
    if (-not $tunnelPid) { return $null }
    Get-Process -Id $tunnelPid -ErrorAction SilentlyContinue
}

function Start-PolyTunnel {
    $existing = Get-PolyTunnel
    if ($existing) {
        Write-Host "Tunnel already up - PID $($existing.Id)" -ForegroundColor Yellow
        return
    }
    Write-Host "Starting SSH tunnel localhost:$PolyTunnelPort -> $PolyVmHost`:6379 ..."
    $p = Start-Process ssh -ArgumentList @(
        '-N',
        '-o','BatchMode=yes',
        '-o','ServerAliveInterval=30',
        '-o','ExitOnForwardFailure=yes',
        '-L',"$($PolyTunnelPort):127.0.0.1:6379",
        $PolyVmHost
    ) -PassThru -WindowStyle Hidden
    $p.Id | Out-File -FilePath $PolyPidFile -Encoding ASCII
    Start-Sleep -Milliseconds 1500
    if (Get-Process -Id $p.Id -ErrorAction SilentlyContinue) {
        Write-Host "Tunnel up (PID $($p.Id))." -ForegroundColor Green
    } else {
        Remove-Item $PolyPidFile -ErrorAction SilentlyContinue
        Write-Host "Tunnel failed to start." -ForegroundColor Red
    }
}

function Stop-PolyTunnel {
    $p = Get-PolyTunnel
    if (-not $p) {
        Write-Host "No tunnel running." -ForegroundColor Yellow
        return
    }
    Stop-Process -Id $p.Id -Force
    Remove-Item $PolyPidFile -ErrorAction SilentlyContinue
    Write-Host "Tunnel stopped (was PID $($p.Id))." -ForegroundColor Green
}

function Test-PolyTunnel {
    $p = Get-PolyTunnel
    if (-not $p) {
        Write-Host "Tunnel: DOWN" -ForegroundColor Red
        return
    }
    Write-Host "Tunnel: UP (PID $($p.Id))" -ForegroundColor Green
    try {
        $r = & redis-cli -p $PolyTunnelPort XLEN poly:prod:trader:events 2>&1
        Write-Host "Events on VM redis: $r"
    } catch {
        Write-Host "redis-cli not on PATH - install it or use TUI to verify"
    }
}

function Start-PolyTuiRemote {
    if (-not (Get-PolyTunnel)) { Start-PolyTunnel }
    if (-not (Test-Path $PolyTuiExe)) {
        Write-Host "TUI binary not found: $PolyTuiExe" -ForegroundColor Red
        Write-Host "Build with: cargo build --release --bin poly-tui"
        return
    }
    $prev = $env:REDIS_URL
    $env:REDIS_URL = "redis://127.0.0.1:$PolyTunnelPort"
    try {
        & $PolyTuiExe
    } finally {
        if ($prev) { $env:REDIS_URL = $prev } else { Remove-Item env:REDIS_URL -ErrorAction SilentlyContinue }
    }
}

Set-Alias poly-tunnel-up   Start-PolyTunnel
Set-Alias poly-tunnel-down Stop-PolyTunnel
Set-Alias poly-tunnel      Test-PolyTunnel
Set-Alias poly-tui-remote  Start-PolyTuiRemote
