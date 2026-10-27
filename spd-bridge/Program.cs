using System;
using System.Collections;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Runtime.Serialization;
using System.Runtime.Serialization.Json;
using System.Security.Cryptography;
using System.Text;
using RAMSPDToolkit.I2CSMBus;
using RAMSPDToolkit.SPD;
using RAMSPDToolkit.SPD.Interop.Shared;
using RAMSPDToolkit.Windows.Driver;

namespace MemoryLens.SpdBridge
{
    internal static class Program
    {
        private const string PawnIOSetupSha256 = "1F519A22E47187F70A1379A48CA604981C4FCF694F4E65B734AAA74A9FBA3032";
        private const string PawnIOLibSha256 = "D71F62627D66983BB9F5B1C269F27BCD8C1B8A46E794377A6330F84C198F4443";
        private static readonly object CleanupLock = new object();
        private static bool _cleaned;

        private static int Main(string[] args)
        {
            Console.SetOut(TextWriter.Null);
            var response = new BridgeResponse();
            var logger = new BlackSharp.Core.Logging.Logger
            {
                IsEnabled = true,
                LogLevel = BlackSharp.Core.Logging.LogLevel.Trace,
            };
            RAMSPDToolkit.Logging.LogSimple.Logger = logger;
            AppDomain.CurrentDomain.ProcessExit += (_, __) => CleanupDriver(response.Warnings);
            int exitCode;
            try
            {
                response.Driver = OpenDriver(response.Warnings);
                ReadAllModules(response);
                response.Success = true;
                exitCode = response.Modules.Count == 0 ? 2 : 0;
            }
            catch (Exception ex)
            {
                response.Error = ex.Message;
                Console.Error.WriteLine(ex);
                exitCode = 1;
            }
            finally
            {
                CleanupDriver(response.Warnings);
                response.Diagnostics = "工作目录：" + Environment.CurrentDirectory + Environment.NewLine + logger.ToString();
                WriteResponse(response, args);
            }
            return exitCode;
        }

        private static string OpenDriver(List<string> warnings)
        {
            string resources = Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "Resources");
            string libraryPath = Path.Combine(resources, "PawnIOLib.dll");
            string installerPath = Path.Combine(resources, "PawnIO_setup.exe");
            VerifyPinnedFile(libraryPath, PawnIOLibSha256);
            VerifyPinnedFile(installerPath, PawnIOSetupSha256);
            var local = new LocalPawnIO(libraryPath);
            DriverManager.Driver = local;
            if (!DriverManager.LoadDriver())
                throw new InvalidOperationException("PawnIO 用户态接口初始化失败。 ");
            int probe = local.ProbeDevice();
            if (probe == 0)
                return "PawnIO 2.2.0";
            warnings.Add(string.Format(CultureInfo.InvariantCulture,
                "PawnIO 设备尚未就绪（0x{0:X8}），正在执行官方签名安装程序。 ", unchecked((uint)probe)));
            DriverManager.UnloadDriver();
            PawnIOService.Install(installerPath);
            local = new LocalPawnIO(libraryPath);
            DriverManager.Driver = local;
            if (!DriverManager.LoadDriver())
                throw new InvalidOperationException("安装后 PawnIO 用户态接口初始化失败。 ");
            probe = local.ProbeDevice();
            if (probe != 0)
                throw new InvalidOperationException(string.Format(CultureInfo.InvariantCulture,
                    "PawnIO 设备仍无法打开（0x{0:X8}）。 ", unchecked((uint)probe)));
            return "PawnIO 2.2.0";
        }

        private static bool HashMatches(string path, string expectedHash)
        {
            using (var sha = SHA256.Create())
                return string.Equals(ToHex(sha.ComputeHash(File.ReadAllBytes(path))), expectedHash,
                    StringComparison.OrdinalIgnoreCase);
        }

        private static void VerifyPinnedFile(string path, string expectedHash)
        {
            if (!File.Exists(path))
                throw new FileNotFoundException("缺少 SPD 硬件访问组件。", path);
            if (!HashMatches(path, expectedHash))
                throw new InvalidOperationException("硬件访问组件哈希与锁定版本不一致：" + path);
            VerifySignedFile(path);
        }

        private static void VerifySignedFile(string path)
        {
            int result = NativeMethods.VerifyAuthenticode(path);
            if (result != 0)
                throw new InvalidOperationException(string.Format(CultureInfo.InvariantCulture,
                    "驱动 Authenticode 签名验证失败：{0}（0x{1:X8}）", path, unchecked((uint)result)));
        }

        private static void ReadAllModules(BridgeResponse response)
        {
            SMBusManager.DetectSMBuses();
            var buses = SMBusManager.RegisteredSMBuses;
            if (buses.Count == 0)
                throw new InvalidOperationException("未检测到兼容的 SMBus，无法读取 SPD。 ");
            for (int busIndex = 0; busIndex < buses.Count; busIndex++)
            {
                var bus = buses[busIndex];
                for (byte address = SPDConstants.SPD_BEGIN; address <= SPDConstants.SPD_END; address++)
                {
                    SPDDetector detector = null;
                    try { detector = new SPDDetector(bus, address); }
                    catch (Exception ex)
                    {
                        response.Warnings.Add(string.Format(CultureInfo.InvariantCulture,
                            "总线 {0} 地址 0x{1:X2} 检测失败：{2}", busIndex, address, ex.Message));
                    }
                    if (detector == null || !detector.IsValid || detector.Accessor == null)
                        continue;
                    var module = new SpdModule
                    {
                        Bus = busIndex,
                        BusName = bus.DeviceName ?? "",
                        Address = address,
                        MemoryType = detector.SPDMemoryType.ToString(),
                    };
                    try
                    {
                        var accessor = detector.Accessor;
                        module.CapacityGb = accessor.GetCapacity();
                        module.Manufacturer = SafeString(accessor.GetModuleManufacturerString);
                        module.DramManufacturer = SafeString(accessor.GetDRAMManufacturerString);
                        module.PartNumber = SafeString(accessor.ModulePartNumber);
                        module.SerialNumber = SafeString(accessor.ModuleSerialNumber);
                        module.RawHex = ToHex(ReadRaw(accessor));
                        module.Timings.AddRange(ReadTimings(accessor));
                    }
                    catch (Exception ex)
                    {
                        module.Error = ex.Message;
                        response.Warnings.Add(string.Format(CultureInfo.InvariantCulture,
                            "地址 0x{0:X2} 的 SPD 读取不完整：{1}", address, ex.Message));
                    }
                    response.Modules.Add(module);
                }
            }
            if (response.Modules.Count == 0)
                response.Warnings.Add("SMBus 上未检测到可用内存 SPD。 ");
        }

        private static byte[] ReadRaw(SPDAccessor accessor)
        {
            int length = accessor is DDR5Accessor ? 1024 : accessor is DDR4Accessor ? 512 : 256;
            using (var stream = new MemoryStream())
            {
                // SMBus block reads are limited to 32 bytes. Asking for 64 bytes
                // can return a successful-looking buffer with the tail zeroed,
                // which corrupts DDR4/DDR5 raw SPD and CRC/timing decoding.
                for (ushort offset = 0; offset < length; offset += 32)
                {
                    byte count = (byte)Math.Min(32, length - offset);
                    byte[] block = accessor.At(offset, count);
                    if (block == null || block.Length != count)
                        throw new InvalidDataException("SPD 返回的数据长度不正确。 ");
                    stream.Write(block, 0, block.Length);
                }
                return stream.ToArray();
            }
        }

        private static IEnumerable<TimingValue> ReadTimings(SPDAccessor accessor)
        {
            object timings;
            if (accessor is DDR5Accessor ddr5) timings = ddr5.SDRAMTimings;
            else if (accessor is DDR4Accessor ddr4) timings = ddr4.SDRAMTimings;
            else if (accessor is DDR3Accessor ddr3) timings = ddr3.SDRAMTimings;
            else yield break;
            foreach (var property in timings.GetType().GetProperties(BindingFlags.Public | BindingFlags.Instance))
            {
                if (property.GetIndexParameters().Length != 0)
                    continue;
                object value = property.GetValue(timings, null);
                string formatted;
                if (value is decimal || value is double || value is float || value is int || value is uint)
                    formatted = Convert.ToString(value, CultureInfo.InvariantCulture);
                else if (value is IEnumerable sequence && !(value is string))
                    formatted = string.Join(", ", sequence.Cast<object>().Select(v => Convert.ToString(v, CultureInfo.InvariantCulture)));
                else
                    continue;
                yield return new TimingValue { Name = property.Name, Value = formatted };
            }
        }

        private static string SafeString(Func<string> read)
        {
            try { return read()?.Trim() ?? ""; }
            catch { return ""; }
        }

        private static string ToHex(byte[] bytes)
        {
            var text = new StringBuilder(bytes.Length * 2);
            foreach (byte value in bytes)
                text.Append(value.ToString("X2", CultureInfo.InvariantCulture));
            return text.ToString();
        }

        private static void CleanupDriver(List<string> warnings)
        {
            lock (CleanupLock)
            {
                if (_cleaned)
                    return;
                _cleaned = true;
                try { DriverManager.UnloadDriver(); }
                catch (Exception ex) { warnings.Add("驱动接口清理失败：" + ex.Message); }
            }
        }

        private static void WriteResponse(BridgeResponse response, string[] args)
        {
            if (args.Length == 2 && args[0] == "--output")
            {
                using (var output = new FileStream(args[1], FileMode.Create, FileAccess.Write, FileShare.Read))
                    new DataContractJsonSerializer(typeof(BridgeResponse)).WriteObject(output, response);
                return;
            }
            using (var output = Console.OpenStandardOutput())
                new DataContractJsonSerializer(typeof(BridgeResponse)).WriteObject(output, response);
        }
    }

    [DataContract]
    internal sealed class BridgeResponse
    {
        [DataMember(Name = "version")] public int Version = 1;
        [DataMember(Name = "success")] public bool Success;
        [DataMember(Name = "driver")] public string Driver = "";
        [DataMember(Name = "error")] public string Error = "";
        [DataMember(Name = "diagnostics")] public string Diagnostics = "";
        [DataMember(Name = "warnings")] public List<string> Warnings = new List<string>();
        [DataMember(Name = "modules")] public List<SpdModule> Modules = new List<SpdModule>();
    }

    [DataContract]
    internal sealed class SpdModule
    {
        [DataMember(Name = "bus")] public int Bus;
        [DataMember(Name = "busName")] public string BusName = "";
        [DataMember(Name = "address")] public int Address;
        [DataMember(Name = "memoryType")] public string MemoryType = "";
        [DataMember(Name = "capacityGb")] public float CapacityGb;
        [DataMember(Name = "manufacturer")] public string Manufacturer = "";
        [DataMember(Name = "dramManufacturer")] public string DramManufacturer = "";
        [DataMember(Name = "partNumber")] public string PartNumber = "";
        [DataMember(Name = "serialNumber")] public string SerialNumber = "";
        [DataMember(Name = "rawHex")] public string RawHex = "";
        [DataMember(Name = "error")] public string Error = "";
        [DataMember(Name = "timings")] public List<TimingValue> Timings = new List<TimingValue>();
    }

    [DataContract]
    internal sealed class TimingValue
    {
        [DataMember(Name = "name")] public string Name = "";
        [DataMember(Name = "value")] public string Value = "";
    }

    internal static class NativeMethods
    {
        private static readonly Guid GenericVerifyAction = new Guid("00AAC56B-CD44-11D0-8CC2-00C04FC295EE");

        [DllImport("wintrust.dll", ExactSpelling = true, CharSet = CharSet.Unicode)]
        private static extern int WinVerifyTrust(IntPtr window, ref Guid action, IntPtr data);

        internal static int VerifyAuthenticode(string path)
        {
            var file = new WintrustFileInfo(path);
            IntPtr filePointer = Marshal.AllocHGlobal(Marshal.SizeOf(file));
            Marshal.StructureToPtr(file, filePointer, false);
            var data = new WintrustData { FileInfo = filePointer };
            IntPtr dataPointer = Marshal.AllocHGlobal(Marshal.SizeOf(data));
            try
            {
                Marshal.StructureToPtr(data, dataPointer, false);
                Guid action = GenericVerifyAction;
                return WinVerifyTrust(IntPtr.Zero, ref action, dataPointer);
            }
            finally
            {
                Marshal.DestroyStructure(dataPointer, typeof(WintrustData));
                Marshal.FreeHGlobal(dataPointer);
                Marshal.DestroyStructure(filePointer, typeof(WintrustFileInfo));
                Marshal.FreeHGlobal(filePointer);
            }
        }

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        private sealed class WintrustFileInfo
        {
            public uint StructSize = (uint)Marshal.SizeOf(typeof(WintrustFileInfo));
            [MarshalAs(UnmanagedType.LPWStr)] public string FilePath;
            public IntPtr File = IntPtr.Zero;
            public IntPtr KnownSubject = IntPtr.Zero;

            internal WintrustFileInfo(string path) { FilePath = path; }
        }

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        private sealed class WintrustData
        {
            public uint StructSize = (uint)Marshal.SizeOf(typeof(WintrustData));
            public IntPtr PolicyCallbackData = IntPtr.Zero;
            public IntPtr SipClientData = IntPtr.Zero;
            public uint UiChoice = 2;
            public uint RevocationChecks;
            public uint UnionChoice = 1;
            public IntPtr FileInfo;
            public uint StateAction;
            public IntPtr StateData = IntPtr.Zero;
            public IntPtr UrlReference = IntPtr.Zero;
            public uint ProviderFlags;
            public uint UiContext;
            public IntPtr SignatureSettings = IntPtr.Zero;
        }
    }
}
