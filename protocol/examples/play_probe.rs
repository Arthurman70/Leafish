//! Local-only live Configuration -> Play conformance, without a renderer or mod runtime.
use leafish_protocol::protocol::{
    configuration as config, modern_transport as wire, play767 as play,
};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, Cursor, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// A total wall-clock deadline applies even when individual reads make progress.
struct DeadlineStream {
    stream: TcpStream,
    deadline: Instant,
}
impl Read for DeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Session deadline exceeded"))?;
        self.stream
            .set_read_timeout(Some(remaining.min(Duration::from_secs(20))))?;
        self.stream.read(buffer)
    }
}
impl Write for DeadlineStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Session deadline exceeded"))?;
        self.stream
            .set_write_timeout(Some(remaining.min(Duration::from_secs(5))))?;
        self.stream.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

fn read_string(cursor: &mut Cursor<&[u8]>, limit: usize) -> io::Result<String> {
    let length = wire::read_varint(cursor)?;
    if length < 0 || length as usize > limit * 3 {
        return Err(invalid("String exceeds byte limit"));
    }
    let remaining = cursor
        .get_ref()
        .len()
        .saturating_sub(cursor.position() as usize);
    if length as usize > remaining {
        return Err(invalid("Truncated string"));
    }
    let mut bytes = vec![0; length as usize];
    cursor.read_exact(&mut bytes)?;
    let text = String::from_utf8(bytes).map_err(|_| invalid("String UTF8"))?;
    if text.encode_utf16().count() > limit {
        return Err(invalid("String exceeds UTF16 limit"));
    }
    Ok(text)
}
fn read_bool(cursor: &mut Cursor<&[u8]>) -> io::Result<bool> {
    let mut b = [0];
    cursor.read_exact(&mut b)?;
    Ok(b[0] != 0)
}
fn validate_profile(payload: &[u8], expected: &str) -> io::Result<()> {
    let mut r = Cursor::new(payload);
    let mut uuid = [0; 16];
    r.read_exact(&mut uuid)?;
    if read_string(&mut r, 16)? != expected {
        return Err(invalid("Unexpected login profile"));
    }
    let properties = wire::read_varint(&mut r)?;
    if !(0..=1024).contains(&properties) {
        return Err(invalid("Profile property count"));
    }
    for _ in 0..properties {
        read_string(&mut r, 32767)?;
        read_string(&mut r, 32767)?;
        if read_bool(&mut r)? {
            read_string(&mut r, 32767)?;
        }
    }
    read_bool(&mut r)?;
    if r.position() as usize != payload.len() {
        return Err(invalid("Trailing login profile bytes"));
    }
    Ok(())
}
fn vanilla_identifier(id: &str) -> bool {
    id.split_once(':')
        .map_or(true, |(ns, _)| ns.is_empty() || ns == "minecraft")
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        return Err("Usage: play_probe 127.0.0.1:25586 <exact-block-state-count> (local vanilla reference only)".into());
    }
    let address: SocketAddr = args[1].parse()?;
    if !address.ip().is_loopback() {
        return Err("Play probe accepts loopback addresses only".into());
    }
    let block_state_count: u32 = args[2].parse()?;
    if block_state_count == 0 || block_state_count > i32::MAX as u32 {
        return Err("Invalid block state cardinality".into());
    }
    let started = Instant::now();
    let deadline = started + Duration::from_secs(90);
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    let mut connection = wire::Framed::new(DeadlineStream { stream, deadline });
    let mut handshake = Vec::new();
    wire::write_varint(&mut handshake, 767)?;
    wire::write_string(&mut handshake, &address.ip().to_string())?;
    handshake.extend_from_slice(&address.port().to_be_bytes());
    wire::write_varint(&mut handshake, 2)?;
    connection.write_packet(0, &handshake)?;
    const NAME: &str = "LeafishPlayProbe";
    let mut hello = Vec::new();
    wire::write_string(&mut hello, NAME)?;
    hello.extend_from_slice(&[0; 16]);
    connection.write_packet(0, &hello)?;
    let mut logged_in = false;
    for _ in 0..64 {
        let (id, payload) = connection.read_packet()?;
        match id {
            0 => return Err("Reference rejected login; inspect isolated server log".into()),
            1 => return Err(
                "Online authentication is outside this local probe; no credentials will be used"
                    .into(),
            ),
            2 => {
                validate_profile(&payload, NAME)?;
                connection.write_packet(3, &[])?;
                logged_in = true;
                break;
            }
            3 => {
                let mut r = Cursor::new(payload.as_slice());
                let threshold = wire::read_varint(&mut r)?;
                if r.position() as usize != payload.len() {
                    return Err("Trailing compression bytes".into());
                }
                connection.set_compression(threshold)?;
            }
            4 => {
                let mut r = Cursor::new(payload.as_slice());
                let transaction = wire::read_varint(&mut r)?;
                read_string(&mut r, 32767)?;
                let mut reply = Vec::new();
                wire::write_varint(&mut reply, transaction)?;
                reply.push(0);
                connection.write_packet(2, &reply)?;
            }
            _ => return Err(format!("Unsupported login packet {id}").into()),
        }
    }
    if !logged_in {
        return Err("Login packet budget exceeded".into());
    }
    let mut registries: BTreeMap<String, Vec<config::RegistryEntry>> = BTreeMap::new();
    let mut configuration = config::ConfigurationState::default();
    let mut configuration_complete = false;
    let mut configuration_packets = 0usize;
    let mut configuration_bytes = 0usize;
    for _ in 0..4096 {
        let (id, payload) = connection.read_packet()?;
        configuration_bytes += payload.len();
        if configuration_bytes > 64 * 1024 * 1024 {
            return Err("Configuration byte budget exceeded".into());
        }
        let packet = config::decode_clientbound767(id, &payload)?;
        configuration.received(&packet)?;
        configuration_packets += 1;
        let response = match packet {
            config::Clientbound::KeepAlive(v) => Some(config::Serverbound::KeepAlive(v)),
            config::Clientbound::Ping(v) => Some(config::Serverbound::Pong(v)),
            config::Clientbound::SelectKnownPacks(_) => {
                Some(config::Serverbound::SelectKnownPacks(vec![]))
            }
            config::Clientbound::RegistryData { registry, entries } => {
                if registries.contains_key(&registry)
                    || entries.iter().any(|entry| entry.data.is_none())
                {
                    return Err("Incomplete or duplicate registry".into());
                }
                registries.insert(registry, entries);
                None
            }
            config::Clientbound::CustomPayload { id, .. } => {
                if !vanilla_identifier(&id) {
                    return Err(format!("Unimplemented mod channel {id}").into());
                }
                None
            }
            config::Clientbound::FinishConfiguration => {
                if !registries.contains_key("minecraft:dimension_type")
                    || !registries.contains_key("minecraft:worldgen/biome")
                {
                    return Err("Missing world registries".into());
                }
                let (id, body) =
                    config::encode_serverbound767(&configuration.acknowledge_finish()?)?;
                connection.write_packet(id, &body)?;
                configuration_complete = true;
                break;
            }
            _ => None,
        };
        if let Some(response) = response {
            let (id, body) = config::encode_serverbound767(&response)?;
            connection.write_packet(id, &body)?;
        }
    }
    if !configuration_complete {
        return Err("Configuration packet budget exceeded".into());
    }
    let dimensions = registries.get("minecraft:dimension_type").unwrap().clone();
    let biome_count = registries.get("minecraft:worldgen/biome").unwrap().len() as u32;
    let mut session = play::PlaySession::new(dimensions, block_state_count, biome_count)?;
    let play_started = Instant::now();
    let mut play_bytes = 0usize;
    let mut play_packets = 0usize;
    let mut observed: BTreeMap<i32, usize> = BTreeMap::new();
    for _ in 0..16_384 {
        if play_started.elapsed() > Duration::from_secs(45) {
            return Err("Play conformance deadline exceeded".into());
        }
        let (id, payload) = connection.read_packet()?;
        play_packets += 1;
        play_bytes += payload.len();
        if play_bytes > 128 * 1024 * 1024 {
            return Err("Play byte budget exceeded".into());
        }
        *observed.entry(id).or_default() += 1;
        for response in session.receive(id, &payload)? {
            let (id, body) = play::encode_serverbound767(&response)?;
            connection.write_packet(id, &body)?;
        }
        if session.conformance_complete() {
            let context = session.context.as_ref().unwrap();
            let join = session.join.as_ref().unwrap();
            let registry_counts: BTreeMap<_, _> = registries
                .iter()
                .map(|(name, entries)| (name, entries.len()))
                .collect();
            let sections: usize = session.chunks.values().map(|c| c.sections.len()).sum();
            let state_values: usize = session
                .chunks
                .values()
                .flat_map(|c| &c.sections)
                .map(|s| s.block_states.len())
                .sum();
            let block_entities: usize = session
                .chunks
                .values()
                .map(|c| c.block_entities.len())
                .sum();
            let entity_nbt_bytes: usize = session
                .chunks
                .values()
                .flat_map(|c| &c.block_entities)
                .filter_map(|e| e.nbt.as_ref())
                .map(|n| n.as_bytes().len())
                .sum();
            let heightmap_bytes: usize = session
                .chunks
                .values()
                .map(|c| c.heightmaps.as_bytes().len())
                .sum();
            let light_arrays: usize = session
                .chunks
                .values()
                .map(|c| c.light.array_count())
                .sum::<usize>()
                + session
                    .light_updates
                    .values()
                    .map(|l| l.array_count())
                    .sum::<usize>();
            let max_block_id = session
                .chunks
                .values()
                .flat_map(|c| &c.sections)
                .flat_map(|s| &s.block_states)
                .copied()
                .max();
            let chunk_fingerprints:Vec<_>=session.chunks.values().map(|chunk|{
                let mut blocks=Sha1::new();let mut biomes=Sha1::new();
                for section in &chunk.sections{for id in &section.block_states{blocks.update(id.to_le_bytes());}for id in &section.biomes{biomes.update(id.to_le_bytes());}}
                serde_json::json!({"x":chunk.x,"z":chunk.z,"min_section_y":chunk.sections[0].section_y,"section_count":chunk.sections.len(),
                    "block_state_values":chunk.sections.len()*4096,"biome_values":chunk.sections.len()*64,
                    "block_states_sha1":format!("{:x}",blocks.finalize()),"biomes_sha1":format!("{:x}",biomes.finalize())})
            }).collect();
            let biome_registry_names: Vec<_> = registries
                .get("minecraft:worldgen/biome")
                .unwrap()
                .iter()
                .map(|entry| &entry.id)
                .collect();
            let output = serde_json::json!({
                "scope":"Headless vanilla 1.21.1 Play conformance only; graphical client and NeoForge mod behavior are not enabled",
                "protocol":767,"configuration_complete":true,"play_conformance_complete":true,"production_connected":false,
                "configuration_packets":configuration_packets,"configuration_payload_bytes":configuration_bytes,
                "registry_counts":registry_counts,"registry_entries":registry_counts,"block_state_count":block_state_count,
                "joined_dimension":{"name":join.dimension_name,"type_name":context.dimension.id,"type_id":context.dimension.registry_id,
                    "min_y":context.dimension.min_y,"height":context.dimension.height,"has_skylight":context.dimension.has_skylight},
                "player_position":session.position.unwrap().0,"confirmed_teleports":session.confirmed_teleports,
                "player_chunk_received":session.player_chunk_received(),"decoded_chunk_packets":session.decoded_chunk_packets,
                "retained_chunks":session.chunks.len(),"decoded_sections":sections,"preserved_numeric_block_states":state_values,
                "maximum_observed_block_state_id":max_block_id,"preserved_block_entities":block_entities,
                "chunk_fingerprints":chunk_fingerprints,"biome_registry_names":biome_registry_names,"retained_decoded_bytes":session.retained_bytes(),
                "preserved_block_entity_nbt_bytes":entity_nbt_bytes,"preserved_heightmap_nbt_bytes":heightmap_bytes,
                "preserved_light_arrays":light_arrays,"preserved_light_bytes":light_arrays*2048,
                "acknowledged_batches":session.acknowledged_batches,"answered_keepalives":session.answered_keepalives,
                "play_packets":play_packets,"play_payload_bytes":play_bytes,"observed_packet_ids":observed,
                "unhandled_packet_ids":session.unhandled_packets,"elapsed_ms":started.elapsed().as_millis()as u64,
                "disconnect":"TCP shutdown after bounded conformance success"
            });
            connection.into_inner().stream.shutdown(Shutdown::Both)?;
            println!("{}", serde_json::to_string_pretty(&output)?);
            return Ok(());
        }
    }
    Err("Play packet budget exceeded".into())
}
