//! Fixed-size GUI control plane. No paths, disk data, or arbitrary IOCTLs are
//! accepted. This protocol is independent of the kernel paging data plane.
use crate::store::Result;
pub const BYTES: usize = 128;
pub const MAGIC: u64 = 0x31304350494d5653;
pub const VERSION: u32 = 1;
pub const HELLO: u32 = 1;
pub const REPORT: u32 = 2;
pub const STOP: u32 = 3;
pub const START: u32 = 4;
pub const QUERY: u32 = 5;
pub const NO_DRIVER: u32 = 0;
pub const UNQUALIFIED: u32 = 1;
pub const PERMISSION_REQUIRED: u32 = 2;
pub const READY_EXPERIMENTAL: u32 = 3;
pub const STARTING: u32 = 4;
pub const ACTIVE_EXPERIMENTAL: u32 = 5;
pub const STOPPED: u32 = 6;
pub const FAULTED: u32 = 7;
pub const UNSUPPORTED_BUILD: u32 = 8;
pub const EXPERIMENTAL: u32 = 1;
pub const WDDM: u32 = 2;
pub const ELEVATED: u32 = 4;
#[derive(Clone, Debug, Default)]
pub struct Packet {
    pub kind: u32,
    pub parent: u32,
    pub broker: u32,
    pub session: u32,
    pub flags: u32,
    pub nonce: [u8; 32],
    pub sequence: u64,
    pub state: u32,
    pub drivers: u32,
    pub ci: u32,
    pub target: u64,
    pub hits: u64,
    pub fallbacks: u64,
    pub cache_bytes: u64,
    pub error: u32,
}
fn put32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
fn get32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}
fn get64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}
impl Packet {
    pub fn encode(&self) -> [u8; BYTES] {
        let mut data = [0; BYTES];
        put64(&mut data, 0, MAGIC);
        put32(&mut data, 8, VERSION);
        for (offset, value) in [
            (12, self.kind),
            (16, self.parent),
            (20, self.broker),
            (24, self.session),
            (28, self.flags),
            (72, self.state),
            (76, self.drivers),
            (80, self.ci),
            (120, self.error),
        ] {
            put32(&mut data, offset, value);
        }
        data[32..64].copy_from_slice(&self.nonce);
        for (offset, value) in [
            (64, self.sequence),
            (88, self.target),
            (96, self.hits),
            (104, self.fallbacks),
            (112, self.cache_bytes),
        ] {
            put64(&mut data, offset, value);
        }
        data
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() != BYTES
            || get64(data, 0) != MAGIC
            || get32(data, 8) != VERSION
            || get32(data, 84) != 0
            || get32(data, 124) != 0
        {
            return Err("GUI control packet size/version/reserved mismatch".into());
        }
        Ok(Self {
            kind: get32(data, 12),
            parent: get32(data, 16),
            broker: get32(data, 20),
            session: get32(data, 24),
            flags: get32(data, 28),
            nonce: data[32..64].try_into().unwrap(),
            sequence: get64(data, 64),
            state: get32(data, 72),
            drivers: get32(data, 76),
            ci: get32(data, 80),
            target: get64(data, 88),
            hits: get64(data, 96),
            fallbacks: get64(data, 104),
            cache_bytes: get64(data, 112),
            error: get32(data, 120),
        })
    }
    pub fn validate_command(&self, identity: &Self, last: u64) -> Result<()> {
        if self.parent != identity.parent
            || self.broker != identity.broker
            || self.session != identity.session
            || self.nonce != identity.nonce
            || self.sequence <= last
            || self.flags != 0
            || self.state != 0
            || self.drivers != 0
            || self.ci != 0
            || self.hits != 0
            || self.fallbacks != 0
            || self.cache_bytes != 0
            || self.error != 0
            || !matches!(self.kind, STOP | START | QUERY)
            || (self.kind != START && self.target != 0)
        {
            return Err("Unauthenticated, replayed, or invalid GUI command".into());
        }
        Ok(())
    }
}
pub fn nonce(text: &str) -> Result<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return Err("Control nonce must be 32 hexadecimal bytes".into());
    }
    let mut value = [0; 32];
    for (index, byte) in value.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| "Invalid control nonce")?;
    }
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_control_messages_reject_payloads_and_replays() {
        let identity = Packet {
            parent: 1,
            broker: 2,
            session: 1,
            nonce: [3; 32],
            ..Packet::default()
        };
        let mut command = Packet {
            kind: QUERY,
            sequence: 1,
            ..identity.clone()
        };
        let wire = command.encode();
        assert_eq!(wire.len(), 128);
        assert!(
            Packet::decode(&wire)
                .unwrap()
                .validate_command(&identity, 0)
                .is_ok()
        );
        assert!(command.validate_command(&identity, 1).is_err());
        command.cache_bytes = 4096;
        assert!(command.validate_command(&identity, 0).is_err());
        assert!(Packet::decode(&wire[..127]).is_err());
        let mut extra = wire;
        extra[84] = 1;
        assert!(Packet::decode(&extra).is_err());
        assert!(nonce(&"a".repeat(64)).is_ok());
        assert!(nonce(&"a".repeat(63)).is_err());
    }
}
