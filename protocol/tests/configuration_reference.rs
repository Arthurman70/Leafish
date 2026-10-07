//! Opt-in conformance check against locally captured Minecraft 1.21.1 packets.
//! Captured registry data is private and is not included in the repository.
//! Set LEAFISH_CONFIGURATION_FIXTURES to a directory containing manifest.json
//! and its relative body/wire files, then explicitly run this ignored test.
use leafish_protocol::protocol::configuration::{
    decode_clientbound767, encode_serverbound767, Clientbound, ConfigurationState,
};
use leafish_protocol::protocol::modern_transport::Framed;
use serde_json::Value;
use std::{collections::BTreeMap, fs, io::Cursor, path::PathBuf};

#[test]
#[ignore = "requires private fixtures selected by LEAFISH_CONFIGURATION_FIXTURES"]
fn minecraft_1211_configuration_frames_decode_without_loss() {
    let root = std::env::var_os("LEAFISH_CONFIGURATION_FIXTURES")
        .map(PathBuf::from)
        .expect("Set LEAFISH_CONFIGURATION_FIXTURES to the private configuration fixture directory before explicitly running this ignored test");
    let manifest: Value =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["protocol_version"], 767);
    let mut state = ConfigurationState::default();
    let mut registries = BTreeMap::new();
    let mut entries = 0usize;
    let mut finished = false;
    let mut compressed = 0usize;
    let mut nbt_bytes = 0usize;
    for frame in manifest["frames"].as_array().unwrap() {
        let id = frame["packet_id"].as_i64().unwrap() as i32;
        let expected_body = fs::read(root.join(frame["body_file"].as_str().unwrap())).unwrap();
        let wire_bytes = fs::read(root.join(frame["wire_file"].as_str().unwrap())).unwrap();
        let mut wire = Framed::new(Cursor::new(wire_bytes));
        wire.set_compression(frame["compression_threshold"].as_i64().unwrap() as i32)
            .unwrap();
        let (decoded_id, body) = wire.read_packet().unwrap();
        assert_eq!(decoded_id, id);
        assert_eq!(
            body, expected_body,
            "transport must preserve exact reference bytes"
        );
        compressed += usize::from(frame["compressed_on_wire"].as_bool().unwrap());
        let packet = decode_clientbound767(id, &body).unwrap();
        state.received(&packet).unwrap();
        match packet {
            Clientbound::RegistryData {
                registry,
                entries: values,
            } => {
                assert!(
                    values.iter().all(|v| v.data.is_some()),
                    "no cached packs were selected"
                );
                for value in &values {
                    let raw = value.data.as_ref().unwrap().as_bytes();
                    assert_eq!(raw[0], 10, "reference registry root is compound");
                    assert!(
                        body.windows(raw.len()).any(|part| part == raw),
                        "network NBT retained byte for byte"
                    );
                    nbt_bytes += raw.len();
                }
                entries += values.len();
                assert!(registries.insert(registry, values.len()).is_none());
            }
            Clientbound::SelectKnownPacks(packs) => {
                assert_eq!(packs.len(), 1);
                assert_eq!(packs[0].namespace, "minecraft");
                assert_eq!(packs[0].id, "core");
                assert_eq!(packs[0].version, "1.21.1");
            }
            Clientbound::FinishConfiguration => {
                assert_eq!(
                    encode_serverbound767(&state.acknowledge_finish().unwrap()).unwrap(),
                    (3, vec![])
                );
                finished = true;
            }
            Clientbound::CustomPayload { id, .. } => assert_eq!(id, "minecraft:brand"),
            Clientbound::EnabledFeatures(features) => {
                assert_eq!(features, vec!["minecraft:vanilla"])
            }
            Clientbound::UpdateTags(tags) => assert!(!tags.is_empty()),
            other => panic!("Unexpected reference packet: {:?}", other),
        }
    }
    assert!(finished);
    assert_eq!(registries.len(), 11);
    assert_eq!(entries, 313);
    assert!(registries.contains_key("minecraft:dimension_type"));
    assert!(registries.contains_key("minecraft:worldgen/biome"));
    assert!(compressed > 0);
    assert!(nbt_bytes > 0);
}
