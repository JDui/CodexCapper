# Verify a running native taskbar widget using HWND relationships, not its
# screen position. RecreateChild only closes our own child, never Explorer.
param(
    [int]$ProcessId = 0,
    [switch]$RecreateChild,
    [switch]$OpenContextMenu
)
$ErrorActionPreference = 'Stop'
if (-not ('CapperVerify.Native' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
namespace CapperVerify {
  public static class Native {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr parent, EnumProc cb, IntPtr l);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr h);
    [DllImport("user32.dll", EntryPoint="GetWindowLongPtrW")] public static extern IntPtr GetWindowLongPtr(IntPtr h, int index);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT point);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    public static string Class(IntPtr h) { var s = new StringBuilder(128); GetClassName(h,s,s.Capacity); return s.ToString(); }
    public static IntPtr[] Find(uint pid, string wanted) {
      var found = new List<IntPtr>();
      EnumProc inspect = (h,l) => {
        uint p; GetWindowThreadProcessId(h,out p);
        if (p == pid && Class(h) == wanted) found.Add(h);
        return true;
      };
      EnumWindows((h,l) => {
        inspect(h,l);
        string c = Class(h);
        if (c == "Shell_TrayWnd" || c == "Shell_SecondaryTrayWnd") EnumChildWindows(h,inspect,IntPtr.Zero);
        return true;
      },IntPtr.Zero);
      return found.ToArray();
    }
    public static RECT PhysicalRect(IntPtr h) {
      IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
      try { RECT r; GetWindowRect(h, out r); return r; }
      finally { SetThreadDpiAwarenessContext(previous); }
    }
  }
}
'@
}
if (-not $ProcessId) {
    $capperProcess = Get-Process -Name codexcapper -ErrorAction Stop | Select-Object -First 1
    $ProcessId = $capperProcess.Id
}
function Find-Widget {
    $found = [CapperVerify.Native]::Find([uint32]$ProcessId, 'CodexCapperTaskbarHost')
    if ($found.Count) { return $found[0] }
    return [IntPtr]::Zero
}
$deadline = [DateTime]::UtcNow.AddSeconds(12)
do {
    $widget = Find-Widget
    if ($widget -ne [IntPtr]::Zero -and [CapperVerify.Native]::IsWindowVisible($widget)) { break }
    Start-Sleep -Milliseconds 250
} while ([DateTime]::UtcNow -lt $deadline)
if ($widget -eq [IntPtr]::Zero) { throw "No native taskbar child for process $ProcessId" }
$parent = [CapperVerify.Native]::GetParent($widget)
$parentClass = [CapperVerify.Native]::Class($parent)
$style = [CapperVerify.Native]::GetWindowLongPtr($widget, -16).ToInt64()
$extended = [CapperVerify.Native]::GetWindowLongPtr($widget, -20).ToInt64()
if ($parentClass -notin @('Shell_TrayWnd', 'Shell_SecondaryTrayWnd')) { throw "Wrong parent: $parentClass" }
if (-not ($style -band 0x40000000)) { throw 'WS_CHILD is missing' }
if ($style -band 0x80000000L) { throw 'Widget is a popup' }
if ($extended -band 8) { throw 'Widget is independently topmost' }
if (-not ($extended -band 0x80000)) { throw 'Layered child composition is missing' }
if (-not [CapperVerify.Native]::IsWindowVisible($widget)) { throw 'Widget is hidden' }
$rect = [CapperVerify.Native]::PhysicalRect($widget)
$bar = [CapperVerify.Native]::PhysicalRect($parent)
if ($rect.Left -lt $bar.Left -or $rect.Top -lt $bar.Top -or $rect.Right -gt $bar.Right -or $rect.Bottom -gt $bar.Bottom) {
    throw 'Widget is outside the taskbar'
}
if ($rect.Right -le $rect.Left -or $rect.Bottom -le $rect.Top) { throw 'Widget has no visible area' }
$recreated = $false
if ($RecreateChild) {
    $oldWidget = $widget
    [void][CapperVerify.Native]::PostMessage($widget, 0x10, [IntPtr]::Zero, [IntPtr]::Zero)
    $deadline = [DateTime]::UtcNow.AddSeconds(12)
    do {
        Start-Sleep -Milliseconds 250
        $widget = Find-Widget
        if ($widget -ne [IntPtr]::Zero -and $widget -ne $oldWidget -and [CapperVerify.Native]::IsWindowVisible($widget)) {
            $recreated = $true
            break
        }
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $recreated) { throw 'Native child did not recover after destruction' }
}
$menuVisible = $false
if ($OpenContextMenu) {
    [void][CapperVerify.Native]::PostMessage($widget, 0x7b, $widget, [IntPtr]::new(-1))
    Start-Sleep -Milliseconds 750
    $menus = [CapperVerify.Native]::Find([uint32]$ProcessId, '#32768')
    $menuVisible = @($menus | Where-Object { [CapperVerify.Native]::IsWindowVisible($_) }).Count -gt 0
    # Cancel only this process's menu loop, leaving unrelated apps alone.
    $owners = [CapperVerify.Native]::Find([uint32]$ProcessId, 'Tauri Window')
    foreach ($owner in $owners) { [void][CapperVerify.Native]::PostMessage($owner, 0x1f, [IntPtr]::Zero, [IntPtr]::Zero) }
    foreach ($menu in $menus) { [void][CapperVerify.Native]::PostMessage($menu, 0x100, [IntPtr]::new(27), [IntPtr]::Zero) }
    if (-not $menuVisible) { throw 'Native widget context menu did not open' }
}
[ordered]@{
    process_id = $ProcessId
    child_hwnd = ('0x{0:x}' -f $widget.ToInt64())
    parent_hwnd = ('0x{0:x}' -f $parent.ToInt64())
    parent_class = $parentClass
    ws_child = $true
    layered_composition = $true
    click_through = [bool]($extended -band 0x20)
    independently_topmost = $false
    visible = [CapperVerify.Native]::IsWindowVisible($widget)
    physical_rect = $rect
    child_recreated = $recreated
    context_menu_opened = $menuVisible
} | ConvertTo-Json -Depth 4
