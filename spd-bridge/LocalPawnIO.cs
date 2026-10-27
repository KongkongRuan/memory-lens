using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Compression;
using System.Runtime.InteropServices;
using RAMSPDToolkit.I2CSMBus.Interop.PawnIO;
using RAMSPDToolkit.Logging;
using RAMSPDToolkit.Windows.Driver;
using RAMSPDToolkit.Windows.Driver.Interfaces;

namespace MemoryLens.SpdBridge
{
    internal sealed class LocalPawnIO : IPawnIODriver
    {
        private readonly string _libraryPath;
        private readonly List<LocalPawnIOModule> _modules = new List<LocalPawnIOModule>();
        private IntPtr _library;
        private PawnIOVersion _version;
        private PawnIOOpen _open;
        private PawnIOLoad _load;
        private PawnIOExecute _execute;
        private PawnIOClose _close;

        internal LocalPawnIO(string libraryPath) { _libraryPath = libraryPath; }
        public bool IsOpen => _library != IntPtr.Zero;

        public bool Load()
        {
            if (IsOpen)
                return true;
            _library = PawnIOLibNative.LoadLibrary(_libraryPath);
            if (_library == IntPtr.Zero)
            {
                LogSimple.LogError("PawnIOLib LoadLibrary 失败，Win32=" + Marshal.GetLastWin32Error());
                return false;
            }
            _version = GetDelegate<PawnIOVersion>("pawnio_version");
            _open = GetDelegate<PawnIOOpen>("pawnio_open");
            _load = GetDelegate<PawnIOLoad>("pawnio_load");
            _execute = GetDelegate<PawnIOExecute>("pawnio_execute");
            _close = GetDelegate<PawnIOClose>("pawnio_close");
            if (_version != null && _open != null && _load != null && _execute != null && _close != null)
                return true;
            LogSimple.LogError("PawnIOLib 导出接口不完整。 ");
            PawnIOLibNative.FreeLibrary(_library);
            _library = IntPtr.Zero;
            return false;
        }

        internal int ProbeDevice()
        {
            if (!IsOpen)
                return unchecked((int)0x80004005);
            var module = new LocalPawnIOModule(this);
            int result = module.Open();
            if (result == 0)
                module.Unload();
            return result;
        }

        public void Unload()
        {
            foreach (var module in _modules)
                module.Unload();
            _modules.Clear();
            if (_library != IntPtr.Zero)
                PawnIOLibNative.FreeLibrary(_library);
            _library = IntPtr.Zero;
        }

        public IPawnIOModule LoadModule(PawnIOSMBusIdentifier identifier)
        {
            if (!IsOpen)
                return null;
            var module = new LocalPawnIOModule(this);
            int opened = module.Open();
            if (opened != 0)
            {
                LogSimple.LogError(HResult("pawnio_open", opened));
                return null;
            }
            byte[] blob = ReadModule(identifier);
            int loaded = module.Load(blob);
            if (loaded != 0)
            {
                LogSimple.LogError(HResult("pawnio_load", loaded));
                module.Unload();
                return null;
            }
            _modules.Add(module);
            return module;
        }

        private byte[] ReadModule(PawnIOSMBusIdentifier identifier)
        {
            string name;
            switch (identifier)
            {
                case PawnIOSMBusIdentifier.I801: name = "SmbusI801.gz"; break;
                case PawnIOSMBusIdentifier.Piix4: name = "SmbusPIIX4.gz"; break;
                case PawnIOSMBusIdentifier.NCT6793: name = "SmbusNCT6793.gz"; break;
                case PawnIOSMBusIdentifier.IntelSkylakeIMC: name = "SmbusIntelSkylakeIMC.gz"; break;
                default: throw new ArgumentOutOfRangeException(nameof(identifier));
            }
            using (var resource = typeof(DriverManager).Assembly.GetManifestResourceStream("RAMSPDToolkit.Resources." + name))
            {
                if (resource == null)
                    throw new InvalidOperationException("缺少 PawnIO 模块资源：" + name);
                using (var gzip = new GZipStream(resource, CompressionMode.Decompress))
                using (var memory = new MemoryStream())
                {
                    gzip.CopyTo(memory);
                    return memory.ToArray();
                }
            }
        }

        private T GetDelegate<T>(string name) where T : class
        {
            IntPtr address = PawnIOLibNative.GetProcAddress(_library, name);
            return address == IntPtr.Zero ? null : (T)(object)Marshal.GetDelegateForFunctionPointer(address, typeof(T));
        }

        private static string HResult(string operation, int result)
        {
            return string.Format("{0} 失败：0x{1:X8}", operation, unchecked((uint)result));
        }

        private int Execute(LocalPawnIOModule module, string name, long[] input, uint inputSize,
            long[] output, uint outputSize, out uint returnSize)
        {
            UIntPtr nativeReturnSize;
            int result = _execute(module.Handle, name, input, (UIntPtr)inputSize,
                output, (UIntPtr)outputSize, out nativeReturnSize);
            returnSize = (uint)nativeReturnSize.ToUInt64();
            return result;
        }

        private int Open(LocalPawnIOModule module) { return _open(out module.Handle); }
        private int Load(LocalPawnIOModule module, byte[] blob) { return _load(module.Handle, blob, (UIntPtr)blob.Length); }
        private int Close(LocalPawnIOModule module) { return _close(module.Handle); }

        internal sealed class LocalPawnIOModule : IPawnIOModule
        {
            private readonly LocalPawnIO _owner;
            internal IntPtr Handle;
            internal LocalPawnIOModule(LocalPawnIO owner) { _owner = owner; }
            internal int Open() { return _owner.Open(this); }
            internal int Load(byte[] blob) { return _owner.Load(this, blob); }

            public int Execute(string name, long[] inBuffer, uint inSize, long[] outBuffer,
                uint outSize, out uint returnSize)
            {
                if (Handle == IntPtr.Zero)
                {
                    returnSize = 0;
                    return unchecked((int)0x80070057);
                }
                return _owner.Execute(this, name, inBuffer, inSize, outBuffer, outSize, out returnSize);
            }

            internal void Unload()
            {
                if (Handle != IntPtr.Zero)
                    _owner.Close(this);
                Handle = IntPtr.Zero;
            }
        }

        [UnmanagedFunctionPointer(CallingConvention.StdCall)]
        private delegate int PawnIOVersion(out uint version);
        [UnmanagedFunctionPointer(CallingConvention.StdCall)]
        private delegate int PawnIOOpen(out IntPtr handle);
        [UnmanagedFunctionPointer(CallingConvention.StdCall)]
        private delegate int PawnIOLoad(IntPtr handle, byte[] blob, UIntPtr size);
        [UnmanagedFunctionPointer(CallingConvention.StdCall)]
        private delegate int PawnIOExecute(IntPtr handle, [MarshalAs(UnmanagedType.LPStr)] string name,
            long[] input, UIntPtr inputSize, long[] output, UIntPtr outputSize, out UIntPtr returnSize);
        [UnmanagedFunctionPointer(CallingConvention.StdCall)]
        private delegate int PawnIOClose(IntPtr handle);
    }

    internal static class PawnIOLibNative
    {
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        internal static extern IntPtr LoadLibrary(string path);
        [DllImport("kernel32.dll", CharSet = CharSet.Ansi, ExactSpelling = true, SetLastError = true)]
        internal static extern IntPtr GetProcAddress(IntPtr module, string name);
        [DllImport("kernel32.dll", ExactSpelling = true)]
        internal static extern bool FreeLibrary(IntPtr module);
    }
}
