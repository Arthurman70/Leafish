//! Bounded Minecraft Java 1.21.1 section-data decoder (protocol 767).
//!
//! Input is ONLY the chunk packet's section-data byte array, without its length,
//! coordinates, heightmaps, block entities, or light data. Sections occur bottom
//! to top; their signed Y coordinates come from the negotiated dimension.
//! Registry counts must describe the connection's contiguous numeric ID maps,
//! not a guessed vanilla registry. Numeric IDs are never replaced with air.
//!
//! Ground truth: `neoforge-21.1.236-sources.jar`, LevelChunkSection.read/write,
//! PalettedContainer.Strategy/Data, SingleValuePalette, LinearPalette,
//! HashMapPalette, GlobalPalette, and SimpleBitStorage. This codec is not wired
//! into Leafish's Play protocol and does not enable or advertise version 767.

use std::convert::TryInto;
use std::fmt;

pub const BLOCKS_PER_SECTION: usize = 16 * 16 * 16;
pub const BIOMES_PER_SECTION: usize = 4 * 4 * 4;
/// Local resource bounds, separate from validation of DimensionType semantics.
pub const MAX_SECTIONS: usize = 256;
pub const MAX_SECTION_DATA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerKind {
    Blocks,
    Biomes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChunkDecodeError {
    InvalidDimension,
    InvalidRegistryCount(ContainerKind),
    LimitExceeded(&'static str),
    Truncated,
    InvalidVarInt,
    NegativeValue,
    InvalidPaletteLength {
        kind: ContainerKind,
        count: usize,
        max: usize,
    },
    InvalidLongArrayLength {
        kind: ContainerKind,
        actual: usize,
        expected: usize,
    },
    RegistryIdOutOfRange {
        kind: ContainerKind,
        id: u32,
        registry_count: u32,
    },
    PaletteIndexOutOfRange {
        kind: ContainerKind,
        index: u32,
        palette_len: usize,
    },
    TrailingBytes(usize),
}

impl fmt::Display for ChunkDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Minecraft 1.21.1 section data: {:?}", self)
    }
}
impl std::error::Error for ChunkDecodeError {}
pub type Result<T> = std::result::Result<T, ChunkDecodeError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkSection767 {
    /// World section coordinate, not array index: e.g. -64 blocks -> section -4.
    pub section_y: i32,
    /// Exact signed short sent by LevelChunkSection. Do not derive this from a
    /// guessed air ID. The NeoForge source's recalcBlockCounts also counts fluids
    /// separately, so this field is not restricted to 0..=4096 here.
    pub non_empty_block_count: i16,
    /// 4096 registry IDs; index = (local_y * 16 + local_z) * 16 + local_x.
    pub block_states: Vec<u32>,
    /// 64 registry IDs; index = (quart_y * 4 + quart_z) * 4 + quart_x.
    pub biomes: Vec<u32>,
}

impl ChunkSection767 {
    pub fn block(&self, local_x: usize, local_y: usize, local_z: usize) -> Option<u32> {
        if local_x >= 16 || local_y >= 16 || local_z >= 16 {
            return None;
        }
        self.block_states
            .get((local_y * 16 + local_z) * 16 + local_x)
            .copied()
    }

    pub fn biome(&self, quart_x: usize, quart_y: usize, quart_z: usize) -> Option<u32> {
        if quart_x >= 4 || quart_y >= 4 || quart_z >= 4 {
            return None;
        }
        self.biomes
            .get((quart_y * 4 + quart_z) * 4 + quart_x)
            .copied()
    }

    pub fn min_block_y(&self) -> i32 {
        self.section_y * 16
    }
}

/// Decode exactly height/16 sections. min_y and height must be multiples of 16,
/// height must be positive, and min_y+height must not overflow i32. Dimensions
/// are supplied by the caller after validating the negotiated dimension type.
/// A registry count must be in 1..=i32::MAX (wire IDs are nonnegative VarInts).
/// Any error discards the partial result; no partially decoded chunk is returned.
pub fn decode_sections767(
    bytes: &[u8],
    min_y: i32,
    height: u32,
    block_state_count: u32,
    biome_count: u32,
) -> Result<Vec<ChunkSection767>> {
    if min_y % 16 != 0
        || height == 0
        || height % 16 != 0
        || height > i32::MAX as u32
        || min_y.checked_add(height as i32).is_none()
    {
        return Err(ChunkDecodeError::InvalidDimension);
    }
    let section_count = (height / 16) as usize;
    if section_count > MAX_SECTIONS {
        return Err(ChunkDecodeError::LimitExceeded("sections"));
    }
    for (kind, count) in [
        (ContainerKind::Blocks, block_state_count),
        (ContainerKind::Biomes, biome_count),
    ] {
        if count == 0 || count > i32::MAX as u32 {
            return Err(ChunkDecodeError::InvalidRegistryCount(kind));
        }
    }
    if bytes.len() > MAX_SECTION_DATA_BYTES {
        return Err(ChunkDecodeError::LimitExceeded("section bytes"));
    }
    // Short + two minimal global containers (selector, empty long-array length).
    // A singleton registry can use global ZeroBitStorage without a palette ID.
    if bytes.len() < section_count * 6 {
        return Err(ChunkDecodeError::Truncated);
    }
    let mut reader = Reader { bytes, pos: 0 };
    let mut sections = Vec::with_capacity(section_count);
    for index in 0..section_count {
        let non_empty_block_count = i16::from_be_bytes(reader.take(2)?.try_into().unwrap());
        let block_states = read_container(&mut reader, ContainerKind::Blocks, block_state_count)?;
        let biomes = read_container(&mut reader, ContainerKind::Biomes, biome_count)?;
        sections.push(ChunkSection767 {
            section_y: min_y / 16 + index as i32,
            non_empty_block_count,
            block_states,
            biomes,
        });
    }
    if reader.remaining() != 0 {
        return Err(ChunkDecodeError::TrailingBytes(reader.remaining()));
    }
    Ok(sections)
}

fn read_container(
    reader: &mut Reader<'_>,
    kind: ContainerKind,
    registry_count: u32,
) -> Result<Vec<u32>> {
    let wire_bits = reader.u8()?;
    let (size, indirect_max) = match kind {
        ContainerKind::Blocks => (BLOCKS_PER_SECTION, 8),
        ContainerKind::Biomes => (BIOMES_PER_SECTION, 3),
    };
    if wire_bits == 0 {
        let id = read_registry_id(reader, kind, registry_count)?;
        read_long_count(reader, kind, 0)?;
        return Ok(vec![id; size]);
    }

    // Strategy.SECTION_STATES normalizes selectors 1,2,3,4 to four storage bits.
    // Other selectors outside the indirect range choose GlobalPalette; its
    // storage width comes from the actual registry, NOT from the wire byte.
    // This includes negative readByte values (128..255 on the wire) in Java.
    let indirect = wire_bits <= indirect_max;
    let bits = if indirect {
        if kind == ContainerKind::Blocks {
            wire_bits.max(4)
        } else {
            wire_bits
        }
    } else {
        (32 - (registry_count - 1).leading_zeros()) as u8
    };
    let palette = if indirect {
        let count = reader.nonnegative_varint()? as usize;
        let max = 1usize << bits;
        if count == 0 || count > max {
            return Err(ChunkDecodeError::InvalidPaletteLength { kind, count, max });
        }
        if count > reader.remaining() {
            return Err(ChunkDecodeError::Truncated);
        }
        let mut palette = Vec::with_capacity(count);
        for _ in 0..count {
            palette.push(read_registry_id(reader, kind, registry_count)?);
        }
        Some(palette)
    } else {
        None
    };

    // A one-entry registry uses ZeroBitStorage even with the global strategy.
    let values_per_long = if bits == 0 { 0 } else { 64 / bits as usize };
    let expected_longs = if bits == 0 {
        0
    } else {
        (size + values_per_long - 1) / values_per_long
    };
    read_long_count(reader, kind, expected_longs)?;
    let packed = reader.take(expected_longs * 8)?;
    if bits == 0 {
        return Ok(vec![0; size]);
    }
    let mask = (1u64 << bits) - 1;
    let mut result = Vec::with_capacity(size);
    for long_bytes in packed.chunks_exact(8) {
        // Big-endian LONG on the wire; entries start at its least-significant bit.
        // Values never straddle longs. Padding at each long's top is ignored.
        let word = u64::from_be_bytes(long_bytes.try_into().unwrap());
        for slot in 0..values_per_long.min(size - result.len()) {
            let value = ((word >> (slot * bits as usize)) & mask) as u32;
            let id = if let Some(palette) = &palette {
                *palette
                    .get(value as usize)
                    .ok_or(ChunkDecodeError::PaletteIndexOutOfRange {
                        kind,
                        index: value,
                        palette_len: palette.len(),
                    })?
            } else {
                validate_registry_id(kind, value, registry_count)?;
                value
            };
            result.push(id);
        }
    }
    Ok(result)
}

fn read_registry_id(reader: &mut Reader<'_>, kind: ContainerKind, count: u32) -> Result<u32> {
    let id = reader.nonnegative_varint()?;
    validate_registry_id(kind, id, count)?;
    Ok(id)
}

fn validate_registry_id(kind: ContainerKind, id: u32, registry_count: u32) -> Result<()> {
    if id >= registry_count {
        return Err(ChunkDecodeError::RegistryIdOutOfRange {
            kind,
            id,
            registry_count,
        });
    }
    Ok(())
}

fn read_long_count(reader: &mut Reader<'_>, kind: ContainerKind, expected: usize) -> Result<()> {
    let actual = reader.nonnegative_varint()? as usize;
    if actual != expected {
        return Err(ChunkDecodeError::InvalidLongArrayLength {
            kind,
            actual,
            expected,
        });
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        if len > self.remaining() {
            return Err(ChunkDecodeError::Truncated);
        }
        let start = self.pos;
        self.pos += len;
        Ok(&self.bytes[start..self.pos])
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn nonnegative_varint(&mut self) -> Result<u32> {
        let mut value = 0u32;
        for shift in 0..5 {
            let byte = self.u8()?;
            if shift == 4 && byte & 0xf0 != 0 {
                return Err(ChunkDecodeError::InvalidVarInt);
            }
            value |= ((byte & 0x7f) as u32) << (shift * 7);
            if byte & 0x80 == 0 {
                if value > i32::MAX as u32 {
                    return Err(ChunkDecodeError::NegativeValue);
                }
                return Ok(value);
            }
        }
        Err(ChunkDecodeError::InvalidVarInt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(out: &mut Vec<u8>, mut value: u32) {
        loop {
            let mut byte = (value & 127) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 128;
            }
            out.push(byte);
            if value == 0 {
                break;
            }
        }
    }
    fn single(id: u32) -> Vec<u8> {
        let mut bytes = vec![0];
        varint(&mut bytes, id);
        bytes.push(0);
        bytes
    }
    fn packed(wire_bits: u8, storage_bits: u8, palette: Option<&[u32]>, values: &[u32]) -> Vec<u8> {
        let mut bytes = vec![wire_bits];
        if let Some(palette) = palette {
            varint(&mut bytes, palette.len() as u32);
            for &id in palette {
                varint(&mut bytes, id);
            }
        }
        let per_long = 64 / storage_bits as usize;
        varint(
            &mut bytes,
            ((values.len() + per_long - 1) / per_long) as u32,
        );
        for chunk in values.chunks(per_long) {
            let mut word = 0u64;
            for (slot, &value) in chunk.iter().enumerate() {
                word |= (value as u64) << (slot * storage_bits as usize);
            }
            bytes.extend_from_slice(&word.to_be_bytes());
        }
        bytes
    }
    fn section(count: i16, blocks: &[u8], biomes: &[u8]) -> Vec<u8> {
        let mut bytes = count.to_be_bytes().to_vec();
        bytes.extend_from_slice(blocks);
        bytes.extend_from_slice(biomes);
        bytes
    }
    fn decode(bytes: &[u8], blocks: u32, biomes: u32) -> Result<ChunkSection767> {
        Ok(decode_sections767(bytes, -64, 16, blocks, biomes)?.remove(0))
    }

    #[test]
    fn fixed_single_palette_fixture_and_signed_section_y() {
        // short 4096; block bits0,id300,longcount0; biome bits0,id5,longcount0.
        let bytes = [0x10, 0, 0, 0xac, 2, 0, 0, 5, 0];
        let s = decode(&bytes, 301, 6).unwrap();
        assert_eq!(s.section_y, -4);
        assert_eq!(s.min_block_y(), -64);
        assert_eq!(s.non_empty_block_count, 4096);
        assert_eq!(s.block_states, vec![300; 4096]);
        assert_eq!(s.biomes, vec![5; 64]);
        assert_eq!(s.block(15, 15, 15), Some(300));
        assert_eq!(s.block(16, 0, 0), None);
        assert_eq!(s.biome(3, 3, 3), Some(5));
        assert_eq!(s.biome(0, 4, 0), None);
    }

    #[test]
    fn tall_dimension_sections_are_bottom_to_top_without_unsigned_wrap() {
        let mut bytes = vec![];
        for n in 0..64 {
            bytes.extend(section(5000 + n as i16, &single(n), &single(n % 6)));
        }
        let sections = decode_sections767(&bytes, -512, 1024, 64, 6).unwrap();
        assert_eq!(sections.len(), 64);
        for (i, s) in sections.iter().enumerate() {
            assert_eq!(s.section_y, i as i32 - 32);
            assert_eq!(s.block(0, 0, 0), Some(i as u32));
            assert_eq!(s.non_empty_block_count, 5000 + i as i16);
        }
        // It is a signed source field, not a count reconstructed from ID zero.
        assert_eq!(
            decode(&section(-1, &single(1), &single(0)), 2, 1)
                .unwrap()
                .non_empty_block_count,
            -1
        );
    }

    #[test]
    fn fixed_five_bit_words_do_not_straddle_and_padding_is_ignored() {
        // 5-bit values 0..11 in first long, 12 in next. Top four bits are padding.
        let mut container = vec![5, 17];
        container.extend(0..17);
        container.extend_from_slice(&[0xd6, 0x02]); // ceil(4096 / 12) = 342 longs
        container.extend_from_slice(&0xf5a9_2839_8a41_8820u64.to_be_bytes());
        container.extend_from_slice(&12u64.to_be_bytes());
        container.resize(container.len() + 340 * 8, 0);
        let s = decode(&section(4096, &container, &single(0)), 17, 1).unwrap();
        assert_eq!(
            &s.block_states[..13],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        );
        assert_eq!(s.block_states[13], 0);
    }

    #[test]
    fn block_index_order_is_y_z_x_and_low_wire_bits_normalize_to_four() {
        let mut values = vec![0; 4096];
        values[1] = 1;
        values[16] = 2;
        values[256] = 3;
        values[4095] = 4;
        for wire_bits in 1..=4 {
            let b = packed(wire_bits, 4, Some(&[20, 21, 22, 23, 24]), &values);
            let s = decode(&section(5, &b, &single(0)), 25, 1).unwrap();
            assert_eq!(s.block(1, 0, 0), Some(21));
            assert_eq!(s.block(0, 0, 1), Some(22));
            assert_eq!(s.block(0, 1, 0), Some(23));
            assert_eq!(s.block(15, 15, 15), Some(24));
        }
    }

    #[test]
    fn direct_palette_width_comes_from_dynamic_registry_not_header() {
        let mut values = vec![299; 4096];
        values[6] = 128;
        values[7] = 257;
        for selector in [9, 15, 32, 255] {
            let b = packed(selector, 9, None, &values); // registry size 300 -> 9 bits
            let s = decode(&section(4096, &b, &single(0)), 300, 1).unwrap();
            assert_eq!(s.block_states, values);
        }
        // Global strategy with a singleton registry has zero storage bits.
        assert_eq!(
            decode(&section(0, &[9, 0], &single(0)), 1, 1)
                .unwrap()
                .block_states,
            vec![0; 4096]
        );
    }

    #[test]
    fn biome_indirect_widths_and_global_ids_are_preserved() {
        for bits in 1..=3 {
            let palette: Vec<u32> = (10..10 + (1 << bits)).collect();
            let indices: Vec<u32> = (0..64).map(|n| n % palette.len() as u32).collect();
            let b = packed(bits, bits, Some(&palette), &indices);
            let s = decode(&section(0, &single(0), &b), 1, 100).unwrap();
            assert_eq!(
                s.biomes,
                indices
                    .iter()
                    .map(|&i| palette[i as usize])
                    .collect::<Vec<_>>()
            );
        }
        let values: Vec<u32> = (0..64).collect();
        let s = decode(&section(0, &single(0), &packed(4, 6, None, &values)), 1, 64).unwrap();
        assert_eq!(s.biome(1, 0, 0), Some(1));
        assert_eq!(s.biome(0, 0, 1), Some(4));
        assert_eq!(s.biome(0, 1, 0), Some(16));
        assert_eq!(s.biomes, values);
    }

    #[test]
    fn palette_references_and_registry_ids_never_fall_back_to_air() {
        assert!(matches!(
            decode(&section(0, &single(42), &single(0)), 42, 1),
            Err(ChunkDecodeError::RegistryIdOutOfRange {
                kind: ContainerKind::Blocks,
                id: 42,
                ..
            })
        ));
        assert!(matches!(
            decode(&section(0, &single(0), &single(1)), 1, 1),
            Err(ChunkDecodeError::RegistryIdOutOfRange {
                kind: ContainerKind::Biomes,
                id: 1,
                ..
            })
        ));
        let bad = packed(4, 4, Some(&[0, 10]), &vec![2; 4096]);
        assert!(matches!(
            decode(&section(0, &bad, &single(0)), 11, 1),
            Err(ChunkDecodeError::PaletteIndexOutOfRange {
                index: 2,
                palette_len: 2,
                ..
            })
        ));
        let bad = packed(9, 9, None, &vec![511; 4096]);
        assert!(matches!(
            decode(&section(0, &bad, &single(0)), 300, 1),
            Err(ChunkDecodeError::RegistryIdOutOfRange { id: 511, .. })
        ));
    }

    #[test]
    fn wrong_palette_and_long_counts_are_rejected_before_allocation() {
        for container in [&[4, 0][..], &[4, 17][..]] {
            assert!(matches!(
                decode(&section(0, container, &[0; 8]), 100, 1),
                Err(ChunkDecodeError::InvalidPaletteLength { .. })
            ));
        }
        assert!(matches!(
            decode(&section(0, &[0, 0, 1], &single(0)), 1, 1),
            Err(ChunkDecodeError::InvalidLongArrayLength {
                expected: 0,
                actual: 1,
                ..
            })
        ));
        assert!(matches!(
            decode(&section(0, &[4, 1, 0, 0], &single(0)), 1, 1),
            Err(ChunkDecodeError::InvalidLongArrayLength {
                expected: 256,
                actual: 0,
                ..
            })
        ));
        let negative = [0, 255, 255, 255, 255, 15];
        assert_eq!(
            decode(&section(0, &negative, &single(0)), 1, 1),
            Err(ChunkDecodeError::NegativeValue)
        );
        let overflow = [0, 255, 255, 255, 255, 127];
        assert_eq!(
            decode(&section(0, &overflow, &single(0)), 1, 1),
            Err(ChunkDecodeError::InvalidVarInt)
        );
    }

    #[test]
    fn all_truncations_and_extra_bytes_are_errors() {
        let fixture = section(1, &packed(5, 5, Some(&[0, 1]), &vec![1; 4096]), &single(0));
        assert!(decode(&fixture, 2, 1).is_ok());
        for end in 0..fixture.len() {
            assert!(decode(&fixture[..end], 2, 1).is_err(), "prefix {}", end);
        }
        let mut extra = fixture;
        extra.push(0);
        assert_eq!(
            decode(&extra, 2, 1),
            Err(ChunkDecodeError::TrailingBytes(1))
        );
    }

    #[test]
    fn dimensions_and_input_resources_are_bounded() {
        for (min_y, height) in [
            (-63, 16),
            (-64, 0),
            (-64, 17),
            (i32::MAX - 15, 16),
            (0, u32::MAX),
        ] {
            assert_eq!(
                decode_sections767(&[], min_y, height, 1, 1),
                Err(ChunkDecodeError::InvalidDimension)
            );
        }
        assert_eq!(
            decode_sections767(&[], 0, (MAX_SECTIONS as u32 + 1) * 16, 1, 1),
            Err(ChunkDecodeError::LimitExceeded("sections"))
        );
        for (blocks, biomes) in [(0, 1), (1, 0), (u32::MAX, 1), (1, u32::MAX)] {
            assert!(matches!(
                decode_sections767(&[], 0, 16, blocks, biomes),
                Err(ChunkDecodeError::InvalidRegistryCount(_))
            ));
        }
        assert_eq!(
            decode_sections767(&vec![0; MAX_SECTION_DATA_BYTES + 1], 0, 16, 1, 1),
            Err(ChunkDecodeError::LimitExceeded("section bytes"))
        );
    }
}
