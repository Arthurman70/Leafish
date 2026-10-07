//! Bounded Minecraft Java 1.21.1 (protocol 767) Configuration codecs.
//!
//! This is a codec prerequisite, not an enabled protocol or a NeoForge mod runtime.
//! Inputs are packet *bodies*: the transport must already have removed the packet
//! length, compression framing, and packet id. Unknown packet ids fail explicitly.
//! Unknown custom payloads remain opaque; decoding them does not negotiate a channel
//! or acknowledge that its mod is implemented. Registry entry order is significant.
//!
//! Ground truth: the user's `neoforge-21.1.236-sources.jar`, classes
//! `configuration.ConfigurationProtocols`, `ClientboundRegistryDataPacket`,
//! `RegistrySynchronization.PackedRegistryEntry`, `KnownPack`, `ByteBufCodecs`,
//! `FriendlyByteBuf`, and `NbtIo`. Registry data is boolean + optional TAG (not
//! OPTIONAL_COMPOUND_TAG). Network NBT has a type byte and payload, *no root name*.
//! The limits below are conservative local resource limits, not parity claims.

use std::convert::TryInto;
use std::fmt;

pub const PROTOCOL_VERSION: i32 = 767;
pub const MAX_PACKET_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NBT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_NBT_DEPTH: usize = 64;
pub const MAX_NBT_NODES: usize = 262_144;
pub const MAX_REGISTRY_ENTRIES: usize = 65_536;
pub const MAX_KNOWN_PACKS: usize = 1_024;
pub const MAX_FEATURE_FLAGS: usize = 4_096;
pub const MAX_TAG_REGISTRIES: usize = 4_096;
pub const MAX_TAGS: usize = 65_536;
pub const MAX_TAG_MEMBERS: usize = 1_048_576;
pub const MAX_REPORT_DETAILS: usize = 32;
pub const MAX_SERVER_LINKS: usize = 1_024;
pub const MAX_STRING_UNITS: usize = 32_767;
pub const MAX_CLIENT_CUSTOM_PAYLOAD_BYTES: usize = 1_048_576;
pub const MAX_SERVER_CUSTOM_PAYLOAD_BYTES: usize = 32_767;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigurationError {
    UnsupportedPacket(i32),
    Truncated,
    InvalidVarInt,
    InvalidUtf8,
    InvalidIdentifier,
    InvalidNbt(&'static str),
    NegativeLength,
    LimitExceeded(&'static str),
    TrailingBytes(usize),
    InvalidTransition,
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Minecraft 1.21.1 configuration: {:?}", self)
    }
}
impl std::error::Error for ConfigurationError {}
pub type Result<T> = std::result::Result<T, ConfigurationError>;

/// A structurally validated, exact network-NBT encoding, including its type byte.
/// Names/string values remain modified-UTF-8 bytes; they are never lossy-decoded.
/// Any non-End NBT root type is allowed by ByteBufCodecs.TAG.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkNbt(Vec<u8>);

impl NetworkNbt {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_NBT_BYTES {
            return Err(ConfigurationError::LimitExceeded("NBT bytes"));
        }
        let mut reader = Reader::new(bytes)?;
        let nbt = reader.network_nbt()?;
        reader.finish()?;
        Ok(nbt)
    }

    /// Read one non-End network-NBT value, leaving later packet bytes untouched.
    /// The input retains the packet bound; only the consumed value has the NBT bound.
    pub fn read_prefix(bytes: &[u8]) -> Result<(Self, usize)> {
        let mut reader = Reader::new(bytes)?;
        let nbt = reader.network_nbt()?;
        Ok((nbt, reader.pos))
    }

    /// Read a required, uniquely named Int directly within a compound root.
    pub fn compound_i32(&self, key: &str) -> Result<i32> {
        let bytes = self.compound_scalar(key, 3)?;
        Ok(i32::from_be_bytes(bytes.try_into().unwrap()))
    }

    /// Read a required, uniquely named Byte directly within a compound root.
    /// NBT booleans use zero for false and every nonzero byte for true.
    pub fn compound_bool(&self, key: &str) -> Result<bool> {
        Ok(self.compound_scalar(key, 1)?[0] != 0)
    }

    fn compound_scalar(&self, key: &str, expected_tag: u8) -> Result<&[u8]> {
        let mut reader = Reader::new(&self.0)?;
        if reader.u8()? != 10 {
            return Err(ConfigurationError::InvalidNbt("root is not a compound"));
        }
        reader.nbt_nodes = 1; // Account for the compound root, as skip_nbt does.
        let mut value = None;
        loop {
            let tag = reader.u8()?;
            if tag == 0 {
                break;
            }
            let name = reader.nbt_string_bytes()?;
            let matches = modified_utf8_matches(name, key);
            let start = reader.pos;
            reader.skip_nbt(tag, 1, 0)?;
            if matches {
                if value.is_some() {
                    return Err(ConfigurationError::InvalidNbt("duplicate compound key"));
                }
                if tag != expected_tag {
                    return Err(ConfigurationError::InvalidNbt(
                        "compound key has wrong type",
                    ));
                }
                value = Some(&self.0[start..reader.pos]);
            }
        }
        reader.finish()?;
        value.ok_or(ConfigurationError::InvalidNbt("missing compound key"))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryEntry {
    pub id: String,
    /// None means data omitted because a known pack supplies it. It is NOT air,
    /// an empty compound, or permission to invent a replacement definition.
    pub data: Option<NetworkNbt>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownPack {
    pub namespace: String,
    pub id: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryTag {
    pub id: String,
    /// Numeric registry ids, preserved in wire order; resolution is a later step.
    pub entries: Vec<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryTags {
    pub registry: String,
    pub tags: Vec<RegistryTag>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerLinkLabel {
    /// Wire id (0..=9 in vanilla 1.21.1). Keep unknown ids rather than relabeling
    /// them as BUG_REPORT, which vanilla's OutOfBoundsStrategy.ZERO would do.
    Known(i32),
    /// Exact Component NBT. The codec does not render text or execute events.
    Custom(NetworkNbt),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerLink {
    pub label: ServerLinkLabel,
    /// Untrusted text, retained without opening, fetching, or interpreting it.
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clientbound {
    CustomPayload {
        id: String,
        data: Vec<u8>,
    },
    FinishConfiguration,
    KeepAlive(i64),
    Ping(i32),
    ResetChat,
    RegistryData {
        registry: String,
        entries: Vec<RegistryEntry>,
    },
    EnabledFeatures(Vec<String>),
    UpdateTags(Vec<RegistryTags>),
    SelectKnownPacks(Vec<KnownPack>),
    CustomReportDetails(Vec<(String, String)>),
    ServerLinks(Vec<ServerLink>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Serverbound {
    CustomPayload {
        id: String,
        data: Vec<u8>,
    },
    FinishConfiguration,
    KeepAlive(i64),
    Pong(i32),
    /// Advertise only packs actually available with exactly matching contents.
    /// An empty selection is valid and requests full registry data from vanilla.
    SelectKnownPacks(Vec<KnownPack>),
}

/// Decode the supported subset of CLIENTBOUND Configuration packets for 767.
/// Cookie/resource-pack/disconnect/transfer packets are currently
/// explicit UnsupportedPacket errors, never silently skipped or parsed as Play.
pub fn decode_clientbound767(packet_id: i32, payload: &[u8]) -> Result<Clientbound> {
    let mut reader = Reader::new(payload)?;
    let packet = match packet_id {
        0x01 => {
            let id = reader.identifier()?;
            let len = reader.remaining();
            if len > MAX_CLIENT_CUSTOM_PAYLOAD_BYTES {
                return Err(ConfigurationError::LimitExceeded(
                    "client custom payload bytes",
                ));
            }
            Clientbound::CustomPayload {
                id,
                data: reader.take(len)?.to_vec(),
            }
        }
        0x03 => Clientbound::FinishConfiguration,
        0x04 => Clientbound::KeepAlive(i64::from_be_bytes(reader.take(8)?.try_into().unwrap())),
        0x05 => Clientbound::Ping(reader.i32()?),
        0x06 => Clientbound::ResetChat,
        0x07 => {
            let registry = reader.identifier()?;
            let count = reader.count(MAX_REGISTRY_ENTRIES, "registry entries")?;
            // Each entry needs at least one byte for string length and a boolean.
            if count > reader.remaining() / 2 {
                return Err(ConfigurationError::Truncated);
            }
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                let id = reader.identifier()?;
                // Netty readBoolean treats every nonzero byte as true.
                let data = if reader.u8()? != 0 {
                    Some(reader.network_nbt()?)
                } else {
                    None
                };
                entries.push(RegistryEntry { id, data });
            }
            Clientbound::RegistryData { registry, entries }
        }
        0x0c => {
            let count = reader.count(MAX_FEATURE_FLAGS, "feature flags")?;
            if count > reader.remaining() {
                return Err(ConfigurationError::Truncated);
            }
            let mut flags = Vec::with_capacity(count);
            for _ in 0..count {
                flags.push(reader.identifier()?);
            }
            Clientbound::EnabledFeatures(flags)
        }
        0x0d => {
            // ClientboundUpdateTagsPacket and TagNetworkSerialization.NetworkPayload:
            // map<registry, map<tag-name, VarInt-list>>. Keep ordering and unknown ids.
            let count = reader.count(MAX_TAG_REGISTRIES, "tag registries")?;
            if count > reader.remaining() / 2 {
                return Err(ConfigurationError::Truncated);
            }
            let mut registries = Vec::with_capacity(count);
            let mut total_tags = 0;
            let mut total_members = 0;
            for _ in 0..count {
                let registry = reader.identifier()?;
                let count = reader.count(MAX_TAGS - total_tags, "tags")?;
                if count > reader.remaining() / 2 {
                    return Err(ConfigurationError::Truncated);
                }
                total_tags += count;
                let mut tags = Vec::with_capacity(count);
                for _ in 0..count {
                    let id = reader.identifier()?;
                    let count = reader.count(MAX_TAG_MEMBERS - total_members, "tag members")?;
                    if count > reader.remaining() {
                        return Err(ConfigurationError::Truncated);
                    }
                    total_members += count;
                    let mut entries = Vec::with_capacity(count);
                    for _ in 0..count {
                        let value = reader.varint()?;
                        if value < 0 {
                            return Err(ConfigurationError::NegativeLength);
                        }
                        entries.push(value);
                    }
                    tags.push(RegistryTag { id, entries });
                }
                registries.push(RegistryTags { registry, tags });
            }
            Clientbound::UpdateTags(registries)
        }
        0x0e => {
            let count = reader.count(MAX_KNOWN_PACKS, "known packs")?;
            if count > reader.remaining() / 3 {
                return Err(ConfigurationError::Truncated);
            }
            let mut packs = Vec::with_capacity(count);
            for _ in 0..count {
                packs.push(KnownPack {
                    namespace: reader.string()?,
                    id: reader.string()?,
                    version: reader.string()?,
                });
            }
            Clientbound::SelectKnownPacks(packs)
        }
        0x0f => {
            // ClientboundCustomReportDetailsPacket: map<stringUtf8(128),
            // stringUtf8(4096)> with at most 32 entries. Preserve wire order.
            let count = reader.count(MAX_REPORT_DETAILS, "report details")?;
            if count > reader.remaining() / 2 {
                return Err(ConfigurationError::Truncated);
            }
            let mut details = Vec::with_capacity(count);
            for _ in 0..count {
                details.push((reader.string_limited(128)?, reader.string_limited(4096)?));
            }
            Clientbound::CustomReportDetails(details)
        }
        0x10 => {
            // ServerLinks.UntrustedEntry: Either<KnownLinkType, Component>, URL.
            // ByteBufCodecs.either uses true for its left/known-type branch.
            let count = reader.count(MAX_SERVER_LINKS, "server links")?;
            if count > reader.remaining() / 3 {
                return Err(ConfigurationError::Truncated);
            }
            let mut links = Vec::with_capacity(count);
            for _ in 0..count {
                let label = if reader.u8()? != 0 {
                    ServerLinkLabel::Known(reader.varint()?)
                } else {
                    ServerLinkLabel::Custom(reader.network_nbt()?)
                };
                links.push(ServerLink {
                    label,
                    url: reader.string()?,
                });
            }
            Clientbound::ServerLinks(links)
        }
        other => return Err(ConfigurationError::UnsupportedPacket(other)),
    };
    reader.finish()?;
    Ok(packet)
}

/// Encode SERVERBOUND packet id and body, without transport framing. Payload
/// ownership and acknowledgment decisions belong to the caller, not this codec.
pub fn encode_serverbound767(packet: &Serverbound) -> Result<(i32, Vec<u8>)> {
    let mut out = Vec::new();
    let id = match packet {
        Serverbound::CustomPayload { id, data } => {
            validate_identifier(id)?;
            if data.len() > MAX_SERVER_CUSTOM_PAYLOAD_BYTES {
                return Err(ConfigurationError::LimitExceeded(
                    "server custom payload bytes",
                ));
            }
            write_string(&mut out, id)?;
            append(&mut out, data)?;
            0x02
        }
        Serverbound::FinishConfiguration => 0x03,
        Serverbound::KeepAlive(value) => {
            append(&mut out, &value.to_be_bytes())?;
            0x04
        }
        Serverbound::Pong(value) => {
            append(&mut out, &value.to_be_bytes())?;
            0x05
        }
        Serverbound::SelectKnownPacks(packs) => {
            if packs.len() > MAX_KNOWN_PACKS {
                return Err(ConfigurationError::LimitExceeded("known packs"));
            }
            write_varint(&mut out, packs.len() as u32)?;
            for pack in packs {
                write_string(&mut out, &pack.namespace)?;
                write_string(&mut out, &pack.id)?;
                write_string(&mut out, &pack.version)?;
            }
            0x07
        }
    };
    Ok((id, out))
}

/// Tracks only Configuration's terminal boundary. Complete does not mean that
/// registries, mods, rendering, or the Play protocol have been implemented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationPhase {
    Receiving,
    FinishReceived,
    Complete,
}

#[derive(Debug)]
pub struct ConfigurationState {
    phase: ConfigurationPhase,
}

impl Default for ConfigurationState {
    fn default() -> Self {
        Self {
            phase: ConfigurationPhase::Receiving,
        }
    }
}

impl ConfigurationState {
    pub fn phase(&self) -> ConfigurationPhase {
        self.phase
    }

    pub fn received(&mut self, packet: &Clientbound) -> Result<()> {
        if self.phase != ConfigurationPhase::Receiving {
            return Err(ConfigurationError::InvalidTransition);
        }
        if matches!(packet, Clientbound::FinishConfiguration) {
            self.phase = ConfigurationPhase::FinishReceived;
        }
        Ok(())
    }

    /// Call ONLY after the caller has applied and validated all required data
    /// and tasks. This state object cannot decide whether any mod is supported.
    pub fn acknowledge_finish(&mut self) -> Result<Serverbound> {
        if self.phase != ConfigurationPhase::FinishReceived {
            return Err(ConfigurationError::InvalidTransition);
        }
        self.phase = ConfigurationPhase::Complete;
        Ok(Serverbound::FinishConfiguration)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    nbt_nodes: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() > MAX_PACKET_BYTES {
            return Err(ConfigurationError::LimitExceeded("packet bytes"));
        }
        Ok(Self {
            bytes,
            pos: 0,
            nbt_nodes: 0,
        })
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn finish(&self) -> Result<()> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(ConfigurationError::TrailingBytes(self.remaining()))
        }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        if len > self.remaining() {
            return Err(ConfigurationError::Truncated);
        }
        let start = self.pos;
        self.pos += len;
        Ok(&self.bytes[start..self.pos])
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn varint(&mut self) -> Result<i32> {
        let mut value = 0u32;
        for shift in 0..5 {
            let byte = self.u8()?;
            if shift == 4 && byte & 0xf0 != 0 {
                return Err(ConfigurationError::InvalidVarInt);
            }
            value |= ((byte & 0x7f) as u32) << (shift * 7);
            if byte & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err(ConfigurationError::InvalidVarInt)
    }

    fn count(&mut self, max: usize, label: &'static str) -> Result<usize> {
        let value = self.varint()?;
        checked_count(value, max, label)
    }

    fn string(&mut self) -> Result<String> {
        self.string_limited(MAX_STRING_UNITS)
    }

    fn string_limited(&mut self, max_units: usize) -> Result<String> {
        let len = self.count(max_units * 3, "encoded string bytes")?;
        let bytes = self.take(len)?;
        let value = std::str::from_utf8(bytes).map_err(|_| ConfigurationError::InvalidUtf8)?;
        if value.encode_utf16().count() > max_units {
            return Err(ConfigurationError::LimitExceeded("string UTF-16 units"));
        }
        Ok(value.to_owned())
    }

    fn identifier(&mut self) -> Result<String> {
        let value = self.string()?;
        validate_identifier(&value)?;
        Ok(value)
    }

    fn network_nbt(&mut self) -> Result<NetworkNbt> {
        let start = self.pos;
        let tag = self.u8()?;
        if tag == 0 {
            return Err(ConfigurationError::InvalidNbt(
                "End root is not a network value",
            ));
        }
        self.skip_nbt(tag, 0, start)?;
        if self.pos - start > MAX_NBT_BYTES {
            return Err(ConfigurationError::LimitExceeded("NBT bytes"));
        }
        Ok(NetworkNbt(self.bytes[start..self.pos].to_vec()))
    }

    fn nbt_string(&mut self) -> Result<()> {
        self.nbt_string_bytes().map(|_| ())
    }

    fn nbt_string_bytes(&mut self) -> Result<&'a [u8]> {
        let len = u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as usize;
        // Java DataInput modified UTF-8 accepts UTF-16 surrogate code units.
        // Validate its byte framing without decoding/replacing those units.
        let bytes = self.take(len)?;
        let mut i = 0;
        while i < bytes.len() {
            let width = match bytes[i] {
                0x00..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                _ => return Err(ConfigurationError::InvalidNbt("modified UTF-8")),
            };
            if i + width > bytes.len() || bytes[i + 1..i + width].iter().any(|b| b & 0xc0 != 0x80) {
                return Err(ConfigurationError::InvalidNbt("modified UTF-8"));
            }
            i += width;
        }
        Ok(bytes)
    }

    fn skip_nbt(&mut self, tag: u8, depth: usize, start: usize) -> Result<()> {
        if depth > MAX_NBT_DEPTH {
            return Err(ConfigurationError::LimitExceeded("NBT depth"));
        }
        self.nbt_nodes += 1;
        if self.nbt_nodes > MAX_NBT_NODES {
            return Err(ConfigurationError::LimitExceeded("NBT nodes"));
        }
        if self.pos - start > MAX_NBT_BYTES {
            return Err(ConfigurationError::LimitExceeded("NBT bytes"));
        }
        match tag {
            1 => {
                self.take(1)?;
            }
            2 => {
                self.take(2)?;
            }
            3 | 5 => {
                self.take(4)?;
            }
            4 | 6 => {
                self.take(8)?;
            }
            7 | 11 | 12 => {
                let width = match tag {
                    7 => 1,
                    11 => 4,
                    _ => 8,
                };
                let length = checked_count(self.i32()?, MAX_NBT_BYTES / width, "NBT array")?;
                self.take(length * width)?;
            }
            8 => self.nbt_string()?,
            9 => {
                let element = self.u8()?;
                let length = checked_count(self.i32()?, MAX_NBT_NODES, "NBT list")?;
                if element > 12 || (element == 0 && length != 0) {
                    return Err(ConfigurationError::InvalidNbt("list element type"));
                }
                if length > MAX_NBT_NODES - self.nbt_nodes {
                    return Err(ConfigurationError::LimitExceeded("NBT nodes"));
                }
                for _ in 0..length {
                    self.skip_nbt(element, depth + 1, start)?;
                }
            }
            10 => loop {
                let child = self.u8()?;
                if child == 0 {
                    break;
                }
                if child > 12 {
                    return Err(ConfigurationError::InvalidNbt("compound child type"));
                }
                self.nbt_string()?;
                self.skip_nbt(child, depth + 1, start)?;
            },
            _ => return Err(ConfigurationError::InvalidNbt("tag type")),
        }
        Ok(())
    }
}

// Names were framing-validated by nbt_string_bytes. Compare decoded UTF-16
// units, not raw bytes: Java modified UTF-8 may spell the same unit in more
// than one accepted byte form, and supplementary characters use surrogate pairs.
fn modified_utf8_matches(bytes: &[u8], key: &str) -> bool {
    let mut expected = key.encode_utf16();
    let mut offset = 0;
    while offset < bytes.len() {
        let first = bytes[offset];
        let (unit, width) = match first {
            0x00..=0x7f => (u16::from(first), 1),
            0xc0..=0xdf => (
                (u16::from(first & 0x1f) << 6) | u16::from(bytes[offset + 1] & 0x3f),
                2,
            ),
            _ => (
                (u16::from(first & 0x0f) << 12)
                    | (u16::from(bytes[offset + 1] & 0x3f) << 6)
                    | u16::from(bytes[offset + 2] & 0x3f),
                3,
            ),
        };
        if expected.next() != Some(unit) {
            return false;
        }
        offset += width;
    }
    expected.next().is_none()
}

fn checked_count(value: i32, max: usize, label: &'static str) -> Result<usize> {
    if value < 0 {
        return Err(ConfigurationError::NegativeLength);
    }
    if value as usize > max {
        return Err(ConfigurationError::LimitExceeded(label));
    }
    Ok(value as usize)
}

fn validate_identifier(value: &str) -> Result<()> {
    let (namespace, path) = value.split_once(':').unwrap_or(("minecraft", value));
    let valid_namespace = namespace
        .bytes()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c));
    let valid_path = path
        .bytes()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"/_.-".contains(&c));
    // ResourceLocation permits an empty explicit namespace, defaulting to minecraft.
    // Its isValidPath also permits the empty path. Keep original spelling here;
    // callers must normalize and resolve identifiers during registry lookup.
    if valid_namespace && valid_path {
        Ok(())
    } else {
        Err(ConfigurationError::InvalidIdentifier)
    }
}

fn append(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_PACKET_BYTES - out.len() {
        return Err(ConfigurationError::LimitExceeded("packet bytes"));
    }
    out.extend_from_slice(bytes);
    Ok(())
}

fn write_varint(out: &mut Vec<u8>, mut value: u32) -> Result<()> {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        append(out, &[byte])?;
        if value == 0 {
            return Ok(());
        }
    }
}

fn write_string(out: &mut Vec<u8>, value: &str) -> Result<()> {
    if value.len() > MAX_STRING_UNITS * 3 || value.encode_utf16().count() > MAX_STRING_UNITS {
        return Err(ConfigurationError::LimitExceeded("string UTF-16 units"));
    }
    write_varint(out, value.len() as u32)?;
    append(out, value.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named_nbt_field(output: &mut Vec<u8>, tag: u8, name: &[u8], payload: &[u8]) {
        output.push(tag);
        output.extend_from_slice(&(name.len() as u16).to_be_bytes());
        output.extend_from_slice(name);
        output.extend_from_slice(payload);
    }

    #[test]
    fn network_nbt_prefix_preserves_tail_and_separates_packet_and_value_limits() {
        let primitive = [3, 0, 0, 0, 42, 0xff, 0xff];
        let (value, consumed) = NetworkNbt::read_prefix(&primitive).unwrap();
        assert_eq!(consumed, 5);
        assert_eq!(value.as_bytes(), &primitive[..5]);
        assert_eq!(&primitive[consumed..], &[0xff, 0xff]);

        // A small NBT value may precede a packet tail larger than the NBT limit.
        let mut packet = vec![10, 0];
        packet.resize(MAX_NBT_BYTES + 32, 0);
        let (value, consumed) = NetworkNbt::read_prefix(&packet).unwrap();
        assert_eq!(value.as_bytes(), &[10, 0]);
        assert_eq!(consumed, 2);
        assert!(NetworkNbt::from_bytes(&packet).is_err());
        assert_eq!(
            NetworkNbt::read_prefix(&vec![0; MAX_PACKET_BYTES + 1]),
            Err(ConfigurationError::LimitExceeded("packet bytes"))
        );

        let mut oversized_value = vec![7];
        oversized_value.extend_from_slice(&((MAX_NBT_BYTES - 4) as i32).to_be_bytes());
        oversized_value.resize(MAX_NBT_BYTES + 1, 0);
        assert_eq!(
            NetworkNbt::read_prefix(&oversized_value),
            Err(ConfigurationError::LimitExceeded("NBT bytes"))
        );
        assert!(matches!(
            NetworkNbt::read_prefix(&[0]),
            Err(ConfigurationError::InvalidNbt(_))
        ));
        assert_eq!(
            NetworkNbt::read_prefix(&[]),
            Err(ConfigurationError::Truncated)
        );
        assert_eq!(
            NetworkNbt::read_prefix(&[3, 0, 0, 0]),
            Err(ConfigurationError::Truncated)
        );
    }

    #[test]
    fn dimension_compound_getters_require_exact_scalar_types() {
        for flag in [0, 1, 2, 0xff] {
            let mut bytes = vec![10];
            named_nbt_field(&mut bytes, 3, b"min_y", &(-64_i32).to_be_bytes());
            named_nbt_field(&mut bytes, 3, b"height", &384_i32.to_be_bytes());
            named_nbt_field(&mut bytes, 1, b"has_skylight", &[flag]);
            bytes.push(0);
            let value = NetworkNbt::from_bytes(&bytes).unwrap();
            assert_eq!(value.compound_i32("min_y").unwrap(), -64);
            assert_eq!(value.compound_i32("height").unwrap(), 384);
            assert_eq!(value.compound_bool("has_skylight").unwrap(), flag != 0);
            assert_eq!(value.as_bytes(), bytes);
            assert_eq!(
                value.compound_i32("has_skylight"),
                Err(ConfigurationError::InvalidNbt(
                    "compound key has wrong type"
                ))
            );
            assert_eq!(
                value.compound_bool("height"),
                Err(ConfigurationError::InvalidNbt(
                    "compound key has wrong type"
                ))
            );
        }
        assert_eq!(
            NetworkNbt::from_bytes(&[3, 0, 0, 0, 42])
                .unwrap()
                .compound_i32("height"),
            Err(ConfigurationError::InvalidNbt("root is not a compound"))
        );
    }

    #[test]
    fn compound_getters_reject_missing_duplicate_and_nested_only_keys() {
        let mut nested = vec![];
        named_nbt_field(&mut nested, 3, b"height", &384_i32.to_be_bytes());
        nested.push(0);
        let mut bytes = vec![10];
        named_nbt_field(&mut bytes, 10, b"nested", &nested);
        bytes.push(0);
        assert_eq!(
            NetworkNbt::from_bytes(&bytes)
                .unwrap()
                .compound_i32("height"),
            Err(ConfigurationError::InvalidNbt("missing compound key"))
        );
        bytes.pop();
        named_nbt_field(&mut bytes, 3, b"height", &384_i32.to_be_bytes());
        named_nbt_field(&mut bytes, 3, b"height", &512_i32.to_be_bytes());
        bytes.push(0);
        assert_eq!(
            NetworkNbt::from_bytes(&bytes)
                .unwrap()
                .compound_i32("height"),
            Err(ConfigurationError::InvalidNbt("duplicate compound key"))
        );
    }

    #[test]
    fn compound_keys_compare_java_utf16_units_without_lossy_decoding() {
        let mut bytes = vec![10];
        // Java modified UTF-8 accepts this alternative spelling of 'h'.
        named_nbt_field(&mut bytes, 3, b"\xc1\xa8eight", &384_i32.to_be_bytes());
        named_nbt_field(
            &mut bytes,
            3,
            b"\xed\xa0\xbd\xed\xb8\x80",
            &42_i32.to_be_bytes(),
        );
        bytes.push(0);
        let value = NetworkNbt::from_bytes(&bytes).unwrap();
        assert_eq!(value.compound_i32("height").unwrap(), 384);
        assert_eq!(value.compound_i32("\u{1f600}").unwrap(), 42);
        bytes.pop();
        named_nbt_field(&mut bytes, 3, b"height", &512_i32.to_be_bytes());
        bytes.push(0);
        assert_eq!(
            NetworkNbt::from_bytes(&bytes)
                .unwrap()
                .compound_i32("height"),
            Err(ConfigurationError::InvalidNbt("duplicate compound key"))
        );
    }

    #[test]
    fn source_schema_registry_fixture_preserves_unknown_id_and_unnamed_nbt() {
        // PackedRegistryEntry: ResourceLocation, Boolean, ByteBufCodecs.TAG.
        // Fixed fixture has no 00 00 root-name prefix after the compound type.
        let fixture = b"\x0bmod:widgets\x02\x09mod:alpha\x01\x0a\x03\x00\x05value\x00\x00\x00\x2a\x00\x08mod:beta\x00";
        let decoded = decode_clientbound767(7, fixture).unwrap();
        match decoded {
            Clientbound::RegistryData { registry, entries } => {
                assert_eq!(registry, "mod:widgets");
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].id, "mod:alpha");
                assert_eq!(
                    entries[0].data.as_ref().unwrap().as_bytes(),
                    b"\x0a\x03\x00\x05value\x00\x00\x00\x2a\x00"
                );
                assert_eq!(
                    entries[1],
                    RegistryEntry {
                        id: "mod:beta".into(),
                        data: None
                    }
                );
            }
            _ => panic!("registry expected"),
        }
    }

    #[test]
    fn network_nbt_accepts_noncompound_and_rejects_named_root_or_end() {
        assert_eq!(
            NetworkNbt::from_bytes(&[3, 0, 0, 0, 42])
                .unwrap()
                .as_bytes(),
            &[3, 0, 0, 0, 42]
        );
        assert!(NetworkNbt::from_bytes(&[0]).is_err());
        assert!(matches!(
            NetworkNbt::from_bytes(&[10, 0, 0, 0]),
            Err(ConfigurationError::TrailingBytes(2))
        ));
    }

    #[test]
    fn signed_fixed_width_ping_and_keepalive_are_not_varints() {
        assert_eq!(
            decode_clientbound767(4, &(-2i64).to_be_bytes()).unwrap(),
            Clientbound::KeepAlive(-2)
        );
        assert_eq!(
            decode_clientbound767(5, &i32::MIN.to_be_bytes()).unwrap(),
            Clientbound::Ping(i32::MIN)
        );
        assert_eq!(
            encode_serverbound767(&Serverbound::KeepAlive(-2)).unwrap(),
            (4, (-2i64).to_be_bytes().to_vec())
        );
        assert_eq!(
            encode_serverbound767(&Serverbound::Pong(i32::MIN)).unwrap(),
            (5, i32::MIN.to_be_bytes().to_vec())
        );
    }

    #[test]
    fn known_pack_source_schema_fixture_and_empty_selection() {
        let bytes = b"\x01\x09minecraft\x04core\x061.21.1";
        let packs = vec![KnownPack {
            namespace: "minecraft".into(),
            id: "core".into(),
            version: "1.21.1".into(),
        }];
        assert_eq!(
            decode_clientbound767(14, bytes).unwrap(),
            Clientbound::SelectKnownPacks(packs.clone())
        );
        assert_eq!(
            encode_serverbound767(&Serverbound::SelectKnownPacks(packs)).unwrap(),
            (7, bytes.to_vec())
        );
        assert_eq!(
            encode_serverbound767(&Serverbound::SelectKnownPacks(vec![])).unwrap(),
            (7, vec![0])
        );
    }

    #[test]
    fn feature_flags_and_unknown_payloads_keep_names_and_bytes() {
        assert_eq!(
            decode_clientbound767(12, b"\x02\x11minecraft:vanilla\x06mod:on").unwrap(),
            Clientbound::EnabledFeatures(vec!["minecraft:vanilla".into(), "mod:on".into()])
        );
        let packet = decode_clientbound767(1, b"\x0ccreate:thing\x00\xff\x80").unwrap();
        assert_eq!(
            packet,
            Clientbound::CustomPayload {
                id: "create:thing".into(),
                data: vec![0, 255, 128]
            }
        );
        assert_eq!(
            encode_serverbound767(&Serverbound::CustomPayload {
                id: "create:thing".into(),
                data: vec![0, 255, 128]
            })
            .unwrap(),
            (2, b"\x0ccreate:thing\x00\xff\x80".to_vec())
        );
    }

    #[test]
    fn unsupported_packets_and_trailing_data_are_errors() {
        assert_eq!(
            decode_clientbound767(0, &[0]),
            Err(ConfigurationError::UnsupportedPacket(0))
        );
        assert_eq!(
            decode_clientbound767(3, &[0]),
            Err(ConfigurationError::TrailingBytes(1))
        );
        assert_eq!(
            decode_clientbound767(3, &[]).unwrap(),
            Clientbound::FinishConfiguration
        );
        assert_eq!(
            decode_clientbound767(6, &[]).unwrap(),
            Clientbound::ResetChat
        );
    }

    #[test]
    fn truncated_and_invalid_lengths_do_not_panic_or_allocate_from_counts() {
        assert_eq!(
            decode_clientbound767(14, &[255, 255, 255, 255, 15]),
            Err(ConfigurationError::NegativeLength)
        );
        assert_eq!(
            decode_clientbound767(14, &[255, 255, 255, 255, 127]),
            Err(ConfigurationError::InvalidVarInt)
        );
        assert_eq!(
            decode_clientbound767(14, &[1]),
            Err(ConfigurationError::Truncated)
        );
        assert_eq!(
            decode_clientbound767(1, &[255]),
            Err(ConfigurationError::Truncated)
        );
        assert_eq!(
            decode_clientbound767(1, &[1, 255]),
            Err(ConfigurationError::InvalidUtf8)
        );
        assert_eq!(
            decode_clientbound767(12, &[1, 3, b'A', b':', b'b']),
            Err(ConfigurationError::InvalidIdentifier)
        );
        assert!(matches!(
            decode_clientbound767(14, &[129, 8]),
            Err(ConfigurationError::LimitExceeded("known packs"))
        ));
    }

    #[test]
    fn every_truncation_of_a_registry_fixture_fails() {
        let fixture = b"\x03a:b\x01\x03a:c\x01\x0a\x08\x00\x01n\x00\x02ok\x00";
        assert!(decode_clientbound767(7, fixture).is_ok());
        for end in 0..fixture.len() {
            assert!(
                decode_clientbound767(7, &fixture[..end]).is_err(),
                "accepted prefix {}",
                end
            );
        }
    }

    #[test]
    fn nbt_depth_nodes_arrays_and_types_are_bounded() {
        let mut deep = vec![10];
        for _ in 0..=MAX_NBT_DEPTH {
            deep.extend_from_slice(&[10, 0, 0]);
        }
        deep.resize(deep.len() + MAX_NBT_DEPTH + 2, 0);
        assert!(matches!(
            NetworkNbt::from_bytes(&deep),
            Err(ConfigurationError::LimitExceeded("NBT depth"))
        ));
        assert_eq!(
            NetworkNbt::from_bytes(&[7, 255, 255, 255, 255]),
            Err(ConfigurationError::NegativeLength)
        );
        assert!(matches!(
            NetworkNbt::from_bytes(&[12, 127, 255, 255, 255]),
            Err(ConfigurationError::LimitExceeded("NBT array"))
        ));
        assert!(NetworkNbt::from_bytes(&[9, 0, 0, 0, 0, 1]).is_err());
        assert!(NetworkNbt::from_bytes(&[13]).is_err());
        let mut list = vec![9, 1];
        list.extend_from_slice(&(MAX_NBT_NODES as i32).to_be_bytes());
        assert!(matches!(
            NetworkNbt::from_bytes(&list),
            Err(ConfigurationError::LimitExceeded("NBT nodes"))
        ));
    }

    #[test]
    fn nbt_modified_utf8_is_preserved_without_lossy_decoding() {
        // Java modified UTF-8 encodes NUL as C0 80 and supplementary characters
        // as two three-byte UTF-16 surrogate code units.
        let bytes = [8, 0, 8, 0xc0, 0x80, 0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80];
        assert_eq!(NetworkNbt::from_bytes(&bytes).unwrap().as_bytes(), &bytes);
        assert!(NetworkNbt::from_bytes(&[8, 0, 2, 0xc0, 0x00]).is_err());
    }

    #[test]
    fn payload_packet_and_string_resource_limits() {
        assert!(matches!(
            Reader::new(&vec![0; MAX_PACKET_BYTES + 1]),
            Err(ConfigurationError::LimitExceeded("packet bytes"))
        ));
        assert!(matches!(
            NetworkNbt::from_bytes(&vec![0; MAX_NBT_BYTES + 1]),
            Err(ConfigurationError::LimitExceeded("NBT bytes"))
        ));
        let packet = Serverbound::CustomPayload {
            id: "mod:data".into(),
            data: vec![0; MAX_SERVER_CUSTOM_PAYLOAD_BYTES + 1],
        };
        assert!(matches!(
            encode_serverbound767(&packet),
            Err(ConfigurationError::LimitExceeded(
                "server custom payload bytes"
            ))
        ));
        let mut oversized = b"\x08mod:data".to_vec();
        oversized.resize(oversized.len() + MAX_CLIENT_CUSTOM_PAYLOAD_BYTES + 1, 0);
        assert!(matches!(
            decode_clientbound767(1, &oversized),
            Err(ConfigurationError::LimitExceeded(
                "client custom payload bytes"
            ))
        ));
        let mut out = vec![];
        assert!(write_string(&mut out, &"x".repeat(MAX_STRING_UNITS + 1)).is_err());
        // Supplementary Unicode counts as TWO Java UTF-16 units, not one char.
        assert!(write_string(&mut out, &"\u{1f600}".repeat(16_384)).is_err());
    }

    #[test]
    fn terminal_state_requires_finish_and_explicit_acknowledgment() {
        let mut state = ConfigurationState::default();
        assert!(state.acknowledge_finish().is_err());
        state
            .received(&Clientbound::CustomPayload {
                id: "neoforge:query".into(),
                data: vec![1],
            })
            .unwrap();
        assert_eq!(state.phase(), ConfigurationPhase::Receiving);
        state.received(&Clientbound::FinishConfiguration).unwrap();
        assert_eq!(state.phase(), ConfigurationPhase::FinishReceived);
        assert!(state.received(&Clientbound::KeepAlive(1)).is_err());
        assert_eq!(
            state.acknowledge_finish().unwrap(),
            Serverbound::FinishConfiguration
        );
        assert_eq!(state.phase(), ConfigurationPhase::Complete);
        assert!(state.acknowledge_finish().is_err());
    }

    #[test]
    fn source_schema_tags_preserve_numeric_registry_references() {
        let fixture = b"\x01\x03a:b\x01\x03a:c\x03\x00\x7f\x80\x01";
        assert_eq!(
            decode_clientbound767(13, fixture).unwrap(),
            Clientbound::UpdateTags(vec![RegistryTags {
                registry: "a:b".into(),
                tags: vec![RegistryTag {
                    id: "a:c".into(),
                    entries: vec![0, 127, 128]
                }],
            }])
        );
        for end in 0..fixture.len() {
            assert!(decode_clientbound767(13, &fixture[..end]).is_err());
        }
        assert_eq!(
            decode_clientbound767(13, &[0]).unwrap(),
            Clientbound::UpdateTags(vec![])
        );
    }

    #[test]
    fn report_detail_source_limits_and_utf16_lengths_are_enforced() {
        assert_eq!(
            decode_clientbound767(15, b"\x01\x04name\x05value").unwrap(),
            Clientbound::CustomReportDetails(vec![("name".into(), "value".into())])
        );
        assert_eq!(
            decode_clientbound767(15, &[0]).unwrap(),
            Clientbound::CustomReportDetails(vec![])
        );
        assert_eq!(
            decode_clientbound767(15, &[33]),
            Err(ConfigurationError::LimitExceeded("report details"))
        );
        for (key, value) in [
            ("x".repeat(129), String::new()),
            (String::new(), "x".repeat(4097)),
            ("\u{1f600}".repeat(65), String::new()),
        ] {
            let mut bytes = vec![1];
            write_string(&mut bytes, &key).unwrap();
            write_string(&mut bytes, &value).unwrap();
            assert!(matches!(
                decode_clientbound767(15, &bytes),
                Err(ConfigurationError::LimitExceeded(_))
            ));
        }
    }

    #[test]
    fn server_links_preserve_labels_and_untrusted_urls_without_interpretation() {
        // Fixed source-schema fixture: website link followed by TAG_String label.
        let fixture = b"\x02\x01\x06\x0chttps://a.co\x00\x08\x00\x04Help\x0cx:untrusted!";
        assert_eq!(
            decode_clientbound767(16, fixture).unwrap(),
            Clientbound::ServerLinks(vec![
                ServerLink {
                    label: ServerLinkLabel::Known(6),
                    url: "https://a.co".into()
                },
                ServerLink {
                    label: ServerLinkLabel::Custom(
                        NetworkNbt::from_bytes(b"\x08\x00\x04Help").unwrap()
                    ),
                    url: "x:untrusted!".into()
                },
            ])
        );
        for end in 0..fixture.len() {
            assert!(decode_clientbound767(16, &fixture[..end]).is_err());
        }
        assert_eq!(
            decode_clientbound767(16, &[0]).unwrap(),
            Clientbound::ServerLinks(vec![])
        );
        assert_eq!(
            decode_clientbound767(16, &[1, 1, 127, 0]).unwrap(),
            Clientbound::ServerLinks(vec![ServerLink {
                label: ServerLinkLabel::Known(127),
                url: String::new()
            },])
        );
        assert!(matches!(
            decode_clientbound767(16, &[129, 8]),
            Err(ConfigurationError::LimitExceeded("server links"))
        ));
    }

    #[test]
    fn resource_location_validation_matches_source_without_normalizing() {
        for id in [
            "",
            ":",
            "minecraft:",
            "stone",
            ":stone",
            "mod:path/name_0.-",
        ] {
            let mut fixture = vec![];
            write_string(&mut fixture, id).unwrap();
            assert_eq!(
                decode_clientbound767(1, &fixture).unwrap(),
                Clientbound::CustomPayload {
                    id: id.into(),
                    data: vec![]
                }
            );
        }
        for id in [
            "Mod:block",
            "minecraft:Upper",
            "mod:a:b",
            "mod:space name",
            "mod:\u{e9}",
        ] {
            assert_eq!(
                validate_identifier(id),
                Err(ConfigurationError::InvalidIdentifier)
            );
        }
    }
}
