using System.Diagnostics;
using System.IO.Pipes;
using System.Security.Cryptography;
using SwappableVRAM.Core;

internal static class ControllerChecks
{
    // Black-box tests of the real Rust process. The probe profile never opens a
    // CUDA context, enables the driver, or changes machine configuration.
    internal static async Task RejectedMessagesAsync(string helper)
    {
        if (!OperatingSystem.IsWindows()) throw new PlatformNotSupportedException("Windows required.");
        foreach (string scenario in new[] { "nonce", "replay", "oversize", "peer-pid" })
        {
            using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(8));
            string name = $"SwappableVRAM-ui-{Guid.NewGuid():N}";
            byte[] nonce = RandomNumberGenerator.GetBytes(32);
            using var pipe = LocalBrokerPipe.Create(name);
            var start = new ProcessStartInfo(helper) { UseShellExecute = false, CreateNoWindow = true, RedirectStandardError = true };
            foreach (string arg in new[] { "broker-host", name, Environment.ProcessId.ToString(System.Globalization.CultureInfo.InvariantCulture), Convert.ToHexString(nonce) })
                start.ArgumentList.Add(arg);
            using var process = Process.Start(start) ?? throw new IOException("Test helper did not start.");
            var stderr = process.StandardError.ReadToEndAsync(timeout.Token);
            try
            {
                await pipe.WaitForConnectionAsync(timeout.Token);
                LocalBrokerPipe.VerifyClient(pipe, process.Id);
                pipe.ReadMode = PipeTransmissionMode.Message;
                var hello = await ReadAsync(pipe, timeout.Token);
                if (hello.Kind != BrokerPacket.Hello) throw new InvalidDataException("Missing test HELLO.");
                var command = new BrokerPacket { Kind = BrokerPacket.Query, Parent = hello.Parent, Broker = hello.Broker,
                    Session = hello.Session, Nonce = nonce, Sequence = 1 };
                if (scenario == "replay")
                {
                    await pipe.WriteAsync(command.Encode(), timeout.Token);
                    while ((await ReadAsync(pipe, timeout.Token)).Sequence != 1) { }
                }
                if (scenario == "nonce") command.Nonce = new byte[32];
                if (scenario == "peer-pid") command.Broker = hello.Parent;
                byte[] wire = command.Encode();
                if (scenario == "oversize") wire = [.. wire, 1];
                await pipe.WriteAsync(wire, timeout.Token);
                await process.WaitForExitAsync(timeout.Token);
                string error = await stderr;
                string expected = scenario == "oversize" ? "GUI control read: Win32 234" : "Unauthenticated, replayed, or invalid GUI command";
                if (process.ExitCode == 0 || !error.Contains(expected, StringComparison.Ordinal))
                    throw new InvalidOperationException($"Invalid control message was not rejected ({scenario}): {error}");
                Console.WriteLine($"PASS: Rust rejected {scenario} and exited without enabling paging.");
            }
            finally
            {
                pipe.Dispose();
                if (!process.HasExited)
                {
                    process.Kill();
                    await process.WaitForExitAsync();
                }
            }
        }
    }

    private static async Task<BrokerPacket> ReadAsync(NamedPipeServerStream pipe, CancellationToken token)
    {
        var wire = new byte[BrokerPacket.Size];
        if (await pipe.ReadAsync(wire, token) != wire.Length || !pipe.IsMessageComplete)
            throw new InvalidDataException("Invalid test message framing.");
        return BrokerPacket.Decode(wire);
    }
}
