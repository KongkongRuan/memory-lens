using System;
using System.Diagnostics;

namespace MemoryLens.SpdBridge
{
    internal static class PawnIOService
    {
        internal static void Install(string installerPath)
        {
            var start = new ProcessStartInfo
            {
                FileName = installerPath,
                Arguments = "-install -silent",
                UseShellExecute = false,
                CreateNoWindow = true,
            };
            using (var process = Process.Start(start))
            {
                if (!process.WaitForExit(120000))
                {
                    try { process.Kill(); } catch { }
                    throw new TimeoutException("PawnIO 官方安装程序执行超时。 ");
                }
                if (process.ExitCode == 1072)
                    throw new InvalidOperationException("Windows 正在等待重启以清理旧驱动。请重启电脑后，再点击“读取详细时序”。 ");
                if (process.ExitCode != 0)
                    throw new InvalidOperationException("PawnIO 官方安装失败，退出码：" + process.ExitCode);
            }
        }
    }
}
