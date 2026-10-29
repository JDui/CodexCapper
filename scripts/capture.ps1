# 截取 CodexCapper 组件窗口的辅助脚本。
#   window 模式：PrintWindow 抓取窗口自身内容（透明背景 PNG，适合放 README）。
#   region 模式：屏幕区域截取，包含组件周围的环境（任务栏等），用于展示固定进任务栏的效果。
# 用法示例：
#   pwsh scripts/capture.ps1 -OutPath docs\media\float-dark.png
#   pwsh scripts/capture.ps1 -Mode region -OutPath docs\media\taskbar.png
param(
    [string]$ProcessName = "codexcapper",
    [Parameter(Mandatory = $true)][string]$OutPath,
    [ValidateSet("window", "region")][string]$Mode = "window",
    [int]$MarginX = 28,
    [int]$MarginY = 14,
    [double]$Scale = 1.25
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
if (-not ("Cap.W" -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
namespace Cap {
  public static class W {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr parent, EnumProc cb, IntPtr l);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, System.Text.StringBuilder name, int max);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr h);
    [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr h, IntPtr dc);
    [DllImport("gdi32.dll")] static extern bool BitBlt(IntPtr target, int x, int y, int w, int h, IntPtr source, int sx, int sy, uint operation);

    public static IntPtr BiggestWindow(uint pid) {
      IntPtr best = IntPtr.Zero; long bestArea = 0;
      EnumProc inspect = (h, l) => {
        uint p; GetWindowThreadProcessId(h, out p);
        if (p != pid || !IsWindowVisible(h)) return true;
        RECT r; if (!GetWindowRect(h, out r)) return true;
        long a = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
        if (a > bestArea) { bestArea = a; best = h; }
        return true;
      };
      EnumWindows((h, l) => {
        inspect(h, l);
        var name = new System.Text.StringBuilder(128);
        GetClassName(h, name, name.Capacity);
        if (name.ToString() == "Shell_TrayWnd" || name.ToString() == "Shell_SecondaryTrayWnd")
          EnumChildWindows(h, inspect, IntPtr.Zero);
        return true;
      }, IntPtr.Zero);
      return best;
    }

    public static string Capture(IntPtr h, string outPath, string mode, int mx, int my, double scale) {
      if (mode == "region") SetProcessDPIAware(); // otherwise the screen is virtualized and the shot comes out soft
      RECT r; if (!GetWindowRect(h, out r)) return "no rect";
      if (mode == "window") {
        int w = (int)Math.Round((r.Right - r.Left) * scale);
        int hh = (int)Math.Round((r.Bottom - r.Top) * scale);
        if (w < 8 || hh < 8) return "too small " + w + "x" + hh;
        using (var bmp = new Bitmap(w, hh, PixelFormat.Format32bppArgb))
        using (var g = Graphics.FromImage(bmp)) {
          IntPtr hdc = g.GetHdc();
          bool ok = PrintWindow(h, hdc, 2); // PW_RENDERFULLCONTENT
          g.ReleaseHdc(hdc);
          bmp.Save(outPath, ImageFormat.Png);
          return "ok window " + w + "x" + hh + " printwindow=" + ok;
        }
      } else {
        int x = r.Left - mx, y = r.Top - my;
        int w = (r.Right - r.Left) + mx * 2, hh = (r.Bottom - r.Top) + my * 2;
        using (var bmp = new Bitmap(w, hh, PixelFormat.Format24bppRgb))
        using (var g = Graphics.FromImage(bmp)) {
          IntPtr source = GetDC(IntPtr.Zero), target = g.GetHdc();
          try { BitBlt(target, 0, 0, w, hh, source, x, y, 0x40cc0020); } // SRCCOPY | CAPTUREBLT includes layered click-through children
          finally { g.ReleaseHdc(target); ReleaseDC(IntPtr.Zero, source); }
          bmp.Save(outPath, ImageFormat.Png);
          return "ok region " + w + "x" + hh + " at " + x + "," + y;
        }
      }
    }
  }
}
'@ -ReferencedAssemblies (@([System.Drawing.Bitmap].Assembly.Location, [System.Drawing.Size].Assembly.Location, 'System.Runtime', 'System.ComponentModel.Primitives') + @(Get-ChildItem -LiteralPath $PSHOME -Filter 'System.Private.Windows*.dll' | ForEach-Object FullName))
}
$p = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $p) { throw "process '$ProcessName' is not running" }
$h = [Cap.W]::BiggestWindow([uint32]$p.Id)
if ($h -eq [IntPtr]::Zero) { throw "no visible window for pid $($p.Id)" }
$dir = Split-Path -Parent $OutPath
if ($dir) { New-Item -ItemType Directory -Force $dir | Out-Null }
[Cap.W]::Capture($h, $OutPath, $Mode, $MarginX, $MarginY, $Scale)
