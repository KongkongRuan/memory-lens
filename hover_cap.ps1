Add-Type -AssemblyName System.Drawing
$src = @'
using System;
using System.Runtime.InteropServices;
public class U32h4 {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
'@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Drawing
$out = $env:CAP_OUT
$p = Get-Process memory-lens
[void][U32h4]::SetForegroundWindow($p.MainWindowHandle)
Start-Sleep -Milliseconds 400
$r = New-Object U32h4+RECT
[void][U32h4]::GetWindowRect($p.MainWindowHandle, [ref]$r)
# 悬停在第一行进程名 node.exe 上（窗口内约 x=80, y=243）
$cx = [int]($r.Left + 80)
$cy = [int]($r.Top + 243)
[void][U32h4]::SetCursorPos($cx, $cy)
Start-Sleep -Milliseconds 1800
[void][U32h4]::GetWindowRect($p.MainWindowHandle, [ref]$r)
$w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$bmp.Save($out)
$g.Dispose(); $bmp.Dispose()
Write-Output ("hovered {0},{1} saved {2}x{3}" -f $cx, $cy, $w, $h)
