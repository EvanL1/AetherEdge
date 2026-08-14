//! Local device-command wire contract.

use core::fmt;

use aether_domain::{CommandId, PhysicalDeviceCommand, PointKind};
use sha2::{Digest, Sha256};

const COMMAND_MAGIC: [u8; 8] = *b"AETHCMD\0";
const ACK_MAGIC: [u8; 8] = *b"AETHACK\0";
const HELLO_MAGIC: [u8; 8] = *b"AETHRDY\0";
const COMMAND_HEADER_BYTES: u16 = 64;
const ACK_HEADER_BYTES: u16 = 48;
const HELLO_HEADER_BYTES: u16 = 16;

/// A malformed command-plane frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandWireError(&'static str);

impl CommandWireError {
    const fn new(message: &'static str) -> Self {
        Self(message)
    }
}

impl fmt::Display for CommandWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for CommandWireError {}

/// Durable IO command state encoded in an acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum CommandLedgerStateCode {
    Unknown = 0,
    Received = 1,
    Queued = 2,
    Dispatching = 3,
    Succeeded = 4,
    Failed = 5,
    Expired = 6,
    PossiblyApplied = 7,
}

impl CommandLedgerStateCode {
    fn decode(value: u16) -> Result<Self, CommandWireError> {
        match value {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::Received),
            2 => Ok(Self::Queued),
            3 => Ok(Self::Dispatching),
            4 => Ok(Self::Succeeded),
            5 => Ok(Self::Failed),
            6 => Ok(Self::Expired),
            7 => Ok(Self::PossiblyApplied),
            _ => Err(CommandWireError::new("unknown command ledger state")),
        }
    }
}

/// IO durable-admission result encoded in an acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum CommandAckStatus {
    Accepted = 0,
    Duplicate = 1,
    Conflict = 100,
    Expired = 101,
    Backpressure = 102,
    Invalid = 103,
    Unavailable = 104,
    Busy = 105,
    Internal = 106,
}

impl CommandAckStatus {
    fn decode(value: u16) -> Result<Self, CommandWireError> {
        match value {
            0 => Ok(Self::Accepted),
            1 => Ok(Self::Duplicate),
            100 => Ok(Self::Conflict),
            101 => Ok(Self::Expired),
            102 => Ok(Self::Backpressure),
            103 => Ok(Self::Invalid),
            104 => Ok(Self::Unavailable),
            105 => Ok(Self::Busy),
            106 => Ok(Self::Internal),
            _ => Err(CommandWireError::new(
                "unknown command acknowledgement status",
            )),
        }
    }

    /// Whether IO durably admitted this command, including an idempotent replay.
    #[must_use]
    pub const fn is_accepted(self) -> bool {
        matches!(self, Self::Accepted | Self::Duplicate)
    }
}

/// Server-first readiness frame for the single durable command protocol.
///
/// A producer does not publish a connection as ready until this frame proves
/// that the peer speaks the acknowledged protocol.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommandHello;

impl CommandHello {
    pub const SIZE: usize = 16;

    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> Result<Self, CommandWireError> {
        validate_prefix(bytes, HELLO_MAGIC, HELLO_HEADER_BYTES, Self::SIZE)?;
        Ok(Self)
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0_u8; Self::SIZE];
        encode_prefix(&mut bytes, HELLO_MAGIC, HELLO_HEADER_BYTES);
        bytes
    }
}

/// Fixed-size, endian-stable device command.
///
/// Layout: magic/reserved/header length/frame length, 128-bit command ID,
/// SHA-256 canonical payload digest, then the 40-byte command payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceCommandFrame {
    command_id: CommandId,
    payload_digest: [u8; 32],
    channel_id: u32,
    point_id: u32,
    point_kind: u8,
    value_bits: u64,
    issued_at_ms: u64,
    expires_at_ms: u64,
}

impl DeviceCommandFrame {
    pub const SIZE: usize = 104;
    const PAYLOAD_SIZE: usize = 40;

    pub fn new(command: PhysicalDeviceCommand) -> Result<Self, CommandWireError> {
        let target = command.target();
        let point_kind = match target.kind() {
            PointKind::Command => 2,
            PointKind::Action => 3,
            PointKind::Telemetry | PointKind::Status => {
                return Err(CommandWireError::new(
                    "acquisition-owned point cannot enter the command wire",
                ));
            },
        };
        let mut frame = Self {
            command_id: command.id(),
            payload_digest: [0; 32],
            channel_id: target.channel_id().get(),
            point_id: target.point_id().get(),
            point_kind,
            value_bits: command.value().to_bits(),
            issued_at_ms: command.issued_at().get(),
            expires_at_ms: command.expires_at().get(),
        };
        frame.payload_digest = digest_payload(&frame.payload_bytes());
        Ok(frame)
    }

    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> Result<Self, CommandWireError> {
        validate_prefix(bytes, COMMAND_MAGIC, COMMAND_HEADER_BYTES, Self::SIZE)?;
        if bytes[73..80] != [0; 7] {
            return Err(CommandWireError::new("command reserved bytes must be zero"));
        }
        let frame = Self {
            command_id: CommandId::new(u128::from_le_bytes(
                bytes[16..32]
                    .try_into()
                    .map_err(|_| CommandWireError::new("invalid command id"))?,
            )),
            payload_digest: bytes[32..64]
                .try_into()
                .map_err(|_| CommandWireError::new("invalid payload digest"))?,
            channel_id: read_u32(bytes, 64)?,
            point_id: read_u32(bytes, 68)?,
            point_kind: bytes[72],
            value_bits: read_u64(bytes, 80)?,
            issued_at_ms: read_u64(bytes, 88)?,
            expires_at_ms: read_u64(bytes, 96)?,
        };
        if !matches!(frame.point_kind, 2 | 3) {
            return Err(CommandWireError::new("invalid command-owned point kind"));
        }
        if !frame.value().is_finite() {
            return Err(CommandWireError::new("non-finite command value"));
        }
        if frame.expires_at_ms <= frame.issued_at_ms {
            return Err(CommandWireError::new("invalid command time window"));
        }
        if digest_payload(&frame.payload_bytes()) != frame.payload_digest {
            return Err(CommandWireError::new("command payload digest mismatch"));
        }
        Ok(frame)
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0_u8; Self::SIZE];
        encode_prefix(&mut bytes, COMMAND_MAGIC, COMMAND_HEADER_BYTES);
        bytes[16..32].copy_from_slice(&self.command_id.get().to_le_bytes());
        bytes[32..64].copy_from_slice(&self.payload_digest);
        bytes[64..104].copy_from_slice(&self.payload_bytes());
        bytes
    }

    fn payload_bytes(self) -> [u8; Self::PAYLOAD_SIZE] {
        let mut payload = [0_u8; Self::PAYLOAD_SIZE];
        payload[0..4].copy_from_slice(&self.channel_id.to_le_bytes());
        payload[4..8].copy_from_slice(&self.point_id.to_le_bytes());
        payload[8] = self.point_kind;
        payload[16..24].copy_from_slice(&self.value_bits.to_le_bytes());
        payload[24..32].copy_from_slice(&self.issued_at_ms.to_le_bytes());
        payload[32..40].copy_from_slice(&self.expires_at_ms.to_le_bytes());
        payload
    }

    /// Stable idempotency digest for the physical operation itself.
    ///
    /// Issuance and expiry remain integrity-protected by `payload_digest`, but
    /// are excluded here so an HTTP retry with the same caller-owned CommandId,
    /// target, and value recognizes the original admission without extending
    /// its persisted deadline.
    #[must_use]
    pub fn semantic_digest(self) -> [u8; 32] {
        let payload = self.payload_bytes();
        let mut semantic = [0_u8; 24];
        semantic[0..16].copy_from_slice(&payload[0..16]);
        semantic[16..24].copy_from_slice(&payload[16..24]);
        Sha256::digest(semantic).into()
    }

    #[must_use]
    pub const fn command_id(self) -> CommandId {
        self.command_id
    }

    #[must_use]
    pub const fn payload_digest(self) -> [u8; 32] {
        self.payload_digest
    }

    #[must_use]
    pub const fn channel_id(self) -> u32 {
        self.channel_id
    }

    #[must_use]
    pub const fn point_id(self) -> u32 {
        self.point_id
    }

    #[must_use]
    pub const fn point_kind_code(self) -> u8 {
        self.point_kind
    }

    #[must_use]
    pub const fn point_kind(self) -> PointKind {
        match self.point_kind {
            2 => PointKind::Command,
            _ => PointKind::Action,
        }
    }

    #[must_use]
    pub fn value(self) -> f64 {
        f64::from_bits(self.value_bits)
    }

    #[must_use]
    pub const fn issued_at_ms(self) -> u64 {
        self.issued_at_ms
    }

    #[must_use]
    pub const fn expires_at_ms(self) -> u64 {
        self.expires_at_ms
    }
}

/// Fixed-size IO durable-admission acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceCommandAck {
    command_id: CommandId,
    status: CommandAckStatus,
    state: CommandLedgerStateCode,
    recorded_at_ms: u64,
}

impl DeviceCommandAck {
    pub const SIZE: usize = 48;

    #[must_use]
    pub const fn new(
        command_id: CommandId,
        status: CommandAckStatus,
        state: CommandLedgerStateCode,
        recorded_at_ms: u64,
    ) -> Self {
        Self {
            command_id,
            status,
            state,
            recorded_at_ms,
        }
    }

    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> Result<Self, CommandWireError> {
        validate_prefix(bytes, ACK_MAGIC, ACK_HEADER_BYTES, Self::SIZE)?;
        if bytes[36..40] != [0; 4] {
            return Err(CommandWireError::new(
                "acknowledgement reserved bytes must be zero",
            ));
        }
        let ack = Self {
            command_id: CommandId::new(u128::from_le_bytes(
                bytes[16..32]
                    .try_into()
                    .map_err(|_| CommandWireError::new("invalid acknowledgement command id"))?,
            )),
            status: CommandAckStatus::decode(read_u16(bytes, 32)?)?,
            state: CommandLedgerStateCode::decode(read_u16(bytes, 34)?)?,
            recorded_at_ms: read_u64(bytes, 40)?,
        };
        if ack.status.is_accepted()
            && (matches!(
                ack.state,
                CommandLedgerStateCode::Unknown | CommandLedgerStateCode::Received
            ) || ack.recorded_at_ms == 0)
        {
            return Err(CommandWireError::new(
                "accepted acknowledgement lacks durable admission state",
            ));
        }
        if ack.status == CommandAckStatus::Expired && ack.state != CommandLedgerStateCode::Expired {
            return Err(CommandWireError::new(
                "expired acknowledgement must report Expired state",
            ));
        }
        if ack.status == CommandAckStatus::Busy && ack.state == CommandLedgerStateCode::Unknown {
            return Err(CommandWireError::new(
                "busy acknowledgement must report its durable state",
            ));
        }
        Ok(ack)
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0_u8; Self::SIZE];
        encode_prefix(&mut bytes, ACK_MAGIC, ACK_HEADER_BYTES);
        bytes[16..32].copy_from_slice(&self.command_id.get().to_le_bytes());
        bytes[32..34].copy_from_slice(&(self.status as u16).to_le_bytes());
        bytes[34..36].copy_from_slice(&(self.state as u16).to_le_bytes());
        bytes[40..48].copy_from_slice(&self.recorded_at_ms.to_le_bytes());
        bytes
    }

    #[must_use]
    pub const fn command_id(self) -> CommandId {
        self.command_id
    }

    #[must_use]
    pub const fn status(self) -> CommandAckStatus {
        self.status
    }

    #[must_use]
    pub const fn state(self) -> CommandLedgerStateCode {
        self.state
    }

    #[must_use]
    pub const fn recorded_at_ms(self) -> u64 {
        self.recorded_at_ms
    }
}

fn digest_payload(payload: &[u8; DeviceCommandFrame::PAYLOAD_SIZE]) -> [u8; 32] {
    Sha256::digest(payload).into()
}

fn encode_prefix<const N: usize>(bytes: &mut [u8; N], magic: [u8; 8], header_bytes: u16) {
    bytes[0..8].copy_from_slice(&magic);
    bytes[8..10].copy_from_slice(&0_u16.to_le_bytes());
    bytes[10..12].copy_from_slice(&header_bytes.to_le_bytes());
    bytes[12..16].copy_from_slice(&(N as u32).to_le_bytes());
}

fn validate_prefix<const N: usize>(
    bytes: &[u8; N],
    magic: [u8; 8],
    header_bytes: u16,
    frame_bytes: usize,
) -> Result<(), CommandWireError> {
    if bytes[0..8] != magic {
        return Err(CommandWireError::new("invalid command-plane magic"));
    }
    if read_u16(bytes, 8)? != 0 {
        return Err(CommandWireError::new(
            "command-plane reserved bytes must be zero",
        ));
    }
    if read_u16(bytes, 10)? != header_bytes {
        return Err(CommandWireError::new("invalid command-plane header length"));
    }
    if usize::try_from(read_u32(bytes, 12)?).ok() != Some(frame_bytes) {
        return Err(CommandWireError::new("invalid command-plane frame length"));
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, CommandWireError> {
    Ok(u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .map_err(|_| CommandWireError::new("truncated u16 field"))?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, CommandWireError> {
    Ok(u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .map_err(|_| CommandWireError::new("truncated u32 field"))?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, CommandWireError> {
    Ok(u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .map_err(|_| CommandWireError::new("truncated u64 field"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_domain::{ChannelCommandAddress, ChannelId, PointId, TimestampMs};
    use core::fmt::Write;

    fn encode_hex(bytes: &[u8]) -> String {
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(&mut encoded, "{byte:02x}").expect("write to String");
        }
        encoded
    }

    fn command() -> PhysicalDeviceCommand {
        PhysicalDeviceCommand::new(
            CommandId::new(0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00),
            ChannelCommandAddress::new(ChannelId::new(7), PointKind::Command, PointId::new(9))
                .expect("command address"),
            12.5,
            TimestampMs::new(100),
            TimestampMs::new(200),
        )
        .expect("physical command")
    }

    #[test]
    fn command_is_fixed_little_endian_and_digest_protected() {
        let frame = DeviceCommandFrame::new(command()).expect("encode command");
        let bytes = frame.to_bytes();
        assert_eq!(&bytes[0..8], b"AETHCMD\0");
        assert_eq!(&bytes[8..10], &0_u16.to_le_bytes());
        assert_eq!(&bytes[12..16], &104_u32.to_le_bytes());
        assert_eq!(DeviceCommandFrame::from_bytes(&bytes).unwrap(), frame);

        let mut corrupt = bytes;
        corrupt[80] ^= 1;
        assert!(DeviceCommandFrame::from_bytes(&corrupt).is_err());

        let mut nonzero_reserved = bytes;
        nonzero_reserved[8] = 1;
        assert!(DeviceCommandFrame::from_bytes(&nonzero_reserved).is_err());
    }

    #[test]
    fn acknowledgement_round_trips() {
        let ack = DeviceCommandAck::new(
            command().id(),
            CommandAckStatus::Duplicate,
            CommandLedgerStateCode::Succeeded,
            123,
        );
        assert_eq!(DeviceCommandAck::from_bytes(&ack.to_bytes()).unwrap(), ack);
    }

    #[test]
    fn command_and_ack_match_the_pinned_golden_vectors() {
        let frame = DeviceCommandFrame::new(command()).expect("encode command");
        assert_eq!(
            encode_hex(&frame.to_bytes()),
            concat!(
                "41455448434d44000000400068000000",
                "00ffeeddccbbaa998877665544332211",
                "04594debc64430ef5d08b9728a81cad8cf46adbd25074830e8c66a713d3adc2a",
                "070000000900000002000000000000000000000000002940",
                "6400000000000000c800000000000000"
            )
        );

        let ack = DeviceCommandAck::new(
            command().id(),
            CommandAckStatus::Duplicate,
            CommandLedgerStateCode::Succeeded,
            123,
        );
        assert_eq!(
            encode_hex(&ack.to_bytes()),
            concat!(
                "4145544841434b000000300030000000",
                "00ffeeddccbbaa998877665544332211",
                "01000400000000007b00000000000000"
            )
        );
    }

    #[test]
    fn server_hello_proves_command_readiness() {
        let hello = CommandHello::new();
        let bytes = hello.to_bytes();
        assert_eq!(&bytes[0..8], b"AETHRDY\0");
        assert_eq!(CommandHello::from_bytes(&bytes).unwrap(), hello);
    }

    #[test]
    fn accepted_ack_requires_a_persisted_state_and_timestamp() {
        let unknown = DeviceCommandAck::new(
            command().id(),
            CommandAckStatus::Accepted,
            CommandLedgerStateCode::Unknown,
            123,
        );
        assert!(DeviceCommandAck::from_bytes(&unknown.to_bytes()).is_err());

        let zero_timestamp = DeviceCommandAck::new(
            command().id(),
            CommandAckStatus::Duplicate,
            CommandLedgerStateCode::Queued,
            0,
        );
        assert!(DeviceCommandAck::from_bytes(&zero_timestamp.to_bytes()).is_err());
    }
}
