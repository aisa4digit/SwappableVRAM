using System.Diagnostics;
using System.Runtime.InteropServices;

namespace SwappableVRAM.Core;

public sealed record SystemSnapshot(DateTimeOffset Time, long PhysicalTotal, long PhysicalAvailable,
    long CommitTotal, long CommitLimit, long ProcessWorkingSet, long ProcessPrivateBytes)
{
    public static SystemSnapshot Capture()
    {
        var memory = new MemoryStatus { Length = (uint)Marshal.SizeOf<MemoryStatus>() };
        if (!GlobalMemoryStatusEx(ref memory)) throw new System.ComponentModel.Win32Exception();
        var perf = new PerformanceInfo { Size = (uint)Marshal.SizeOf<PerformanceInfo>() };
        if (!GetPerformanceInfo(ref perf, perf.Size)) throw new System.ComponentModel.Win32Exception();
        using var process = Process.GetCurrentProcess();
        return new(DateTimeOffset.Now, (long)memory.PhysicalTotal, (long)memory.PhysicalAvailable,
            checked((long)(perf.CommitTotal * perf.PageSize)), checked((long)(perf.CommitLimit * perf.PageSize)),
            process.WorkingSet64, process.PrivateMemorySize64);
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MemoryStatus
    {
        public uint Length, Load;
        public ulong PhysicalTotal, PhysicalAvailable, PageFileTotal, PageFileAvailable, VirtualTotal, VirtualAvailable, Extended;
    }
    [StructLayout(LayoutKind.Sequential)]
    private struct PerformanceInfo
    {
        public uint Size;
        public nuint CommitTotal, CommitLimit, CommitPeak, PhysicalTotal, PhysicalAvailable, SystemCache,
            KernelTotal, KernelPaged, KernelNonpaged, PageSize;
        public uint HandleCount, ProcessCount, ThreadCount;
    }
    [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GlobalMemoryStatusEx(ref MemoryStatus status);
    [DllImport("psapi.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetPerformanceInfo(ref PerformanceInfo info, uint size);
}
