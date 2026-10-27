Add-Type -AssemblyName System.Drawing
$src = @'
using System;
using System.Runtime.InteropServices;
public class U32e {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(int f, int dx, int dy, int d, int e);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
'@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Drawing
$out = $env:CAP_OUT
$dx = [int]$env:CLICK_DX
$dy = [int]$env:CLICK_DY
$p = Get-Process memory-lens
[void][U32e]::SetForegroundWindow($p.MainWindowHandle)
Start-Sleep -Milliseconds 500
$r = New-Object U32e+RECT
[void][U32e]::GetWindowRect($p.MainWindowHandle, [ref]$r)
$cx = [int]($r.Left + $dx)
$cy = [int]($r.Top + $dy)
[void][U32e]::SetCursorPos($cx, $cy)
Start-Sleep -Milliseconds 250
[U32e]::mouse_event(0x0002, 0, 0, 0, 0)
[U32e]::mouse_event(0x0004, 0, 0, 0, 0)
Start-Sleep -Milliseconds 900
[void][U32e]::GetWindowRect($p.MainWindowHandle, [ref]$r)
$w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$bmp.Save($out)
$g.Dispose(); $bmp.Dispose()
Write-Output ("window L={0} T={1}; clicked {2},{3} (dx={4} dy={5})" -f $r.Left,$r.Top,$cx,$cy,$dx,$dy)
