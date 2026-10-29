# Inspect the cross-process layout acknowledgement independently of widget
# position. A free-gap placement cannot satisfy -RequireApplied.
param([switch]$RequireApplied, [switch]$WaitRestored, [int]$TimeoutSeconds = 15)
$ErrorActionPreference = 'Stop'
$deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
$result = $null
do {
    $mapping = $null
    $view = $null
    try {
        $mapping = [System.IO.MemoryMappedFiles.MemoryMappedFile]::OpenExisting(
            'Local\CodexCapper.TaskbarReservation.v2', [System.IO.MemoryMappedFiles.MemoryMappedFileRights]::Read)
        $view = $mapping.CreateViewAccessor(0, 80, [System.IO.MemoryMappedFiles.MemoryMappedFileAccess]::Read)
        $generation = $view.ReadUInt32(28)
        $result = [ordered]@{
            owner_pid = $view.ReadUInt32(8)
            enabled = [bool]$view.ReadUInt32(12)
            taskbar_hwnd = ('0x{0:x}' -f $view.ReadUInt64(16))
            requested_px = $view.ReadUInt32(24)
            generation = $generation
            acknowledged_generation = $view.ReadUInt32(40)
            status = $view.ReadInt32(44)
            before_width_px = $view.ReadSingle(48)
            after_width_px = $view.ReadSingle(52)
            slot = @($view.ReadInt32(56), $view.ReadInt32(60), $view.ReadInt32(64), $view.ReadInt32(68))
            side = $view.ReadUInt32(72)
        }
        $result['reserved_px'] = $result.before_width_px - $result.after_width_px
        $result['stable'] = $generation -eq $view.ReadUInt32(28)
        if (-not $RequireApplied -and -not $WaitRestored) { break }
        if ($WaitRestored -and $result.status -eq -3) { break }
        if ($RequireApplied -and $result.stable -and $result.enabled -and $result.status -eq 1 -and
            $result.generation -eq $result.acknowledged_generation -and
            $result.reserved_px -ge ($result.requested_px - 2)) { break }
    } catch [System.IO.FileNotFoundException] {
        if (-not $RequireApplied -and -not $WaitRestored) { throw }
    } finally {
        if ($view) { $view.Dispose() }
        if ($mapping) { $mapping.Dispose() }
    }
    Start-Sleep -Milliseconds 250
} while ([DateTime]::UtcNow -lt $deadline)
if ($RequireApplied -and (-not $result -or $result.status -ne 1 -or -not $result.stable -or
    $result.generation -ne $result.acknowledged_generation -or
    $result.reserved_px -lt ($result.requested_px - 2))) {
    throw ('Taskbar label reservation was not applied: ' + ($result | ConvertTo-Json -Compress -Depth 3))
}
if ($WaitRestored -and (-not $result -or $result.status -ne -3)) {
    throw ('Taskbar labels were not restored: ' + ($result | ConvertTo-Json -Compress -Depth 3))
}
$result | ConvertTo-Json -Depth 3
