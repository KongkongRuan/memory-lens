Add-Type -AssemblyName System.Drawing
$src = @'
using System;
using System.Runtime.InteropServices;
public class U32h {
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
[void][U32h]::SetForegroundWindow($p.MainWindowHandle)
Start-Sleep -Milliseconds 500
$r = New-Object U32h+RECT
[void][U32h]::GetWindowRect($p.MainWindowHandle, [ref]$r)
# 切到进程明细
[void][U32h]::SetCursorPos($r.Left + 200, $r.Top + 118)
[U32h]::mouse_event(2,0,0,0,0); [U32h]::mouse_event(4,0,0,0,0)
Start-Sleep -Milliseconds 900
# 右键第一行进程名（左键 up 用右键 0x08 down / 0x10 up）
[void][U32h]::SetCursorPos($r.Left + 70, $r.Top + 285)
Start-Sleep -Milliseconds 400
[U32h]::mouse_event(0x0008,0,0,0,0)  # right down
[U32h]::mouse_event(0x0010,0,0,0,0)  # right up
Start-Sleep -Milliseconds 800
# 截图（应显示右键菜单）
$w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$bmp.Save($out)
$g.Dispose(); $bmp.Dispose()
Write-Output "right-clicked, saved menu"
