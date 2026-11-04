using System.Collections.Concurrent;
using System.Diagnostics;
using System.IO.Pipes;
using System.Security.Cryptography;

namespace SwappableVRAM.Core;

/// <summary>Owns exactly one interactive Rust helper; opening a session never enables paging.</summary>
public sealed class BrokerSession : IAsyncDisposable
{
    private readonly NamedPipeServerStream pipe;
    private readonly Process process;
    private readonly BrokerPacket identity;
    private readonly SemaphoreSlim commands = new(1, 1);
    private readonly ConcurrentDictionary<ulong, TaskCompletionSource<BrokerStatus>> replies = new();
    private readonly CancellationTokenSource lifetime = new();
    private Task reader = Task.CompletedTask;
    private ulong sequence;
    private int disposed;
    public BrokerStatus? Latest { get; private set; }
    public int ProcessId => process.Id;

    private BrokerSession(NamedPipeServerStream pipe, Process process, BrokerPacket identity)
    {
        this.pipe = pipe; this.process = process; this.identity = identity;
    }

    public static async Task<BrokerSession> ConnectAsync(string helperPath, CancellationToken cancellation = default)
    {
        if (!OperatingSystem.IsWindows()) throw new PlatformNotSupportedException("Windows required.");
        helperPath = Path.GetFullPath(helperPath);
        if (!File.Exists(helperPath)) throw new FileNotFoundException("Rust broker bulunamadı.", helperPath);
        string name = $"SwappableVRAM-ui-{Guid.NewGuid():N}";
        var nonce = RandomNumberGenerator.GetBytes(32);
        var pipe = LocalBrokerPipe.Create(name);
        Process? process = null;
        BrokerSession? session = null;
        try
        {
            var start = new ProcessStartInfo(helperPath) { UseShellExecute = false, CreateNoWindow = true };
            foreach (string arg in new[] { "broker-host", name, Environment.ProcessId.ToString(System.Globalization.CultureInfo.InvariantCulture), Convert.ToHexString(nonce) })
                start.ArgumentList.Add(arg);
            process = Process.Start(start) ?? throw new IOException("Rust broker başlatılamadı.");
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            timeout.CancelAfter(TimeSpan.FromSeconds(5));
            await pipe.WaitForConnectionAsync(timeout.Token).ConfigureAwait(false);
            LocalBrokerPipe.VerifyClient(pipe, process.Id);
            pipe.ReadMode = PipeTransmissionMode.Message;
            var hello = await ReadPacketAsync(pipe, timeout.Token).ConfigureAwait(false);
            if (hello.Kind != BrokerPacket.Hello || hello.Sequence != 0 || hello.Parent != (uint)Environment.ProcessId ||
                hello.Broker != (uint)process.Id || hello.Session != (uint)Process.GetCurrentProcess().SessionId ||
                !CryptographicOperations.FixedTimeEquals(hello.Nonce, nonce) || (hello.Flags & ~7u) != 0 ||
                (hello.Flags & 1) != 0 || hello.State != 0 || hello.Drivers != 0 || hello.Ci != 0 ||
                hello.Target != 0 || hello.Hits != 0 || hello.Fallbacks != 0 || hello.CacheBytes != 0 || hello.Error != 0)
                throw new InvalidDataException("Rust broker handshake rejected.");
            session = new(pipe, process, hello);
            session.reader = session.ReadLoopAsync();
            await session.QueryAsync(timeout.Token).ConfigureAwait(false);
            return session;
        }
        catch
        {
            if (session is not null) await session.DisposeAsync().ConfigureAwait(false);
            else
            {
                pipe.Dispose();
                if (process is not null) { await WaitOwnedExitAsync(process).ConfigureAwait(false); process.Dispose(); }
            }
            throw;
        }
    }

    private static async Task<BrokerPacket> ReadPacketAsync(NamedPipeServerStream pipe, CancellationToken token)
    {
        var data = new byte[BrokerPacket.Size];
        int length = await pipe.ReadAsync(data, token).ConfigureAwait(false);
        if (length != data.Length || !pipe.IsMessageComplete) throw new InvalidDataException("Partial or oversized broker message.");
        return BrokerPacket.Decode(data);
    }

    private async Task ReadLoopAsync()
    {
        Exception end = new EndOfStreamException("Rust broker bağlantısı kapandı.");
        try
        {
            while (!lifetime.IsCancellationRequested)
            {
                var packet = await ReadPacketAsync(pipe, lifetime.Token).ConfigureAwait(false);
                if (packet.Kind != BrokerPacket.Report || packet.Parent != identity.Parent || packet.Broker != identity.Broker ||
                    packet.Session != identity.Session || packet.Flags != identity.Flags || packet.State > (uint)BrokerState.UnsupportedBuild ||
                    !CryptographicOperations.FixedTimeEquals(packet.Nonce, identity.Nonce))
                    throw new InvalidDataException("Invalid Rust broker status identity.");
                Latest = packet.Status;
                if (packet.Sequence != 0)
                {
                    if (!replies.TryRemove(packet.Sequence, out var reply)) throw new InvalidDataException("Unexpected broker reply sequence.");
                    reply.TrySetResult(packet.Status);
                }
                if (packet.State == (uint)BrokerState.Stopped) break;
            }
        }
        catch (Exception ex) { end = ex; }
        finally
        {
            // Closing the peer is also the broker's stop signal on protocol failure.
            pipe.Dispose();
            foreach (var entry in replies) if (replies.TryRemove(entry.Key, out var reply)) reply.TrySetException(end);
        }
    }

    private async Task<BrokerStatus> CommandAsync(uint kind, ulong target, CancellationToken cancellation)
    {
        ObjectDisposedException.ThrowIf(Volatile.Read(ref disposed) != 0, this);
        await commands.WaitAsync(cancellation).ConfigureAwait(false);
        try
        {
            ObjectDisposedException.ThrowIf(Volatile.Read(ref disposed) != 0, this);
            if (reader.IsCompleted) throw new IOException("Rust broker bağlantısı sona erdi.");
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token);
            timeout.CancelAfter(TimeSpan.FromSeconds(7));
            ulong request = checked(++sequence);
            var response = new TaskCompletionSource<BrokerStatus>(TaskCreationOptions.RunContinuationsAsynchronously);
            if (!replies.TryAdd(request, response)) throw new InvalidOperationException("Duplicate broker request.");
            try
            {
                var command = new BrokerPacket { Kind = kind, Parent = identity.Parent, Broker = identity.Broker,
                    Session = identity.Session, Nonce = identity.Nonce, Sequence = request, Target = target };
                await pipe.WriteAsync(command.Encode(), timeout.Token).ConfigureAwait(false);
                return await response.Task.WaitAsync(timeout.Token).ConfigureAwait(false);
            }
            catch
            {
                // A timed-out command has ambiguous state; revoke the entire session.
                lifetime.Cancel(); pipe.Dispose();
                throw;
            }
            finally { replies.TryRemove(request, out _); }
        }
        finally { commands.Release(); }
    }

    public Task<BrokerStatus> QueryAsync(CancellationToken cancellation = default) => CommandAsync(BrokerPacket.Query, 0, cancellation);
    // Default helper refuses START. No GUI switch or elevation path opts into the lab profile.
    public Task<BrokerStatus> StartAsync(ulong target, CancellationToken cancellation = default) => CommandAsync(BrokerPacket.Start, target, cancellation);
    public async Task<BrokerStatus> StopAsync(CancellationToken cancellation = default)
    {
        var status = await CommandAsync(BrokerPacket.Stop, 0, cancellation).ConfigureAwait(false);
        if (status.State != BrokerState.Stopped) throw new IOException("Broker stop was not acknowledged.");
        await WaitOwnedExitAsync(process).ConfigureAwait(false);
        return status;
    }

    private static async Task WaitOwnedExitAsync(Process process)
    {
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(6));
        try { await process.WaitForExitAsync(timeout.Token).ConfigureAwait(false); }
        catch (OperationCanceledException)
        {
            // Only the child Process handle we created; never search/kill by name.
            if (!process.HasExited) process.Kill();
            await process.WaitForExitAsync().ConfigureAwait(false);
        }
    }

    public async ValueTask DisposeAsync()
    {
        if (Interlocked.Exchange(ref disposed, 1) != 0) return;
        lifetime.Cancel(); pipe.Dispose();
        await commands.WaitAsync().ConfigureAwait(false);
        try
        {
            await reader.ConfigureAwait(false);
            await WaitOwnedExitAsync(process).ConfigureAwait(false);
            process.Dispose(); lifetime.Dispose();
        }
        finally { commands.Release(); }
    }
}
