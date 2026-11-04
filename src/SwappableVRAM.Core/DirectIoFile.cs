using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace SwappableVRAM.Core;

/// <summary>Disposable benchmark file; never opens a volume or existing file.</summary>
internal sealed unsafe class DirectIoFile : IDisposable
{
    private readonly string path;
    private readonly SafeFileHandle handle;
    private nint buffer;
    public const int PageBytes = 4096;
    public DirectIoFile(string directory, long bytes, CancellationToken token)
    {
        Directory.CreateDirectory(directory);
        path = Path.Combine(directory, $"swappablevram-lab-{Guid.NewGuid():N}.tmp");
        // CREATE_NEW + NO_BUFFERING + WRITE_THROUGH. Never overwrites an existing file.
        handle = CreateFileW(path, 0xC0000000, 0, 0, 1, 0xA0000080, 0);
        if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
        try
        {
            buffer = VirtualAlloc(0, PageBytes, 0x3000, 0x04);
            if (buffer == 0) throw new Win32Exception(Marshal.GetLastWin32Error());
            var data = new byte[1024 * 1024];
            Random.Shared.NextBytes(data);
            nint bulk = VirtualAlloc(0, (nuint)data.Length, 0x3000, 0x04);
            if (bulk == 0) throw new Win32Exception(Marshal.GetLastWin32Error());
            try
            {
                data.CopyTo(new Span<byte>((void*)bulk, data.Length));
                for (long offset = 0; offset < bytes; offset += data.Length)
                {
                    token.ThrowIfCancellationRequested();
                    uint count = (uint)Math.Min(data.Length, bytes - offset);
                    if (!WriteFile(handle, bulk, count, out var written, 0) || written != count)
                        throw new Win32Exception(Marshal.GetLastWin32Error(), "Doğrudan I/O hazırlama başarısız.");
                }
            }
            finally { VirtualFree(bulk, 0, 0x8000); }
            Verify();
        }
        catch { Dispose(); throw; }
    }

    public void Read(long offset) => Io(offset, false);
    public void Write(long offset) => Io(offset, true);
    private void Io(long offset, bool write)
    {
        if (!SetFilePointerEx(handle, offset, out _, 0)) throw new Win32Exception(Marshal.GetLastWin32Error());
        uint transferred;
        bool success = write ? WriteFile(handle, buffer, PageBytes, out transferred, 0)
            : ReadFile(handle, buffer, PageBytes, out transferred, 0);
        if (!success || transferred != PageBytes) throw new Win32Exception(Marshal.GetLastWin32Error(), "4 KiB doğrudan I/O başarısız.");
    }
    private void Verify()
    {
        var expected = new byte[PageBytes];
        Random.Shared.NextBytes(expected);
        expected.CopyTo(new Span<byte>((void*)buffer, PageBytes));
        Write(0);
        new Span<byte>((void*)buffer, PageBytes).Clear();
        Read(0);
        if (!expected.AsSpan().SequenceEqual(new ReadOnlySpan<byte>((void*)buffer, PageBytes)))
            throw new IOException("Doğrudan disk I/O veri doğrulaması başarısız.");
    }
    public void Dispose()
    {
        handle.Dispose();
        if (buffer != 0) { VirtualFree(buffer, 0, 0x8000); buffer = 0; }
        File.Delete(path);
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFileW(string name, uint access, uint share, nint security, uint disposition, uint attributes, nint template);
    [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetFilePointerEx(SafeFileHandle file, long distance, out long position, uint method);
    [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ReadFile(SafeFileHandle file, nint buffer, uint count, out uint read, nint overlapped);
    [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WriteFile(SafeFileHandle file, nint buffer, uint count, out uint written, nint overlapped);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern nint VirtualAlloc(nint address, nuint size, uint allocation, uint protection);
    [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool VirtualFree(nint address, nuint size, uint freeType);
}
