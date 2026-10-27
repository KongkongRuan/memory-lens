Add-Type -AssemblyName System.Drawing
$src = @'
using System;
using System.Runtime.InteropServices;
public class U32f {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetCursor();
    [DllImport("user32.dll")] public static extern bool GetCursorInfo(out CURSORINFO info);
    [DllImport("user32.dll")] public static extern IntPtr LoadCursor(IntPtr h, int id);
    [DllImport("user32.dll")] public static extern IntPtr GetIconInfo(IntPtr hIcon, out ICONINFO ii);
    [StructLayout(LayoutKind.Sequential)] public struct CURSORINFO { public int cbSize; public int flags; public IntPtr hCursor; public POINT pt; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int x; public int y; }
    [StructLayout(LayoutKind.Sequential)] public struct ICONINFO { public bool fIcon; public int xHotspot; public int yHotspot; public IntPtr hbmMask; public IntPtr hbmColor; }
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    public const int IDC_ARROW = 32512;
    public const int IDC_IBEAM = 32513;
    public const int IDC_HAND = 32649;
}
'@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Drawing
$p = Get-Process memory-lens
[void][U32f]::SetForegroundWindow($p.MainWindowHandle)
Start-Sleep -Milliseconds 500
$r = New-Object U32f+RECT
[void][U32f]::GetWindowRect($p.MainWindowHandle, [ref]$r)
# 移到进程明细标签并点击
[void][U32f]::SetCursorPos($r.Left + 200, $r.Top + 118)
Start-Sleep -Milliseconds 300
$src2 = @'
using System;
using System.Runtime.InteropServices;
public class M { [DllImport("user32.dll")] public static extern void mouse_event(int f,int a,int b,int c,int d); }
'@
Add-Type -TypeDefinition $src2
[M]::mouse_event(2,0,0,0,0); [M]::mouse_event(4,0,0,0,0)
Start-Sleep -Milliseconds 900
# 移到第一行进程名上（窗口内约 x=60, y=270）
[void][U32f]::SetCursorPos($r.Left + 60, $r.Top + 280)
Start-Sleep -Milliseconds 600
# 读取光标
$ibeam = [U32f]::LoadCursor([IntPtr]::Zero, [U32f]::IDC_IBEAM)
$arrow = [U32f]::LoadCursor([IntPtr]::Zero, [U32f]::IDC_ARROW)
$hand  = [U32f]::LoadCursor([IntPtr]::Zero, [U32f]::IDC_HAND)
$cur = [U32f]::GetCursor()
$kind = if ($cur -eq $ibeam) { "IBEAM(文本插入点)" } elseif ($cur -eq $arrow) { "ARROW(箭头)" } elseif ($cur -eq $hand) { "HAND(手型)" } else { "OTHER($cur)" }
Write-Output ("cursor on process name = {0}" -f $kind)
