//! Live conformance check, deliberately limited to a loopback reference server.
//! Completes the 1.21.1 configuration phase, then disconnects before gameplay.
//! This is not a playable protocol advertisement or a NeoForge mod handshake.
use leafish_protocol::protocol::configuration::{
    decode_clientbound767, encode_serverbound767, Clientbound, ConfigurationState, Serverbound,
};
use leafish_protocol::protocol::modern_transport::{
    read_varint, write_string, write_varint, Framed,
};
use std::error::Error;
use std::io::{self, Cursor, Read};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_string(cursor: &mut Cursor<&[u8]>, limit: usize) -> io::Result<String> {
    let length = read_varint(cursor)?;
    if length < 0 || length as usize > limit * 3 {
        return Err(invalid("String exceeds limit"));
    }
    let mut bytes = vec![0; length as usize];
    cursor.read_exact(&mut bytes)?;
    let text = String::from_utf8(bytes).map_err(|_| invalid("Invalid string UTF-8"))?;
    if text.encode_utf16().count() > limit {
        return Err(invalid("String exceeds character limit"));
    }
    Ok(text)
}

fn read_bool(cursor: &mut Cursor<&[u8]>) -> io::Result<bool> {
    let mut byte = [0];
    cursor.read_exact(&mut byte)?;
    match byte[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid("Invalid boolean")),
    }
}

fn validate_profile(payload: &[u8]) -> io::Result<()> {
    let mut cursor = Cursor::new(payload);
    let mut uuid = [0; 16];
    cursor.read_exact(&mut uuid)?;
    if read_string(&mut cursor, 16)? != "LeafishProbe" {
        return Err(invalid("Unexpected login profile"));
    }
    let properties = read_varint(&mut cursor)?;
    if !(0..=1024).contains(&properties) {
        return Err(invalid("Too many profile properties"));
    }
    for _ in 0..properties {
        read_string(&mut cursor, 32767)?;
        read_string(&mut cursor, 32767)?;
        if read_bool(&mut cursor)? {
            read_string(&mut cursor, 32767)?;
        }
    }
    read_bool(&mut cursor)?; // 1.21.1 strictErrorHandling
    if cursor.position() as usize != payload.len() {
        return Err(invalid("Trailing profile bytes"));
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        return Err("Usage: configuration_probe 127.0.0.1:25586 (local test only)".into());
    }
    let address: SocketAddr = args[1].parse()?;
    if !address.ip().is_loopback() {
        return Err("Reference probe only accepts loopback addresses".into());
    }
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut wire = Framed::new(stream);
    let mut handshake = Vec::new();
    write_varint(&mut handshake, 767)?;
    write_string(&mut handshake, &address.ip().to_string())?;
    handshake.extend_from_slice(&address.port().to_be_bytes());
    write_varint(&mut handshake, 2)?;
    wire.write_packet(0, &handshake)?;
    let mut hello = Vec::new();
    write_string(&mut hello, "LeafishProbe")?;
    hello.extend_from_slice(&[0; 16]);
    wire.write_packet(0, &hello)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut logged_in = false;
    for _ in 0..64 {
        if Instant::now() > deadline {
            return Err("Login deadline exceeded".into());
        }
        let (id, payload) = wire.read_packet()?;
        match id {
            0 => return Err("Reference server rejected login (inspect its isolated log)".into()),
            1 => return Err("Reference server requires online authentication; probe will not use account credentials".into()),
            2 => { validate_profile(&payload)?; wire.write_packet(3, &[])?; logged_in = true; break; },
            3 => {
                let mut cursor = Cursor::new(payload.as_slice());
                let threshold = read_varint(&mut cursor)?;
                if cursor.position() as usize != payload.len() { return Err("Trailing compression bytes".into()); }
                wire.set_compression(threshold)?;
            },
            4 => {
                let mut cursor = Cursor::new(payload.as_slice());
                let transaction = read_varint(&mut cursor)?;
                read_string(&mut cursor,32767)?;
                let mut reply = Vec::new();
                write_varint(&mut reply, transaction)?;
                reply.push(0); // Explicitly decline every unknown login plugin.
                wire.write_packet(2, &reply)?;
            },
            _ => return Err(format!("Unsupported login packet {id}").into()),
        }
    }
    if !logged_in {
        return Err("Login packet budget exceeded".into());
    }
    let mut state = ConfigurationState::default();
    let mut registries = serde_json::Map::new();
    let mut packet_count = 0usize;
    let mut nbt_bytes = 0usize;
    let mut total_bytes = 0usize;
    for _ in 0..4096 {
        if Instant::now() > deadline {
            return Err("Configuration deadline exceeded".into());
        }
        let (id, payload) = wire.read_packet()?;
        total_bytes += payload.len();
        if total_bytes > 64 * 1024 * 1024 {
            return Err("Configuration exceeds total byte budget".into());
        }
        let packet = decode_clientbound767(id, &payload)?;
        state.received(&packet)?;
        packet_count += 1;
        let response = match packet {
            Clientbound::KeepAlive(value) => Some(Serverbound::KeepAlive(value)),
            Clientbound::Ping(value) => Some(Serverbound::Pong(value)),
            Clientbound::SelectKnownPacks(_) => Some(Serverbound::SelectKnownPacks(vec![])),
            Clientbound::RegistryData { registry, entries } => {
                // Empty known-packs response requires full registry NBT from the server.
                if entries.iter().any(|entry| entry.data.is_none()) {
                    return Err("Registry data omitted despite no shared packs".into());
                }
                if registries.contains_key(&registry) {
                    return Err("Duplicate registry snapshot".into());
                }
                nbt_bytes += entries
                    .iter()
                    .filter_map(|entry| entry.data.as_ref())
                    .map(|data| data.as_bytes().len())
                    .sum::<usize>();
                registries.insert(registry, serde_json::json!(entries.len()));
                None
            }
            Clientbound::CustomPayload { id, .. } => {
                if !id.starts_with("minecraft:") {
                    return Err(format!("Mod channel requires implementation: {id}").into());
                }
                None
            }
            Clientbound::FinishConfiguration => {
                if !registries.contains_key("minecraft:dimension_type")
                    || !registries.contains_key("minecraft:worldgen/biome")
                {
                    return Err("Reference configuration lacks required world registries".into());
                }
                let (id, payload) = encode_serverbound767(&state.acknowledge_finish()?)?;
                wire.write_packet(id, &payload)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "scope":"Leafish 1.21.1 configuration conformance only; gameplay and NeoForge not implemented",
                        "protocol":767,"configuration_complete":true,"configuration_packets":packet_count,
                        "registry_entries":registries,"preserved_network_nbt_bytes":nbt_bytes,
                        "configuration_payload_bytes":total_bytes,"production_connected":false
                    }))?
                );
                return Ok(());
            }
            _ => None,
        };
        if let Some(response) = response {
            let (id, payload) = encode_serverbound767(&response)?;
            wire.write_packet(id, &payload)?;
        }
    }
    Err("Configuration packet budget exceeded".into())
}
