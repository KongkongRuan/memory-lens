Add-Type -AssemblyName System.Drawing
$src = @'
using System;
using System.Runtime.InteropServices;
public class U32g {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int x; public int y; }
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
'@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Drawing
$out = $env:CAP_OUT
$p = Get-Process memory-lens
[void][U32g]::SetForegroundWindow($p.MainWindowHandle)
Start-Sleep -Milliseconds 500
$r = New-Object U32g+RECT
[void][U32g]::GetWindowRect($p.MainWindowHandle, [ref]$r)
# 点进程明细标签
[void][U32g]::SetCursorPos($r.Left + 200, $r.Top + 118)
$s2 = 'using System;using System.Runtime.InteropServices;public class M2{[DllImport("user32.dll")]public static extern void mouse_event(int f,int a,int b,int c,int d);}'
Add-Type -TypeDefinition $s2
[M2]::mouse_event(2,0,0,0,0); [M2]::mouse_event(4,0,0,0,0)
Start-Sleep -Milliseconds 900
# 移到第一行进程名
[void][U32g]::SetCursorPos($r.Left + 70, $r.Top + 285)
Start-Sleep -Milliseconds 700
$pt = New-Object U32g+POINT
[void][U32g]::GetCursorPos([ref]$pt)
Write-Output ("cursor at {0},{1}" -f $pt.x, $pt.y)
# 截光标周围 64x64 并放大 6 倍
$sw=64; $sh=64
$bmp = New-Object System.Drawing.Bitmap $sw, $sh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($pt.x - 20, $pt.y - 20, 0, 0, (New-Object System.Drawing.Size($sw,$sh)))
$big = New-Object System.Drawing.Bitmap ($sw*6), ($sh*6)
$g2 = [System.Drawing.Graphics]::FromImage($big)
$g2.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
$g2.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
$g2.DrawImage($bmp, 0, 0, ($sw*6), ($sh*6))
$big.Save($out)
Write-Output "saved zoom"
