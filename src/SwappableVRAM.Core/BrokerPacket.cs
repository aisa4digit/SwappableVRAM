using System.Buffers.Binary;

namespace SwappableVRAM.Core;

public enum BrokerState : uint
{
    NoDriver, Unqualified, PermissionRequired, ReadyExperimental, Starting,
    ActiveExperimental, Stopped, Faulted, UnsupportedBuild
}

public sealed record BrokerStatus(BrokerState State, uint DriverCount, uint CodeIntegrityOptions,
    ulong Target, ulong Hits, ulong Fallbacks, ulong CacheBytes, uint Error, uint Flags);

// Deliberately separate from the kernel data protocol: no paths or disk payloads.
internal sealed record BrokerPacket
{
    internal const int Size = 128;
    internal const uint Hello = 1, Report = 2, Stop = 3, Start = 4, Query = 5;
    internal uint Kind, Parent, Broker, Session, Flags, State, Drivers, Ci, Error;
    internal ulong Sequence, Target, Hits, Fallbacks, CacheBytes;
    internal byte[] Nonce = new byte[32];

    internal byte[] Encode()
    {
        var data = new byte[Size];
        void U32(int offset, uint value) => BinaryPrimitives.WriteUInt32LittleEndian(data.AsSpan(offset), value);
        void U64(int offset, ulong value) => BinaryPrimitives.WriteUInt64LittleEndian(data.AsSpan(offset), value);
        U64(0, 0x31304350494d5653); U32(8, 1); U32(12, Kind);
        U32(16, Parent); U32(20, Broker); U32(24, Session); U32(28, Flags);
        if (Nonce.Length != 32) throw new InvalidDataException("Invalid broker nonce size.");
        Nonce.CopyTo(data, 32); U64(64, Sequence); U32(72, State); U32(76, Drivers);
        U32(80, Ci); U64(88, Target); U64(96, Hits); U64(104, Fallbacks);
        U64(112, CacheBytes); U32(120, Error);
        return data;
    }

    internal static BrokerPacket Decode(byte[] data)
    {
        if (data.Length != Size) throw new InvalidDataException("Broker message size mismatch.");
        uint U32(int offset) => BinaryPrimitives.ReadUInt32LittleEndian(data.AsSpan(offset));
        ulong U64(int offset) => BinaryPrimitives.ReadUInt64LittleEndian(data.AsSpan(offset));
        if (U64(0) != 0x31304350494d5653 || U32(8) != 1 || U32(84) != 0 || U32(124) != 0)
            throw new InvalidDataException("Broker protocol/version/reserved mismatch.");
        return new()
        {
            Kind = U32(12), Parent = U32(16), Broker = U32(20), Session = U32(24), Flags = U32(28),
            Nonce = data[32..64], Sequence = U64(64), State = U32(72), Drivers = U32(76), Ci = U32(80),
            Target = U64(88), Hits = U64(96), Fallbacks = U64(104), CacheBytes = U64(112), Error = U32(120)
        };
    }

    internal BrokerStatus Status => new((BrokerState)State, Drivers, Ci, Target, Hits, Fallbacks, CacheBytes, Error, Flags);
}
