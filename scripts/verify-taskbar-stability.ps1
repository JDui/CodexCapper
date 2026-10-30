# Exercise the shell menu that used to erase one monitor's taskbar measurement.
# Run against a taskbar-mode instance. No settings or Explorer processes change.
param(
    [int]$ProcessId = 0,
    [ValidateRange(1, 100)][int]$Iterations = 5,
    [ValidateRange(500, 30000)][int]$HoldMilliseconds = 1800,
    [switch]$RightClickStart
)
$ErrorActionPreference = 'Stop'
if (-not $ProcessId) { $ProcessId = (Get-Process -Name codexcapper | Select-Object -First 1).Id }
& (Join-Path $PSScriptRoot 'verify-taskbar.ps1') -ProcessId $ProcessId | Out-Null

if (-not ('CapperStability.Input' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
namespace CapperStability {
  public static class Input {
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool GetPhysicalCursorPos(out POINT point);
    [DllImport("user32.dll")] public static extern bool SetPhysicalCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern void mouse_event(uint flags, uint x, uint y, uint data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr parent, IntPtr after, string name, string title);
    static void Key(byte key, bool up) { keybd_event(key, 0, up ? 2u : 0u, UIntPtr.Zero); }
    public static void WinX() { Key(0x5b,false); Key(0x58,false); Key(0x58,true); Key(0x5b,true); }
    public static void Escape() { Key(0x1b,false); Key(0x1b,true); }
    public static void RightClick() { mouse_event(8,0,0,0,UIntPtr.Zero); mouse_event(16,0,0,0,UIntPtr.Zero); }
  }
}
'@
}
$widget = [CapperVerify.Native]::Find([uint32]$ProcessId, 'CodexCapperTaskbarHost')[0]
$parent = [CapperVerify.Native]::GetParent($widget)
$explorerId = [uint32]0
[void][CapperVerify.Native]::GetWindowThreadProcessId($parent, [ref]$explorerId)
$checks = 0
$opened = 0
$menuRequested = $false
$savedCursor = $null

function Assert-StillEmbedded {
    if (-not (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)) { throw "Process $ProcessId exited" }
    $children = [CapperVerify.Native]::Find([uint32]$ProcessId, 'CodexCapperTaskbarHost')
    $visible = @($children | Where-Object { [CapperVerify.Native]::IsWindowVisible($_) })
    if ($visible.Count -ne 1 -or [CapperVerify.Native]::GetParent($visible[0]) -ne $parent) {
        throw 'Native child left its taskbar while the shell menu was open'
    }
    $childRect = [CapperVerify.Native]::PhysicalRect($visible[0])
    $barRect = [CapperVerify.Native]::PhysicalRect($parent)
    if ($childRect.Left -lt $barRect.Left -or $childRect.Top -lt $barRect.Top -or
        $childRect.Right -gt $barRect.Right -or $childRect.Bottom -gt $barRect.Bottom) {
        throw 'Native child moved outside the taskbar'
    }
    $webviews = [CapperVerify.Native]::Find([uint32]$ProcessId, 'Tauri Window')
    if (@($webviews | Where-Object { [CapperVerify.Native]::IsWindowVisible($_) }).Count) {
        throw 'Floating WebView appeared during taskbar placement'
    }
}

try {
    for ($iteration = 0; $iteration -lt $Iterations; $iteration++) {
        if ($RightClickStart) {
            $start = [CapperStability.Input]::FindWindowExW($parent, [IntPtr]::Zero, 'Start', $null)
            if ($start -ne [IntPtr]::Zero) {
                $r = [CapperVerify.Native]::PhysicalRect($start)
                $x = [int](($r.Left + $r.Right) / 2)
                $y = [int](($r.Top + $r.Bottom) / 2)
                $point = (($y -band 0xffff) -shl 16) -bor ($x -band 0xffff)
                if (-not [CapperStability.Input]::PostMessageW($start, 0x7b, $start, [IntPtr]::new($point))) {
                    throw 'Could not request the Start button context menu'
                }
            } else {
                # Modern Win11 draws Start in XAML instead of a child HWND.
                Add-Type -AssemblyName UIAutomationClient
                Add-Type -AssemblyName UIAutomationTypes
                $root = [System.Windows.Automation.AutomationElement]::FromHandle($parent)
                $condition = [System.Windows.Automation.PropertyCondition]::new(
                    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'StartButton')
                $button = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
                if (-not $button) { throw 'Start button could not be measured' }
                $r = $button.Current.BoundingRectangle
                if ($r.IsEmpty -or $r.Width -le 0 -or $r.Height -le 0) { throw 'Start button has no visible area' }
                $savedCursor = [CapperStability.Input+POINT]::new()
                if (-not [CapperStability.Input]::GetPhysicalCursorPos([ref]$savedCursor)) { throw 'Could not save the cursor position' }
                if (-not [CapperStability.Input]::SetPhysicalCursorPos([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))) {
                    throw 'Could not point at the Start button'
                }
                [CapperStability.Input]::RightClick()
            }
        } else { [CapperStability.Input]::WinX() }

        $menuRequested = $true
        $menuDeadline = [DateTime]::UtcNow.AddSeconds(2)
        do {
            Start-Sleep -Milliseconds 100
            $menus = [CapperVerify.Native]::Find($explorerId, '#32768')
            $menus += [CapperVerify.Native]::Find($explorerId, 'WinXMenu')
            $menus += [CapperVerify.Native]::Find($explorerId, 'Xaml_WindowedPopupClass')
            $menuVisible = @($menus | Where-Object { [CapperVerify.Native]::IsWindowVisible($_) }).Count -gt 0
        } while (-not $menuVisible -and [DateTime]::UtcNow -lt $menuDeadline)
        if ($null -ne $savedCursor) {
            [void][CapperStability.Input]::SetPhysicalCursorPos($savedCursor.X, $savedCursor.Y)
            $savedCursor = $null
        }
        if (-not $menuVisible) {
            throw 'The shell menu did not open; this run cannot verify the regression'
        }
        $opened++
        $until = [DateTime]::UtcNow.AddMilliseconds($HoldMilliseconds)
        do {
            Assert-StillEmbedded
            $checks++
            Start-Sleep -Milliseconds 100
        } while ([DateTime]::UtcNow -lt $until)
        [CapperStability.Input]::Escape()
        $menuRequested = $false
        Start-Sleep -Milliseconds 400
        Assert-StillEmbedded
    }
} finally {
    if ($null -ne $savedCursor) { [void][CapperStability.Input]::SetPhysicalCursorPos($savedCursor.X, $savedCursor.Y) }
    if ($menuRequested) { [CapperStability.Input]::Escape() }
}
[ordered]@{
    process_id = $ProcessId
    trigger = $(if ($RightClickStart) { 'Start context menu' } else { 'Win+X' })
    menus_opened = $opened
    embedded_checks = $checks
    floating_fallback_seen = $false
    process_alive = [bool](Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)
} | ConvertTo-Json
