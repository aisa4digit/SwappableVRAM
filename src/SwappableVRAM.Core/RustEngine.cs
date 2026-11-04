using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

[assembly: DefaultDllImportSearchPaths(DllImportSearchPath.AssemblyDirectory | DllImportSearchPath.System32)]
namespace SwappableVRAM.Core;

internal sealed class RustStoreHandle() : SafeHandleZeroOrMinusOneIsInvalid(true)
{
    internal RustStoreHandle(nint value) : this() => SetHandle(value);
    protected override bool ReleaseHandle() => RustNative.sv_store_destroy(handle) == 0;
}
internal static class RustNative
{
    private const string Dll = "swappable_vram_engine.dll";
    [StructLayout(LayoutKind.Sequential)]
    internal struct DeviceInfo
    {
        public int Ordinal, DriverVersion;
        public ulong TotalBytes;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 256)] public byte[] Name;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 32)] public byte[] PciBusId;
    }
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern uint sv_api_version();
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern int sv_host_status(out uint options, out uint interfaces);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern nuint sv_last_error([Out] byte[] buffer, nuint length);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern int sv_device_count(out int count);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern int sv_device_info(int ordinal, out DeviceInfo info);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern int sv_store_create(int ordinal, ulong bytes, out nint handle);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern int sv_store_info(RustStoreHandle handle, out ulong free, out ulong total);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern int sv_store_destroy(nint handle);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern unsafe int sv_store_read(RustStoreHandle handle, ulong offset, byte* buffer, nuint length);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern unsafe int sv_store_write(RustStoreHandle handle, ulong offset, byte* buffer, nuint length);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern int sv_measure(RustStoreHandle handle, int write, uint gapMs, [Out] double[] samples, nuint count);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern int sv_cancel(RustStoreHandle handle);
    [DllImport(Dll, CallingConvention = CallingConvention.Cdecl)] internal static extern int sv_reset_cancel(RustStoreHandle handle);
    internal static void Check(int result)
    {
        if (result == 0) return;
        byte[] text = new byte[4096];
        sv_last_error(text, (nuint)text.Length);
        throw new InvalidOperationException(Decode(text));
    }
    internal static string Decode(byte[] text) => Encoding.UTF8.GetString(text.AsSpan(0, Array.IndexOf(text, (byte)0) is var end && end >= 0 ? end : text.Length));
}
public sealed record GpuDevice(int Ordinal, string Name, long TotalBytes, string DriverApiVersion, string PciBusId);
public sealed record LocalBackendStatus(uint CodeIntegrityOptions, uint DriverInterfaces)
{
    public bool TestSigning => (CodeIntegrityOptions & 2) != 0;
    public static LocalBackendStatus Capture()
    {
        RustNative.Check(RustNative.sv_host_status(out uint options, out uint count));
        return new(options, count);
    }
}
public static class GpuCatalog
{
    public static IReadOnlyList<GpuDevice> Enumerate()
    {
        if (!OperatingSystem.IsWindows() || !Environment.Is64BitProcess) throw new PlatformNotSupportedException("Windows x64 gereklidir.");
        if (RustNative.sv_api_version() != 1) throw new InvalidOperationException("Rust engine ABI version mismatch.");
        RustNative.Check(RustNative.sv_device_count(out int count));
        var result = new List<GpuDevice>();
        for (int i = 0; i < count; i++)
        {
            RustNative.Check(RustNative.sv_device_info(i, out var info));
            result.Add(new(i, RustNative.Decode(info.Name), checked((long)info.TotalBytes),
                $"{info.DriverVersion / 1000}.{info.DriverVersion % 1000 / 10}", RustNative.Decode(info.PciBusId)));
        }
        return result;
    }
}
