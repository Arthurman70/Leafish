//! Explicit loopback-only 1.21.1 runtime. This does not advertise GUI or mod support.
//!
//! The caller supplies an exact generated catalog and consumes every event.
//! Unknown vanilla packets are counted; mod channels, authentication, respawn,
//! and reconfiguration fail explicitly. Block changes are server authoritative:
//! this module never predicts a replacement block or translates an unknown ID.
use leafish_blocks::catalog::StateCatalog;
use leafish_protocol::protocol::{
    configuration as config, modern_transport as wire, play767 as play,
};
use std::collections::BTreeMap;
use std::convert::TryInto;
use std::io::{self, Cursor, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

const MAX_QUEUED_BYTES: usize = 16 * 1024 * 1024;
const MAX_PENDING_ACTIONS: i32 = 4096;
pub type Result<T> = io::Result<T>;
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
fn protocol(error: impl std::fmt::Display) -> io::Error {
    invalid(error.to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalTransform {
    pub position: [f64; 3],
    /// Wire yaw and pitch in degrees, not Leafish renderer radians.
    pub rotation: [f32; 2],
    /// Changes at every server teleport. Stale GUI input must not undo a correction.
    pub generation: u64,
}

#[derive(Debug)]
pub enum ModernEvent {
    Joined {
        join: play::JoinGame,
        context: play::WorldContext,
    },
    Teleported {
        transform: LocalTransform,
        relative_flags: u8,
    },
    Chunk(play::ChunkWithLight),
    Light {
        x: i32,
        z: i32,
        data: play::LightData,
    },
    Unload {
        x: i32,
        z: i32,
    },
    BlockChanges(Vec<play::BlockUpdate>),
    Abilities(play::PlayerAbilities),
    BlockActionAcknowledged(i32),
    /// An ordered cut after complete batches/bundles and all earlier typed
    /// events. Emitted once; polling asynchronous counters is not conformance.
    ConformanceCheckpoint {
        stats: RuntimeStats,
    },
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeStats {
    pub packets_received: u64,
    pub bytes_received: u64,
    pub chunks_received: usize,
    pub answered_keepalives: usize,
    pub acknowledged_batches: usize,
    pub confirmed_teleports: usize,
    pub authoritative_block_updates: u64,
    pub unimplemented_packets: BTreeMap<i32, usize>,
    pub close_reason: Option<String>,
}

struct Actions {
    next: i32,
    acknowledged: i32,
}
impl Actions {
    fn next_sequence(&self) -> Result<i32> {
        if self.next == i32::MAX || self.next - self.acknowledged > MAX_PENDING_ACTIONS {
            Err(invalid("Action sequence budget exhausted"))
        } else {
            Ok(self.next)
        }
    }
    fn acknowledge(&mut self, sequence: i32) -> Result<()> {
        if sequence < self.acknowledged || sequence >= self.next {
            return Err(invalid("Unsent or regressing block-action acknowledgment"));
        }
        self.acknowledged = sequence;
        Ok(())
    }
}

struct Shared {
    biome_names: Vec<String>,
    registry_counts: BTreeMap<String, usize>,
    writer: Mutex<wire::Framed<TcpStream>>,
    shutdown: TcpStream,
    closed: AtomicBool,
    queued_bytes: AtomicUsize,
    player: Mutex<Option<LocalTransform>>,
    actions: Mutex<Actions>,
    stats: Mutex<RuntimeStats>,
}
impl Shared {
    fn close(&self, reason: &str) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.stats.lock().unwrap().close_reason = Some(reason.to_owned());
            let _ = self.shutdown.shutdown(Shutdown::Both);
        }
    }
    fn write(&self, packet: &play::Serverbound) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(invalid("Connection is closed"));
        }
        let (id, payload) = play::encode_serverbound767(packet).map_err(protocol)?;
        let result = self.writer.lock().unwrap().write_packet(id, &payload);
        if result.is_err() {
            self.close("Network write failed");
        }
        result
    }
}

pub struct ModernConnection {
    shared: Arc<Shared>,
}
impl Drop for ModernConnection {
    fn drop(&mut self) {
        self.close();
    }
}

/// Dropping either the connection or event consumer closes the socket and reader.
pub struct ModernEvents {
    receiver: Receiver<(ModernEvent, usize)>,
    shared: Weak<Shared>,
}
impl ModernEvents {
    fn received(&self, item: (ModernEvent, usize)) -> ModernEvent {
        if let Some(shared) = self.shared.upgrade() {
            shared.queued_bytes.fetch_sub(item.1, Ordering::AcqRel);
        }
        item.0
    }
    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<ModernEvent, RecvTimeoutError> {
        self.receiver
            .recv_timeout(timeout)
            .map(|item| self.received(item))
    }
    pub fn try_recv(&self) -> std::result::Result<ModernEvent, TryRecvError> {
        self.receiver.try_recv().map(|item| self.received(item))
    }
}
impl Drop for ModernEvents {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            shared.close("Event consumer closed");
        }
    }
}

impl ModernConnection {
    pub fn connect_loopback(
        address: SocketAddr,
        username: &str,
        catalog: Arc<StateCatalog>,
    ) -> Result<(Arc<Self>, ModernEvents)> {
        if !address.ip().is_loopback() {
            return Err(invalid("Experimental runtime accepts loopback only"));
        }
        if username.is_empty()
            || username.len() > 16
            || !username
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || v == b'_')
        {
            return Err(invalid("Invalid offline profile name"));
        }
        let count = catalog.len();
        if count == 0 || count > i32::MAX as usize {
            return Err(invalid("Invalid exact catalog size"));
        }
        if catalog
            .states()
            .iter()
            .any(|state| state.namespace() != "minecraft")
        {
            return Err(invalid(
                "Modded state catalogs require an implemented mod negotiation",
            ));
        }
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let deadline = Arc::new(Mutex::new(Instant::now() + Duration::from_secs(60)));
        let mut connection = wire::Framed::new(DeadlineStream {
            stream,
            deadline: deadline.clone(),
        });
        let mut body = Vec::new();
        wire::write_varint(&mut body, 767)?;
        wire::write_string(&mut body, &address.ip().to_string())?;
        body.extend_from_slice(&address.port().to_be_bytes());
        wire::write_varint(&mut body, 2)?;
        connection.write_packet(0, &body)?;
        body.clear();
        wire::write_string(&mut body, username)?;
        body.extend_from_slice(&[0; 16]);
        connection.write_packet(0, &body)?;
        let mut threshold = None;
        let mut logged_in = false;
        for _ in 0..64 {
            let (id, payload) = connection.read_packet()?;
            match id {
                0 => return Err(invalid("Server rejected login")),
                1 => {
                    return Err(invalid(
                        "Online authentication is not implemented by this development adapter",
                    ))
                }
                2 => {
                    validate_profile(&payload, username)?;
                    connection.write_packet(3, &[])?;
                    logged_in = true;
                    break;
                }
                3 => {
                    let mut r = Cursor::new(payload.as_slice());
                    let value = wire::read_varint(&mut r)?;
                    finish(&r)?;
                    if threshold.is_some() {
                        return Err(invalid("Duplicate compression negotiation"));
                    }
                    connection.set_compression(value)?;
                    threshold = Some(value);
                }
                // No successful plugin reply is sent without an implementation.
                4 => return Err(invalid("Login plugin negotiation is not implemented")),
                _ => return Err(invalid("Unsupported login packet")),
            }
        }
        if !logged_in {
            return Err(invalid("Login packet budget exceeded"));
        }
        connection.write_packet(0, &client_information()?)?;
        let registries = configure(&mut connection)?;
        let registry_counts = registries
            .iter()
            .map(|(name, entries)| (name.clone(), entries.len()))
            .collect();
        let dimensions = registries
            .get("minecraft:dimension_type")
            .ok_or_else(|| invalid("Missing dimension registry"))?
            .clone();
        let biome_names: Vec<String> = registries
            .get("minecraft:worldgen/biome")
            .ok_or_else(|| invalid("Missing biome registry"))?
            .iter()
            .map(|entry| entry.id.clone())
            .collect();
        let biomes = biome_names.len();
        let session =
            play::PlaySession::new(dimensions, count as u32, biomes as u32).map_err(protocol)?;
        let stream = connection.into_inner().stream;
        let reader_stream = stream.try_clone()?;
        let shutdown = stream.try_clone()?;
        let mut writer = wire::Framed::new(stream);
        if let Some(value) = threshold {
            writer.set_compression(value)?;
        }
        let shared = Arc::new(Shared {
            biome_names,
            registry_counts,
            writer: Mutex::new(writer),
            shutdown,
            closed: AtomicBool::new(false),
            queued_bytes: AtomicUsize::new(0),
            player: Mutex::new(None),
            actions: Mutex::new(Actions {
                next: 1,
                acknowledged: 0,
            }),
            stats: Mutex::new(RuntimeStats::default()),
        });
        let mut reader = wire::Framed::new(DeadlineStream {
            stream: reader_stream,
            deadline: deadline.clone(),
        });
        if let Some(value) = threshold {
            reader.set_compression(value)?;
        }
        let (sender, receiver) = mpsc::sync_channel(2);
        let worker = shared.clone();
        thread::Builder::new()
            .name("leafish-767-reader".into())
            .spawn(move || {
                // Holding the catalog keeps the exact ID domain alive for this session.
                let _catalog = catalog;
                if let Err(error) = read_play(reader, deadline, session, &worker, sender) {
                    worker.close(&error.to_string());
                }
            })?;
        let events = ModernEvents {
            receiver,
            shared: Arc::downgrade(&shared),
        };
        Ok((Arc::new(Self { shared }), events))
    }

    pub fn is_connected(&self) -> bool {
        !self.shared.closed.load(Ordering::Acquire)
    }
    pub fn close(&self) {
        self.shared.close("Connection closed by client");
    }
    pub fn stats(&self) -> RuntimeStats {
        self.shared.stats.lock().unwrap().clone()
    }
    pub fn biome_registry_names(&self) -> &[String] {
        &self.shared.biome_names
    }
    pub fn registry_counts(&self) -> &BTreeMap<String, usize> {
        &self.shared.registry_counts
    }
    pub fn local_transform(&self) -> Option<LocalTransform> {
        self.shared.player.lock().unwrap().clone()
    }

    pub fn update_local_transform(&self, transform: LocalTransform) -> Result<()> {
        let mut player = self.shared.player.lock().unwrap();
        validate_transform(&player, &transform)?;
        *player = Some(transform);
        Ok(())
    }

    pub fn move_player(&self, transform: LocalTransform, on_ground: bool) -> Result<()> {
        let mut player = self.shared.player.lock().unwrap();
        validate_transform(&player, &transform)?;
        self.shared.write(&play::Serverbound::MovePosRot {
            position: transform.position,
            rotation: transform.rotation,
            on_ground,
        })?;
        *player = Some(transform);
        Ok(())
    }

    /// Only stateless UI commands use this path. Movement has a teleport generation;
    /// actions allocate tracked sequences. Server-control replies remain internal.
    pub fn send(&self, packet: play::Serverbound) -> Result<()> {
        match packet {
            play::Serverbound::Flying(_)
            | play::Serverbound::PlayerCommand { .. }
            | play::Serverbound::Swing { .. }
            | play::Serverbound::SelectedSlot(_) => {
                if self.shared.player.lock().unwrap().is_none() {
                    return Err(invalid("Player has not spawned"));
                }
                self.shared.write(&packet)
            }
            _ => Err(invalid(
                "Use movement or sequenced-action methods for this packet",
            )),
        }
    }

    fn sequenced(&self, build: impl FnOnce(i32) -> play::Serverbound) -> Result<i32> {
        if self.shared.player.lock().unwrap().is_none() {
            return Err(invalid("Player has not spawned"));
        }
        let mut actions = self.shared.actions.lock().unwrap();
        let sequence = actions.next_sequence()?;
        self.shared.write(&build(sequence))?;
        actions.next += 1;
        Ok(sequence)
    }
    pub fn dig(&self, action: u8, position: [i32; 3], face: u8) -> Result<i32> {
        if action > 2 {
            return Err(invalid("Dig action must be start, abort, or finish"));
        }
        self.sequenced(|sequence| play::Serverbound::PlayerAction {
            action,
            position,
            face,
            sequence,
        })
    }
    pub fn use_item_on(
        &self,
        hand: u8,
        position: [i32; 3],
        face: u8,
        hit: [f32; 3],
        inside: bool,
    ) -> Result<i32> {
        self.sequenced(|sequence| play::Serverbound::UseItemOn {
            hand,
            position,
            face,
            hit,
            inside,
            sequence,
        })
    }
    pub fn use_item(&self, hand: u8, rotation: [f32; 2]) -> Result<i32> {
        self.sequenced(|sequence| play::Serverbound::UseItem {
            hand,
            sequence,
            rotation,
        })
    }
}

fn validate_transform(current: &Option<LocalTransform>, next: &LocalTransform) -> Result<()> {
    play::validate_position(next.position).map_err(protocol)?;
    play::validate_rotation(next.rotation).map_err(protocol)?;
    if current.as_ref().map(|v| v.generation) != Some(next.generation) {
        Err(invalid(
            "Stale movement generation or player not yet spawned",
        ))
    } else {
        Ok(())
    }
}

struct DeadlineStream {
    stream: TcpStream,
    deadline: Arc<Mutex<Instant>>,
}
impl Read for DeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let remaining = self
            .deadline
            .lock()
            .unwrap()
            .checked_duration_since(Instant::now())
            .filter(|v| !v.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Packet deadline exceeded"))?;
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(buffer)
    }
}
impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> Result<usize> {
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> Result<()> {
        self.stream.flush()
    }
}

fn configure<S: Read + Write>(
    connection: &mut wire::Framed<S>,
) -> Result<BTreeMap<String, Vec<config::RegistryEntry>>> {
    let mut registries = BTreeMap::new();
    let mut state = config::ConfigurationState::default();
    let mut bytes = 0usize;
    for _ in 0..4096 {
        let (id, payload) = connection.read_packet()?;
        bytes += payload.len();
        if bytes > 64 * 1024 * 1024 {
            return Err(invalid("Configuration byte budget exceeded"));
        }
        let packet = config::decode_clientbound767(id, &payload).map_err(protocol)?;
        state.received(&packet).map_err(protocol)?;
        let response = match packet {
            config::Clientbound::KeepAlive(v) => Some(config::Serverbound::KeepAlive(v)),
            config::Clientbound::Ping(v) => Some(config::Serverbound::Pong(v)),
            config::Clientbound::SelectKnownPacks(_) => {
                Some(config::Serverbound::SelectKnownPacks(vec![]))
            }
            config::Clientbound::RegistryData { registry, entries } => {
                if registries.contains_key(&registry) || entries.iter().any(|v| v.data.is_none()) {
                    return Err(invalid("Incomplete or duplicate registry"));
                }
                if !vanilla(&registry) || entries.iter().any(|v| !vanilla(&v.id)) {
                    return Err(invalid(
                        "Modded registry identities are not supported by this vanilla adapter",
                    ));
                }
                registries.insert(registry, entries);
                None
            }
            config::Clientbound::CustomPayload { id, .. } => {
                if !vanilla(&id) {
                    return Err(invalid("Unimplemented mod configuration payload"));
                }
                None
            }
            config::Clientbound::FinishConfiguration => {
                if !registries.contains_key("minecraft:dimension_type")
                    || !registries.contains_key("minecraft:worldgen/biome")
                {
                    return Err(invalid(
                        "World registries missing before configuration finish",
                    ));
                }
                let (id, payload) =
                    config::encode_serverbound767(&state.acknowledge_finish().map_err(protocol)?)
                        .map_err(protocol)?;
                connection.write_packet(id, &payload)?;
                return Ok(registries);
            }
            _ => None,
        };
        if let Some(response) = response {
            let (id, payload) = config::encode_serverbound767(&response).map_err(protocol)?;
            connection.write_packet(id, &payload)?;
        }
    }
    Err(invalid("Configuration packet budget exceeded"))
}

fn read_play(
    mut reader: wire::Framed<DeadlineStream>,
    deadline: Arc<Mutex<Instant>>,
    mut session: play::PlaySession,
    shared: &Arc<Shared>,
    sender: SyncSender<(ModernEvent, usize)>,
) -> Result<()> {
    let mut checkpoint_sent = false;
    while !shared.closed.load(Ordering::Acquire) {
        *deadline.lock().unwrap() = Instant::now() + Duration::from_secs(30);
        let (id, payload) = reader.read_packet()?;
        if !(0..=0x7b).contains(&id) {
            return Err(invalid("Unknown 1.21.1 packet ID"));
        }
        let event = if id == 0x40 {
            let mut player = shared.player.lock().unwrap();
            if let Some(transform) = player.as_ref() {
                session.position = Some((transform.position, transform.rotation));
            }
            let responses = session.receive(id, &payload).map_err(protocol)?;
            for response in &responses {
                shared.write(response)?;
            }
            let (position, rotation) = session
                .position
                .ok_or_else(|| invalid("Missing teleport result"))?;
            let generation = player.as_ref().map_or(Ok(1), |v| {
                v.generation
                    .checked_add(1)
                    .ok_or_else(|| invalid("Teleport generation overflow"))
            })?;
            let transform = LocalTransform {
                position,
                rotation,
                generation,
            };
            *player = Some(transform.clone());
            Some(ModernEvent::Teleported {
                transform,
                relative_flags: payload[32],
            })
        } else {
            let responses = session.receive(id, &payload).map_err(protocol)?;
            for response in &responses {
                shared.write(response)?;
            }
            match id {
                0x2b => Some(ModernEvent::Joined {
                    join: session.join.as_ref().unwrap().clone(),
                    context: session.context.as_ref().unwrap().clone(),
                }),
                0x27 => {
                    let x = i32::from_be_bytes(payload[0..4].try_into().unwrap());
                    let z = i32::from_be_bytes(payload[4..8].try_into().unwrap());
                    Some(ModernEvent::Chunk(session.chunks[&(x, z)].clone()))
                }
                0x05 | 0x09 | 0x21 | 0x2a | 0x38 | 0x49 => {
                    match play::decode_clientbound767(id, &payload, session.context.as_ref())
                        .map_err(protocol)?
                    {
                        play::Clientbound::BlockActionAcknowledged(sequence) => {
                            shared.actions.lock().unwrap().acknowledge(sequence)?;
                            Some(ModernEvent::BlockActionAcknowledged(sequence))
                        }
                        play::Clientbound::BlockUpdates(updates) => {
                            shared.stats.lock().unwrap().authoritative_block_updates +=
                                updates.len() as u64;
                            Some(ModernEvent::BlockChanges(updates))
                        }
                        play::Clientbound::Abilities(value) => Some(ModernEvent::Abilities(value)),
                        play::Clientbound::Unload { x, z } => Some(ModernEvent::Unload { x, z }),
                        play::Clientbound::Light { x, z, data } => {
                            Some(ModernEvent::Light { x, z, data })
                        }
                        _ => unreachable!(),
                    }
                }
                _ => None,
            }
        };
        {
            let mut stats = shared.stats.lock().unwrap();
            stats.packets_received += 1;
            stats.bytes_received += payload.len() as u64;
            stats.chunks_received = session.decoded_chunk_packets;
            stats.answered_keepalives = session.answered_keepalives;
            stats.acknowledged_batches = session.acknowledged_batches;
            stats.confirmed_teleports = session.confirmed_teleports;
            stats.unimplemented_packets = session.unhandled_packets.clone();
        }
        if let Some(event) = event {
            send_event(shared, &sender, event)?;
        }
        // Interactive movement may have crossed a chunk boundary without a
        // server correction. Check the current player chunk at this cut too.
        if let Some(transform) = shared.player.lock().unwrap().as_ref() {
            session.position = Some((transform.position, transform.rotation));
        }
        if !checkpoint_sent && session.conformance_complete() {
            let stats = shared.stats.lock().unwrap().clone();
            send_event(
                shared,
                &sender,
                ModernEvent::ConformanceCheckpoint { stats },
            )?;
            checkpoint_sent = true;
        }
    }
    Ok(())
}

fn event_bytes(event: &ModernEvent) -> usize {
    match event {
        ModernEvent::Chunk(chunk) => {
            chunk
                .sections
                .iter()
                .map(|s| (s.block_states.len() + s.biomes.len()) * 4)
                .sum::<usize>()
                + chunk.heightmaps.as_bytes().len()
                + chunk.light.byte_count()
                + chunk
                    .block_entities
                    .iter()
                    .map(|e| 32 + e.nbt.as_ref().map_or(0, |n| n.as_bytes().len()))
                    .sum::<usize>()
                + 4096
        }
        ModernEvent::Light { data, .. } => data.byte_count() + 4096,
        ModernEvent::BlockChanges(values) => values.len() * 32 + 4096,
        _ => 4096,
    }
}
fn send_event(
    shared: &Shared,
    sender: &SyncSender<(ModernEvent, usize)>,
    event: ModernEvent,
) -> Result<()> {
    let bytes = event_bytes(&event);
    if bytes > MAX_QUEUED_BYTES {
        return Err(invalid("Event exceeds runtime queue byte budget"));
    }
    let end = Instant::now() + Duration::from_secs(5);
    let mut item = (event, bytes);
    loop {
        if shared.closed.load(Ordering::Acquire) {
            return Err(invalid("Connection closed"));
        }
        let queued = shared.queued_bytes.load(Ordering::Acquire);
        if queued + bytes <= MAX_QUEUED_BYTES {
            // Only the reader adds; the consumer only subtracts.
            shared.queued_bytes.fetch_add(bytes, Ordering::AcqRel);
            match sender.try_send(item) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_)) => {
                    shared.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
                    return Err(invalid("Event consumer closed"));
                }
                Err(TrySendError::Full(returned)) => {
                    item = returned;
                    shared.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
                }
            }
        }
        if Instant::now() >= end {
            return Err(invalid("Event consumer exceeded bounded queue deadline"));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn vanilla(id: &str) -> bool {
    id.split_once(':')
        .map_or(true, |(ns, _)| ns.is_empty() || ns == "minecraft")
}
fn client_information() -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    wire::write_string(&mut bytes, "en_us")?;
    // view distance 2, full chat, colors, model parts, right hand, filtering, listing.
    bytes.extend_from_slice(&[2, 0, 1, 127, 1, 0, 0]);
    Ok(bytes)
}
fn finish(cursor: &Cursor<&[u8]>) -> Result<()> {
    if cursor.position() as usize == cursor.get_ref().len() {
        Ok(())
    } else {
        Err(invalid("Trailing login fields"))
    }
}
fn string(cursor: &mut Cursor<&[u8]>, limit: usize) -> Result<String> {
    let length = wire::read_varint(cursor)?;
    if length < 0
        || length as usize > limit * 3
        || length as usize > cursor.get_ref().len() - cursor.position() as usize
    {
        return Err(invalid("Invalid login string length"));
    }
    let mut bytes = vec![0; length as usize];
    cursor.read_exact(&mut bytes)?;
    let value = String::from_utf8(bytes).map_err(protocol)?;
    if value.encode_utf16().count() > limit {
        return Err(invalid("Login string too long"));
    }
    Ok(value)
}
fn boolean(cursor: &mut Cursor<&[u8]>) -> Result<bool> {
    let mut byte = [0];
    cursor.read_exact(&mut byte)?;
    Ok(byte[0] != 0)
}
fn validate_profile(bytes: &[u8], expected: &str) -> Result<()> {
    let mut r = Cursor::new(bytes);
    let mut uuid = [0; 16];
    r.read_exact(&mut uuid)?;
    if string(&mut r, 16)? != expected {
        return Err(invalid("Unexpected login profile"));
    }
    let count = wire::read_varint(&mut r)?;
    if !(0..=1024).contains(&count) {
        return Err(invalid("Profile property budget"));
    }
    for _ in 0..count {
        string(&mut r, 32767)?;
        string(&mut r, 32767)?;
        if boolean(&mut r)? {
            string(&mut r, 32767)?;
        }
    }
    boolean(&mut r)?;
    finish(&r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn catalog() -> Arc<StateCatalog> {
        Arc::new(
            StateCatalog::from_json(br#"{"minecraft:air":{"states":[{"id":0,"default":true}]}}"#)
                .unwrap(),
        )
    }
    fn text(out: &mut Vec<u8>, value: &str) {
        wire::write_string(out, value).unwrap();
    }
    fn registry(name: &str, entry: &str, nbt: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        text(&mut body, name);
        body.push(1);
        text(&mut body, entry);
        body.push(1);
        body.extend_from_slice(nbt);
        body
    }
    fn join() -> Vec<u8> {
        let mut bytes = 1i32.to_be_bytes().to_vec();
        bytes.extend_from_slice(&[0, 1]);
        text(&mut bytes, "minecraft:overworld");
        bytes.extend_from_slice(&[1, 2, 2, 0, 1, 0, 0]);
        text(&mut bytes, "minecraft:overworld");
        bytes.extend_from_slice(&0i64.to_be_bytes());
        bytes.extend_from_slice(&[1, 255, 0, 0, 0, 0, 0]);
        bytes
    }
    fn teleport(position: [f64; 3], rotation: [f32; 2], flags: u8, id: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in position {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        for value in rotation {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(&[flags, id]);
        bytes
    }
    fn peer_login(connection: &mut wire::Framed<TcpStream>) {
        assert_eq!(connection.read_packet().unwrap().0, 0);
        assert_eq!(connection.read_packet().unwrap().0, 0);
        let mut success = vec![0; 16];
        text(&mut success, "RuntimeTest");
        success.extend_from_slice(&[0, 0]);
        connection.write_packet(2, &success).unwrap();
        assert_eq!(connection.read_packet().unwrap(), (3, vec![]));
        assert_eq!(
            connection.read_packet().unwrap(),
            (0, client_information().unwrap())
        );
        let dimension = b"\x0a\x03\x00\x05min_y\x00\x00\x00\x00\x03\x00\x06height\x00\x00\x00\x10\x01\x00\x0chas_skylight\x01\x00";
        connection
            .write_packet(
                7,
                &registry("minecraft:dimension_type", "minecraft:overworld", dimension),
            )
            .unwrap();
        connection
            .write_packet(
                7,
                &registry("minecraft:worldgen/biome", "minecraft:plains", &[10, 0]),
            )
            .unwrap();
        connection.write_packet(3, &[]).unwrap();
        assert_eq!(connection.read_packet().unwrap(), (3, vec![]));
    }

    #[test]
    fn adapter_uses_latest_position_tracks_actions_and_closes_when_consumer_drops() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let peer = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut connection = wire::Framed::new(stream);
            peer_login(&mut connection);
            connection.write_packet(0x2b, &join()).unwrap();
            connection
                .write_packet(0x40, &teleport([1.0, 2.0, 3.0], [4.0, 5.0], 0, 7))
                .unwrap();
            assert_eq!(connection.read_packet().unwrap(), (0, vec![7]));
            assert_eq!(connection.read_packet().unwrap().0, 0x1b);
            let (id, movement) = connection.read_packet().unwrap();
            assert_eq!(id, 0x1b);
            assert_eq!(f64::from_be_bytes(movement[..8].try_into().unwrap()), 10.0);
            connection
                .write_packet(0x40, &teleport([1.0, 2.0, 3.0], [3.0, 4.0], 31, 8))
                .unwrap();
            assert_eq!(connection.read_packet().unwrap(), (0, vec![8]));
            let (_, correction) = connection.read_packet().unwrap();
            assert_eq!(
                f64::from_be_bytes(correction[..8].try_into().unwrap()),
                11.0
            );
            let (id, dig) = connection.read_packet().unwrap();
            assert_eq!((id, dig[0], *dig.last().unwrap()), (0x24, 0, 1));
            connection.write_packet(5, &[1]).unwrap();
            connection.write_packet(9, &[0; 9]).unwrap();
            let mut abilities = vec![4];
            abilities.extend_from_slice(&0.05f32.to_be_bytes());
            abilities.extend_from_slice(&0.1f32.to_be_bytes());
            connection.write_packet(0x38, &abilities).unwrap();
            connection.write_packet(0x0d, &[]).unwrap();
            let mut chunk = 0i32.to_be_bytes().to_vec();
            chunk.extend_from_slice(&2i32.to_be_bytes());
            chunk.extend_from_slice(&[10, 0, 8]);
            chunk.extend_from_slice(&[0; 8]); // one all-air section, one biome
            chunk.extend_from_slice(&[0; 7]); // no entities, four masks, two array lists
            connection.write_packet(0x27, &chunk).unwrap();
            connection.write_packet(0x0c, &[1]).unwrap();
            assert_eq!(connection.read_packet().unwrap().0, 8);
            for value in [101i64, 102] {
                connection.write_packet(0x26, &value.to_be_bytes()).unwrap();
                assert_eq!(
                    connection.read_packet().unwrap(),
                    (0x18, value.to_be_bytes().to_vec())
                );
            }
            assert!(
                connection.read_packet().is_err(),
                "dropping consumer must close the socket"
            );
            done_tx.send(()).unwrap();
        });
        let (runtime, events) =
            ModernConnection::connect_loopback(address, "RuntimeTest", catalog()).unwrap();
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)).unwrap(),
            ModernEvent::Joined { .. }
        ));
        let mut first = match events.recv_timeout(Duration::from_secs(5)).unwrap() {
            ModernEvent::Teleported { transform, .. } => transform,
            _ => panic!(),
        };
        first.position = [10.0, 20.0, 30.0];
        first.rotation = [40.0, 50.0];
        runtime.move_player(first.clone(), true).unwrap();
        let second = match events.recv_timeout(Duration::from_secs(5)).unwrap() {
            ModernEvent::Teleported {
                transform,
                relative_flags,
            } => {
                assert_eq!(relative_flags, 31);
                transform
            }
            _ => panic!(),
        };
        assert_eq!(second.position, [11.0, 22.0, 33.0]);
        assert_eq!(second.rotation, [43.0, 54.0]);
        assert!(
            runtime.move_player(first, false).is_err(),
            "stale input cannot undo a correction"
        );
        assert_eq!(runtime.dig(0, [0, 0, 0], 1).unwrap(), 1);
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)).unwrap(),
            ModernEvent::BlockActionAcknowledged(1)
        ));
        assert!(
            matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), ModernEvent::BlockChanges(v) if v[0].state_id == 0)
        );
        assert!(
            matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), ModernEvent::Abilities(v) if v.may_fly && !v.flying)
        );
        assert!(
            matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), ModernEvent::Chunk(v) if v.x == 0 && v.z == 2)
        );
        let checkpoint = match events.recv_timeout(Duration::from_secs(5)).unwrap() {
            ModernEvent::ConformanceCheckpoint { stats } => stats,
            _ => panic!("checkpoint must follow the chunk and authoritative updates"),
        };
        assert_eq!(
            (
                checkpoint.answered_keepalives,
                checkpoint.acknowledged_batches,
                checkpoint.chunks_received
            ),
            (2, 1, 1)
        );
        assert_eq!(runtime.stats().confirmed_teleports, 2);
        assert_eq!(runtime.stats().authoritative_block_updates, 1);
        drop(events);
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!runtime.is_connected());
        peer.join().unwrap();
    }

    #[test]
    fn action_sequence_budget_and_ack_order_fail_closed() {
        let mut actions = Actions {
            next: 1,
            acknowledged: 0,
        };
        assert!(actions.acknowledge(1).is_err());
        actions.next = 4;
        actions.acknowledge(3).unwrap();
        assert!(actions.acknowledge(2).is_err());
        actions.next = 5000;
        assert!(actions.next_sequence().is_err());
        actions.next = i32::MAX;
        assert!(actions.next_sequence().is_err());
    }

    #[test]
    fn remote_addresses_and_mod_catalogs_rejected_before_connecting() {
        assert!(ModernConnection::connect_loopback(
            "192.0.2.1:25565".parse().unwrap(),
            "RuntimeTest",
            catalog()
        )
        .is_err());
        let modded = Arc::new(
            StateCatalog::from_json(br#"{"create:test":{"states":[{"id":0,"default":true}]}}"#)
                .unwrap(),
        );
        assert!(ModernConnection::connect_loopback(
            "127.0.0.1:1".parse().unwrap(),
            "RuntimeTest",
            modded
        )
        .is_err());
    }
}
