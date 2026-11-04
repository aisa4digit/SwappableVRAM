using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text.Json;

namespace SwappableVRAM.Core;

public sealed record GpuSnapshot(long TotalBytes, long UsedBytes, long FreeBytes, uint? Utilization, string? Error);
public sealed class GpuTelemetry : IDisposable
{
    private nint device;
    private bool initialized;
    private string? error;
    public GpuTelemetry(string pciBusId)
    {
        try
        {
            Check(nvmlInit_v2());
            initialized = true;
            Check(nvmlDeviceGetHandleByPciBusId_v2(pciBusId, out device));
        }
        catch (Exception ex) { error = ex.Message; }
    }
    public GpuSnapshot Capture()
    {
        if (device == 0) return new(0, 0, 0, null, error ?? "NVML aygıtı yok.");
        try
        {
            Check(nvmlDeviceGetMemoryInfo(device, out var info));
            var code = nvmlDeviceGetUtilizationRates(device, out var utilization);
            return new((long)info.Total, (long)info.Used, (long)info.Free, code == 0 ? utilization.Gpu : null, null);
        }
        catch (Exception ex) { return new(0, 0, 0, null, ex.Message); }
    }
    public void Dispose()
    {
        device = 0;
        if (initialized) { nvmlShutdown(); initialized = false; }
    }
    private static void Check(int code) { if (code != 0) throw new InvalidOperationException($"NVML hata {code}"); }
    [StructLayout(LayoutKind.Sequential)] private struct NvMemory { public ulong Total, Free, Used; }
    [StructLayout(LayoutKind.Sequential)] private struct NvUtilization { public uint Gpu, Memory; }
    [DllImport("nvml.dll")] private static extern int nvmlInit_v2();
    [DllImport("nvml.dll")] private static extern int nvmlShutdown();
    [DllImport("nvml.dll", CharSet = CharSet.Ansi)] private static extern int nvmlDeviceGetHandleByPciBusId_v2(string pci, out nint device);
    [DllImport("nvml.dll")] private static extern int nvmlDeviceGetMemoryInfo(nint device, out NvMemory memory);
    [DllImport("nvml.dll")] private static extern int nvmlDeviceGetUtilizationRates(nint device, out NvUtilization utilization);
}

public sealed record ProcessMemory(int Id, string Name, long WorkingSetBytes, long PrivateBytes);
public static class ProcessTelemetry
{
    private static readonly HashSet<string> Known = new(StringComparer.OrdinalIgnoreCase)
        { "cs2", "steam", "steamwebhelper", "ChatGPT", "claude", "lightshot", "obs64", "obs32", "Discord" };
    public static IReadOnlyList<ProcessMemory> Capture()
    {
        var result = new List<ProcessMemory>();
        foreach (var process in Process.GetProcesses())
        {
            using (process)
            {
                try
                {
                    if (Known.Contains(process.ProcessName))
                        result.Add(new(process.Id, process.ProcessName, process.WorkingSet64, process.PrivateMemorySize64));
                }
                catch (Exception ex) when (ex is System.ComponentModel.Win32Exception or InvalidOperationException or NotSupportedException)
                { /* A process may exit or deny metadata access between enumeration and query. */ }
            }
        }
        return result.OrderByDescending(p => p.WorkingSetBytes).ToArray();
    }
}

public sealed record SessionSample(SystemSnapshot System, GpuSnapshot? Gpu, IReadOnlyList<ProcessMemory> Processes);
public sealed class SessionRecorder
{
    private readonly List<SessionSample> samples = [];
    public int Count => samples.Count;
    public bool Recording { get; private set; }
    public void Start() { samples.Clear(); Recording = true; }
    public void Stop() => Recording = false;
    public void Add(SessionSample sample)
    {
        if (!Recording) return;
        // At one sample per 2 seconds, six hours of bounded telemetry.
        if (samples.Count >= 10800) { Recording = false; return; }
        samples.Add(sample);
    }
    public void Save(string path)
    {
        File.WriteAllText(path, JsonSerializer.Serialize(new
        {
            Mode = "baseline-no-system-offload", OffloadActive = false,
            Note = "Working set toplamları paylaşılan sayfaları tekrar sayabilir. Private bytes fiziksel RAM değildir. Frametime kaydı içermez.",
            Samples = samples
        }, new JsonSerializerOptions { WriteIndented = true }));
    }
}
