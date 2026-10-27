Add-Type -AssemblyName System.Drawing
$src = @'
using System;
using System.Runtime.InteropServices;
public class U32d {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(int f, int dx, int dy, int d, int e);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
'@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Drawing
$out = $env:CAP_OUT
$p = Get-Process memory-lens
[void][U32d]::SetForegroundWindow($p.MainWindowHandle)
Start-Sleep -Milliseconds 500
$r = New-Object U32d+RECT
[void][U32d]::GetWindowRect($p.MainWindowHandle, [ref]$r)
# 点"树状"按钮（窗口内约 x=248, y=162 相对客户区）
$cx = [int]($r.Left + 248)
$cy = [int]($r.Top + 162)
[void][U32d]::SetCursorPos($cx, $cy)
Start-Sleep -Milliseconds 250
[U32d]::mouse_event(0x0002, 0, 0, 0, 0)
[U32d]::mouse_event(0x0004, 0, 0, 0, 0)
Start-Sleep -Milliseconds 900
[void][U32d]::GetWindowRect($p.MainWindowHandle, [ref]$r)
$w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$bmp.Save($out)
$g.Dispose(); $bmp.Dispose()
Write-Output ("clicked {0},{1} saved" -f $cx, $cy)
