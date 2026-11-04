using System.Diagnostics;
using System.Security.Cryptography;
using System.Text.Json;

namespace SwappableVRAM.Core;

public sealed record BenchmarkOptions(int DeviceOrdinal = 0, int PoolMiB = 64, int Samples = 2000,
    int IdleGapMs = 0, string? DiskDirectory = null);
public sealed record LatencyDistribution(string Path, string Operation, int BlockBytes, int QueueDepth,
    double MinUs, double P50Us, double P95Us, double P99Us, double MaxUs, double[] RawMicroseconds);
public sealed record BenchmarkReport(DateTimeOffset Time, GpuDevice Device, string OperatingSystem,
    BenchmarkOptions Options, bool IntegrityPassed, string IntegritySha256, SystemSnapshot Before,
    SystemSnapshot Allocated, SystemSnapshot Released, IReadOnlyList<LatencyDistribution> Results,
    string[] Limits)
{
    public void Save(string path) => File.WriteAllText(path, JsonSerializer.Serialize(this, new JsonSerializerOptions { WriteIndented = true }));
}
public sealed record BenchmarkProgress(string Stage, int Percent, LatencyDistribution? Result = null);

public static class LatencyBenchmark
{
    public static BenchmarkReport Run(BenchmarkOptions options, IProgress<BenchmarkProgress>? progress = null,
        CancellationToken token = default)
    {
        if (options.PoolMiB is < 16 or > 1024 || options.Samples is < 100 or > 100_000 || options.IdleGapMs is < 0 or > 100)
            throw new ArgumentOutOfRangeException(nameof(options));
        var device = GpuCatalog.Enumerate().Single(d => d.Ordinal == options.DeviceOrdinal);
        var results = new List<LatencyDistribution>();
        var before = SystemSnapshot.Capture();
        SystemSnapshot allocated;
        string hash;
        progress?.Report(new("Gerçek VRAM ayrılıyor", 5));
        using (var store = new VramBlockStore(options.DeviceOrdinal, options.PoolMiB * VramBlockStore.MiB))
        {
            allocated = SystemSnapshot.Capture();
            progress?.Report(new("VRAM bütünlük ve sınır testi", 10));
            hash = VerifyStore(store, token);
            var block = new byte[4096];
            RandomNumberGenerator.Fill(block);
            var offsets = Offsets(options.Samples, store.Capacity);
            for (int i = 0; i < 64; i++) { store.Write(offsets[i % offsets.Length], block); store.Read(offsets[i % offsets.Length], block); }
            var write = Summarize("Rust CUDA VRAM", "write / volatile", store.Measure(true, options.Samples, options.IdleGapMs, token));
            results.Add(write);
            progress?.Report(new("VRAM yazma tamamlandı", 35, write));
            var read = Summarize("Rust CUDA VRAM", "read", store.Measure(false, options.Samples, options.IdleGapMs, token));
            results.Add(read);
            progress?.Report(new("VRAM okuma tamamlandı", 55, read));
            VerifyStore(store, token);
        }
        var released = SystemSnapshot.Capture();
        if (options.DiskDirectory is { Length: > 0 } directory)
        {
            progress?.Report(new("Karşılaştırma dosyası hazırlanıyor (64 MiB)", 60));
            const long diskBytes = 64 * VramBlockStore.MiB;
            using var disk = new DirectIoFile(directory, diskBytes, token);
            var offsets = Offsets(options.Samples, diskBytes);
            for (int i = 0; i < 64; i++) disk.Read(offsets[i % offsets.Length]);
            var read = Measure("Disk / unbuffered", "read", options, offsets, disk.Read, token);
            results.Add(read);
            progress?.Report(new("Disk okuma tamamlandı", 80, read));
            var write = Measure("Disk / write-through", "write / durable requested", options, offsets, disk.Write, token);
            results.Add(write);
            progress?.Report(new("Disk yazma tamamlandı", 95, write));
        }
        progress?.Report(new("Ölçüm tamamlandı", 100));
        return new(DateTimeOffset.Now, device, Environment.OSVersion.VersionString, options, true, hash,
            before, allocated, released, results,
            ["Bu test Windows pagefile veya page-fault gecikmesini ölçmez; pagefile etkin değildir.",
             "Rust CUDA ölçümü bağlam seçimi, DMA, senkronizasyon ve 4 KiB CPU kopyasını içerir; örnek başına .NET geçişi yoktur.",
             "WDDM tahsisleri geri alınabilir; fiziksel VRAM yerleşikliği garantisi yoktur.",
             "Disk okuma OS önbelleğini atlar; SSD denetleyici önbelleğini atlamaz. Disk türü otomatik belirlenmez.",
             "VRAM yazması uçucudur; disk write-through yazmasıyla eşit dayanıklılık sağlamaz.",
             "RAM/commit farkları arka plan süreçlerinden etkilenir; bellek kazanımı kanıtı değildir.",
             "Sıcak QD1 testi GPU baskısı, RAM tükenmesi, oyun performansı veya uzun süreli kararlılık kanıtı değildir."]);
    }

    public static string VerifyStore(VramBlockStore store, CancellationToken token = default)
    {
        int chunk = VramBlockStore.TransferBytes + 513;
        var expected = new byte[chunk];
        var actual = new byte[chunk];
        var random = new Random(0x564741);
        using var sha = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
        for (long offset = 0; offset < store.Capacity; offset += chunk)
        {
            token.ThrowIfCancellationRequested();
            int count = (int)Math.Min(chunk, store.Capacity - offset);
            random.NextBytes(expected.AsSpan(0, count));
            store.Write(offset, expected.AsSpan(0, count));
        }
        random = new Random(0x564741);
        for (long offset = 0; offset < store.Capacity; offset += chunk)
        {
            token.ThrowIfCancellationRequested();
            int count = (int)Math.Min(chunk, store.Capacity - offset);
            random.NextBytes(expected.AsSpan(0, count));
            store.Read(offset, actual.AsSpan(0, count));
            if (!expected.AsSpan(0, count).SequenceEqual(actual.AsSpan(0, count)))
                throw new IOException($"VRAM veri doğrulaması başarısız: offset {offset}.");
            sha.AppendData(actual.AsSpan(0, count));
        }
        return Convert.ToHexString(sha.GetHashAndReset());
    }

    private static long[] Offsets(int count, long capacity)
    {
        var random = new Random(14041);
        return Enumerable.Range(0, count).Select(_ => random.NextInt64(capacity / 4096) * 4096).ToArray();
    }
    private static LatencyDistribution Measure(string path, string operation, BenchmarkOptions options,
        long[] offsets, Action<long> action, CancellationToken token)
    {
        var samples = new double[offsets.Length];
        for (int i = 0; i < offsets.Length; i++)
        {
            token.ThrowIfCancellationRequested();
            if (options.IdleGapMs > 0 && token.WaitHandle.WaitOne(options.IdleGapMs)) token.ThrowIfCancellationRequested();
            long start = Stopwatch.GetTimestamp();
            action(offsets[i]);
            samples[i] = Stopwatch.GetElapsedTime(start).TotalMicroseconds;
        }
        return Summarize(path, operation, samples);
    }
    public static LatencyDistribution Summarize(string path, string operation, double[] raw)
    {
        if (raw.Length == 0 || raw.Any(d => !double.IsFinite(d) || d < 0)) throw new ArgumentException("Geçersiz örnekler.", nameof(raw));
        var sorted = raw.Order().ToArray();
        double Q(double p) => sorted[Math.Clamp((int)Math.Ceiling(p * sorted.Length) - 1, 0, sorted.Length - 1)];
        return new(path, operation, 4096, 1, sorted[0], Q(.5), Q(.95), Q(.99), sorted[^1], raw.ToArray());
    }
}
