use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use leafish_protocol::types::hash::FNVHash;
use leafish_shared::position::Position;

use crate::world::{biome, WorldBounds};

pub use self::chunk_section::*;
use bevy_ecs::prelude::Entity;

mod chunk_section;

#[derive(PartialEq, Eq, Hash, Clone, Copy, PartialOrd, Ord)]
pub struct CPos(pub i32, pub i32);

#[derive(Clone)]
pub struct Chunk {
    pub(crate) position: CPos,

    pub(crate) sections: Vec<Option<ChunkSection>>,
    pub(crate) bounds: WorldBounds,
    pub(crate) sections_rendered_on: Vec<u32>,
    pub(crate) biomes: [u8; 16 * 16],

    pub(crate) heightmap: [i32; 16 * 16],
    pub(crate) heightmap_dirty: bool,

    pub(crate) block_entities: HashMap<Position, Entity, BuildHasherDefault<FNVHash>>,
}

impl Chunk {
    pub(crate) fn new(pos: CPos) -> Chunk {
        Self::with_bounds(pos, WorldBounds::LEGACY)
    }

    pub(crate) fn with_bounds(pos: CPos, bounds: WorldBounds) -> Chunk {
        Chunk {
            position: pos,
            sections: vec![None; bounds.section_count()],
            sections_rendered_on: vec![0; bounds.section_count()],
            bounds,
            biomes: [0; 16 * 16],
            heightmap: [bounds.min_y() - 1; 16 * 16],
            heightmap_dirty: true,
            block_entities: HashMap::with_hasher(BuildHasherDefault::default()),
        }
    }

    pub fn bounds(&self) -> WorldBounds {
        self.bounds
    }
    pub fn section_index(&self, section_y: i32) -> Option<usize> {
        self.bounds.section_index(section_y)
    }
    pub fn section(&self, section_y: i32) -> Option<&ChunkSection> {
        self.sections
            .get(self.section_index(section_y)?)
            .and_then(Option::as_ref)
    }
    pub fn section_mut(&mut self, section_y: i32) -> Option<&mut ChunkSection> {
        let index = self.section_index(section_y)?;
        self.sections.get_mut(index).and_then(Option::as_mut)
    }

    fn recalculate_column_height(&mut self, x: i32, z: i32) {
        let height = (self.bounds.min_y()..self.bounds.max_y_exclusive())
            .rev()
            .find(|&y| !matches!(self.get_block(x, y, z), block::Air {}))
            .unwrap_or(self.bounds.min_y() - 1);
        self.heightmap[((z << 4) | x) as usize] = height;
        self.heightmap_dirty = true;
    }

    pub(crate) fn calculate_heightmap(&mut self) {
        for x in 0..16 {
            for z in 0..16 {
                self.recalculate_column_height(x, z);
            }
        }
    }

    pub(crate) fn set_block(&mut self, x: i32, y: i32, z: i32, b: block::Block) -> bool {
        let s_idx = match self.bounds.block_section_index(y) {
            Some(index) => index,
            None => return false,
        };
        if self.sections[s_idx].is_none() {
            if let block::Air {} = b {
                return false;
            }
            let fill_sky = self.sections.iter().skip(s_idx).all(|v| v.is_none());
            self.sections[s_idx] = Some(ChunkSection::new(y.div_euclid(16), fill_sky));
        }
        {
            let section = self.sections[s_idx].as_mut().unwrap();
            if !section.set_block(x, y & 0xF, z, b) {
                return false;
            }
        }
        let idx = ((z << 4) | x) as usize;
        if !matches!(b, block::Air {}) && y > self.heightmap[idx] {
            self.heightmap[idx] = y;
            self.heightmap_dirty = true;
        } else if matches!(b, block::Air {}) && y == self.heightmap[idx] {
            self.recalculate_column_height(x, z);
        }
        true
    }

    pub(crate) fn get_block(&self, x: i32, y: i32, z: i32) -> block::Block {
        let s_idx = match self.bounds.block_section_index(y) {
            Some(index) => index,
            None => return block::Air {},
        };
        match self.sections[s_idx].as_ref() {
            Some(sec) => sec.get_block(x, y & 0xF, z),
            None => block::Air {},
        }
    }

    pub(crate) fn get_block_light(&self, x: i32, y: i32, z: i32) -> u8 {
        let s_idx = match self.bounds.block_section_index(y) {
            Some(index) => index,
            None => return 0,
        };
        match self.sections[s_idx].as_ref() {
            Some(sec) => sec.get_block_light(x, y & 0xF, z),
            None => 0,
        }
    }

    pub(crate) fn set_block_light(&mut self, x: i32, y: i32, z: i32, light: u8) {
        let s_idx = match self.bounds.block_section_index(y) {
            Some(index) => index,
            None => return,
        };
        if self.sections[s_idx].is_none() {
            if light == 0 {
                return;
            }
            let fill_sky = self.sections.iter().skip(s_idx).all(|v| v.is_none());
            self.sections[s_idx] = Some(ChunkSection::new(y.div_euclid(16), fill_sky));
        }
        if let Some(sec) = self.sections[s_idx].as_mut() {
            sec.set_block_light(x, y & 0xF, z, light)
        }
    }

    pub(crate) fn get_sky_light(&self, x: i32, y: i32, z: i32) -> u8 {
        let s_idx = match self.bounds.block_section_index(y) {
            Some(index) => index,
            None => return 15,
        };
        match self.sections[s_idx].as_ref() {
            Some(sec) => sec.get_sky_light(x, y & 0xF, z),
            None => 15,
        }
    }

    pub(crate) fn set_sky_light(&mut self, x: i32, y: i32, z: i32, light: u8) {
        let s_idx = match self.bounds.block_section_index(y) {
            Some(index) => index,
            None => return,
        };
        if self.sections[s_idx].is_none() {
            if light == 15 {
                return;
            }
            let fill_sky = self.sections.iter().skip(s_idx).all(|v| v.is_none());
            self.sections[s_idx] = Some(ChunkSection::new(y.div_euclid(16), fill_sky));
        }
        if let Some(sec) = self.sections[s_idx].as_mut() {
            sec.set_sky_light(x, y & 0xF, z, light)
        }
    }

    // TODO: make use of "get_biome"
    #[allow(dead_code)]
    fn get_biome(&self, x: i32, z: i32) -> biome::Biome {
        biome::Biome::by_id(self.biomes[((z << 4) | x) as usize] as usize)
    }

    pub fn capture_snapshot(&self) -> ChunkSnapshot {
        let mut snapshot_sections = vec![None; self.bounds.section_count()];
        for section in self.sections.iter().enumerate() {
            if section.1.is_some() {
                snapshot_sections[section.0] =
                    Some(section.1.as_ref().unwrap().capture_snapshot(self.biomes));
            }
        }
        ChunkSnapshot {
            position: self.position,
            sections: snapshot_sections,
            bounds: self.bounds,
            biomes: self.biomes,
            heightmap: self.heightmap,
        }
    }
}

pub struct ChunkSnapshot {
    pub position: CPos,
    pub sections: Vec<Option<ChunkSectionSnapshot>>,
    pub bounds: WorldBounds,
    pub biomes: [u8; 16 * 16],
    /// Highest non-air block, or bounds.min_y() - 1 for an empty column.
    pub heightmap: [i32; 16 * 16],
}

impl ChunkSnapshot {
    pub fn section(&self, section_y: i32) -> Option<&ChunkSectionSnapshot> {
        self.sections
            .get(self.bounds.section_index(section_y)?)
            .and_then(Option::as_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_sections_and_light_survive_snapshots() {
        let bounds = WorldBounds::new(-64, 384).unwrap();
        let mut chunk = Chunk::with_bounds(CPos(-2, 3), bounds);
        for y in [-64i32, -49, -48, -17, -16, -1, 0, 15, 16, 255, 256, 319] {
            assert!(chunk.set_block(3, y, 5, block::Stone {}));
            chunk.set_block_light(3, y, 5, 7);
            chunk.set_sky_light(3, y, 5, 11);
            assert_eq!(chunk.get_block(3, y, 5), block::Stone {});
            assert_eq!(chunk.get_block_light(3, y, 5), 7);
            assert_eq!(chunk.get_sky_light(3, y, 5), 11);
        }
        let snapshot = chunk.capture_snapshot();
        assert_eq!(snapshot.sections.len(), 24);
        for y in [-64i32, -49, -48, -17, -16, -1, 0, 15, 16, 255, 256, 319] {
            let section = snapshot.section(y.div_euclid(16)).unwrap();
            assert_eq!(section.y, y.div_euclid(16));
            assert_eq!(section.get_block(3, y.rem_euclid(16), 5), block::Stone {});
            assert_eq!(section.get_block_light(3, y.rem_euclid(16), 5), 7);
            assert_eq!(section.get_sky_light(3, y.rem_euclid(16), 5), 11);
        }
        assert!(snapshot.section(-5).is_none());
        assert!(snapshot.section(20).is_none());
    }

    #[test]
    fn heightmap_is_signed_and_resets_when_last_block_removed() {
        let mut chunk = Chunk::with_bounds(CPos(0, 0), WorldBounds::new(-64, 1024).unwrap());
        assert_eq!(chunk.heightmap[0], -65);
        chunk.set_block(0, -64, 0, block::Stone {});
        chunk.set_block(0, -12, 0, block::Stone {});
        chunk.set_block(0, 700, 0, block::Stone {});
        assert_eq!(chunk.heightmap[0], 700);
        // Replacing the top solid block must not spuriously lower the map.
        chunk.set_block(0, 700, 0, block::Dirt {});
        assert_eq!(chunk.heightmap[0], 700);
        chunk.set_block(0, 700, 0, block::Air {});
        assert_eq!(chunk.heightmap[0], -12);
        chunk.set_block(0, -12, 0, block::Air {});
        assert_eq!(chunk.heightmap[0], -64);
        chunk.set_block(0, -64, 0, block::Air {});
        assert_eq!(chunk.heightmap[0], -65);
        chunk.heightmap[0] = 999;
        chunk.calculate_heightmap();
        assert_eq!(chunk.heightmap[0], -65);
    }

    #[test]
    fn out_of_bounds_does_not_alias_valid_section() {
        let mut chunk = Chunk::with_bounds(CPos(0, 0), WorldBounds::new(-64, 384).unwrap());
        for y in [i32::MIN, -65, 320, i32::MAX] {
            assert!(!chunk.set_block(0, y, 0, block::Stone {}));
            chunk.set_block_light(0, y, 0, 15);
            chunk.set_sky_light(0, y, 0, 0);
            assert_eq!(chunk.get_block(0, y, 0), block::Air {});
            assert_eq!(chunk.get_block_light(0, y, 0), 0);
            assert_eq!(chunk.get_sky_light(0, y, 0), 15);
        }
        assert!(chunk.sections.iter().all(Option::is_none));
    }

    #[test]
    fn legacy_layout_stays_sixteen_sections() {
        let mut chunk = Chunk::new(CPos(0, 0));
        assert_eq!(chunk.bounds(), WorldBounds::LEGACY);
        assert_eq!(chunk.sections.len(), 16);
        assert!(chunk.set_block(0, 255, 0, block::Stone {}));
        assert_eq!(chunk.section(15).unwrap().y, 15);
        assert_eq!(chunk.heightmap[0], 255);
        assert!(!chunk.set_block(0, -1, 0, block::Stone {}));
        assert!(!chunk.set_block(0, 256, 0, block::Stone {}));
    }
}
