//! Portable conformance fixtures containing only synthetic numeric identities.
//! The handwritten tools/PaletteReference.java harness invokes a separate
//! Minecraft 1.21.1 PalettedContainer.write implementation for wire independence.
//! No captured world, real registry content, or game archive is embedded here.
use leafish_protocol::protocol::chunk767::decode_sections767;

fn ids(bytes: &[u8]) -> Vec<u32> {
    assert_eq!(bytes.len() % 4, 0);
    bytes
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

#[test]
fn decodes_every_id_from_minecraft_palette_encoder() {
    let sections = decode_sections767(
        include_bytes!("fixtures/chunks-767/sections.bin"),
        -64,
        96,
        65536,
        64,
    )
    .unwrap();
    let expected_blocks = ids(include_bytes!(
        "fixtures/chunks-767/expected-blocks-u32le.bin"
    ));
    let expected_biomes = ids(include_bytes!(
        "fixtures/chunks-767/expected-biomes-u32le.bin"
    ));
    assert_eq!(sections.len(), 6);
    for (index, section) in sections.iter().enumerate() {
        assert_eq!(section.section_y, index as i32 - 4);
        assert_eq!(section.min_block_y(), index as i32 * 16 - 64);
        assert_eq!(
            section.non_empty_block_count,
            if index == 5 { 0 } else { 4096 }
        );
        assert_eq!(
            section.block_states,
            expected_blocks[index * 4096..(index + 1) * 4096]
        );
        assert_eq!(
            section.biomes,
            expected_biomes[index * 64..(index + 1) * 64]
        );
    }
    // A large unknown-to-legacy-Leafish ID is retained, not turned into air.
    assert_eq!(sections[0].block(0, 0, 0), Some(50000));
    assert_eq!(sections[5].block(15, 15, 15), Some(0));
    assert_eq!(sections[2].biome(3, 3, 3), Some(7));
    assert_eq!(sections[3].biome(3, 3, 3), Some(0));
}

#[test]
fn wrong_negotiated_registry_size_fails_instead_of_substitution() {
    assert!(decode_sections767(
        include_bytes!("fixtures/chunks-767/sections.bin"),
        -64,
        96,
        32768,
        64,
    )
    .is_err());
    assert!(decode_sections767(
        include_bytes!("fixtures/chunks-767/sections.bin"),
        -64,
        96,
        65536,
        8,
    )
    .is_err());
}
