# Launch CodexCapper with --demo and capture one widget screenshot.
# Demo values are triangle waves; wait delaySec so the bars sit at nice numbers.
param(
    [ValidateSet("dark", "light")][string]$Theme = "dark",
    [ValidateSet("float", "taskbar")][string]$Mode = "float",
    [bool]$Compact = $false,
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$DelaySec = 133,
    [double]$Scale = 1.25
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "src-tauri\target\release\codexcapper.exe"
if (-not (Test-Path $exe)) { throw "release exe missing: $exe" }

Stop-Process -Name codexcapper -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 800

$dir = Join-Path $env:APPDATA "CodexCapper"
New-Item -ItemType Directory -Force $dir | Out-Null
# Mirrors WidgetSettings in settings.rs; fullscreen hiding is off so the
# capture does not race the app hiding itself.
$settings = [ordered]@{
    schema_version = 1
    widget         = [ordered]@{
        visible               = $true
        monitor_stable_id     = $null
        monitor_friendly_name = $null
        corner                = "bottom-right"
        edge_offset_x         = 0
        edge_offset_y         = 0
        compact_mode          = $Compact
        click_through         = $false
        mode                  = $Mode
        tray_gap              = 8
        dock_side             = "right"
        reset_time_style      = "countdown"
        hide_on_fullscreen    = $false
    }
    appearance     = @{ theme = $Theme }
    polling        = @{ http_interval_secs = 3600 }
    autostart      = $false
    updates        = @{ check = $false; last_check = $null }
} | ConvertTo-Json -Depth 6
Set-Content -Path (Join-Path $dir "settings.json") -Value $settings -Encoding ascii

Write-Host "launch --demo theme=$Theme mode=$Mode compact=$Compact"
Start-Process -FilePath $exe -ArgumentList "--demo" | Out-Null
Start-Sleep -Seconds $DelaySec

$capMode = if ($Mode -eq "taskbar") { "region" } else { "window" }
& (Join-Path $PSScriptRoot "capture.ps1") -OutPath $Out -Mode $capMode -Scale $Scale
Stop-Process -Name codexcapper -Force -ErrorAction SilentlyContinue
