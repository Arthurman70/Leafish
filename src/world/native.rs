//! Exact registry-backed modern chunk storage, separate from legacy Block enums.
//!
//! This is a data boundary, not a substitute block mapping or a protocol switch.
//! The caller must bind the catalog to the connection's registry (for vanilla,
//! the official version-specific report); cardinality alone is not identity proof.
use super::{Position, WorldBounds};
use leafish_blocks::catalog::{NamedState, StateCatalog};
use leafish_protocol::protocol::chunk767::ChunkSection767;
use leafish_protocol::protocol::play767::{BlockUpdate, ChunkWithLight, LightData, WorldContext};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub const MAX_NATIVE_CHUNKS: usize = 256;
pub const MAX_NATIVE_BYTES: usize = 64 * 1024 * 1024;
const MAX_BLOCK_UPDATES: usize = 65_536;

#[derive(Debug, PartialEq, Eq)]
pub enum NativeStoreError {
    Invalid(&'static str),
    UnknownBlockState(u32),
    UnloadedChunk(i32, i32),
    Limit(&'static str),
}
impl std::fmt::Display for NativeStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "modern world storage: {:?}", self)
    }
}
impl std::error::Error for NativeStoreError {}
type Result<T> = std::result::Result<T, NativeStoreError>;

pub struct NativeChunk {
    /// Exact section IDs, quart biomes, entity NBT, and merged light data.
    pub data: ChunkWithLight,
    /// Authoritative block changes invalidate the original section-count and
    /// heightmap metadata. Never cull an updated section using its old count.
    pub changed_sections: BTreeSet<i32>,
}
impl NativeChunk {
    pub fn heightmaps_current(&self) -> bool {
        self.changed_sections.is_empty()
    }
    /// Block changes may remove or replace an entity. Until its authoritative
    /// update is implemented, consumers must not display the old entity data.
    pub fn block_entities_current(&self) -> bool {
        self.changed_sections.is_empty()
    }
    pub fn section_count_current(&self, section_y: i32) -> bool {
        !self.changed_sections.contains(&section_y)
    }
}

pub struct NativeChunkStore {
    context: WorldContext,
    bounds: WorldBounds,
    catalog: Arc<StateCatalog>,
    chunks: BTreeMap<(i32, i32), NativeChunk>,
    retained_bytes: usize,
}
impl NativeChunkStore {
    pub fn new(context: WorldContext, catalog: Arc<StateCatalog>) -> Result<Self> {
        let bounds = WorldBounds::new(context.dimension.min_y, context.dimension.height)
            .map_err(|_| NativeStoreError::Invalid("dimension bounds"))?;
        if context.block_state_count == 0 || context.block_state_count != catalog.state_count() {
            return Err(NativeStoreError::Invalid(
                "catalog cardinality differs from connection",
            ));
        }
        if context.biome_count == 0 {
            return Err(NativeStoreError::Invalid("empty biome registry"));
        }
        Ok(Self {
            context,
            bounds,
            catalog,
            chunks: BTreeMap::new(),
            retained_bytes: 0,
        })
    }
    pub fn bounds(&self) -> WorldBounds {
        self.bounds
    }
    pub fn context(&self) -> &WorldContext {
        &self.context
    }
    pub fn catalog(&self) -> &StateCatalog {
        &self.catalog
    }
    pub fn len(&self) -> usize {
        self.chunks.len()
    }
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
    pub fn chunk(&self, x: i32, z: i32) -> Option<&ChunkWithLight> {
        self.chunks.get(&(x, z)).map(|c| &c.data)
    }
    pub fn chunk_state(&self, x: i32, z: i32) -> Option<&NativeChunk> {
        self.chunks.get(&(x, z))
    }
    pub fn chunks(&self) -> impl Iterator<Item = (&(i32, i32), &NativeChunk)> {
        self.chunks.iter()
    }

    pub fn insert_chunk(&mut self, chunk: ChunkWithLight) -> Result<()> {
        self.validate_chunk(&chunk)?;
        let key = (chunk.x, chunk.z);
        if !self.chunks.contains_key(&key) && self.chunks.len() >= MAX_NATIVE_CHUNKS {
            return Err(NativeStoreError::Limit("chunk count"));
        }
        let old = self.chunks.get(&key).map_or(0, |c| chunk_bytes(&c.data));
        let replacement = self.replacement_budget(old, chunk_bytes(&chunk))?;
        self.chunks.insert(
            key,
            NativeChunk {
                data: chunk,
                changed_sections: BTreeSet::new(),
            },
        );
        self.retained_bytes = replacement;
        Ok(())
    }
    pub fn unload_chunk(&mut self, x: i32, z: i32) -> bool {
        if let Some(chunk) = self.chunks.remove(&(x, z)) {
            self.retained_bytes -= chunk_bytes(&chunk.data);
            true
        } else {
            false
        }
    }
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.retained_bytes = 0;
    }

    pub fn block_state_id(&self, pos: Position) -> Option<u32> {
        let section = self.section(pos)?;
        Some(section.block_states[block_index(pos)])
    }
    pub fn biome_id(&self, pos: Position) -> Option<u32> {
        let section = self.section(pos)?;
        let index =
            (((pos.y & 15) >> 2) * 16 + ((pos.z & 15) >> 2) * 4 + ((pos.x & 15) >> 2)) as usize;
        Some(section.biomes[index])
    }
    pub fn named_state(&self, pos: Position) -> Result<Option<&NamedState>> {
        match self.block_state_id(pos) {
            Some(id) => self
                .catalog
                .state(id)
                .map(Some)
                .map_err(|_| NativeStoreError::UnknownBlockState(id)),
            None => Ok(None),
        }
    }
    fn section(&self, pos: Position) -> Option<&ChunkSection767> {
        let index = self.bounds.block_section_index(pos.y)?;
        self.chunk(pos.x >> 4, pos.z >> 4)?.sections.get(index)
    }

    /// Every update is checked before any state is changed, including updates
    /// late in a batch. Repeated positions retain server wire order.
    pub fn apply_block_updates(&mut self, updates: &[BlockUpdate]) -> Result<()> {
        if updates.len() > MAX_BLOCK_UPDATES {
            return Err(NativeStoreError::Limit("block update count"));
        }
        for update in updates {
            let [x, y, z] = update.position;
            if !self.bounds.contains_y(y) {
                return Err(NativeStoreError::Invalid("block update height"));
            }
            self.check_state(update.state_id)?;
            if !self.chunks.contains_key(&(x >> 4, z >> 4)) {
                return Err(NativeStoreError::UnloadedChunk(x >> 4, z >> 4));
            }
        }
        for update in updates {
            let [x, y, z] = update.position;
            let index = self.bounds.block_section_index(y).unwrap();
            let chunk = self.chunks.get_mut(&(x >> 4, z >> 4)).unwrap();
            let value =
                &mut chunk.data.sections[index].block_states[block_index(Position::new(x, y, z))];
            if *value != update.state_id {
                *value = update.state_id;
                chunk.changed_sections.insert(y.div_euclid(16));
            }
        }
        Ok(())
    }

    /// Merge partial light updates: set bits replace, empty bits zero, absent
    /// bits preserve previous state. Boundary sections are retained too.
    pub fn apply_light(&mut self, x: i32, z: i32, update: LightData) -> Result<()> {
        validate_light(&update, self.bounds)?;
        let chunk = self
            .chunks
            .get(&(x, z))
            .ok_or(NativeStoreError::UnloadedChunk(x, z))?;
        let merged = merge_light(&chunk.data.light, &update);
        let replacement =
            self.replacement_budget(light_bytes(&chunk.data.light), light_bytes(&merged))?;
        self.chunks.get_mut(&(x, z)).unwrap().data.light = merged;
        self.retained_bytes = replacement;
        Ok(())
    }
    /// None denotes missing light data, not guessed sky brightness. Explicit
    /// empty-mask sections return zero. Queries include both boundary sections.
    pub fn light_at(&self, pos: Position, sky: bool) -> Option<u8> {
        let light = &self.chunk(pos.x >> 4, pos.z >> 4)?.light;
        let index = pos.y.div_euclid(16).checked_sub(light.min_section_y)?;
        if index < 0 || index as usize >= light.section_count {
            return None;
        }
        let (mask, empty, arrays) = if sky {
            (&light.sky_mask, &light.empty_sky_mask, &light.sky_arrays)
        } else {
            (
                &light.block_mask,
                &light.empty_block_mask,
                &light.block_arrays,
            )
        };
        match light_section(mask, empty, arrays, index as usize) {
            LightSection::Data(array) => {
                let index = block_index(pos);
                Some((array[index / 2] >> ((index & 1) * 4)) & 15)
            }
            LightSection::Empty => Some(0),
            LightSection::Unknown => None,
        }
    }

    fn check_state(&self, id: u32) -> Result<()> {
        self.catalog
            .state(id)
            .map(|_| ())
            .map_err(|_| NativeStoreError::UnknownBlockState(id))
    }
    fn validate_chunk(&self, chunk: &ChunkWithLight) -> Result<()> {
        if chunk.sections.len() != self.bounds.section_count() {
            return Err(NativeStoreError::Invalid("section count"));
        }
        if chunk.heightmaps.as_bytes().first() != Some(&10) {
            return Err(NativeStoreError::Invalid("heightmap compound"));
        }
        for (index, section) in chunk.sections.iter().enumerate() {
            if self.bounds.section_y(index) != Some(section.section_y)
                || section.block_states.len() != 4096
                || section.biomes.len() != 64
            {
                return Err(NativeStoreError::Invalid("section shape"));
            }
            for &id in &section.block_states {
                self.check_state(id)?;
            }
            if section
                .biomes
                .iter()
                .any(|&id| id >= self.context.biome_count)
            {
                return Err(NativeStoreError::Invalid("biome registry ID"));
            }
        }
        if chunk.block_entities.len() > 65_536 {
            return Err(NativeStoreError::Limit("block entities"));
        }
        for entity in &chunk.block_entities {
            if !self.bounds.contains_y(i32::from(entity.y)) {
                return Err(NativeStoreError::Invalid("block entity height"));
            }
            if entity
                .nbt
                .as_ref()
                .map_or(false, |nbt| nbt.as_bytes().first() != Some(&10))
            {
                return Err(NativeStoreError::Invalid("block entity compound"));
            }
        }
        validate_light(&chunk.light, self.bounds)
    }
    fn replacement_budget(&self, old: usize, new: usize) -> Result<usize> {
        self.retained_bytes
            .checked_sub(old)
            .and_then(|v| v.checked_add(new))
            .filter(|&v| v <= MAX_NATIVE_BYTES)
            .ok_or(NativeStoreError::Limit("decoded retained bytes"))
    }
}
fn block_index(pos: Position) -> usize {
    (((pos.y & 15) << 8) | ((pos.z & 15) << 4) | (pos.x & 15)) as usize
}
fn bit(mask: &[u64], index: usize) -> bool {
    mask.get(index / 64)
        .map_or(false, |word| word & (1u64 << (index % 64)) != 0)
}
fn set_bit(mask: &mut [u64], index: usize) {
    mask[index / 64] |= 1u64 << (index % 64);
}
fn validate_light(data: &LightData, bounds: WorldBounds) -> Result<()> {
    let count = bounds.section_count() + 2;
    if data.min_section_y != bounds.min_section_y() - 1 || data.section_count != count {
        return Err(NativeStoreError::Invalid("light dimension"));
    }
    for mask in [
        &data.sky_mask,
        &data.block_mask,
        &data.empty_sky_mask,
        &data.empty_block_mask,
    ] {
        if mask.len() > (count + 63) / 64 {
            return Err(NativeStoreError::Invalid("light mask length"));
        }
        for index in count..mask.len() * 64 {
            if bit(mask, index) {
                return Err(NativeStoreError::Invalid("light mask height"));
            }
        }
    }
    for (mask, empty, arrays) in [
        (&data.sky_mask, &data.empty_sky_mask, &data.sky_arrays),
        (&data.block_mask, &data.empty_block_mask, &data.block_arrays),
    ] {
        if arrays.len() != mask.iter().map(|w| w.count_ones() as usize).sum::<usize>()
            || arrays.iter().any(|a| a.len() != 2048)
        {
            return Err(NativeStoreError::Invalid("light array shape"));
        }
        for index in 0..count {
            if bit(mask, index) && bit(empty, index) {
                return Err(NativeStoreError::Invalid("overlapping light masks"));
            }
        }
    }
    Ok(())
}
enum LightSection<'a> {
    Unknown,
    Empty,
    Data(&'a [u8]),
}
fn light_section<'a>(
    mask: &[u64],
    empty: &[u64],
    arrays: &'a [Vec<u8>],
    index: usize,
) -> LightSection<'a> {
    if bit(mask, index) {
        let previous: usize = mask
            .iter()
            .take(index / 64)
            .map(|w| w.count_ones() as usize)
            .sum();
        let below = mask[index / 64] & ((1u64 << (index % 64)) - 1);
        LightSection::Data(&arrays[previous + below.count_ones() as usize])
    } else if bit(empty, index) {
        LightSection::Empty
    } else {
        LightSection::Unknown
    }
}
fn merge_light(old: &LightData, update: &LightData) -> LightData {
    let count = old.section_count;
    let mut result = LightData {
        min_section_y: old.min_section_y,
        section_count: count,
        sky_mask: vec![0; (count + 63) / 64],
        block_mask: vec![0; (count + 63) / 64],
        empty_sky_mask: vec![0; (count + 63) / 64],
        empty_block_mask: vec![0; (count + 63) / 64],
        sky_arrays: Vec::new(),
        block_arrays: Vec::new(),
    };
    for (mask, empty, arrays, old_mask, old_empty, old_arrays, new_mask, new_empty, new_arrays) in [
        (
            &mut result.sky_mask,
            &mut result.empty_sky_mask,
            &mut result.sky_arrays,
            &old.sky_mask,
            &old.empty_sky_mask,
            &old.sky_arrays,
            &update.sky_mask,
            &update.empty_sky_mask,
            &update.sky_arrays,
        ),
        (
            &mut result.block_mask,
            &mut result.empty_block_mask,
            &mut result.block_arrays,
            &old.block_mask,
            &old.empty_block_mask,
            &old.block_arrays,
            &update.block_mask,
            &update.empty_block_mask,
            &update.block_arrays,
        ),
    ] {
        for index in 0..count {
            let next = match light_section(new_mask, new_empty, new_arrays, index) {
                LightSection::Unknown => light_section(old_mask, old_empty, old_arrays, index),
                value => value,
            };
            match next {
                LightSection::Data(array) => {
                    set_bit(mask, index);
                    arrays.push(array.to_vec());
                }
                LightSection::Empty => set_bit(empty, index),
                LightSection::Unknown => {}
            }
        }
    }
    result
}
fn light_bytes(light: &LightData) -> usize {
    std::mem::size_of::<LightData>()
        + [
            &light.sky_mask,
            &light.block_mask,
            &light.empty_sky_mask,
            &light.empty_block_mask,
        ]
        .iter()
        .map(|v| v.capacity() * 8)
        .sum::<usize>()
        + [&light.sky_arrays, &light.block_arrays]
            .iter()
            .map(|v| {
                v.capacity() * std::mem::size_of::<Vec<u8>>()
                    + v.iter().map(Vec::capacity).sum::<usize>()
            })
            .sum::<usize>()
}
fn chunk_bytes(chunk: &ChunkWithLight) -> usize {
    // Reserve bounded metadata for one changed marker per negotiated section.
    std::mem::size_of::<NativeChunk>()
        + chunk.sections.len() * 256
        + chunk.heightmaps.as_bytes().len()
        + chunk.sections.capacity() * std::mem::size_of::<ChunkSection767>()
        + chunk
            .sections
            .iter()
            .map(|s| (s.block_states.capacity() + s.biomes.capacity()) * 4)
            .sum::<usize>()
        + chunk.block_entities.capacity()
            * std::mem::size_of::<leafish_protocol::protocol::play767::BlockEntity>()
        + chunk
            .block_entities
            .iter()
            .filter_map(|e| e.nbt.as_ref())
            .map(|n| n.as_bytes().len())
            .sum::<usize>()
        + light_bytes(&chunk.light)
}

#[cfg(test)]
mod tests {
    use super::*;
    use leafish_protocol::protocol::configuration::NetworkNbt;
    use leafish_protocol::protocol::play767::{BlockEntity, Dimension};

    fn catalog() -> Arc<StateCatalog> {
        Arc::new(
            StateCatalog::from_json(
                br#"{
            "minecraft:air":{"states":[{"id":0,"default":true}]},
            "minecraft:stone":{"states":[{"id":1,"default":true}]},
            "minecraft:oak_log":{"properties":{"axis":["x","y","z"]},"states":[
                {"id":2,"default":true,"properties":{"axis":"y"}},
                {"id":3,"properties":{"axis":"x"}},
                {"id":4,"properties":{"axis":"z"}}]}
        }"#,
            )
            .unwrap(),
        )
    }
    fn context() -> WorldContext {
        WorldContext {
            dimension: Dimension {
                registry_id: 0,
                id: "minecraft:overworld".into(),
                min_y: -64,
                height: 384,
                has_skylight: true,
            },
            block_state_count: 5,
            biome_count: 3,
        }
    }
    fn empty_light(bounds: WorldBounds) -> LightData {
        LightData {
            min_section_y: bounds.min_section_y() - 1,
            section_count: bounds.section_count() + 2,
            sky_mask: vec![],
            block_mask: vec![],
            empty_sky_mask: vec![],
            empty_block_mask: vec![],
            sky_arrays: vec![],
            block_arrays: vec![],
        }
    }
    fn chunk() -> ChunkWithLight {
        let bounds = WorldBounds::new(-64, 384).unwrap();
        ChunkWithLight {
            x: -1,
            z: 2,
            heightmaps: NetworkNbt::from_bytes(&[10, 0]).unwrap(),
            sections: (0..24)
                .map(|i| ChunkSection767 {
                    section_y: bounds.section_y(i).unwrap(),
                    non_empty_block_count: 0,
                    block_states: vec![(i % 5) as u32; 4096],
                    biomes: vec![(i % 3) as u32; 64],
                })
                .collect(),
            block_entities: vec![BlockEntity {
                packed_xz: 0x12,
                y: -60,
                type_id: 999,
                nbt: Some(NetworkNbt::from_bytes(&[10, 0]).unwrap()),
            }],
            light: empty_light(bounds),
        }
    }
    #[test]
    fn preserves_ids_quart_biomes_signed_coordinates_and_nbt() {
        let mut store = NativeChunkStore::new(context(), catalog()).unwrap();
        let mut original = chunk();
        original.sections[0].block_states[4095] = 4;
        original.sections[0].biomes[63] = 2;
        store.insert_chunk(original.clone()).unwrap();
        assert_eq!(store.chunk(-1, 2), Some(&original));
        for y in [-64, -17, -16, -1, 0, 255, 256, 319] {
            let index = ((y + 64) / 16) as usize;
            assert_eq!(
                store.block_state_id(Position::new(-16, y, 32)),
                Some((index % 5) as u32)
            );
            assert_eq!(
                store.biome_id(Position::new(-16, y, 32)),
                Some((index % 3) as u32)
            );
        }
        assert_eq!(
            store
                .named_state(Position::new(-1, -49, 47))
                .unwrap()
                .unwrap()
                .properties()
                .get("axis")
                .map(String::as_str),
            Some("z")
        );
        assert_eq!(store.biome_id(Position::new(-1, -49, 47)), Some(2));
        for pos in [
            Position::new(-1, -65, 32),
            Position::new(-1, 320, 32),
            Position::new(0, 0, 32),
        ] {
            assert_eq!(store.block_state_id(pos), None);
            assert!(store.named_state(pos).unwrap().is_none());
        }
    }
    #[test]
    fn catalog_cardinality_and_missing_biomes_rejected() {
        let mut wrong = context();
        wrong.block_state_count = 6;
        assert!(NativeChunkStore::new(wrong, catalog()).is_err());
        let mut wrong = context();
        wrong.biome_count = 0;
        assert!(NativeChunkStore::new(wrong, catalog()).is_err());
    }
    #[test]
    fn malformed_replacements_leave_original_chunk_and_budget_unchanged() {
        let mut store = NativeChunkStore::new(context(), catalog()).unwrap();
        let original = chunk();
        store.insert_chunk(original.clone()).unwrap();
        let bytes = store.retained_bytes();
        for failure in 0..5 {
            let mut malformed = original.clone();
            match failure {
                0 => malformed.sections[23].block_states[4095] = 5,
                1 => malformed.sections[23].biomes[63] = 3,
                2 => malformed.sections[0].section_y = 0,
                3 => {
                    malformed.sections.pop();
                }
                _ => malformed.light.block_mask = vec![1],
            }
            assert!(store.insert_chunk(malformed).is_err());
            assert_eq!(store.chunk(-1, 2), Some(&original));
            assert_eq!(store.retained_bytes(), bytes);
        }
        store.insert_chunk(original).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.retained_bytes(), bytes);
    }
    #[test]
    fn authoritative_updates_are_transactional_ordered_and_invalidate_metadata() {
        let mut store = NativeChunkStore::new(context(), catalog()).unwrap();
        store.insert_chunk(chunk()).unwrap();
        let pos = Position::new(-1, -49, 47);
        let first = BlockUpdate {
            position: [pos.x, pos.y, pos.z],
            state_id: 2,
        };
        let invalid = BlockUpdate {
            position: [pos.x, 320, pos.z],
            state_id: 1,
        };
        assert!(store
            .apply_block_updates(&[first.clone(), invalid])
            .is_err());
        assert_eq!(store.block_state_id(pos), Some(0));
        assert!(store.chunk_state(-1, 2).unwrap().heightmaps_current());
        let missing = BlockUpdate {
            position: [0, 0, 0],
            state_id: 1,
        };
        assert!(store
            .apply_block_updates(&[first.clone(), missing])
            .is_err());
        let unknown = BlockUpdate {
            position: [pos.x, pos.y, pos.z],
            state_id: 99,
        };
        assert!(store
            .apply_block_updates(&[first.clone(), unknown])
            .is_err());
        store
            .apply_block_updates(&[
                first.clone(),
                BlockUpdate {
                    state_id: 4,
                    ..first
                },
            ])
            .unwrap();
        assert_eq!(store.block_state_id(pos), Some(4));
        let state = store.chunk_state(-1, 2).unwrap();
        assert!(!state.heightmaps_current());
        assert!(!state.section_count_current(-4));
        assert!(state.section_count_current(-3));
        assert_eq!(state.data.sections[0].non_empty_block_count, 0); // original wire metadata
    }
    #[test]
    fn partial_light_preserves_boundaries_and_absent_bits() {
        let mut store = NativeChunkStore::new(context(), catalog()).unwrap();
        let mut initial = chunk();
        initial.light.sky_mask = vec![(1 << 0) | (1 << 1) | (1 << 25)];
        initial.light.sky_arrays = vec![vec![0x21; 2048], vec![0x43; 2048], vec![0x65; 2048]];
        store.insert_chunk(initial).unwrap();
        assert_eq!(store.light_at(Position::new(-16, -80, 32), true), Some(1));
        assert_eq!(store.light_at(Position::new(-15, -80, 32), true), Some(2));
        assert_eq!(store.light_at(Position::new(-16, 320, 32), true), Some(5));
        let mut update = empty_light(store.bounds());
        update.empty_sky_mask = vec![1 << 1];
        update.sky_mask = vec![1 << 2];
        update.sky_arrays = vec![vec![0xfe; 2048]];
        store.apply_light(-1, 2, update).unwrap();
        assert_eq!(store.light_at(Position::new(-16, -80, 32), true), Some(1));
        assert_eq!(store.light_at(Position::new(-16, -64, 32), true), Some(0));
        assert_eq!(store.light_at(Position::new(-16, -48, 32), true), Some(14));
        assert_eq!(store.light_at(Position::new(-15, -48, 32), true), Some(15));
        assert_eq!(store.light_at(Position::new(-16, 320, 32), true), Some(5));
        assert_eq!(store.light_at(Position::new(-16, 0, 32), true), None);
        let before = store.chunk(-1, 2).unwrap().clone();
        let mut bad = empty_light(store.bounds());
        bad.sky_mask = vec![1];
        bad.empty_sky_mask = vec![1];
        bad.sky_arrays = vec![vec![0; 2048]];
        assert!(store.apply_light(-1, 2, bad).is_err());
        assert_eq!(store.chunk(-1, 2), Some(&before));
    }
    #[test]
    fn light_masks_cross_word_boundary_for_tall_dimensions() {
        let mut tall_context = context();
        tall_context.dimension.height = 1024;
        let mut store = NativeChunkStore::new(tall_context, catalog()).unwrap();
        let mut tall = chunk();
        tall.sections = (0..64)
            .map(|index| ChunkSection767 {
                section_y: store.bounds().section_y(index).unwrap(),
                non_empty_block_count: 0,
                block_states: vec![0; 4096],
                biomes: vec![0; 64],
            })
            .collect();
        tall.light = empty_light(store.bounds());
        tall.light.block_mask = vec![1u64 << 63, 3];
        tall.light.block_arrays = vec![vec![0x11; 2048], vec![0x22; 2048], vec![0x33; 2048]];
        store.insert_chunk(tall).unwrap();
        for (y, value) in [(928, 1), (944, 2), (960, 3)] {
            assert_eq!(
                store.light_at(Position::new(-16, y, 32), false),
                Some(value)
            );
        }
        let mut update = empty_light(store.bounds());
        update.empty_block_mask = vec![0, 1];
        store.apply_light(-1, 2, update).unwrap();
        assert_eq!(store.light_at(Position::new(-16, 928, 32), false), Some(1));
        assert_eq!(store.light_at(Position::new(-16, 944, 32), false), Some(0));
        assert_eq!(store.light_at(Position::new(-16, 960, 32), false), Some(3));
        let mut invalid = empty_light(store.bounds());
        invalid.empty_block_mask = vec![0, 4];
        assert!(store.apply_light(-1, 2, invalid).is_err());
    }

    #[test]
    fn chunk_count_limit_is_enforced_without_evicting_existing_data() {
        let mut small_context = context();
        small_context.dimension.min_y = 0;
        small_context.dimension.height = 16;
        let mut store = NativeChunkStore::new(small_context, catalog()).unwrap();
        let mut small = chunk();
        small.sections.truncate(1);
        small.sections.shrink_to_fit();
        small.sections[0].section_y = 0;
        small.block_entities.clear();
        small.block_entities.shrink_to_fit();
        small.light = empty_light(store.bounds());
        for x in 0..MAX_NATIVE_CHUNKS {
            small.x = x as i32;
            store.insert_chunk(small.clone()).unwrap();
        }
        let retained = store.retained_bytes();
        small.x = MAX_NATIVE_CHUNKS as i32;
        assert!(store.insert_chunk(small.clone()).is_err());
        assert_eq!(store.retained_bytes(), retained);
        assert_eq!(store.len(), MAX_NATIVE_CHUNKS);
        small.x = 0;
        store.insert_chunk(small).unwrap();
        assert_eq!(store.len(), MAX_NATIVE_CHUNKS);
        assert_eq!(store.retained_bytes(), retained);
    }

    #[test]
    fn bounded_retention_replacement_and_unload() {
        let mut store = NativeChunkStore::new(context(), catalog()).unwrap();
        assert_eq!(
            store.replacement_budget(0, MAX_NATIVE_BYTES),
            Ok(MAX_NATIVE_BYTES)
        );
        assert!(store.replacement_budget(0, MAX_NATIVE_BYTES + 1).is_err());
        assert!(store.replacement_budget(1, 1).is_err());
        assert!(store.replacement_budget(0, usize::MAX).is_err());
        store.insert_chunk(chunk()).unwrap();
        assert!(store.retained_bytes() > 24 * 4160 * 4);
        assert!(store.unload_chunk(-1, 2));
        assert_eq!(store.retained_bytes(), 0);
        assert!(!store.unload_chunk(-1, 2));
        assert!(store.block_state_id(Position::new(-1, 0, 32)).is_none());
        store.insert_chunk(chunk()).unwrap();
        store.clear();
        assert_eq!(store.len(), 0);
        assert_eq!(store.retained_bytes(), 0);
    }
}
