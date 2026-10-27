Add-Type -AssemblyName System.Drawing
$src = @'
using System;
using System.Runtime.InteropServices;
public class U32m {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(int f, int dx, int dy, int d, int e);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
'@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Drawing
$out = "C:\\Windows\\Temp\\ml_dialog.png"
$p = Get-Process memory-lens
[void][U32m]::SetForegroundWindow($p.MainWindowHandle)
Start-Sleep -Milliseconds 500
$r = New-Object U32m+RECT
[void][U32m]::GetWindowRect($p.MainWindowHandle, [ref]$r)
# 切到进程明细
[void][U32m]::SetCursorPos($r.Left + 200, $r.Top + 118)
[U32m]::mouse_event(2,0,0,0,0); [U32m]::mouse_event(4,0,0,0,0)
Start-Sleep -Milliseconds 900
# 右键第一行进程名
$px = $r.Left + 70; $py = $r.Top + 285
[void][U32m]::SetCursorPos($px, $py)
Start-Sleep -Milliseconds 400
[U32m]::mouse_event(0x0008,0,0,0,0)
[U32m]::mouse_event(0x0010,0,0,0,0)
Start-Sleep -Milliseconds 800
# 点击"结束进程"菜单项（菜单第3项，文字中心约 py+72，水平居中约 px+55）
[void][U32m]::SetCursorPos($px + 55, $py + 72)
Start-Sleep -Milliseconds 300
[U32m]::mouse_event(2,0,0,0,0); [U32m]::mouse_event(4,0,0,0,0)
Start-Sleep -Milliseconds 1000
# 重新取窗口矩形并截窗口区域
[void][U32m]::GetWindowRect($p.MainWindowHandle, [ref]$r)
$w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
if ($w -le 0 -or $h -le 0) { Write-Output "bad rect $w x $h"; exit 1 }
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size($w, $h)))
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output ("saved {0}x{1} to {2}" -f $w, $h, $out)
