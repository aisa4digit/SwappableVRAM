namespace SwappableVRAM.Core;

/// <summary>SafeHandle bridge to the Rust engine. Not a Windows pagefile device.</summary>
public sealed unsafe class VramBlockStore : IDisposable
{
    public const int TransferBytes = 4 * 1024 * 1024;
    public const long MiB = 1024L * 1024;
    private readonly object gate = new();
    private readonly RustStoreHandle handle;
    public long Capacity { get; }
    public long FreeBeforeAllocation { get; }
    public long TotalDeviceMemory { get; }
    public VramBlockStore(int deviceOrdinal, long capacity)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(capacity);
        RustNative.Check(RustNative.sv_store_create(deviceOrdinal, (ulong)capacity, out var pointer));
        handle = new(pointer);
        try
        {
            RustNative.Check(RustNative.sv_store_info(handle, out var free, out var total));
            Capacity = capacity;
            FreeBeforeAllocation = checked((long)free);
            TotalDeviceMemory = checked((long)total);
        }
        catch { handle.Dispose(); throw; }
    }
    public void Read(long offset, Span<byte> destination)
    {
        ValidateRange(offset, destination.Length, Capacity);
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(handle.IsClosed, this);
            fixed (byte* data = destination) RustNative.Check(RustNative.sv_store_read(handle, (ulong)offset, data, (nuint)destination.Length));
        }
    }
    public void Write(long offset, ReadOnlySpan<byte> source)
    {
        ValidateRange(offset, source.Length, Capacity);
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(handle.IsClosed, this);
            fixed (byte* data = source) RustNative.Check(RustNative.sv_store_write(handle, (ulong)offset, data, (nuint)source.Length));
        }
    }
    public double[] Measure(bool write, int samples, int gapMs, CancellationToken token = default)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(handle.IsClosed, this);
            if (samples is < 100 or > 100_000 || gapMs is < 0 or > 100) throw new ArgumentOutOfRangeException(nameof(samples));
            var values = new double[samples];
            RustNative.Check(RustNative.sv_reset_cancel(handle));
            using var registration = token.Register(() => RustNative.sv_cancel(handle));
            int result = RustNative.sv_measure(handle, write ? 1 : 0, (uint)gapMs, values, (nuint)samples);
            token.ThrowIfCancellationRequested();
            RustNative.Check(result);
            return values;
        }
    }
    public static void ValidateRange(long offset, long length, long capacity)
    {
        if (offset < 0 || length < 0 || capacity < 0 || offset > capacity || length > capacity - offset)
            throw new ArgumentOutOfRangeException(nameof(offset), "İstek VRAM sınırları dışında.");
    }
    public void Dispose() { lock (gate) handle.Dispose(); }
}
