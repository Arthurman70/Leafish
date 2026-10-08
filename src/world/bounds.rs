//! Validated vertical bounds. Section coordinates are signed world coordinates,
//! while vector offsets are always relative to the dimension's minimum section.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldBounds {
    min_y: i32,
    height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidWorldBounds;

impl std::fmt::Display for InvalidWorldBounds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "world bounds must be section aligned and fit Minecraft's signed position range",
        )
    }
}

impl std::error::Error for InvalidWorldBounds {}

impl WorldBounds {
    pub const LEGACY: Self = Self {
        min_y: 0,
        height: 256,
    };

    /// Uses the same dimension limits as the modern packet decoder. Validation
    /// happens before allocating any section storage.
    pub fn new(min_y: i32, height: u32) -> Result<Self, InvalidWorldBounds> {
        if min_y < -2032
            || min_y % 16 != 0
            || !(16..=4064).contains(&height)
            || height % 16 != 0
            || i64::from(min_y) + i64::from(height) > 2032
        {
            return Err(InvalidWorldBounds);
        }
        Ok(Self { min_y, height })
    }

    pub fn min_y(self) -> i32 {
        self.min_y
    }
    pub fn height(self) -> u32 {
        self.height
    }
    pub fn max_y_exclusive(self) -> i32 {
        self.min_y + self.height as i32
    }
    pub fn min_section_y(self) -> i32 {
        self.min_y / 16
    }
    pub fn section_count(self) -> usize {
        (self.height / 16) as usize
    }
    pub fn contains_y(self, y: i32) -> bool {
        y >= self.min_y && y < self.max_y_exclusive()
    }
    /// Rendering can begin at the closest valid section when the camera is
    /// above or below the dimension, without treating that coordinate as storage.
    pub fn clamp_section_y(self, section_y: i32) -> i32 {
        section_y.clamp(self.min_section_y(), self.max_y_exclusive() / 16 - 1)
    }
    pub fn section_index(self, section_y: i32) -> Option<usize> {
        let index = section_y.checked_sub(self.min_section_y())?;
        if index >= 0 && (index as usize) < self.section_count() {
            Some(index as usize)
        } else {
            None
        }
    }
    pub fn block_section_index(self, y: i32) -> Option<usize> {
        self.section_index(y.div_euclid(16))
    }
    pub fn section_y(self, index: usize) -> Option<i32> {
        if index < self.section_count() {
            Some(self.min_section_y() + index as i32)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_indices_cover_exact_dimension() {
        let bounds = WorldBounds::new(-64, 384).unwrap();
        assert_eq!(bounds.section_count(), 24);
        for y in -80..336 {
            let actual = bounds.block_section_index(y);
            if (-64..320).contains(&y) {
                assert_eq!(actual, Some(((y + 64) / 16) as usize));
                assert_eq!(bounds.section_y(actual.unwrap()), Some(y.div_euclid(16)));
            } else {
                assert_eq!(actual, None);
            }
        }
        assert_eq!(bounds.section_index(i32::MIN), None);
        assert_eq!(bounds.section_index(i32::MAX), None);
        assert_eq!(bounds.section_y(usize::MAX), None);
        assert_eq!(bounds.clamp_section_y(i32::MIN), -4);
        assert_eq!(bounds.clamp_section_y(i32::MAX), 19);
        assert_eq!(bounds.clamp_section_y(-1), -1);
    }
    #[test]
    fn rejects_invalid_and_extreme_bounds() {
        for (min_y, height) in [
            (-65, 384),
            (0, 0),
            (0, 17),
            (-2048, 16),
            (0, 4096),
            (2032, 16),
            (i32::MAX, u32::MAX),
        ] {
            assert!(WorldBounds::new(min_y, height).is_err());
        }
        assert_eq!(WorldBounds::new(0, 256).unwrap(), WorldBounds::LEGACY);
        assert_eq!(WorldBounds::new(-2032, 4064).unwrap().section_count(), 254);
        assert_eq!(WorldBounds::new(0, 1024).unwrap().section_count(), 64);
    }
}
