//! Bounded Minecraft 1.21.1 Play codecs for a headless conformance session.
//!
//! Packet IDs are checked against vanilla 1.21.1's generated packets report;
//! field order follows ClientboundLoginPacket, CommonPlayerSpawnInfo,
//! ClientboundPlayerPositionPacket, ClientboundLevelChunkPacketData and
//! ClientboundLightUpdatePacketData. This does not enable protocol 767 in the
//! graphical client, implement NeoForge channels, or replace unknown IDs.
use super::chunk767::{decode_sections767, ChunkDecodeError, ChunkSection767};
use super::configuration::{ConfigurationError, NetworkNbt, RegistryEntry};
use std::collections::BTreeMap;
use std::convert::TryInto;
use std::fmt;

const MAX_PACKET: usize = 8 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 2 * 1024 * 1024;
const MAX_BLOCK_ENTITIES: usize = 65_536;
pub const MAX_RETAINED_CHUNKS: usize = 256;
pub const MAX_RETAINED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum PlayError {
    Invalid(&'static str),
    Unsupported(&'static str),
    Truncated,
    Limit(&'static str),
    Trailing(usize),
    Configuration(ConfigurationError),
    Chunk(ChunkDecodeError),
}
impl fmt::Display for PlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Minecraft 1.21.1 Play: {:?}", self)
    }
}
impl std::error::Error for PlayError {}
impl From<ConfigurationError> for PlayError {
    fn from(e: ConfigurationError) -> Self {
        Self::Configuration(e)
    }
}
impl From<ChunkDecodeError> for PlayError {
    fn from(e: ChunkDecodeError) -> Self {
        Self::Chunk(e)
    }
}
pub type Result<T> = std::result::Result<T, PlayError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dimension {
    pub registry_id: u32,
    pub id: String,
    pub min_y: i32,
    pub height: u32,
    pub has_skylight: bool,
}
impl Dimension {
    pub fn from_registry(id: u32, entries: &[RegistryEntry]) -> Result<Self> {
        let entry = entries
            .get(id as usize)
            .ok_or(PlayError::Invalid("dimension registry index"))?;
        let nbt = entry
            .data
            .as_ref()
            .ok_or(PlayError::Invalid("dimension definition omitted"))?;
        let min_y = nbt.compound_i32("min_y")?;
        let height = nbt.compound_i32("height")?;
        let has_skylight = nbt.compound_bool("has_skylight")?;
        // DimensionType's actual packed-position limits, independent of view distance.
        if min_y < -2032
            || height < 16
            || height > 4064
            || min_y % 16 != 0
            || height % 16 != 0
            || min_y.checked_add(height).map_or(true, |top| top > 2032)
        {
            return Err(PlayError::Invalid("dimension bounds"));
        }
        Ok(Self {
            registry_id: id,
            id: entry.id.clone(),
            min_y,
            height: height as u32,
            has_skylight,
        })
    }
    pub fn light_section_count(&self) -> usize {
        self.height as usize / 16 + 2
    }
    pub fn min_light_section(&self) -> i32 {
        self.min_y / 16 - 1
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldContext {
    pub dimension: Dimension,
    pub block_state_count: u32,
    pub biome_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinGame {
    pub entity_id: i32,
    pub hardcore: bool,
    pub levels: Vec<String>,
    pub max_players: i32,
    pub view_distance: i32,
    pub simulation_distance: i32,
    pub reduced_debug: bool,
    pub show_death_screen: bool,
    pub limited_crafting: bool,
    pub dimension_type: u32,
    pub dimension_name: String,
    pub hashed_seed: i64,
    pub game_mode: u8,
    pub previous_game_mode: i8,
    pub debug: bool,
    pub flat: bool,
    pub last_death: Option<(String, i64)>,
    pub portal_cooldown: i32,
    pub enforces_secure_chat: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerPosition {
    pub position: [f64; 3],
    /// yaw then pitch, in degrees.
    pub rotation: [f32; 2],
    pub relative_flags: u8,
    pub teleport_id: i32,
}
impl PlayerPosition {
    pub fn resolve(&self, previous: [f64; 3], rotation: [f32; 2]) -> Result<([f64; 3], [f32; 2])> {
        let mut position = self.position;
        let mut resolved_rotation = self.rotation;
        for i in 0..3 {
            if self.relative_flags & (1 << i) != 0 {
                position[i] += previous[i];
            }
        }
        for i in 0..2 {
            if self.relative_flags & (1 << (i + 3)) != 0 {
                resolved_rotation[i] += rotation[i];
            }
        }
        if position
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 30_000_000.0)
            || resolved_rotation.iter().any(|v| !v.is_finite())
        {
            return Err(PlayError::Invalid("resolved player position"));
        }
        Ok((position, resolved_rotation))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockEntity {
    pub packed_xz: u8,
    pub y: i16,
    pub type_id: u32,
    pub nbt: Option<NetworkNbt>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightData {
    pub min_section_y: i32,
    pub section_count: usize,
    pub sky_mask: Vec<u64>,
    pub block_mask: Vec<u64>,
    pub empty_sky_mask: Vec<u64>,
    pub empty_block_mask: Vec<u64>,
    /// Arrays follow ascending set bits, including the lower/upper boundary sections.
    pub sky_arrays: Vec<Vec<u8>>,
    pub block_arrays: Vec<Vec<u8>>,
}
impl LightData {
    pub fn array_count(&self) -> usize {
        self.sky_arrays.len() + self.block_arrays.len()
    }
    pub fn byte_count(&self) -> usize {
        self.array_count() * 2048
    }
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + [
                &self.sky_mask,
                &self.block_mask,
                &self.empty_sky_mask,
                &self.empty_block_mask,
            ]
            .iter()
            .map(|v| v.capacity() * 8)
            .sum::<usize>()
            + [&self.sky_arrays, &self.block_arrays]
                .iter()
                .map(|v| {
                    v.capacity() * std::mem::size_of::<Vec<u8>>()
                        + v.iter().map(|a| a.capacity()).sum::<usize>()
                })
                .sum::<usize>()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkWithLight {
    pub x: i32,
    pub z: i32,
    pub heightmaps: NetworkNbt,
    pub sections: Vec<ChunkSection767>,
    pub block_entities: Vec<BlockEntity>,
    pub light: LightData,
}
impl ChunkWithLight {
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.heightmaps.as_bytes().len()
            + self.sections.capacity() * std::mem::size_of::<ChunkSection767>()
            + self
                .sections
                .iter()
                .map(|s| (s.block_states.capacity() + s.biomes.capacity()) * 4)
                .sum::<usize>()
            + self.block_entities.capacity() * std::mem::size_of::<BlockEntity>()
            + self
                .block_entities
                .iter()
                .filter_map(|e| e.nbt.as_ref())
                .map(|n| n.as_bytes().len())
                .sum::<usize>()
            + self.light.retained_bytes()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerAbilities {
    pub invulnerable: bool,
    pub flying: bool,
    pub may_fly: bool,
    pub instant_build: bool,
    pub flying_speed: f32,
    pub walking_speed: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockUpdate {
    pub position: [i32; 3],
    pub state_id: u32,
}

/// Numeric item registry ID, not a block-state ID. Only an empty component
/// patch is decoded here; defaults belong to the negotiated item definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlainItemStack {
    pub item_id: u32,
    pub count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryContent {
    pub container_id: u8,
    pub state_id: u32,
    pub slots: Vec<Option<PlainItemStack>>,
    pub carried: Option<PlainItemStack>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventorySlot {
    /// -1 is the carried stack; -2 uses player-inventory indices directly.
    pub container_id: i8,
    pub state_id: u32,
    pub slot: i16,
    pub item: Option<PlainItemStack>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Clientbound {
    BundleDelimiter,
    Join(JoinGame),
    Position(PlayerPosition),
    KeepAlive(i64),
    Ping(i32),
    Chunk(ChunkWithLight),
    Light {
        x: i32,
        z: i32,
        data: LightData,
    },
    Unload {
        x: i32,
        z: i32,
    },
    BatchStart,
    BatchFinished(u32),
    Abilities(PlayerAbilities),
    BlockUpdates(Vec<BlockUpdate>),
    BlockActionAcknowledged(i32),
    InventoryContent(InventoryContent),
    InventorySlot(InventorySlot),
    SelectedSlot(u8),
    /// Component payloads are not length-prefixed. Preserve the entire bounded
    /// packet instead of guessing boundaries or silently clearing inventory.
    UnsupportedInventory {
        packet_id: i32,
        data: Vec<u8>,
    },
    CustomPayload {
        id: String,
        data: Vec<u8>,
    },
    Disconnect(NetworkNbt),
    StartConfiguration,
    /// Explicitly observed but not implemented; never counted as translated behavior.
    Unhandled {
        packet_id: i32,
        data: Vec<u8>,
    },
}

pub fn decode_clientbound767(
    id: i32,
    payload: &[u8],
    context: Option<&WorldContext>,
) -> Result<Clientbound> {
    let mut r = Reader::new(payload)?;
    if id == 0x13 || id == 0x15 {
        return match read_inventory(id, &mut r) {
            Err(PlayError::Unsupported("item components")) => {
                Ok(Clientbound::UnsupportedInventory {
                    packet_id: id,
                    data: payload.to_vec(),
                })
            }
            result => result,
        };
    }
    let packet = match id {
        0x00 => Clientbound::BundleDelimiter,
        0x05 => Clientbound::BlockActionAcknowledged(r.nonnegative()? as i32),
        0x09 => {
            let ctx = context.ok_or(PlayError::Invalid("block update before dimension"))?;
            let position = unpack_block_position(r.i64()?);
            let state_id = r.nonnegative()?;
            validate_block_update(position, state_id, ctx)?;
            Clientbound::BlockUpdates(vec![BlockUpdate { position, state_id }])
        }
        0x0c => Clientbound::BatchFinished(r.nonnegative()?),
        0x0d => Clientbound::BatchStart,
        0x19 => {
            let id = r.identifier()?;
            if r.remaining() > 1_048_576 {
                return Err(PlayError::Limit("custom payload"));
            }
            let data = r.take(r.remaining())?.to_vec();
            Clientbound::CustomPayload { id, data }
        }
        0x1d => Clientbound::Disconnect(r.nbt(false)?.unwrap()),
        0x21 => {
            let packed = r.i64()? as u64;
            Clientbound::Unload {
                x: packed as u32 as i32,
                z: (packed >> 32) as u32 as i32,
            }
        }
        0x26 => Clientbound::KeepAlive(r.i64()?),
        0x27 => {
            let ctx = context.ok_or(PlayError::Invalid("chunk before dimension negotiation"))?;
            let x = r.i32()?;
            let z = r.i32()?;
            let heightmaps = r.nbt(false)?.unwrap();
            if heightmaps.as_bytes()[0] != 10 {
                return Err(PlayError::Invalid("heightmap is not compound"));
            }
            let size = r.count(MAX_CHUNK_BYTES, "section data bytes")?;
            let sections = decode_sections767(
                r.take(size)?,
                ctx.dimension.min_y,
                ctx.dimension.height,
                ctx.block_state_count,
                ctx.biome_count,
            )?;
            let count = r.count(MAX_BLOCK_ENTITIES, "block entities")?;
            if count > r.remaining() / 5 {
                return Err(PlayError::Truncated);
            }
            let mut block_entities = Vec::with_capacity(count);
            for _ in 0..count {
                let packed_xz = r.u8()?;
                let y = r.i16()?;
                let type_id = r.nonnegative()?;
                let nbt = r.nbt(true)?;
                if nbt.as_ref().map_or(false, |n| n.as_bytes()[0] != 10) {
                    return Err(PlayError::Invalid("block entity NBT is not compound"));
                }
                block_entities.push(BlockEntity {
                    packed_xz,
                    y,
                    type_id,
                    nbt,
                });
            }
            let light = read_light(&mut r, &ctx.dimension)?;
            Clientbound::Chunk(ChunkWithLight {
                x,
                z,
                heightmaps,
                sections,
                block_entities,
                light,
            })
        }
        0x2a => {
            let ctx = context.ok_or(PlayError::Invalid("light before dimension negotiation"))?;
            let x = r.varint()?;
            let z = r.varint()?;
            Clientbound::Light {
                x,
                z,
                data: read_light(&mut r, &ctx.dimension)?,
            }
        }
        0x2b => Clientbound::Join(read_join(&mut r)?),
        0x35 => Clientbound::Ping(r.i32()?),
        0x38 => {
            let flags = r.u8()?;
            let flying_speed = r.f32()?;
            let walking_speed = r.f32()?;
            if flags & !15 != 0 || flying_speed < 0.0 || walking_speed < 0.0 {
                return Err(PlayError::Invalid("player abilities"));
            }
            Clientbound::Abilities(PlayerAbilities {
                invulnerable: flags & 1 != 0,
                flying: flags & 2 != 0,
                may_fly: flags & 4 != 0,
                instant_build: flags & 8 != 0,
                flying_speed,
                walking_speed,
            })
        }
        0x40 => {
            let position = [r.f64()?, r.f64()?, r.f64()?];
            let rotation = [r.f32()?, r.f32()?];
            let relative_flags = r.u8()?;
            let teleport_id = r.varint()?;
            if relative_flags & !31 != 0 {
                return Err(PlayError::Invalid("unknown relative position flags"));
            }
            Clientbound::Position(PlayerPosition {
                position,
                rotation,
                relative_flags,
                teleport_id,
            })
        }
        0x47 => return Err(PlayError::Invalid("respawn not yet implemented")),
        0x49 => {
            let ctx = context.ok_or(PlayError::Invalid("section update before dimension"))?;
            let packed = r.i64()?;
            let section = [
                (packed >> 42) as i32,
                (packed << 44 >> 44) as i32,
                (packed << 22 >> 42) as i32,
            ];
            let count = r.count(4096, "section block updates")?;
            let mut updates = Vec::with_capacity(count);
            let mut seen = [false; 4096];
            for _ in 0..count {
                let entry = r.varlong()?;
                let local = (entry & 4095) as usize;
                let state = entry >> 12;
                if state > u32::MAX as u64 || seen[local] {
                    return Err(PlayError::Invalid(
                        "section update identity or duplicate position",
                    ));
                }
                seen[local] = true;
                let position = [
                    section[0] * 16 + ((local >> 8) & 15) as i32,
                    section[1] * 16 + (local & 15) as i32,
                    section[2] * 16 + ((local >> 4) & 15) as i32,
                ];
                validate_block_update(position, state as u32, ctx)?;
                updates.push(BlockUpdate {
                    position,
                    state_id: state as u32,
                });
            }
            Clientbound::BlockUpdates(updates)
        }
        0x53 => {
            let slot = r.u8()?;
            if slot > 8 {
                return Err(PlayError::Invalid("selected hotbar slot"));
            }
            Clientbound::SelectedSlot(slot)
        }
        0x69 => Clientbound::StartConfiguration,
        id if id >= 0 => Clientbound::Unhandled {
            packet_id: id,
            data: r.take(r.remaining())?.to_vec(),
        },
        _ => return Err(PlayError::Invalid("negative packet id")),
    };
    r.finish()?;
    Ok(packet)
}

// ClientboundContainerSetContentPacket/SetSlotPacket and ItemStack's
// OPTIONAL_STREAM_CODEC in 1.21.1. Registry holder IDs have no +1 offset here.
fn read_inventory(id: i32, r: &mut Reader<'_>) -> Result<Clientbound> {
    let container = r.u8()?;
    let state_id = r.nonnegative()?;
    let packet = if id == 0x13 {
        let count = r.count(1024, "inventory slots")?;
        let mut slots = Vec::with_capacity(count);
        for _ in 0..count {
            slots.push(read_plain_stack(r)?);
        }
        Clientbound::InventoryContent(InventoryContent {
            container_id: container,
            state_id,
            slots,
            carried: read_plain_stack(r)?,
        })
    } else {
        Clientbound::InventorySlot(InventorySlot {
            container_id: container as i8,
            state_id,
            slot: r.i16()?,
            item: read_plain_stack(r)?,
        })
    };
    r.finish()?;
    Ok(packet)
}

fn read_plain_stack(r: &mut Reader<'_>) -> Result<Option<PlainItemStack>> {
    let count = r.nonnegative()?;
    if count == 0 {
        return Ok(None);
    }
    let item_id = r.nonnegative()?;
    let added = r.count(256, "item component additions")?;
    let removed = r.count(256, "item component removals")?;
    if added != 0 || removed != 0 {
        return Err(PlayError::Unsupported("item components"));
    }
    Ok(Some(PlainItemStack { item_id, count }))
}

fn unpack_block_position(value: i64) -> [i32; 3] {
    [
        (value >> 38) as i32,
        (value << 52 >> 52) as i32,
        (value << 26 >> 38) as i32,
    ]
}

fn pack_block_position(position: [i32; 3]) -> Result<i64> {
    let [x, y, z] = position;
    if !(-33_554_432..=33_554_431).contains(&x)
        || !(-2048..=2047).contains(&y)
        || !(-33_554_432..=33_554_431).contains(&z)
    {
        return Err(PlayError::Invalid("packed block position bounds"));
    }
    Ok(
        (((x as u64 & 0x3ffffff) << 38) | ((z as u64 & 0x3ffffff) << 12) | (y as u64 & 0xfff))
            as i64,
    )
}

fn validate_block_update(position: [i32; 3], state: u32, ctx: &WorldContext) -> Result<()> {
    if state >= ctx.block_state_count
        || position[1] < ctx.dimension.min_y
        || position[1] >= ctx.dimension.min_y + ctx.dimension.height as i32
    {
        Err(PlayError::Invalid(
            "block update outside registry or dimension",
        ))
    } else {
        Ok(())
    }
}

fn read_join(r: &mut Reader<'_>) -> Result<JoinGame> {
    let entity_id = r.i32()?;
    let hardcore = r.boolean()?;
    let count = r.count(1024, "dimension names")?;
    let mut levels = Vec::with_capacity(count);
    for _ in 0..count {
        levels.push(r.identifier()?);
    }
    let max_players = r.varint()?;
    let view_distance = r.varint()?;
    let simulation_distance = r.varint()?;
    let reduced_debug = r.boolean()?;
    let show_death_screen = r.boolean()?;
    let limited_crafting = r.boolean()?;
    let dimension_type = r.nonnegative()?;
    let dimension_name = r.identifier()?;
    let hashed_seed = r.i64()?;
    let game_mode = r.u8()?;
    let previous_game_mode = r.u8()? as i8;
    let debug = r.boolean()?;
    let flat = r.boolean()?;
    let last_death = if r.boolean()? {
        Some((r.identifier()?, r.i64()?))
    } else {
        None
    };
    let portal_cooldown = r.varint()?;
    let enforces_secure_chat = r.boolean()?;
    if max_players < 0
        || view_distance < 0
        || simulation_distance < 0
        || game_mode > 3
        || !(-1..=3).contains(&previous_game_mode)
    {
        return Err(PlayError::Invalid("join game values"));
    }
    Ok(JoinGame {
        entity_id,
        hardcore,
        levels,
        max_players,
        view_distance,
        simulation_distance,
        reduced_debug,
        show_death_screen,
        limited_crafting,
        dimension_type,
        dimension_name,
        hashed_seed,
        game_mode,
        previous_game_mode,
        debug,
        flat,
        last_death,
        portal_cooldown,
        enforces_secure_chat,
    })
}

fn read_light(r: &mut Reader<'_>, dimension: &Dimension) -> Result<LightData> {
    let count = dimension.light_section_count();
    if count > 256 {
        return Err(PlayError::Limit("light sections"));
    }
    let sky_mask = r.bitset(count)?;
    let block_mask = r.bitset(count)?;
    let empty_sky_mask = r.bitset(count)?;
    let empty_block_mask = r.bitset(count)?;
    for (data, empty) in [
        (&sky_mask, &empty_sky_mask),
        (&block_mask, &empty_block_mask),
    ] {
        if data.iter().zip(empty.iter()).any(|(a, b)| a & b != 0) {
            return Err(PlayError::Invalid("overlapping light masks"));
        }
    }
    let sky_arrays = r.light_arrays(&sky_mask, count)?;
    let block_arrays = r.light_arrays(&block_mask, count)?;
    Ok(LightData {
        min_section_y: dimension.min_light_section(),
        section_count: count,
        sky_mask,
        block_mask,
        empty_sky_mask,
        empty_block_mask,
        sky_arrays,
        block_arrays,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum Serverbound {
    AcceptTeleport(i32),
    MovePosRot {
        position: [f64; 3],
        rotation: [f32; 2],
        on_ground: bool,
    },
    KeepAlive(i64),
    Pong(i32),
    ChunkBatchReceived(f32),
    MovePosition {
        position: [f64; 3],
        on_ground: bool,
    },
    MoveRotation {
        rotation: [f32; 2],
        on_ground: bool,
    },
    MoveStatus {
        on_ground: bool,
    },
    Flying(bool),
    PlayerCommand {
        entity_id: i32,
        action: u8,
    },
    Swing {
        hand: u8,
    },
    SelectedSlot(u8),
    /// Inventory-menu slot 1..45, or -1 to drop. The interactive adapter only
    /// exposes hotbar slots 36..44 and fresh items with empty component patches.
    CreativeSlot {
        slot: i16,
        item: Option<PlainItemStack>,
    },
    /// Sequence allocation and acknowledgment tracking belong to the runtime.
    PlayerAction {
        action: u8,
        position: [i32; 3],
        face: u8,
        sequence: i32,
    },
    UseItemOn {
        hand: u8,
        position: [i32; 3],
        face: u8,
        hit: [f32; 3],
        inside: bool,
        sequence: i32,
    },
    UseItem {
        hand: u8,
        sequence: i32,
        rotation: [f32; 2],
    },
}
pub fn encode_serverbound767(packet: &Serverbound) -> Result<(i32, Vec<u8>)> {
    let mut out = Vec::new();
    let id = match packet {
        Serverbound::MovePosition {
            position,
            on_ground,
        } => {
            validate_position(*position)?;
            for value in position {
                out.extend_from_slice(&value.to_be_bytes());
            }
            out.push(u8::from(*on_ground));
            0x1a
        }
        Serverbound::MoveRotation {
            rotation,
            on_ground,
        } => {
            validate_rotation(*rotation)?;
            for value in rotation {
                out.extend_from_slice(&value.to_be_bytes());
            }
            out.push(u8::from(*on_ground));
            0x1c
        }
        Serverbound::MoveStatus { on_ground } => {
            out.push(u8::from(*on_ground));
            0x1d
        }
        Serverbound::Flying(flying) => {
            out.push(if *flying { 2 } else { 0 });
            0x23
        }
        Serverbound::PlayerCommand { entity_id, action } => {
            if *action > 8 {
                return Err(PlayError::Invalid("player command"));
            }
            push_varint(&mut out, *entity_id);
            push_varint(&mut out, *action as i32);
            push_varint(&mut out, 0);
            0x25
        }
        Serverbound::Swing { hand } => {
            validate_hand(*hand)?;
            push_varint(&mut out, *hand as i32);
            0x36
        }
        Serverbound::SelectedSlot(slot) => {
            if *slot > 8 {
                return Err(PlayError::Invalid("selected slot"));
            }
            out.extend_from_slice(&(*slot as i16).to_be_bytes());
            0x2f
        }
        Serverbound::CreativeSlot { slot, item } => {
            if *slot != -1 && !(1..=45).contains(slot) {
                return Err(PlayError::Invalid("creative inventory slot"));
            }
            out.extend_from_slice(&slot.to_be_bytes());
            if let Some(item) = item {
                if item.count == 0 || item.count > 64 || item.item_id > i32::MAX as u32 {
                    return Err(PlayError::Invalid("plain creative stack"));
                }
                push_varint(&mut out, item.count as i32);
                push_varint(&mut out, item.item_id as i32);
                out.extend_from_slice(&[0, 0]);
            } else {
                out.push(0);
            }
            0x32
        }
        Serverbound::PlayerAction {
            action,
            position,
            face,
            sequence,
        } => {
            if *action > 6 || *face > 5 || *sequence < 0 {
                return Err(PlayError::Invalid("player action"));
            }
            push_varint(&mut out, *action as i32);
            out.extend_from_slice(&pack_block_position(*position)?.to_be_bytes());
            out.push(*face);
            push_varint(&mut out, *sequence);
            0x24
        }
        Serverbound::UseItemOn {
            hand,
            position,
            face,
            hit,
            inside,
            sequence,
        } => {
            validate_hand(*hand)?;
            if *face > 5 || *sequence < 0 || hit.iter().any(|v| !v.is_finite()) {
                return Err(PlayError::Invalid("use item on block"));
            }
            push_varint(&mut out, *hand as i32);
            out.extend_from_slice(&pack_block_position(*position)?.to_be_bytes());
            push_varint(&mut out, *face as i32);
            // Minecraft writes offsets verbatim; some block shapes extend
            // outside a unit cube. The authoritative server validates reach.
            for value in hit {
                out.extend_from_slice(&value.to_be_bytes());
            }
            out.push(u8::from(*inside));
            push_varint(&mut out, *sequence);
            0x38
        }
        Serverbound::UseItem {
            hand,
            sequence,
            rotation,
        } => {
            validate_hand(*hand)?;
            validate_rotation(*rotation)?;
            if *sequence < 0 {
                return Err(PlayError::Invalid("use item sequence"));
            }
            push_varint(&mut out, *hand as i32);
            push_varint(&mut out, *sequence);
            for value in rotation {
                out.extend_from_slice(&value.to_be_bytes());
            }
            0x39
        }
        Serverbound::AcceptTeleport(id) => {
            push_varint(&mut out, *id);
            0
        }
        Serverbound::KeepAlive(id) => {
            out.extend_from_slice(&id.to_be_bytes());
            0x18
        }
        Serverbound::Pong(id) => {
            out.extend_from_slice(&id.to_be_bytes());
            0x27
        }
        Serverbound::ChunkBatchReceived(rate) => {
            if !rate.is_finite() || *rate <= 0.0 {
                return Err(PlayError::Invalid("chunk batch rate"));
            }
            out.extend_from_slice(&rate.to_be_bytes());
            0x08
        }
        Serverbound::MovePosRot {
            position,
            rotation,
            on_ground,
        } => {
            validate_position(*position)?;
            validate_rotation(*rotation)?;
            for v in position {
                out.extend_from_slice(&v.to_be_bytes());
            }
            for v in rotation {
                out.extend_from_slice(&v.to_be_bytes());
            }
            out.push(u8::from(*on_ground));
            0x1b
        }
    };
    Ok((id, out))
}

pub fn validate_position(position: [f64; 3]) -> Result<()> {
    if position
        .iter()
        .any(|v| !v.is_finite() || v.abs() > 30_000_000.0)
    {
        Err(PlayError::Invalid("movement position"))
    } else {
        Ok(())
    }
}
pub fn validate_rotation(rotation: [f32; 2]) -> Result<()> {
    if rotation.iter().any(|v| !v.is_finite()) {
        Err(PlayError::Invalid("movement rotation"))
    } else {
        Ok(())
    }
}
fn validate_hand(hand: u8) -> Result<()> {
    if hand > 1 {
        Err(PlayError::Invalid("interaction hand"))
    } else {
        Ok(())
    }
}

/// Session ordering and exact numeric chunk storage, reusable by a future client
/// adapter. Unsupported packets remain an explicit count; they are not applied.
pub struct PlaySession {
    pub context: Option<WorldContext>,
    pub join: Option<JoinGame>,
    pub position: Option<([f64; 3], [f32; 2])>,
    pub chunks: BTreeMap<(i32, i32), ChunkWithLight>,
    pub light_updates: BTreeMap<(i32, i32), LightData>,
    pub confirmed_teleports: usize,
    pub acknowledged_batches: usize,
    pub answered_keepalives: usize,
    pub decoded_chunk_packets: usize,
    pub unhandled_packets: BTreeMap<i32, usize>,
    retained_bytes: usize,
    last_keepalive: Option<i64>,
    dimension_entries: Vec<RegistryEntry>,
    block_state_count: u32,
    biome_count: u32,
    current_batch: Option<usize>,
    in_bundle: bool,
}
impl PlaySession {
    pub fn new(
        dimension_entries: Vec<RegistryEntry>,
        block_state_count: u32,
        biome_count: u32,
    ) -> Result<Self> {
        if block_state_count == 0
            || block_state_count > i32::MAX as u32
            || biome_count == 0
            || biome_count > i32::MAX as u32
        {
            return Err(PlayError::Invalid("registry cardinality"));
        }
        Ok(Self {
            context: None,
            join: None,
            position: None,
            chunks: BTreeMap::new(),
            light_updates: BTreeMap::new(),
            confirmed_teleports: 0,
            acknowledged_batches: 0,
            answered_keepalives: 0,
            decoded_chunk_packets: 0,
            unhandled_packets: BTreeMap::new(),
            dimension_entries,
            block_state_count,
            biome_count,
            current_batch: None,
            in_bundle: false,
            retained_bytes: 0,
            last_keepalive: None,
        })
    }
    /// Responses must be successfully written before the caller records success.
    pub fn receive(&mut self, id: i32, payload: &[u8]) -> Result<Vec<Serverbound>> {
        // Singleton palettes can expand a tiny packet into millions of IDs.
        // Check the unavoidable decoded section allocation before decoding it.
        if id == 0x27 && payload.len() >= 8 {
            if let Some(ctx) = &self.context {
                let x = i32::from_be_bytes(payload[..4].try_into().unwrap());
                let z = i32::from_be_bytes(payload[4..8].try_into().unwrap());
                let old = self
                    .chunks
                    .get(&(x, z))
                    .map_or(0, ChunkWithLight::retained_bytes);
                let minimum = ctx.dimension.height as usize / 16 * (4096 + 64) * 4;
                self.replacement_budget(old, minimum)?;
            }
        }
        let packet = decode_clientbound767(id, payload, self.context.as_ref())?;
        let mut responses = Vec::new();
        match packet {
            Clientbound::Join(join) => {
                if self.join.is_some() {
                    return Err(PlayError::Invalid("duplicate join"));
                }
                let dimension =
                    Dimension::from_registry(join.dimension_type, &self.dimension_entries)?;
                self.context = Some(WorldContext {
                    dimension,
                    block_state_count: self.block_state_count,
                    biome_count: self.biome_count,
                });
                self.join = Some(join);
            }
            Clientbound::Position(position) => {
                if self.join.is_none() {
                    return Err(PlayError::Invalid("position before join"));
                }
                let (previous, rotation) = self.position.unwrap_or(([0.0; 3], [0.0; 2]));
                let (position_resolved, rotation_resolved) =
                    position.resolve(previous, rotation)?;
                self.position = Some((position_resolved, rotation_resolved));
                responses.push(Serverbound::AcceptTeleport(position.teleport_id));
                responses.push(Serverbound::MovePosRot {
                    position: position_resolved,
                    rotation: rotation_resolved,
                    on_ground: false,
                });
                self.confirmed_teleports += 1;
            }
            Clientbound::KeepAlive(value) => {
                if self.last_keepalive == Some(value) {
                    return Err(PlayError::Invalid("duplicate keepalive challenge"));
                }
                responses.push(Serverbound::KeepAlive(value));
                self.answered_keepalives += 1;
                self.last_keepalive = Some(value);
            }
            Clientbound::Ping(value) => responses.push(Serverbound::Pong(value)),
            // The runtime emits these authoritative events. No speculative
            // mutation is applied to the retained wire-chunk snapshots here.
            Clientbound::Abilities(_)
            | Clientbound::BlockUpdates(_)
            | Clientbound::BlockActionAcknowledged(_)
            | Clientbound::InventoryContent(_)
            | Clientbound::InventorySlot(_)
            | Clientbound::SelectedSlot(_) => {}
            Clientbound::UnsupportedInventory { packet_id, .. } => {
                *self.unhandled_packets.entry(packet_id).or_default() += 1;
            }
            Clientbound::BatchStart => {
                if self.join.is_none() || self.current_batch.is_some() {
                    return Err(PlayError::Invalid("chunk batch start order"));
                }
                self.current_batch = Some(0);
            }
            Clientbound::BatchFinished(count) => {
                if self.current_batch != Some(count as usize) {
                    return Err(PlayError::Invalid("chunk batch count or order"));
                }
                self.current_batch = None;
                self.acknowledged_batches += 1;
                responses.push(Serverbound::ChunkBatchReceived(2.0));
            }
            Clientbound::Chunk(chunk) => {
                let batch = self
                    .current_batch
                    .ok_or(PlayError::Invalid("chunk outside batch"))?;
                if batch >= MAX_RETAINED_CHUNKS {
                    return Err(PlayError::Limit("chunk batch"));
                }
                let key = (chunk.x, chunk.z);
                if !self.chunks.contains_key(&key) && self.chunks.len() >= MAX_RETAINED_CHUNKS {
                    return Err(PlayError::Limit("retained chunks"));
                }
                let old = self
                    .chunks
                    .get(&key)
                    .map_or(0, ChunkWithLight::retained_bytes);
                let replacement = self.replacement_budget(old, chunk.retained_bytes())?;
                self.chunks.insert(key, chunk);
                self.decoded_chunk_packets += 1;
                self.current_batch = Some(batch + 1);
                self.retained_bytes = replacement;
            }
            Clientbound::Light { x, z, data } => {
                if !self.light_updates.contains_key(&(x, z))
                    && self.light_updates.len() >= MAX_RETAINED_CHUNKS
                {
                    return Err(PlayError::Limit("retained light updates"));
                }
                let old = self
                    .light_updates
                    .get(&(x, z))
                    .map_or(0, LightData::retained_bytes);
                self.retained_bytes = self.replacement_budget(old, data.retained_bytes())?;
                self.light_updates.insert((x, z), data);
            }
            Clientbound::Unload { x, z } => {
                if let Some(chunk) = self.chunks.remove(&(x, z)) {
                    self.retained_bytes -= chunk.retained_bytes();
                }
                if let Some(light) = self.light_updates.remove(&(x, z)) {
                    self.retained_bytes -= light.retained_bytes();
                }
            }
            Clientbound::BundleDelimiter => self.in_bundle = !self.in_bundle,
            Clientbound::CustomPayload { id, .. } => {
                if id
                    .split_once(':')
                    .map_or(false, |(ns, _)| !ns.is_empty() && ns != "minecraft")
                {
                    return Err(PlayError::Invalid("unimplemented mod payload"));
                }
                *self.unhandled_packets.entry(0x19).or_default() += 1;
            }
            Clientbound::Disconnect(_) => return Err(PlayError::Invalid("server disconnected")),
            Clientbound::StartConfiguration => {
                return Err(PlayError::Invalid("reconfiguration not yet implemented"))
            }
            Clientbound::Unhandled { packet_id, .. } => {
                *self.unhandled_packets.entry(packet_id).or_default() += 1;
            }
        }
        Ok(responses)
    }
    fn replacement_budget(&self, old: usize, new: usize) -> Result<usize> {
        let total = self
            .retained_bytes
            .checked_sub(old)
            .and_then(|v| v.checked_add(new))
            .ok_or(PlayError::Limit("decoded retention arithmetic"))?;
        if total > MAX_RETAINED_BYTES {
            Err(PlayError::Limit("retained decoded bytes"))
        } else {
            Ok(total)
        }
    }
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
    pub fn player_chunk_received(&self) -> bool {
        self.position.map_or(false, |(p, _)| {
            self.chunks.contains_key(&(
                (p[0].floor() as i32).div_euclid(16),
                (p[2].floor() as i32).div_euclid(16),
            ))
        })
    }
    pub fn conformance_complete(&self) -> bool {
        self.join.is_some()
            && self.confirmed_teleports > 0
            && self.player_chunk_received()
            && self.acknowledged_batches > 0
            && self.answered_keepalives >= 2
            && self.current_batch.is_none()
            && !self.in_bundle
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() > MAX_PACKET {
            return Err(PlayError::Limit("packet bytes"));
        }
        Ok(Self { bytes, pos: 0 })
    }
    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(PlayError::Truncated);
        }
        let start = self.pos;
        self.pos += n;
        Ok(&self.bytes[start..self.pos])
    }
    fn finish(&self) -> Result<()> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(PlayError::Trailing(self.remaining()))
        }
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn boolean(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32> {
        let v = f32::from_bits(self.i32()? as u32);
        if v.is_finite() {
            Ok(v)
        } else {
            Err(PlayError::Invalid("nonfinite float"))
        }
    }
    fn f64(&mut self) -> Result<f64> {
        let v = f64::from_bits(self.i64()? as u64);
        if v.is_finite() {
            Ok(v)
        } else {
            Err(PlayError::Invalid("nonfinite double"))
        }
    }
    fn varint(&mut self) -> Result<i32> {
        let mut value = 0u32;
        for shift in 0..5 {
            let b = self.u8()?;
            if shift == 4 && b & 0xf0 != 0 {
                return Err(PlayError::Invalid("VarInt overflow"));
            }
            value |= ((b & 0x7f) as u32) << (7 * shift);
            if b & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err(PlayError::Invalid("VarInt overflow"))
    }
    fn nonnegative(&mut self) -> Result<u32> {
        let v = self.varint()?;
        if v < 0 {
            Err(PlayError::Invalid("negative value"))
        } else {
            Ok(v as u32)
        }
    }
    fn varlong(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for i in 0..10 {
            let b = self.u8()?;
            if i == 9 && b & 0xfe != 0 {
                return Err(PlayError::Invalid("VarLong overflow"));
            }
            value |= ((b & 127) as u64) << (7 * i);
            if b & 128 == 0 {
                return Ok(value);
            }
        }
        Err(PlayError::Invalid("VarLong overflow"))
    }
    fn count(&mut self, max: usize, label: &'static str) -> Result<usize> {
        let n = self.nonnegative()? as usize;
        if n > max {
            Err(PlayError::Limit(label))
        } else {
            Ok(n)
        }
    }
    fn identifier(&mut self) -> Result<String> {
        let n = self.count(32767 * 3, "identifier bytes")?;
        let s = std::str::from_utf8(self.take(n)?)
            .map_err(|_| PlayError::Invalid("identifier UTF8"))?;
        let (namespace, path) = s.split_once(':').unwrap_or(("minecraft", s));
        if !namespace
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c))
            || !path
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"/_.-".contains(&c))
            || s.encode_utf16().count() > 32767
        {
            return Err(PlayError::Invalid("identifier"));
        }
        Ok(s.to_owned())
    }
    fn nbt(&mut self, nullable: bool) -> Result<Option<NetworkNbt>> {
        if self.remaining() == 0 {
            return Err(PlayError::Truncated);
        }
        if nullable && self.bytes[self.pos] == 0 {
            self.pos += 1;
            return Ok(None);
        }
        let (nbt, used) = NetworkNbt::read_prefix(&self.bytes[self.pos..])?;
        self.pos += used;
        Ok(Some(nbt))
    }
    fn bitset(&mut self, bits: usize) -> Result<Vec<u64>> {
        let count = self.count((bits + 63) / 64, "light mask words")?;
        let mut mask = Vec::with_capacity(count);
        for _ in 0..count {
            mask.push(self.i64()? as u64);
        }
        if count == (bits + 63) / 64
            && bits % 64 != 0
            && mask.last().map_or(false, |v| *v >> (bits % 64) != 0)
        {
            return Err(PlayError::Invalid("light mask outside dimension"));
        }
        Ok(mask)
    }
    fn light_arrays(&mut self, mask: &[u64], max: usize) -> Result<Vec<Vec<u8>>> {
        let count = self.count(max, "light array count")?;
        if count != mask.iter().map(|v| v.count_ones() as usize).sum::<usize>() {
            return Err(PlayError::Invalid("light array mask mismatch"));
        }
        let mut arrays = Vec::with_capacity(count);
        for _ in 0..count {
            let n = self.count(2048, "light array bytes")?;
            if n != 2048 {
                return Err(PlayError::Invalid("light array must have 2048 bytes"));
            }
            arrays.push(self.take(n)?.to_vec());
        }
        Ok(arrays)
    }
}
fn push_varint(out: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        let mut b = (v & 127) as u8;
        v >>= 7;
        if v != 0 {
            b |= 128;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn creative_stack_and_inventory_fields_follow_1211_stream_codecs() {
        let item = PlainItemStack {
            item_id: 300,
            count: 64,
        };
        // SHORT slot, VAR_INT count, unshifted registry holder ID, two empty patch counts.
        assert_eq!(
            encode_serverbound767(&Serverbound::CreativeSlot {
                slot: 36,
                item: Some(item.clone()),
            })
            .unwrap(),
            (0x32, vec![0, 36, 64, 0xac, 2, 0, 0])
        );
        assert_eq!(
            encode_serverbound767(&Serverbound::CreativeSlot {
                slot: 44,
                item: None,
            })
            .unwrap(),
            (0x32, vec![0, 44, 0])
        );
        let content = vec![0, 0x81, 1, 2, 0, 64, 0xac, 2, 0, 0, 0];
        assert_eq!(
            decode_clientbound767(0x13, &content, None).unwrap(),
            Clientbound::InventoryContent(InventoryContent {
                container_id: 0,
                state_id: 129,
                slots: vec![None, Some(item.clone())],
                carried: None,
            })
        );
        assert_eq!(
            decode_clientbound767(0x15, &[0xfe, 0, 0, 8, 64, 0xac, 2, 0, 0], None).unwrap(),
            Clientbound::InventorySlot(InventorySlot {
                container_id: -2,
                state_id: 0,
                slot: 8,
                item: Some(item),
            })
        );
        assert_eq!(
            decode_clientbound767(0x15, &[0xff, 0, 0xff, 0xff, 0], None).unwrap(),
            Clientbound::InventorySlot(InventorySlot {
                container_id: -1,
                state_id: 0,
                slot: -1,
                item: None,
            })
        );
        assert_eq!(
            decode_clientbound767(0x53, &[8], None).unwrap(),
            Clientbound::SelectedSlot(8)
        );
    }

    #[test]
    fn unknown_item_components_preserve_whole_packet_without_partial_inventory() {
        // One empty slot, then a stack whose addition has an opaque component body.
        let body = vec![0, 0, 2, 0, 1, 1, 1, 0, 42, 99, 88];
        assert_eq!(
            decode_clientbound767(0x13, &body, None).unwrap(),
            Clientbound::UnsupportedInventory {
                packet_id: 0x13,
                data: body
            }
        );
        let removal = vec![0, 0, 0, 36, 1, 1, 0, 1, 42];
        assert_eq!(
            decode_clientbound767(0x15, &removal, None).unwrap(),
            Clientbound::UnsupportedInventory {
                packet_id: 0x15,
                data: removal
            }
        );
        // This is deliberately not a claim that an unsupported component is valid.
        // Its raw packet stays bounded and must not become a trusted empty stack.
    }

    #[test]
    fn inventory_limits_truncation_and_invalid_creative_requests_are_rejected() {
        let packet = [0, 0, 0, 36, 1, 1, 0, 0];
        for end in 0..packet.len() {
            assert!(decode_clientbound767(0x15, &packet[..end], None).is_err());
        }
        let mut trailing = packet.to_vec();
        trailing.push(0);
        assert!(decode_clientbound767(0x15, &trailing, None).is_err());
        assert!(decode_clientbound767(0x13, &[0, 0, 0x81, 8], None).is_err());
        assert!(decode_clientbound767(0x53, &[9], None).is_err());
        for slot in [-2, 0, 46, i16::MAX] {
            assert!(
                encode_serverbound767(&Serverbound::CreativeSlot { slot, item: None }).is_err()
            );
        }
        for (item_id, count) in [(1, 0), (1, 65), (u32::MAX, 1)] {
            assert!(encode_serverbound767(&Serverbound::CreativeSlot {
                slot: 36,
                item: Some(PlainItemStack { item_id, count }),
            })
            .is_err());
        }
    }

    #[test]
    fn modern_movement_and_action_wire_fields_are_exact() {
        assert_eq!(
            encode_serverbound767(&Serverbound::Flying(true)).unwrap(),
            (0x23, vec![2])
        );
        assert_eq!(
            encode_serverbound767(&Serverbound::SelectedSlot(8)).unwrap(),
            (0x2f, vec![0, 8])
        );
        assert_eq!(
            encode_serverbound767(&Serverbound::MoveStatus { on_ground: true }).unwrap(),
            (0x1d, vec![1])
        );
        let packet = Serverbound::PlayerAction {
            action: 2,
            position: [-1, -64, -2],
            face: 5,
            sequence: 300,
        };
        let (id, bytes) = encode_serverbound767(&packet).unwrap();
        assert_eq!(id, 0x24);
        assert_eq!(
            bytes,
            vec![2, 255, 255, 255, 255, 255, 255, 239, 192, 5, 172, 2]
        );
        let (id, bytes) = encode_serverbound767(&Serverbound::UseItem {
            hand: 1,
            sequence: 128,
            rotation: [90.0, -45.0],
        })
        .unwrap();
        assert_eq!(id, 0x39);
        assert_eq!(bytes, vec![1, 128, 1, 66, 180, 0, 0, 194, 52, 0, 0]);
        let (id, bytes) = encode_serverbound767(&Serverbound::UseItemOn {
            hand: 0,
            position: [0, -64, 0],
            face: 1,
            hit: [0.25, 1.0, 0.75],
            inside: true,
            sequence: 7,
        })
        .unwrap();
        assert_eq!((id, bytes.len(), bytes[22], bytes[23]), (0x38, 24, 1, 7));
        let (_, extended_shape) = encode_serverbound767(&Serverbound::UseItemOn {
            hand: 0,
            position: [0, 0, 0],
            face: 1,
            hit: [0.5, 1.5, 0.5],
            inside: false,
            sequence: 8,
        })
        .unwrap();
        assert_eq!(&extended_shape[14..18], &1.5f32.to_be_bytes());
        assert!(encode_serverbound767(&Serverbound::SelectedSlot(9)).is_err());
        assert!(encode_serverbound767(&Serverbound::Swing { hand: 2 }).is_err());
        assert!(pack_block_position([0, -2049, 0]).is_err());
        for position in [
            [-33_554_432, -2048, 33_554_431],
            [33_554_431, 2047, -33_554_432],
            [-1, -64, -2],
        ] {
            assert_eq!(
                unpack_block_position(pack_block_position(position).unwrap()),
                position
            );
        }
    }
    #[test]
    fn authoritative_updates_preserve_signed_coords_and_reject_unknown_ids() {
        let mut bytes = pack_block_position([-17, -63, 31])
            .unwrap()
            .to_be_bytes()
            .to_vec();
        push_varint(&mut bytes, 26000);
        let expected = BlockUpdate {
            position: [-17, -63, 31],
            state_id: 26000,
        };
        assert_eq!(
            decode_clientbound767(9, &bytes, Some(&context())).unwrap(),
            Clientbound::BlockUpdates(vec![expected.clone()])
        );
        // SectionPos x=-2, y=-4, z=1; local short x=15,z=15,y=1.
        let section = (((-2i64 as u64 & 0x3fffff) << 42) | (1 << 20) | 0xffffc) as i64;
        let mut batch = section.to_be_bytes().to_vec();
        batch.push(1);
        let mut packed = (26000u64 << 12) | 0xff1;
        while packed >= 128 {
            batch.push((packed as u8 & 127) | 128);
            packed >>= 7;
        }
        batch.push(packed as u8);
        assert_eq!(
            decode_clientbound767(0x49, &batch, Some(&context())).unwrap(),
            Clientbound::BlockUpdates(vec![expected])
        );
        for end in 0..batch.len() {
            assert!(decode_clientbound767(0x49, &batch[..end], Some(&context())).is_err());
        }
        let mut duplicate = batch.clone();
        duplicate[8] = 2;
        duplicate.extend_from_slice(&batch[9..]);
        assert!(decode_clientbound767(0x49, &duplicate, Some(&context())).is_err());
        bytes.truncate(8);
        push_varint(&mut bytes, 26684);
        assert!(decode_clientbound767(9, &bytes, Some(&context())).is_err());
        assert!(decode_clientbound767(9, &bytes, None).is_err());
    }
    #[test]
    fn abilities_and_ack_are_strict_and_no_implicit_flight_is_invented() {
        let mut bytes = vec![5];
        bytes.extend_from_slice(&0.05f32.to_be_bytes());
        bytes.extend_from_slice(&0.1f32.to_be_bytes());
        let abilities = match decode_clientbound767(0x38, &bytes, None).unwrap() {
            Clientbound::Abilities(v) => v,
            _ => panic!(),
        };
        assert!(
            abilities.may_fly
                && abilities.invulnerable
                && !abilities.flying
                && !abilities.instant_build
        );
        assert_eq!(
            decode_clientbound767(5, &[0xac, 2], None).unwrap(),
            Clientbound::BlockActionAcknowledged(300)
        );
        assert!(decode_clientbound767(5, &[255, 255, 255, 255, 15], None).is_err());
        bytes[0] = 16;
        assert!(decode_clientbound767(0x38, &bytes, None).is_err());
        bytes[0] = 0;
        bytes[1..5].copy_from_slice(&f32::NAN.to_be_bytes());
        assert!(decode_clientbound767(0x38, &bytes, None).is_err());
    }
    fn string(out: &mut Vec<u8>, s: &str) {
        push_varint(out, s.len() as i32);
        out.extend_from_slice(s.as_bytes());
    }
    fn dimension_entries() -> Vec<RegistryEntry> {
        let bytes=b"\x0a\x03\x00\x05min_y\xff\xff\xff\xc0\x03\x00\x06height\x00\x00\x01\x80\x01\x00\x0chas_skylight\x01\x00";
        vec![RegistryEntry {
            id: "minecraft:overworld".into(),
            data: Some(NetworkNbt::from_bytes(bytes).unwrap()),
        }]
    }
    fn context() -> WorldContext {
        WorldContext {
            dimension: Dimension::from_registry(0, &dimension_entries()).unwrap(),
            block_state_count: 26684,
            biome_count: 64,
        }
    }
    fn join_bytes() -> Vec<u8> {
        let mut v = 12i32.to_be_bytes().to_vec();
        v.push(0);
        push_varint(&mut v, 1);
        string(&mut v, "minecraft:overworld");
        for n in [20, 2, 2] {
            push_varint(&mut v, n);
        }
        v.extend_from_slice(&[0, 1, 0]);
        push_varint(&mut v, 0);
        string(&mut v, "minecraft:overworld");
        v.extend_from_slice(&(-42i64).to_be_bytes());
        v.extend_from_slice(&[0, 255, 0, 0, 0]);
        push_varint(&mut v, 0);
        v.push(1);
        v
    }
    fn position_bytes() -> Vec<u8> {
        let mut v = Vec::new();
        for x in [1.5f64, 70.0, -0.5] {
            v.extend_from_slice(&x.to_be_bytes());
        }
        for x in [30.0f32, -10.0] {
            v.extend_from_slice(&x.to_be_bytes());
        }
        v.push(0);
        push_varint(&mut v, 3);
        v
    }
    fn light_bytes() -> Vec<u8> {
        let mut v = Vec::new();
        push_varint(&mut v, 1);
        v.extend_from_slice(&1u64.to_be_bytes());
        v.extend_from_slice(&[0, 0, 0]);
        push_varint(&mut v, 1);
        push_varint(&mut v, 2048);
        v.extend_from_slice(&[0xab; 2048]);
        v.push(0);
        v
    }
    fn chunk_bytes() -> Vec<u8> {
        let mut v = 0i32.to_be_bytes().to_vec();
        v.extend_from_slice(&(-1i32).to_be_bytes());
        v.extend_from_slice(&[10, 0]);
        let mut sections = Vec::new();
        for _ in 0..24 {
            sections.extend_from_slice(&[0, 1, 0]);
            push_varint(&mut sections, 26000);
            sections.extend_from_slice(&[0, 0, 63, 0]);
        }
        push_varint(&mut v, sections.len() as i32);
        v.extend_from_slice(&sections);
        v.push(1);
        v.push(0xf2);
        v.extend_from_slice(&(-20i16).to_be_bytes());
        push_varint(&mut v, 40000);
        v.push(0);
        v.extend_from_slice(&light_bytes());
        v
    }
    #[test]
    fn join_resolves_exact_dimension_registry_definition() {
        let join = match decode_clientbound767(0x2b, &join_bytes(), None).unwrap() {
            Clientbound::Join(v) => v,
            _ => panic!(),
        };
        assert_eq!(join.dimension_type, 0);
        assert_eq!(join.hashed_seed, -42);
        assert_eq!(join.previous_game_mode, -1);
        let dim = Dimension::from_registry(join.dimension_type, &dimension_entries()).unwrap();
        assert_eq!(
            (
                dim.min_y,
                dim.height,
                dim.min_light_section(),
                dim.light_section_count()
            ),
            (-64, 384, -5, 26)
        );
        assert!(Dimension::from_registry(1, &dimension_entries()).is_err());
    }
    #[test]
    fn full_chunk_preserves_unknown_ids_negative_entity_y_and_light_boundary() {
        let chunk = match decode_clientbound767(0x27, &chunk_bytes(), Some(&context())).unwrap() {
            Clientbound::Chunk(c) => c,
            _ => panic!(),
        };
        assert_eq!(chunk.sections.len(), 24);
        assert_eq!(chunk.sections[0].section_y, -4);
        assert_eq!(chunk.sections[23].section_y, 19);
        assert!(chunk.sections.iter().all(
            |s| s.block_states.iter().all(|v| *v == 26000) && s.biomes.iter().all(|v| *v == 63)
        ));
        assert_eq!(
            (chunk.block_entities[0].type_id, chunk.block_entities[0].y),
            (40000, -20)
        );
        assert!(chunk.block_entities[0].nbt.is_none());
        assert_eq!(chunk.light.min_section_y, -5);
        assert_eq!(chunk.light.sky_arrays[0], [0xab; 2048]);
    }
    #[test]
    fn position_flags_and_exact_ack_packet_shapes() {
        let p = PlayerPosition {
            position: [1.0, 80.0, -2.0],
            rotation: [10.0, 20.0],
            relative_flags: 1 | 4 | 8,
            teleport_id: 300,
        };
        assert_eq!(
            p.resolve([4.0, 5.0, 6.0], [30.0, 40.0]).unwrap(),
            ([5.0, 80.0, 4.0], [40.0, 20.0])
        );
        assert_eq!(
            encode_serverbound767(&Serverbound::AcceptTeleport(300)).unwrap(),
            (0, vec![0xac, 2])
        );
        assert_eq!(
            encode_serverbound767(&Serverbound::KeepAlive(-2)).unwrap(),
            (0x18, (-2i64).to_be_bytes().to_vec())
        );
        let (id, body) = encode_serverbound767(&Serverbound::MovePosRot {
            position: [1.0, 2.0, 3.0],
            rotation: [4.0, 5.0],
            on_ground: false,
        })
        .unwrap();
        assert_eq!((id, body.len(), body[32]), (0x1b, 33, 0));
        assert!(encode_serverbound767(&Serverbound::ChunkBatchReceived(f32::NAN)).is_err());
    }
    #[test]
    fn all_chunk_truncations_and_trailing_bytes_fail() {
        let bytes = chunk_bytes();
        for n in 0..bytes.len() {
            assert!(
                decode_clientbound767(0x27, &bytes[..n], Some(&context())).is_err(),
                "prefix {}",
                n
            );
        }
        let mut extra = bytes;
        extra.push(0);
        assert!(decode_clientbound767(0x27, &extra, Some(&context())).is_err());
    }
    #[test]
    fn malformed_light_masks_and_arrays_fail() {
        let mut outside = light_bytes();
        outside[8] = 0;
        outside[5] = 4;
        assert!(read_light(&mut Reader::new(&outside).unwrap(), &context().dimension).is_err());
        let mut overlap = light_bytes();
        overlap.splice(10..11, [1, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert!(read_light(&mut Reader::new(&overlap).unwrap(), &context().dimension).is_err());
        let mut mismatch = light_bytes();
        mismatch[12] = 0;
        assert!(read_light(&mut Reader::new(&mismatch).unwrap(), &context().dimension).is_err());
    }
    #[test]
    fn session_requires_join_teleport_complete_batch_player_chunk_and_keepalive() {
        let mut s = PlaySession::new(dimension_entries(), 26684, 64).unwrap();
        assert!(s.receive(0x40, &position_bytes()).is_err());
        s.receive(0x2b, &join_bytes()).unwrap();
        let ack = s.receive(0x40, &position_bytes()).unwrap();
        assert_eq!(ack.len(), 2);
        assert!(!s.conformance_complete());
        s.receive(0x0d, &[]).unwrap();
        s.receive(0x27, &chunk_bytes()).unwrap();
        assert!(s.player_chunk_received());
        assert!(!s.conformance_complete());
        assert!(s.receive(0x0c, &[2]).is_err());
        s.receive(0x0c, &[1]).unwrap();
        assert!(!s.conformance_complete());
        s.receive(0x26, &123i64.to_be_bytes()).unwrap();
        assert!(!s.conformance_complete());
        assert!(s.receive(0x26, &123i64.to_be_bytes()).is_err());
        s.receive(0x26, &124i64.to_be_bytes()).unwrap();
        assert!(s.conformance_complete());
        s.receive(0, &[]).unwrap();
        assert!(!s.conformance_complete());
        s.receive(0, &[]).unwrap();
        assert!(s.conformance_complete());
        let packed = (-1i64) << 32;
        s.receive(0x21, &packed.to_be_bytes()).unwrap();
        assert!(!s.player_chunk_received());
        assert_eq!(s.retained_bytes(), 0);
    }
    #[test]
    fn unrelated_packets_explicit_but_mod_channels_and_reconfiguration_not_accepted() {
        let mut s = PlaySession::new(dimension_entries(), 26684, 64).unwrap();
        s.receive(0x7e, &[1, 2, 3]).unwrap();
        assert_eq!(s.unhandled_packets.get(&0x7e), Some(&1));
        let mut mod_payload = Vec::new();
        string(&mut mod_payload, "neoforge:network");
        assert!(s.receive(0x19, &mod_payload).is_err());
        assert!(s.receive(0x69, &[]).is_err());
        assert!(s.receive(0x47, &[]).is_err());
    }
    #[test]
    fn decoded_retention_budget_checks_replacements_and_preflight_expansion() {
        let mut s = PlaySession::new(dimension_entries(), 26684, 64).unwrap();
        s.receive(0x2b, &join_bytes()).unwrap();
        s.receive(0x0d, &[]).unwrap();
        s.receive(0x27, &chunk_bytes()).unwrap();
        let original = s.retained_bytes();
        s.receive(0x27, &chunk_bytes()).unwrap();
        assert_eq!(s.retained_bytes(), original);
        assert!(s.replacement_budget(0, MAX_RETAINED_BYTES).is_err());
        assert_eq!(s.replacement_budget(original, 1).unwrap(), 1);
        s.retained_bytes = MAX_RETAINED_BYTES;
        let mut another = chunk_bytes();
        another[..4].copy_from_slice(&1i32.to_be_bytes());
        assert_eq!(
            s.receive(0x27, &another),
            Err(PlayError::Limit("retained decoded bytes"))
        );
    }
}
