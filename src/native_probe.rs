//! Live verification of the same modern adapter and native store used by the
//! client port. This deliberately does not enter the unfinished graphical path.
use crate::server::modern::{ModernConnection, ModernEvent};
use crate::world::native::NativeChunkStore;
use leafish_blocks::catalog::StateCatalog;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::error::Error;
use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn run(address: SocketAddr, catalog_path: &Path) -> Result<Value, Box<dyn Error>> {
    if !address.ip().is_loopback() {
        return Err("Native verification requires a separate loopback reference server".into());
    }
    let started = Instant::now();
    let mut bytes = Vec::new();
    File::open(catalog_path)?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Block catalog exceeds size limit".into());
    }
    let catalog = Arc::new(StateCatalog::from_json(&bytes)?);
    let catalog_hash = format!("{:x}", Sha1::digest(&bytes));
    let (connection, events) =
        ModernConnection::connect_loopback(address, "LeafishNative", catalog.clone())?;
    let mut store: Option<NativeChunkStore> = None;
    let mut dimension_name = None;
    let mut position = None;
    let mut movement_packets = 0;
    let mut applied_updates = 0usize;
    let mut light_updates = 0;
    let mut abilities_seen = false;
    let mut event_count = 0;
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if Instant::now() > deadline || event_count > 16384 {
            return Err("Native verification exceeded its bounded session budget".into());
        }
        let mut checkpoint = None;
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                event_count += 1;
                match event {
                    ModernEvent::Joined { join, context } => {
                        if store.is_some() {
                            return Err("Unexpected second joined dimension".into());
                        }
                        dimension_name = Some(join.dimension_name);
                        store = Some(NativeChunkStore::new(context, catalog.clone())?);
                    }
                    ModernEvent::Teleported { transform, .. } => {
                        // Echo only the authoritative transform; this does not
                        // invent local movement, ground contact, or physics.
                        position = Some(transform.position);
                        connection.move_player(transform, false)?;
                        movement_packets += 1;
                    }
                    ModernEvent::Chunk(chunk) => {
                        store
                            .as_mut()
                            .ok_or("Chunk before join")?
                            .insert_chunk(chunk)?;
                    }
                    ModernEvent::Light { x, z, data } => {
                        store
                            .as_mut()
                            .ok_or("Light before join")?
                            .apply_light(x, z, data)?;
                        light_updates += 1;
                    }
                    ModernEvent::Unload { x, z } => {
                        store
                            .as_mut()
                            .ok_or("Unload before join")?
                            .unload_chunk(x, z);
                    }
                    ModernEvent::BlockChanges(updates) => {
                        store
                            .as_mut()
                            .ok_or("Block change before join")?
                            .apply_block_updates(&updates)?;
                        applied_updates += updates.len();
                    }
                    ModernEvent::Abilities(_) => abilities_seen = true,
                    ModernEvent::BlockActionAcknowledged(_) => {}
                    ModernEvent::ConformanceCheckpoint { stats } => checkpoint = Some(stats),
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(format!(
                    "Native connection ended: {:?}",
                    connection.stats().close_reason
                )
                .into());
            }
        }
        // Only a FIFO checkpoint proves all preceding world events were applied
        // and that the reader closed its chunk batch and packet bundle. Live
        // statistics can race ahead of this event consumer and cannot prove it.
        let stats = match checkpoint {
            Some(stats) => stats,
            None => continue,
        };
        let player_chunk_received = position.zip(store.as_ref()).map_or(false, |(pos, world)| {
            world
                .chunk((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4)
                .is_some()
        });
        if !player_chunk_received
            || stats.answered_keepalives < 2
            || stats.acknowledged_batches == 0
            || stats.confirmed_teleports == 0
            || movement_packets == 0
            || !abilities_seen
        {
            return Err("Ordered native checkpoint lacks required applied world evidence".into());
        }
        let world = store.as_ref().unwrap();
        let context = world.context();
        let mut fingerprints = Vec::new();
        let mut sections = 0;
        let mut light_arrays = 0;
        let mut entities = 0;
        for (_, chunk) in world.chunks() {
            let chunk = &chunk.data;
            let mut blocks = Sha1::new();
            let mut biomes = Sha1::new();
            for section in &chunk.sections {
                for id in &section.block_states {
                    // Check the actual stored identities against the catalog,
                    // not the protocol's transient decoded copy.
                    catalog.state(*id)?;
                    blocks.update(id.to_le_bytes());
                }
                for id in &section.biomes {
                    biomes.update(id.to_le_bytes());
                }
            }
            sections += chunk.sections.len();
            light_arrays += chunk.light.array_count();
            entities += chunk.block_entities.len();
            fingerprints.push(json!({
                "x":chunk.x, "z":chunk.z, "min_section_y":chunk.sections[0].section_y,
                "section_count":chunk.sections.len(), "block_state_values":chunk.sections.len()*4096,
                "biome_values":chunk.sections.len()*64,
                "block_states_sha1":format!("{:x}", blocks.finalize()),
                "biomes_sha1":format!("{:x}", biomes.finalize())
            }));
        }
        connection.close();
        return Ok(json!({
            "scope":"Native adapter and registry-backed world storage only; no graphical gameplay, NeoForge, or server replacement",
            "protocol":767, "configuration_complete":true, "play_conformance_complete":true,
            "native_world_verified":true, "production_connected":false,
            "block_state_count":catalog.len(), "block_catalog_sha1":catalog_hash,
            "registry_counts":connection.registry_counts(),
            "biome_registry_names":connection.biome_registry_names(),
            "joined_dimension":{"name":dimension_name,"type_name":context.dimension.id,
                "type_id":context.dimension.registry_id,"min_y":context.dimension.min_y,
                "height":context.dimension.height,"has_skylight":context.dimension.has_skylight},
            "player_position":position,"player_chunk_received":player_chunk_received,
            "confirmed_teleports":stats.confirmed_teleports,
            "acknowledged_batches":stats.acknowledged_batches,
            "answered_keepalives":stats.answered_keepalives,
            "decoded_chunk_packets":stats.chunks_received,"retained_chunks":world.len(),
            "decoded_sections":sections,"preserved_numeric_block_states":sections*4096,
            "preserved_light_arrays":light_arrays,"preserved_block_entities":entities,
            "applied_light_updates":light_updates,"applied_authoritative_block_updates":applied_updates,
            "movement_packets_sent":movement_packets,"player_abilities_received":abilities_seen,
            "retained_decoded_bytes":world.retained_bytes(),"chunk_fingerprints":fingerprints,
            "unhandled_packet_ids":stats.unimplemented_packets,"play_packets":stats.packets_received,
            "play_payload_bytes":stats.bytes_received,"elapsed_ms":started.elapsed().as_millis() as u64
        }));
    }
}
