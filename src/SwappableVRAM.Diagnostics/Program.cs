// See https://aka.ms/new-console-template for more information
using System.Text.Json;
using SwappableVRAM.Core;

try
{
    if (args.Length == 2 && args[0] == "controller-test")
    {
        string helper = Path.GetFullPath(args[1]);
        int stoppedPid;
        await using (var broker = await BrokerSession.ConnectAsync(helper))
        {
            stoppedPid = broker.ProcessId;
            var status = await broker.QueryAsync();
            Console.WriteLine(JsonSerializer.Serialize(status));
            Require((status.Flags & 1) == 0, "normal profile has no experimental permission");
            var denied = await broker.StartAsync(0);
            Require(denied.State != BrokerState.Starting && denied.State != BrokerState.ActiveExperimental, "default START never enables unqualified paging");
            var queries = await Task.WhenAll(Enumerable.Range(0, 16).Select(_ => broker.QueryAsync()));
            Require(queries.All(p => p.State == status.State), "serialized concurrent status requests");
            Require((await broker.StopAsync()).State == BrokerState.Stopped, "STOP acknowledged after teardown");
        }
        Require(!Alive(stoppedPid), "owned helper exited after STOP");
        int disconnectedPid;
        await using (var broker = await BrokerSession.ConnectAsync(helper)) { disconnectedPid = broker.ProcessId; }
        Require(!Alive(disconnectedPid), "pipe disconnect exits owned helper");
        await ControllerChecks.RejectedMessagesAsync(helper);
        Console.WriteLine("PASS: local .NET/Rust PID/session/nonce handshake, status, START refusal, 16 concurrent queries, STOP, disconnect and child exit. No GPU allocations, driver install, VM or security changes.");
        return 0;
    }
    if (args.Length > 0 && args[0] == "host-status")
    {
        Console.WriteLine(JsonSerializer.Serialize(LocalBackendStatus.Capture(), new JsonSerializerOptions { WriteIndented = true }));
        return 0;
    }
    if (args.Length == 0 || args[0] == "devices")
    {
        Console.WriteLine(JsonSerializer.Serialize(GpuCatalog.Enumerate(), new JsonSerializerOptions { WriteIndented = true }));
        return 0;
    }
    if (args[0] == "test")
    {
        var raw = Enumerable.Range(1, 100).Select(x => (double)x).Reverse().ToArray();
        var summary = LatencyBenchmark.Summarize("test", "read", raw);
        Require(summary.P50Us == 50 && summary.P95Us == 95 && summary.P99Us == 99, "nearest-rank percentiles");
        Require(raw[0] == 100, "sample order preserved");
        foreach (var (offset, length, capacity) in new (long, long, long)[] { (-1, 1, 10), (0, -1, 10), (9, 2, 10), (long.MaxValue - 1, 10, long.MaxValue) })
        {
            bool rejected = false;
            try { VramBlockStore.ValidateRange(offset, length, capacity); }
            catch (ArgumentOutOfRangeException) { rejected = true; }
            Require(rejected, $"overflow-safe range {offset},{length},{capacity}");
        }
        VramBlockStore.ValidateRange(10, 0, 10);
        using (var store = new VramBlockStore(0, 16 * VramBlockStore.MiB))
        {
            var zeroes = new byte[4096];
            var read = Enumerable.Repeat((byte)1, 4096).ToArray();
            store.Read(store.Capacity - 4096, read);
            Require(zeroes.SequenceEqual(read), "new allocation is zero initialized");
            Console.WriteLine($"SHA256={LatencyBenchmark.VerifyStore(store)}");
            Parallel.For(0, 8, i =>
            {
                byte[] value = Enumerable.Repeat((byte)(i + 1), 8192).ToArray();
                byte[] back = new byte[value.Length];
                store.Write(i * 8192L, value);
                store.Read(i * 8192L, back);
                Require(value.SequenceEqual(back), "cross-thread context and integrity");
            });
            store.Dispose();
            bool rejected = false;
            try { store.Read(0, read); } catch (ObjectDisposedException) { rejected = true; }
            Require(rejected, "use after dispose");
        }
        Console.WriteLine("PASS: bounds, percentiles, zero initialization, full VRAM integrity, >4 MiB split, multithread context, disposal.");
        return 0;
    }
    if (args[0] == "benchmark")
    {
        string output = args.Length > 1 ? Path.GetFullPath(args[1]) : Path.GetFullPath("benchmark.json");
        string? disk = args.Length > 2 && args[2] != "--no-disk" ? Path.GetFullPath(args[2]) : null;
        int gap = args.Length > 3 ? int.Parse(args[3]) : 0;
        using var cancellation = new CancellationTokenSource();
        Console.CancelKeyPress += (_, e) => { e.Cancel = true; cancellation.Cancel(); };
        var progress = new Progress<BenchmarkProgress>(p => Console.Error.WriteLine($"{p.Percent}% {p.Stage}"));
        var report = LatencyBenchmark.Run(new(DiskDirectory: disk, IdleGapMs: gap), progress, cancellation.Token);
        Directory.CreateDirectory(Path.GetDirectoryName(output)!);
        report.Save(output);
        foreach (var result in report.Results)
            Console.WriteLine($"{result.Path} {result.Operation}: p50={result.P50Us:F1} p95={result.P95Us:F1} p99={result.P99Us:F1} max={result.MaxUs:F1} us");
        Console.WriteLine(output);
        return 0;
    }
    Console.Error.WriteLine("devices | host-status | controller-test <Rust-helper.exe> | test | benchmark <report.json> [disk-test-directory | --no-disk] [idle-gap-ms]");
    return 2;
}
catch (Exception e)
{
    Console.Error.WriteLine(e.Message);
    return 1;
}

static void Require(bool condition, string name)
{
    if (!condition) throw new InvalidOperationException($"FAIL: {name}");
}

static bool Alive(int pid)
{
    try { using var process = System.Diagnostics.Process.GetProcessById(pid); return !process.HasExited; }
    catch (ArgumentException) { return false; }
}
