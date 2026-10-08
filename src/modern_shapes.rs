//! Collision and interaction geometry exported from the matching Java runtime.
//! Block models are never used as a substitute. Unresolved dynamic/contextual
//! geometry remains an explicit error at the movement/interaction boundary.
use crate::shared::Position;
use crate::world::native::NativeChunkStore;
use leafish_blocks::catalog::StateCatalog;
use serde::Deserialize;
use std::collections::BTreeMap;

const MAX_REPORT_BYTES: usize = 64 * 1024 * 1024;
const MAX_BOXES_PER_STATE: usize = 256;
const MAX_QUERY_CELLS: usize = 32_768;
const EPSILON: f64 = 1e-9;

#[derive(Debug, PartialEq)]
pub enum ShapeError {
    Invalid(String),
    UnknownState(u32),
    Unresolved { state_id: u32, kind: &'static str },
    Unloaded([i32; 3]),
    QueryTooLarge,
}
impl std::fmt::Display for ShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "invalid shape data: {}", message),
            Self::UnknownState(id) => write!(f, "no exact geometry for block state {}", id),
            Self::Unresolved { state_id, kind } => write!(
                f,
                "{} geometry for block state {} requires world/player context",
                kind, state_id
            ),
            Self::Unloaded(position) => {
                write!(f, "geometry query reached unloaded cell {:?}", position)
            }
            Self::QueryTooLarge => write!(f, "geometry query exceeds its cell budget"),
        }
    }
}
impl std::error::Error for ShapeError {}
type Result<T> = std::result::Result<T, ShapeError>;
fn invalid(message: impl Into<String>) -> ShapeError {
    ShapeError::Invalid(message.into())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}
impl Aabb {
    fn from_array(values: [f64; 6]) -> Result<Self> {
        if !values.iter().all(|v| v.is_finite() && v.abs() <= 1024.0)
            || (0..3).any(|axis| values[axis] >= values[axis + 3])
        {
            return Err(invalid("nonfinite, degenerate or unbounded shape box"));
        }
        Ok(Self {
            min: [values[0], values[1], values[2]],
            max: [values[3], values[4], values[5]],
        })
    }
    fn translated(self, offset: [f64; 3]) -> Self {
        Self {
            min: std::array::from_fn(|a| self.min[a] + offset[a]),
            max: std::array::from_fn(|a| self.max[a] + offset[a]),
        }
    }
    fn union(self, other: Self) -> Self {
        Self {
            min: std::array::from_fn(|a| self.min[a].min(other.min[a])),
            max: std::array::from_fn(|a| self.max[a].max(other.max[a])),
        }
    }
    fn intersects(self, other: Self) -> bool {
        (0..3).all(|a| self.max[a] > other.min[a] + EPSILON && self.min[a] < other.max[a] - EPSILON)
    }
}

#[derive(Deserialize)]
struct Report {
    schema_version: u32,
    minecraft_version: String,
    block_catalog_sha256: String,
    states: Vec<ReportState>,
}
#[derive(Deserialize)]
struct ReportState {
    id: u32,
    name: String,
    #[serde(default)]
    properties: BTreeMap<String, String>,
    dynamic: bool,
    has_offset: bool,
    collision: Option<Vec<[f64; 6]>>,
    outline: Option<Vec<[f64; 6]>>,
    collision_unresolved: Option<String>,
    outline_unresolved: Option<String>,
    #[serde(default)]
    movement: Option<MovementFactors>,
    #[serde(default)]
    offset: Option<ShapeOffset>,
    #[serde(default)]
    collision_offset: bool,
    #[serde(default)]
    outline_offset: bool,
    #[serde(default)]
    collision_context: Option<String>,
}
/// Original Block getters. Cast to f32 when reproducing Java movement arithmetic.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct MovementFactors {
    pub friction: f64,
    pub speed_factor: f64,
    pub jump_factor: f64,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OffsetKind {
    Xz,
    Xyz,
}
#[derive(Clone, Copy, Debug, Deserialize)]
struct ShapeOffset {
    kind: OffsetKind,
    max_horizontal: f64,
    max_vertical: f64,
}
impl ShapeOffset {
    fn at(self, position: [i32; 3]) -> [f64; 3] {
        // Mth.getSeed: X multiplication wraps as Java int before widening.
        let mut seed = position[0].wrapping_mul(3_129_871) as i64
            ^ (position[2] as i64).wrapping_mul(116_129_781);
        seed = seed
            .wrapping_mul(seed)
            .wrapping_mul(42_317_861)
            .wrapping_add(seed.wrapping_mul(11))
            >> 16;
        let fraction = |shift: u32| (((seed >> shift) & 15_i64) as f32 / 15.0f32) as f64;
        let horizontal = |shift| {
            ((fraction(shift) - 0.5) * 0.5).clamp(-self.max_horizontal, self.max_horizontal)
        };
        [
            horizontal(0),
            match self.kind {
                OffsetKind::Xz => 0.0,
                OffsetKind::Xyz => (fraction(4) - 1.0) * self.max_vertical,
            },
            horizontal(8),
        ]
    }
    fn expanded(self, bounds: Aabb) -> Aabb {
        Aabb {
            min: [
                bounds.min[0] - self.max_horizontal,
                bounds.min[1]
                    - if matches!(self.kind, OffsetKind::Xyz) {
                        self.max_vertical
                    } else {
                        0.0
                    },
                bounds.min[2] - self.max_horizontal,
            ],
            max: [
                bounds.max[0] + self.max_horizontal,
                bounds.max[1],
                bounds.max[2] + self.max_horizontal,
            ],
        }
    }
}
#[derive(Clone)]
struct Shapes {
    collision: Option<Vec<Aabb>>,
    outline: Option<Vec<Aabb>>,
    movement: Option<MovementFactors>,
    offset: Option<ShapeOffset>,
    collision_offset: bool,
    outline_offset: bool,
    has_offset: bool,
}
pub struct ShapeCatalog {
    states: Vec<Shapes>,
    extent: Aabb,
}

impl ShapeCatalog {
    pub fn from_json(
        bytes: &[u8],
        catalog: &StateCatalog,
        expected_catalog_sha256: &str,
    ) -> Result<Self> {
        if bytes.len() > MAX_REPORT_BYTES {
            return Err(invalid("shape report exceeds 64 MiB"));
        }
        let report: Report = serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
        if !(1..=2).contains(&report.schema_version) || report.minecraft_version != "1.21.1" {
            return Err(invalid("wrong shape schema or Minecraft version"));
        }
        if expected_catalog_sha256.len() != 64
            || !expected_catalog_sha256
                .bytes()
                .all(|v| v.is_ascii_hexdigit())
            || !report
                .block_catalog_sha256
                .eq_ignore_ascii_case(expected_catalog_sha256)
        {
            return Err(invalid(
                "shape report is bound to a different block catalog",
            ));
        }
        if report.states.len() != catalog.len() {
            return Err(invalid("shape report does not cover the catalog exactly"));
        }
        let mut states = vec![None; catalog.len()];
        let mut extent = Aabb {
            min: [0.0; 3],
            max: [1.0; 3],
        };
        for state in report.states {
            let exact = catalog
                .state(state.id)
                .map_err(|_| ShapeError::UnknownState(state.id))?;
            if exact.name() != state.name
                || exact.properties() != &state.properties
                || states[state.id as usize].is_some()
            {
                return Err(invalid("duplicate or mismatched block-state identity"));
            }
            let positioned = report.schema_version == 2 && state.offset.is_some();
            if (state.dynamic
                && !positioned
                && (state.collision.is_some() || state.outline.is_some()))
                || (state.has_offset && !positioned && state.outline.is_some())
                || state.collision.is_none() != state.collision_unresolved.is_some()
                || state.outline.is_none() != state.outline_unresolved.is_some()
            {
                return Err(invalid("inconsistent resolved/contextual geometry"));
            }
            if report.schema_version == 1 {
                if state.offset.is_some()
                    || state.movement.is_some()
                    || state.collision_offset
                    || state.outline_offset
                    || state.collision_context.is_some()
                {
                    return Err(invalid(
                        "schema one cannot claim schema two context support",
                    ));
                }
            } else {
                if state.has_offset != positioned
                    || state.collision_offset && (!positioned || state.collision.is_none())
                    || state.outline_offset && (!positioned || state.outline.is_none())
                {
                    return Err(invalid("inconsistent positional offset metadata"));
                }
                if let Some(offset) = state.offset {
                    if ![offset.max_horizontal, offset.max_vertical]
                        .iter()
                        .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
                    {
                        return Err(invalid("invalid offset limits"));
                    }
                }
                let factors = state
                    .movement
                    .ok_or_else(|| invalid("schema two requires movement factors"))?;
                if ![factors.friction, factors.speed_factor, factors.jump_factor]
                    .iter()
                    .all(|v| v.is_finite() && (0.0..=16.0).contains(v))
                {
                    return Err(invalid("invalid movement factors"));
                }
                match state.collision_context.as_deref() {
                    Some("independent") => {}
                    Some("vanilla_player_no_fluid_standing")
                        if matches!(state.name.as_str(), "minecraft:water" | "minecraft:lava")
                            && state.collision.as_ref().is_some_and(Vec::is_empty)
                            && !state.collision_offset => {}
                    _ => return Err(invalid("unrecognized or inconsistent collision context")),
                }
            }
            let state_offset = state.offset;
            let mut convert =
                |boxes: Option<Vec<[f64; 6]>>, shifted: bool| -> Result<Option<Vec<Aabb>>> {
                    boxes
                        .map(|boxes| {
                            if boxes.len() > MAX_BOXES_PER_STATE {
                                return Err(invalid("too many boxes in one state"));
                            }
                            boxes
                                .into_iter()
                                .map(|values| {
                                    let bounds = Aabb::from_array(values)?;
                                    extent = extent.union(if shifted {
                                        state_offset.unwrap().expanded(bounds)
                                    } else {
                                        bounds
                                    });
                                    Ok(bounds)
                                })
                                .collect()
                        })
                        .transpose()
                };
            let collision = convert(state.collision, state.collision_offset)?;
            let outline = convert(state.outline, state.outline_offset)?;
            states[state.id as usize] = Some(Shapes {
                collision,
                outline,
                movement: state.movement,
                offset: state.offset,
                collision_offset: state.collision_offset,
                outline_offset: state.outline_offset,
                has_offset: state.has_offset,
            });
        }
        let states = states
            .into_iter()
            .map(|state| state.ok_or_else(|| invalid("missing shape state")))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { states, extent })
    }

    pub fn collision(&self, id: u32) -> Result<&[Aabb]> {
        if self
            .states
            .get(id as usize)
            .is_some_and(|s| s.collision_offset)
        {
            return Err(ShapeError::Unresolved {
                state_id: id,
                kind: "position-dependent collision; use collision_at",
            });
        }
        self.states
            .get(id as usize)
            .ok_or(ShapeError::UnknownState(id))?
            .collision
            .as_deref()
            .ok_or(ShapeError::Unresolved {
                state_id: id,
                kind: "collision",
            })
    }
    pub fn outline(&self, id: u32) -> Result<&[Aabb]> {
        if self
            .states
            .get(id as usize)
            .is_some_and(|s| s.outline_offset)
        {
            return Err(ShapeError::Unresolved {
                state_id: id,
                kind: "position-dependent outline; use outline_at",
            });
        }
        self.states
            .get(id as usize)
            .ok_or(ShapeError::UnknownState(id))?
            .outline
            .as_deref()
            .ok_or(ShapeError::Unresolved {
                state_id: id,
                kind: "outline",
            })
    }
    pub fn movement_factors(&self, id: u32) -> Result<MovementFactors> {
        self.states
            .get(id as usize)
            .ok_or(ShapeError::UnknownState(id))?
            .movement
            .ok_or(ShapeError::Unresolved {
                state_id: id,
                kind: "movement factors absent from schema one",
            })
    }
    /// Exact positional offset for rendering. A schema-one offset state cannot
    /// supply this information and remains an error, rather than moving to zero.
    pub fn offset_for(&self, id: u32, position: [i32; 3]) -> Result<[f64; 3]> {
        let state = self
            .states
            .get(id as usize)
            .ok_or(ShapeError::UnknownState(id))?;
        match state.offset {
            Some(offset) => Ok(offset.at(position)),
            None if state.has_offset => Err(ShapeError::Unresolved {
                state_id: id,
                kind: "positional offset absent from schema one",
            }),
            None => Ok([0.0; 3]),
        }
    }
    /// Block-local boxes with the exact offset for this world cell applied.
    pub fn collision_at(&self, id: u32, position: [i32; 3]) -> Result<Vec<Aabb>> {
        self.boxes_at(id, position, true)
    }
    pub fn outline_at(&self, id: u32, position: [i32; 3]) -> Result<Vec<Aabb>> {
        self.boxes_at(id, position, false)
    }
    fn boxes_at(&self, id: u32, position: [i32; 3], collision: bool) -> Result<Vec<Aabb>> {
        let state = self
            .states
            .get(id as usize)
            .ok_or(ShapeError::UnknownState(id))?;
        let (boxes, shifted, kind) = if collision {
            (&state.collision, state.collision_offset, "collision")
        } else {
            (&state.outline, state.outline_offset, "outline")
        };
        let boxes = boxes
            .as_ref()
            .ok_or(ShapeError::Unresolved { state_id: id, kind })?;
        let offset = if shifted {
            state.offset.unwrap().at(position)
        } else {
            [0.0; 3]
        };
        Ok(boxes.iter().map(|b| b.translated(offset)).collect())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub position: [i32; 3],
    pub face: u8,
    pub local: [f32; 3],
    pub distance: f64,
    pub inside: bool,
}

fn finite_position(position: [f64; 3]) -> bool {
    position
        .iter()
        .all(|v| v.is_finite() && v.abs() <= 30_000_000.0)
}
fn state_at(store: &NativeChunkStore, cell: [i32; 3]) -> Result<Option<u32>> {
    if !store.bounds().contains_y(cell[1]) {
        return Ok(None);
    }
    store
        .block_state_id(Position::new(cell[0], cell[1], cell[2]))
        .map(Some)
        .ok_or(ShapeError::Unloaded(cell))
}
fn cells(bounds: Aabb, extent: Aabb) -> Result<Vec<[i32; 3]>> {
    if !finite_position(bounds.min) || !finite_position(bounds.max) {
        return Err(invalid("query coordinates exceed world bounds"));
    }
    let min: [i32; 3] = std::array::from_fn(|a| (bounds.min[a] - extent.max[a]).floor() as i32);
    let max: [i32; 3] = std::array::from_fn(|a| (bounds.max[a] - extent.min[a]).floor() as i32);
    let count = (0..3)
        .try_fold(1usize, |count, a| {
            count.checked_mul((max[a] as i64 - min[a] as i64 + 1) as usize)
        })
        .ok_or(ShapeError::QueryTooLarge)?;
    if count > MAX_QUERY_CELLS {
        return Err(ShapeError::QueryTooLarge);
    }
    let mut result = Vec::with_capacity(count);
    for y in min[1]..=max[1] {
        for z in min[2]..=max[2] {
            for x in min[0]..=max[0] {
                result.push([x, y, z]);
            }
        }
    }
    Ok(result)
}

fn entry_face(axis: usize, positive: bool) -> u8 {
    match (axis, positive) {
        (0, true) => 4,
        (0, false) => 5,
        (1, true) => 0,
        (1, false) => 1,
        (2, true) => 2,
        _ => 3,
    }
}

/// Slab intersection; direction must have unit length so distance is in blocks.
fn ray_box(
    origin: [f64; 3],
    direction: [f64; 3],
    reach: f64,
    bounds: Aabb,
) -> Option<(f64, u8, bool)> {
    let inside = (0..3).all(|a| origin[a] > bounds.min[a] && origin[a] < bounds.max[a]);
    let dominant = (0..3)
        .max_by(|&a, &b| direction[a].abs().total_cmp(&direction[b].abs()))
        .unwrap();
    let mut face = entry_face(dominant, direction[dominant] > 0.0);
    let mut near = 0.0_f64;
    let mut far = reach;
    for axis in 0..3 {
        if direction[axis].abs() < EPSILON {
            if origin[axis] < bounds.min[axis] || origin[axis] > bounds.max[axis] {
                return None;
            }
            continue;
        }
        let first = (bounds.min[axis] - origin[axis]) / direction[axis];
        let second = (bounds.max[axis] - origin[axis]) / direction[axis];
        let enter = first.min(second);
        if enter > near {
            near = enter;
            face = entry_face(axis, direction[axis] > 0.0);
        }
        far = far.min(first.max(second));
        if near > far + EPSILON {
            return None;
        }
    }
    // Merely touching a face while pointing away is not an intersection with
    // that block; otherwise a ray starting on a grid plane can select behind
    // the camera instead of the block it is looking into.
    (far > EPSILON && near <= reach).then_some((near, face, inside))
}

pub fn raycast(
    store: &NativeChunkStore,
    shapes: &ShapeCatalog,
    origin: [f64; 3],
    direction: [f64; 3],
    reach: f64,
) -> Result<Option<Hit>> {
    if !finite_position(origin)
        || !direction.iter().all(|v| v.is_finite())
        || !reach.is_finite()
        || !(0.0..=16.0).contains(&reach)
    {
        return Err(invalid("invalid interaction ray"));
    }
    let length = direction.iter().map(|v| v * v).sum::<f64>().sqrt();
    if !length.is_finite() || length < EPSILON {
        return Err(invalid("interaction ray has no direction"));
    }
    let direction = direction.map(|v| v / length);
    let end = std::array::from_fn(|a| origin[a] + direction[a] * reach);
    let bounds = Aabb {
        min: origin,
        max: origin,
    }
    .union(Aabb { min: end, max: end });
    let mut hit: Option<Hit> = None;
    for cell in cells(bounds, shapes.extent)? {
        let offset = cell.map(|v| v as f64);
        let limit = hit.as_ref().map_or(reach, |hit| hit.distance);
        if ray_box(origin, direction, limit, shapes.extent.translated(offset)).is_none() {
            continue;
        }
        let Some(id) = state_at(store, cell)? else {
            continue;
        };
        for bounds in shapes.outline_at(id, cell)? {
            if let Some((distance, face, inside)) =
                ray_box(origin, direction, limit, bounds.translated(offset))
            {
                if hit.as_ref().is_none_or(|hit| distance < hit.distance) {
                    hit = Some(Hit {
                        position: cell,
                        face,
                        local: std::array::from_fn(|a| {
                            (origin[a] + direction[a] * distance - offset[a]) as f32
                        }),
                        distance,
                        inside,
                    });
                }
            }
        }
    }
    Ok(hit)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveResult {
    pub position: [f64; 3],
    pub applied: [f64; 3],
    pub blocked: [bool; 3],
}

fn standing_box(position: [f64; 3]) -> Aabb {
    Aabb {
        min: [position[0] - 0.3, position[1], position[2] - 0.3],
        max: [position[0] + 0.3, position[1] + 1.8, position[2] + 0.3],
    }
}

fn clip_axis(body: Aabb, obstacle: Aabb, axis: usize, delta: f64) -> f64 {
    if (0..3).filter(|&a| a != axis).any(|a| {
        body.max[a] <= obstacle.min[a] + EPSILON || body.min[a] >= obstacle.max[a] - EPSILON
    }) {
        return delta;
    }
    if delta > 0.0 && body.max[axis] <= obstacle.min[axis] + EPSILON {
        delta.min((obstacle.min[axis] - body.max[axis]).max(0.0))
    } else if delta < 0.0 && body.min[axis] >= obstacle.max[axis] - EPSILON {
        delta.max((obstacle.max[axis] - body.min[axis]).min(0.0))
    } else {
        delta
    }
}

fn move_boxes(position: [f64; 3], delta: [f64; 3], obstacles: &[Aabb]) -> MoveResult {
    let mut body = standing_box(position);
    let mut applied = delta;
    // Match Entity's Y-first clipping and major horizontal direction order.
    let order = if delta[0].abs() < delta[2].abs() {
        [1, 2, 0]
    } else {
        [1, 0, 2]
    };
    for axis in order {
        for &obstacle in obstacles {
            applied[axis] = clip_axis(body, obstacle, axis, applied[axis]);
        }
        let mut offset = [0.0; 3];
        offset[axis] = applied[axis];
        body = body.translated(offset);
    }
    MoveResult {
        position: std::array::from_fn(|a| position[a] + applied[a]),
        applied,
        blocked: std::array::from_fn(|a| (applied[a] - delta[a]).abs() > EPSILON),
    }
}

/// Clip a standing player's requested displacement against exact exported
/// collision boxes. No auto-step, fluid physics, gravity or entity collision.
pub fn move_player(
    store: &NativeChunkStore,
    shapes: &ShapeCatalog,
    position: [f64; 3],
    delta: [f64; 3],
) -> Result<MoveResult> {
    if !finite_position(position) || !delta.iter().all(|v| v.is_finite() && v.abs() <= 16.0) {
        return Err(invalid("invalid player displacement"));
    }
    let body = standing_box(position);
    let swept = body.union(body.translated(delta));
    let obstacles = collect_boxes(store, shapes, swept)?;
    Ok(move_boxes(position, delta, &obstacles))
}

fn collect_boxes(
    store: &NativeChunkStore,
    shapes: &ShapeCatalog,
    swept: Aabb,
) -> Result<Vec<Aabb>> {
    let mut obstacles = Vec::new();
    for cell in cells(swept, shapes.extent)? {
        let offset = cell.map(|v| v as f64);
        if !swept.intersects(shapes.extent.translated(offset)) {
            continue;
        }
        let Some(id) = state_at(store, cell)? else {
            continue;
        };
        obstacles.extend(
            shapes
                .collision_at(id, cell)?
                .iter()
                .map(|bounds| bounds.translated(offset)),
        );
    }
    Ok(obstacles)
}

fn step_candidates(body: Aabb, obstacles: &[Aabb], max_step: f32, clipped_y: f64) -> Vec<f32> {
    let excluded = clipped_y as f32;
    let mut heights: Vec<f32> = obstacles
        .iter()
        .flat_map(|b| [b.min[1], b.max[1]])
        .map(|y| (y - body.min[1]) as f32)
        .filter(|y| y.is_finite() && *y >= 0.0 && *y <= max_step && *y != excluded)
        .collect();
    heights.sort_by(f32::total_cmp);
    heights.dedup_by(|a, b| *a == *b);
    heights
}

fn try_step(
    position: [f64; 3],
    delta: [f64; 3],
    original: MoveResult,
    max_step: f32,
    falling_onto_surface: bool,
    obstacles: &[Aabb],
) -> MoveResult {
    let mut base = position;
    if falling_onto_surface {
        base[1] += original.applied[1];
    }
    let candidates = step_candidates(standing_box(base), obstacles, max_step, original.applied[1]);
    let original_horizontal =
        original.applied[0] * original.applied[0] + original.applied[2] * original.applied[2];
    for height in candidates {
        let candidate = move_boxes(base, [delta[0], height as f64, delta[2]], obstacles);
        if candidate.applied[0] * candidate.applied[0] + candidate.applied[2] * candidate.applied[2]
            > original_horizontal
        {
            // Entity.collide returns the first improving sorted height, and
            // measures its Y displacement from the original (possibly falling) box.
            let applied = [
                candidate.applied[0],
                candidate.applied[1] + base[1] - position[1],
                candidate.applied[2],
            ];
            return MoveResult {
                position: std::array::from_fn(|a| position[a] + applied[a]),
                applied,
                blocked: std::array::from_fn(|a| (applied[a] - delta[a]).abs() > EPSILON),
            };
        }
    }
    original
}

/// Vanilla 1.21.1 candidate-height step-up for dry standing movement. Uses exact
/// exported block boxes; entities, world-border collision and support tracking
/// remain the caller's responsibility. `max_step` has Java float precision.
pub fn move_player_ground(
    store: &NativeChunkStore,
    shapes: &ShapeCatalog,
    position: [f64; 3],
    delta: [f64; 3],
    grounded: bool,
    max_step: f32,
) -> Result<MoveResult> {
    if !max_step.is_finite() || !(0.0..=16.0).contains(&max_step) {
        return Err(invalid("invalid maximum step height"));
    }
    let original = move_player(store, shapes, position, delta)?;
    let falling_onto_surface = original.applied[1] != delta[1] && delta[1] < 0.0;
    if max_step <= 0.0
        || !(grounded || falling_onto_surface)
        || original.applied[0] == delta[0] && original.applied[2] == delta[2]
    {
        return Ok(original);
    }
    let mut base = position;
    if falling_onto_surface {
        base[1] += original.applied[1];
    }
    let body = standing_box(base);
    let mut bounds = body.union(body.translated([delta[0], max_step as f64, delta[2]]));
    if !falling_onto_surface {
        bounds.min[1] -= 1.0e-5f32 as f64;
    }
    let obstacles = collect_boxes(store, shapes, bounds)?;
    Ok(try_step(
        position,
        delta,
        original,
        max_step,
        falling_onto_surface,
        &obstacles,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(min: [f64; 3], max: [f64; 3]) -> Aabb {
        Aabb { min, max }
    }
    #[test]
    fn ray_misses_empty_space_above_slab_and_returns_exact_slab_face() {
        let slab = bounds([0.0; 3], [1.0, 0.5, 1.0]);
        assert_eq!(ray_box([-1.0, 0.75, 0.5], [1.0, 0.0, 0.0], 5.0, slab), None);
        assert_eq!(
            ray_box([-1.0, 0.25, 0.5], [1.0, 0.0, 0.0], 5.0, slab),
            Some((1.0, 4, false))
        );
    }
    #[test]
    fn outline_is_not_clamped_to_block_cube() {
        let fence = Aabb::from_array([0.375, 0.0, 0.375, 0.625, 1.5, 0.625]).unwrap();
        assert_eq!(
            ray_box([0.5, 2.0, 0.5], [0.0, -1.0, 0.0], 5.0, fence),
            Some((0.5, 1, false))
        );
    }
    #[test]
    fn negative_ray_parallel_miss_and_inside_origin_are_handled() {
        let cube = bounds([0.0; 3], [1.0; 3]);
        assert_eq!(
            ray_box([2.0, 0.5, 0.5], [-1.0, 0.0, 0.0], 5.0, cube),
            Some((1.0, 5, false))
        );
        assert_eq!(ray_box([2.0, 2.0, 0.5], [-1.0, 0.0, 0.0], 5.0, cube), None);
        assert_eq!(
            ray_box([0.5; 3], [0.0, 0.0, 1.0], 5.0, cube),
            Some((0.0, 2, true))
        );
        assert_eq!(ray_box([0.0, 0.5, 0.5], [-1.0, 0.0, 0.0], 5.0, cube), None);
        assert_eq!(
            ray_box([0.0, 0.5, 0.5], [1.0, 0.0, 0.0], 5.0, cube),
            Some((0.0, 4, false))
        );
    }
    #[test]
    fn player_stops_at_wall_and_slides_along_it() {
        let wall = bounds([1.0, -1.0, -10.0], [2.0, 3.0, 10.0]);
        let result = move_boxes([0.5, 0.0, 0.0], [1.0, 0.0, 0.5], &[wall]);
        assert!((result.position[0] - 0.7).abs() < 1e-9);
        assert_eq!(result.position[2], 0.5);
        assert_eq!(result.blocked, [true, false, false]);
    }
    #[test]
    fn player_lands_on_slab_and_head_stops_below_ceiling() {
        let slab = bounds([-1.0, 0.0, -1.0], [1.0, 0.5, 1.0]);
        assert_eq!(
            move_boxes([0.0, 1.0, 0.0], [0.0, -1.0, 0.0], &[slab]).position[1],
            0.5
        );
        let ceiling = bounds([-1.0, 2.0, -1.0], [1.0, 3.0, 1.0]);
        assert!((move_boxes([0.0; 3], [0.0, 1.0, 0.0], &[ceiling]).position[1] - 0.2).abs() < 1e-9);
    }
    #[test]
    fn negative_wall_and_touching_floor_do_not_block_horizontal_motion() {
        let wall = bounds([-2.0, -1.0, -1.0], [-1.0, 3.0, 1.0]);
        assert!((move_boxes([0.0; 3], [-2.0, 0.0, 0.0], &[wall]).position[0] + 0.7).abs() < 1e-9);
        let floor = bounds([-10.0, -1.0, -10.0], [10.0, 0.0, 10.0]);
        assert_eq!(
            move_boxes([0.0; 3], [1.0, 0.0, 1.0], &[floor]).applied,
            [1.0, 0.0, 1.0]
        );
    }
    #[test]
    fn shape_identity_hash_and_unresolved_status_are_checked() {
        let catalog = StateCatalog::from_json(br#"{"minecraft:air":{"states":[{"id":0,"default":true}]},"minecraft:stone":{"states":[{"id":1,"default":true}]}}"#).unwrap();
        let hash = "0".repeat(64);
        let mut report = serde_json::json!({"schema_version":1,"minecraft_version":"1.21.1","block_catalog_sha256":hash,"states":[
            {"id":0,"name":"minecraft:air","properties":{},"dynamic":false,"has_offset":false,"collision":[],"outline":[],"collision_unresolved":null,"outline_unresolved":null},
            {"id":1,"name":"minecraft:stone","properties":{},"dynamic":true,"has_offset":false,"collision":null,"outline":null,"collision_unresolved":"context","outline_unresolved":"context"}
        ]});
        let shapes =
            ShapeCatalog::from_json(&serde_json::to_vec(&report).unwrap(), &catalog, &hash)
                .unwrap();
        assert_eq!(shapes.collision(0).unwrap(), &[]);
        assert!(matches!(
            shapes.outline(1),
            Err(ShapeError::Unresolved { .. })
        ));
        assert!(ShapeCatalog::from_json(
            &serde_json::to_vec(&report).unwrap(),
            &catalog,
            &"1".repeat(64)
        )
        .is_err());
        report["states"][1]["name"] = "minecraft:dirt".into();
        assert!(
            ShapeCatalog::from_json(&serde_json::to_vec(&report).unwrap(), &catalog, &hash)
                .is_err()
        );
    }
    #[test]
    fn extended_shapes_expand_query_and_excessive_queries_are_bounded() {
        let extent = bounds([0.0; 3], [1.0, 1.5, 1.0]);
        assert!(cells(bounds([0.5, 1.25, 0.5], [0.5, 1.25, 0.5]), extent)
            .unwrap()
            .contains(&[0, 0, 0]));
        assert_eq!(
            cells(bounds([0.0; 3], [100.0; 3]), extent),
            Err(ShapeError::QueryTooLarge)
        );
    }

    fn schema_two() -> (StateCatalog, serde_json::Value) {
        let names = [
            "minecraft:air",
            "minecraft:water",
            "minecraft:bamboo",
            "minecraft:moving_piston",
            "minecraft:short_grass",
        ];
        let mut blocks = serde_json::Map::new();
        let states: Vec<_> = names.iter().enumerate().map(|(id, name)| {
            blocks.insert((*name).into(), serde_json::json!({"states":[{"id":id,"default":true}]}));
            serde_json::json!({"id":id,"name":name,"properties":{},"dynamic":false,"has_offset":false,
                "collision":[],"outline":[],"collision_unresolved":null,"outline_unresolved":null,
                "movement":{"friction":0.6,"speed_factor":1.0,"jump_factor":1.0},
                "offset":null,"collision_offset":false,"outline_offset":false,"collision_context":"independent"})
        }).collect();
        let catalog = StateCatalog::from_json(&serde_json::to_vec(&blocks).unwrap()).unwrap();
        let mut report = serde_json::json!({"schema_version":2,"minecraft_version":"1.21.1",
            "block_catalog_sha256":"0".repeat(64),"states":states});
        report["states"][1]["collision_context"] = "vanilla_player_no_fluid_standing".into();
        for id in [2, 4] {
            report["states"][id]["has_offset"] = true.into();
            report["states"][id]["offset"] = serde_json::json!({"kind":if id==4 {"xyz"} else {"xz"},
                "max_horizontal":0.25,"max_vertical":0.2_f32 as f64});
        }
        report["states"][2]["dynamic"] = true.into();
        report["states"][2]["collision"] =
            serde_json::json!([[0.40625, 0.0, 0.40625, 0.59375, 1.0, 0.59375]]);
        report["states"][2]["outline"] =
            serde_json::json!([[0.3125, 0.0, 0.3125, 0.6875, 1.0, 0.6875]]);
        report["states"][2]["collision_offset"] = true.into();
        report["states"][2]["outline_offset"] = true.into();
        report["states"][3]["dynamic"] = true.into();
        for kind in ["collision", "outline"] {
            report["states"][3][kind] = serde_json::Value::Null;
            report["states"][3][format!("{}_unresolved", kind)] = "dynamic shape".into();
        }
        // Original short grass has an artwork offset but a constant outline.
        report["states"][4]["outline"] =
            serde_json::json!([[0.125, 0.0, 0.125, 0.875, 0.8125, 0.875]]);
        (catalog, report)
    }

    #[test]
    fn schema_two_preserves_fluid_plant_context_and_unresolved_dynamics() {
        let (catalog, report) = schema_two();
        let shapes = ShapeCatalog::from_json(
            &serde_json::to_vec(&report).unwrap(),
            &catalog,
            &"0".repeat(64),
        )
        .unwrap();
        assert!(shapes.collision_at(1, [-1, 5, -1]).unwrap().is_empty());
        assert_eq!(shapes.offset_for(0, [-1, 5, -1]).unwrap(), [0.0; 3]);
        let bamboo = shapes.collision_at(2, [-1, -64, -1]).unwrap()[0];
        assert_eq!(bamboo.min, [0.3229166716337204, 0.0, 0.3229166716337204]);
        assert_eq!(bamboo.max, [0.5104166716337204, 1.0, 0.5104166716337204]);
        assert!(matches!(
            shapes.collision(2),
            Err(ShapeError::Unresolved { .. })
        ));
        assert!(matches!(
            shapes.collision_at(3, [0; 3]),
            Err(ShapeError::Unresolved { .. })
        ));
        assert_eq!(
            shapes.outline_at(4, [-1, -64, -1]).unwrap()[0].min,
            [0.125, 0.0, 0.125]
        );
        assert_ne!(shapes.offset_for(4, [-1, -64, -1]).unwrap(), [0.0; 3]);
        assert_eq!(shapes.movement_factors(0).unwrap().friction as f32, 0.6_f32);
        assert_eq!(
            shapes.offset_for(99, [0; 3]),
            Err(ShapeError::UnknownState(99))
        );
        for change in 0..5 {
            let mut invalid = report.clone();
            match change {
                0 => {
                    invalid["states"][0]["collision_context"] =
                        "vanilla_player_no_fluid_standing".into()
                }
                1 => invalid["states"][1]["collision"] = serde_json::json!([[0, 0, 0, 1, 1, 1]]),
                2 => invalid["states"][2]["offset"] = serde_json::Value::Null,
                3 => invalid["states"][0]["movement"]["friction"] = (-1).into(),
                _ => invalid["states"][3]["collision_offset"] = true.into(),
            }
            assert!(
                ShapeCatalog::from_json(
                    &serde_json::to_vec(&invalid).unwrap(),
                    &catalog,
                    &"0".repeat(64)
                )
                .is_err(),
                "case {}",
                change
            );
        }
    }

    #[test]
    fn schema_one_does_not_invent_missing_offsets_or_movement_factors() {
        let (catalog, mut report) = schema_two();
        report["schema_version"] = 1.into();
        for state in report["states"].as_array_mut().unwrap() {
            let s = state.as_object_mut().unwrap();
            for key in [
                "movement",
                "offset",
                "collision_offset",
                "outline_offset",
                "collision_context",
            ] {
                s.remove(key);
            }
            if s["has_offset"] == true || s["dynamic"] == true {
                for kind in ["collision", "outline"] {
                    s.insert(kind.into(), serde_json::Value::Null);
                    s.insert(format!("{}_unresolved", kind), "context".into());
                }
            }
        }
        let shapes = ShapeCatalog::from_json(
            &serde_json::to_vec(&report).unwrap(),
            &catalog,
            &"0".repeat(64),
        )
        .unwrap();
        assert!(matches!(
            shapes.offset_for(2, [0; 3]),
            Err(ShapeError::Unresolved { .. })
        ));
        assert!(matches!(
            shapes.movement_factors(0),
            Err(ShapeError::Unresolved { .. })
        ));
        assert_eq!(shapes.offset_for(0, [0; 3]).unwrap(), [0.0; 3]);
    }

    #[test]
    fn positional_offsets_match_original_runtime_negative_and_world_edge_vectors() {
        // Values obtained by invoking original BlockState.getOffset with the
        // pinned official 1.21.1 runtime, independently of this Rust formula.
        let grass = ShapeOffset {
            kind: OffsetKind::Xyz,
            max_horizontal: 0.25,
            max_vertical: 0.2_f32 as f64,
        };
        assert_eq!(
            grass.at([-1, -64, -1]),
            [
                -0.0833333283662796,
                -0.053333330949147495,
                -0.0833333283662796
            ]
        );
        assert_eq!(
            grass.at([29_999_999, 319, -29_999_999]),
            [
                -0.14999999850988388,
                -0.12000000059604643,
                -0.0833333283662796
            ]
        );
        assert_eq!(
            grass.at([-29_999_999, 255, 29_999_999]),
            grass.at([29_999_999, 319, -29_999_999])
        );
        let dripstone = ShapeOffset {
            kind: OffsetKind::Xz,
            max_horizontal: 0.125,
            ..grass
        };
        assert_eq!(
            dripstone.at([29_999_999, 319, -29_999_999]),
            [-0.125, 0.0, -0.0833333283662796]
        );
        let expanded = grass.expanded(bounds([0.0; 3], [1.0; 3]));
        assert_eq!(expanded.min, [-0.25, -(0.2_f32 as f64), -0.25]);
        assert_eq!(expanded.max, [1.25, 1.0, 1.25]);
    }

    #[test]
    fn candidate_step_climbs_slab_but_respects_ceiling_and_height_limit() {
        let pos = [0.5, 0.0, 0.5];
        let delta = [1.0, -0.08, 0.0];
        let floor = bounds([-10.0, -1.0, -10.0], [10.0, 0.0, 10.0]);
        let slab = bounds([1.0, 0.0, 0.0], [2.0, 0.5, 1.0]);
        let obstacles = [floor, slab];
        let clipped = move_boxes(pos, delta, &obstacles);
        let stepped = try_step(pos, delta, clipped, 0.6, true, &obstacles);
        assert_eq!(stepped.position, [1.5, 0.5, 0.5]);
        assert_eq!(
            try_step(pos, delta, clipped, 0.49, true, &obstacles),
            clipped
        );
        let ceiling = bounds([-10.0, 2.1, -10.0], [10.0, 3.0, 10.0]);
        assert_eq!(
            try_step(pos, delta, clipped, 0.6, true, &[floor, slab, ceiling]),
            clipped
        );
    }

    #[test]
    fn candidate_step_uses_first_improvement_and_falling_base() {
        let pos = [0.5, 0.2, 0.5];
        let delta = [1.0, -0.4, 0.0];
        let obstacles = [
            bounds([-10.0, -1.0, -10.0], [10.0, 0.0, 10.0]),
            bounds([1.0, 0.0, 0.0], [1.5, 0.25, 1.0]),
            bounds([1.5, 0.0, 0.0], [2.0, 0.5, 1.0]),
        ];
        let original = move_boxes(pos, delta, &obstacles);
        assert_eq!(original.position[1], 0.0);
        let step = try_step(pos, delta, original, 0.6, true, &obstacles);
        assert!((step.position[0] - 1.2).abs() < 1e-12);
        assert_eq!(step.position[1], 0.25); // stop at first improvement, not the highest step
        assert!((step.applied[1] - 0.05).abs() < 1e-12);
        assert_eq!(
            step_candidates(standing_box([0.0; 3]), &obstacles, 0.6, 0.25),
            vec![0.0, 0.5]
        );
    }

    #[test]
    fn player_and_interaction_queries_pass_through_resolved_fluid_cells() {
        use leafish_protocol::protocol::chunk767::ChunkSection767;
        use leafish_protocol::protocol::configuration::NetworkNbt;
        use leafish_protocol::protocol::play767::{
            BlockUpdate, ChunkWithLight, Dimension, LightData, WorldContext,
        };
        let (catalog, report) = schema_two();
        let shapes = ShapeCatalog::from_json(
            &serde_json::to_vec(&report).unwrap(),
            &catalog,
            &"0".repeat(64),
        )
        .unwrap();
        let mut store = NativeChunkStore::new(
            WorldContext {
                dimension: Dimension {
                    registry_id: 0,
                    id: "minecraft:overworld".into(),
                    min_y: 0,
                    height: 16,
                    has_skylight: true,
                },
                block_state_count: 5,
                biome_count: 1,
            },
            std::sync::Arc::new(catalog),
        )
        .unwrap();
        store
            .insert_chunk(ChunkWithLight {
                x: 0,
                z: 0,
                heightmaps: NetworkNbt::from_bytes(&[10, 0]).unwrap(),
                block_entities: vec![],
                sections: vec![ChunkSection767 {
                    section_y: 0,
                    non_empty_block_count: 4096,
                    block_states: vec![1; 4096],
                    biomes: vec![0; 64],
                }],
                light: LightData {
                    min_section_y: -1,
                    section_count: 3,
                    sky_mask: vec![],
                    block_mask: vec![],
                    empty_sky_mask: vec![],
                    empty_block_mask: vec![],
                    sky_arrays: vec![],
                    block_arrays: vec![],
                },
            })
            .unwrap();
        assert_eq!(
            move_player(&store, &shapes, [8.0, 4.0, 8.0], [1.0, 0.0, 0.0])
                .unwrap()
                .position,
            [9.0, 4.0, 8.0]
        );
        assert_eq!(
            raycast(&store, &shapes, [8.0, 5.0, 8.0], [1.0, 0.0, 0.0], 3.0).unwrap(),
            None
        );
        store
            .apply_block_updates(&[BlockUpdate {
                position: [9, 4, 8],
                state_id: 3,
            }])
            .unwrap();
        assert!(matches!(
            move_player(&store, &shapes, [8.0, 4.0, 8.0], [1.0, 0.0, 0.0]),
            Err(ShapeError::Unresolved { state_id: 3, .. })
        ));
    }

    #[test]
    #[ignore = "requires caller-owned vanilla catalog, schema-two shapes and original-runtime offset reference"]
    fn installed_positional_shapes_match_original_runtime() {
        let catalog = StateCatalog::from_json(
            &std::fs::read(std::env::var("LEAFISH_BLOCK_REFERENCE").unwrap()).unwrap(),
        )
        .unwrap();
        let shapes = ShapeCatalog::from_json(
            &std::fs::read(std::env::var("LEAFISH_SHAPE_REFERENCE").unwrap()).unwrap(),
            &catalog,
            "0dde7f869588905763ea6ff2e7e01bec1db740a58d27477fd27c2f07fb029f73",
        )
        .unwrap();
        #[derive(Deserialize)]
        struct Reference {
            id: u32,
            name: String,
            position: [i32; 3],
            offset: [f64; 3],
            collision: Vec<[f64; 6]>,
            outline: Vec<[f64; 6]>,
        }
        let reference: Vec<Reference> = serde_json::from_slice(
            &std::fs::read(std::env::var("LEAFISH_OFFSET_REFERENCE").unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(reference.len(), 272);
        for row in reference {
            assert_eq!(catalog.state(row.id).unwrap().name(), row.name);
            for (actual, expected) in shapes
                .offset_for(row.id, row.position)
                .unwrap()
                .into_iter()
                .zip(row.offset)
            {
                assert!(
                    (actual - expected).abs() < 1e-12,
                    "offset {} {:?}",
                    row.id,
                    row.position
                );
            }
            for (actual, expected) in [
                (
                    shapes.collision_at(row.id, row.position).unwrap(),
                    row.collision,
                ),
                (
                    shapes.outline_at(row.id, row.position).unwrap(),
                    row.outline,
                ),
            ] {
                assert_eq!(actual.len(), expected.len());
                for (a, b) in actual.into_iter().zip(expected) {
                    for i in 0..3 {
                        assert!(
                            (a.min[i] - b[i]).abs() < 1e-12,
                            "min {} {:?}",
                            row.id,
                            row.position
                        );
                        assert!(
                            (a.max[i] - b[i + 3]).abs() < 1e-12,
                            "max {} {:?}",
                            row.id,
                            row.position
                        );
                    }
                }
            }
        }
        let mut fluids = 0;
        for id in 0..catalog.len() as u32 {
            if matches!(
                catalog.state(id).unwrap().name(),
                "minecraft:water" | "minecraft:lava"
            ) {
                assert!(shapes.collision_at(id, [0; 3]).unwrap().is_empty());
                fluids += 1;
            }
        }
        assert_eq!(fluids, 32);
    }
}
