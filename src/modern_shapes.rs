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
}
#[derive(Clone)]
struct Shapes {
    collision: Option<Vec<Aabb>>,
    outline: Option<Vec<Aabb>>,
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
        if report.schema_version != 1 || report.minecraft_version != "1.21.1" {
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
            if (state.dynamic && (state.collision.is_some() || state.outline.is_some()))
                || (state.has_offset && state.outline.is_some())
                || state.collision.is_none() != state.collision_unresolved.is_some()
                || state.outline.is_none() != state.outline_unresolved.is_some()
            {
                return Err(invalid("inconsistent resolved/contextual geometry"));
            }
            let mut convert = |boxes: Option<Vec<[f64; 6]>>| -> Result<Option<Vec<Aabb>>> {
                boxes
                    .map(|boxes| {
                        if boxes.len() > MAX_BOXES_PER_STATE {
                            return Err(invalid("too many boxes in one state"));
                        }
                        boxes
                            .into_iter()
                            .map(|values| {
                                let bounds = Aabb::from_array(values)?;
                                extent = extent.union(bounds);
                                Ok(bounds)
                            })
                            .collect()
                    })
                    .transpose()
            };
            let collision = convert(state.collision)?;
            let outline = convert(state.outline)?;
            states[state.id as usize] = Some(Shapes { collision, outline });
        }
        let states = states
            .into_iter()
            .map(|state| state.ok_or_else(|| invalid("missing shape state")))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { states, extent })
    }

    pub fn collision(&self, id: u32) -> Result<&[Aabb]> {
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
        for bounds in shapes.outline(id)? {
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
                .collision(id)?
                .iter()
                .map(|bounds| bounds.translated(offset)),
        );
    }
    Ok(move_boxes(position, delta, &obstacles))
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
}
