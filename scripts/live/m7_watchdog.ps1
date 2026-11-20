# Process-count watchdog for the M7 review: samples sh/bash/kelir counts every
# second; if they grow by more than $Limit over the baseline taken at start it
# kills the NEW pids (exact pids, never by image name) and exits 9.
# usage: pwsh -File m7_watchdog.ps1 -Seconds 300 [-Limit 20] [-Log path]
param([int]$Seconds = 300, [int]$Limit = 20, [string]$Log = "")
$names = 'sh','bash','kelir'
$base = @{}
Get-Process -Name $names -ErrorAction SilentlyContinue | ForEach-Object { $base[$_.Id] = 1 }
$n0 = $base.Count
$end = (Get-Date).AddSeconds($Seconds)
$peak = 0
while ((Get-Date) -lt $end) {
    $now = @(Get-Process -Name $names -ErrorAction SilentlyContinue)
    $new = @($now | Where-Object { -not $base.ContainsKey($_.Id) })
    if ($new.Count -gt $peak) { $peak = $new.Count }
    if ($new.Count -gt $Limit) {
        $msg = "WATCHDOG TRIP: $($new.Count) new processes; killing by pid"
        Write-Output $msg
        if ($Log) { Add-Content $Log $msg }
        foreach ($p in $new) { try { Stop-Process -Id $p.Id -Force -ErrorAction Stop } catch {} }
        exit 9
    }
    Start-Sleep -Milliseconds 700
}
$msg = "watchdog done: baseline=$n0 peak_new=$peak"
Write-Output $msg
if ($Log) { Add-Content $Log $msg }
