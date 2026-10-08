//! Rendering exact modern states without passing IDs through the legacy Block enum.
//! The output is Leafish's existing 40-byte block vertex format, section-local.
//! Empty/custom models, missing biome tints, and missing light remain observable.

use super::{BlockVertex, Face, Factory, NamedStateModel};
use crate::shared::{Direction, Position};
use crate::world::native::NativeChunkStore;
use image::GenericImageView;
use leafish_blocks::catalog::NamedState;
use leafish_protocol::protocol::configuration::RegistryEntry;
use leafish_protocol::protocol::play767::LightData;
use parking_lot::RwLock;
use rand::SeedableRng;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::sync::Arc;

const MAX_VERTICES: usize = 1_048_576;

#[derive(Default)]
pub struct NativeMesh {
    pub solid_buffer: Vec<u8>,
    pub trans_buffer: Vec<u8>,
    pub solid_count: usize,
    pub trans_count: usize,
    pub unsupported_states: BTreeSet<u32>,
    pub missing_tint_states: BTreeSet<u32>,
    pub missing_light_samples: usize,
}

struct Prepared {
    model: NamedStateModel,
    opaque_faces: u8,
    translucent: Vec<bool>,
}

#[derive(Clone, Copy)]
pub(super) struct Alpha {
    opaque: bool,
    translucent: bool,
}

/// Biome definitions retain the exact numeric order sent by the connected server.
pub struct BiomeTints {
    biomes: Vec<Biome>,
}
struct Biome {
    temperature: f64,
    downfall: f64,
    grass: Option<u32>,
    foliage: Option<u32>,
    water: u32,
    grass_modifier: String,
}

impl BiomeTints {
    pub fn from_registry(entries: &[RegistryEntry]) -> Result<Self, String> {
        if entries.is_empty() || entries.len() > 65536 {
            return Err("invalid biome registry size".into());
        }
        let mut biomes = Vec::with_capacity(entries.len());
        for entry in entries {
            let nbt = entry
                .data
                .as_ref()
                .ok_or_else(|| format!("biome data omitted: {}", entry.id))?;
            let fields = read_biome_fields(nbt.as_bytes())?;
            let number = |name: &str| -> Result<f64, String> {
                match fields.get(name) {
                    Some(Scalar::Number(number)) if number.is_finite() => Ok(*number),
                    _ => Err(format!("biome {} missing numeric {}", entry.id, name)),
                }
            };
            let color = |name: &str| -> Result<Option<u32>, String> {
                match fields.get(name) {
                    None => Ok(None),
                    Some(Scalar::Number(value))
                        if *value >= 0.0 && *value <= 0xffffff as f64 && value.fract() == 0.0 =>
                    {
                        Ok(Some(*value as u32))
                    }
                    _ => Err(format!("invalid biome color {}", name)),
                }
            };
            biomes.push(Biome {
                temperature: number("temperature")?.clamp(0.0, 1.0),
                downfall: number("downfall")?.clamp(0.0, 1.0),
                grass: color("effects/grass_color")?,
                foliage: color("effects/foliage_color")?,
                water: color("effects/water_color")?.ok_or("missing biome water color")?,
                grass_modifier: match fields.get("effects/grass_color_modifier") {
                    Some(Scalar::Text(value)) => value.clone(),
                    None => "none".into(),
                    _ => return Err("invalid grass color modifier".into()),
                },
            });
        }
        Ok(Self { biomes })
    }

    fn color(
        &self,
        store: &NativeChunkStore,
        factory: &Factory,
        state: &NamedState,
        mut pos: Position,
        index: i32,
    ) -> Option<[u8; 3]> {
        if index < 0 {
            return Some([255; 3]);
        }
        if state.namespace() != "minecraft" {
            return None;
        }
        let foliage = match state.path() {
            "spruce_leaves" => return Some(rgb((-10380959i32) as u32)),
            "birch_leaves" => return Some(rgb((-8345771i32) as u32)),
            "lily_pad" => return Some(rgb((-14647248i32) as u32)),
            "attached_melon_stem" | "attached_pumpkin_stem" => {
                return Some(rgb((-2046180i32) as u32))
            }
            "melon_stem" | "pumpkin_stem" => {
                let age: u8 = state.properties().get("age")?.parse().ok()?;
                return (age <= 7).then_some([age * 32, 255 - age * 8, age * 4]);
            }
            "redstone_wire" => {
                let power: u8 = state.properties().get("power")?.parse().ok()?;
                if power > 15 {
                    return None;
                }
                let f = power as f32 / 15.0;
                return Some([
                    ((f * 0.6 + if power > 0 { 0.4 } else { 0.3 }) * 255.0) as u8,
                    ((f * f * 0.7 - 0.5).clamp(0.0, 1.0) * 255.0) as u8,
                    ((f * f * 0.6 - 0.7).clamp(0.0, 1.0) * 255.0) as u8,
                ]);
            }
            "pink_petals" if index == 0 => return Some([255; 3]),
            "large_fern" | "tall_grass" => {
                if state
                    .properties()
                    .get("half")
                    .is_some_and(|half| half == "upper")
                {
                    pos.y -= 1;
                }
                false
            }
            "grass_block" | "fern" | "short_grass" | "potted_fern" | "pink_petals"
            | "sugar_cane" => false,
            "oak_leaves" | "jungle_leaves" | "acacia_leaves" | "dark_oak_leaves" | "vine"
            | "mangrove_leaves" => true,
            "water" | "bubble_column" | "water_cauldron" => {
                return self
                    .biomes
                    .get(store.biome_id(pos)? as usize)
                    .map(|biome| rgb(biome.water));
            }
            // Vanilla BlockColors returns white for unregistered block color handlers.
            _ => return Some([255; 3]),
        };
        let biome = self.biomes.get(store.biome_id(pos)? as usize)?;
        let colormap = if foliage {
            &factory.foliage_colors
        } else {
            &factory.grass_colors
        };
        let override_color = if foliage { biome.foliage } else { biome.grass };
        let color = if let Some(color) = override_color {
            color
        } else {
            if colormap.dimensions() != (256, 256) {
                return None;
            }
            let x = ((1.0 - biome.temperature) * 255.0) as u32;
            let y = ((1.0 - biome.downfall * biome.temperature) * 255.0) as u32;
            let pixel = colormap.get_pixel(x, y).0;
            ((pixel[0] as u32) << 16) | ((pixel[1] as u32) << 8) | pixel[2] as u32
        };
        if foliage {
            return Some(rgb(color));
        }
        match biome.grass_modifier.as_str() {
            "none" => Some(rgb(color)),
            "dark_forest" => Some(rgb(((color & 0xfefefe) + 0x28340a) >> 1)),
            // Swamp uses seeded legacy noise. A guessed constant is not equivalent.
            _ => None,
        }
    }
}

fn rgb(color: u32) -> [u8; 3] {
    [(color >> 16) as u8, (color >> 8) as u8, color as u8]
}

/// Emits the actual installed model geometry. Ambient occlusion and biome-edge
/// blending are not synthesized from the old version's material/biome tables.
pub fn build_section(
    store: &NativeChunkStore,
    section: (i32, i32, i32),
    models: &Arc<RwLock<Factory>>,
    tints: &BiomeTints,
) -> Result<NativeMesh, String> {
    build_section_internal(store, section, models, tints, None)
}

/// Uses the same exact position-dependent offset metadata as collision and
/// selection. The older entry point remains for callers without shape data.
pub fn build_section_with_shapes(
    store: &NativeChunkStore,
    section: (i32, i32, i32),
    models: &Arc<RwLock<Factory>>,
    tints: &BiomeTints,
    shapes: &crate::modern_shapes::ShapeCatalog,
) -> Result<NativeMesh, String> {
    build_section_internal(store, section, models, tints, Some(shapes))
}

fn build_section_internal(
    store: &NativeChunkStore,
    section: (i32, i32, i32),
    models: &Arc<RwLock<Factory>>,
    tints: &BiomeTints,
    shapes: Option<&crate::modern_shapes::ShapeCatalog>,
) -> Result<NativeMesh, String> {
    let model_offset = |id, pos| -> Result<[f64; 3], String> {
        shapes.map_or(Ok([0.0; 3]), |shapes| {
            shapes
                .offset_for(id, pos)
                .map_err(|error| error.to_string())
        })
    };
    if tints.biomes.len() != store.context().biome_count as usize {
        return Err("biome tint registry differs from connected world".into());
    }
    let (cx, sy, cz) = section;
    let section_index = store
        .bounds()
        .section_index(sy)
        .ok_or("section outside dimension")?;
    let chunk = store.chunk(cx, cz).ok_or("chunk is not loaded")?;
    let data = chunk
        .sections
        .get(section_index)
        .ok_or("chunk section is missing")?;
    let origin = [
        cx.checked_mul(16).ok_or("chunk x overflow")?,
        sy.checked_mul(16).ok_or("section y overflow")?,
        cz.checked_mul(16).ok_or("chunk z overflow")?,
    ];
    let mut mesh = NativeMesh::default();
    let mut prepared = HashMap::<[i32; 3], Arc<Prepared>>::new();
    let mut alphas = HashMap::<String, Alpha>::new();
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                let state_id = data.block_states[(y * 256 + z * 16 + x) as usize];
                let state = store
                    .catalog()
                    .state(state_id)
                    .map_err(|error| error.to_string())?;
                if is_air(state) {
                    continue;
                }
                let pos = [origin[0] + x, origin[1] + y, origin[2] + z];
                if let Some(fluid) = Fluid::from_state(state) {
                    render_fluid(
                        store,
                        pos,
                        state,
                        fluid,
                        models,
                        tints,
                        &mut prepared,
                        &mut alphas,
                        &mut mesh,
                    )?;
                    continue;
                }
                let offset = model_offset(state_id, pos)?;
                if state
                    .properties()
                    .get("waterlogged")
                    .is_some_and(|value| value == "true")
                {
                    mesh.unsupported_states.insert(state_id);
                }
                let model = prepare(store, pos, models, &mut prepared, &mut alphas)?;
                if model.model.is_empty() {
                    mesh.unsupported_states.insert(state_id);
                    continue;
                }
                for (face_index, face) in model.model.model.faces.iter().enumerate() {
                    if offset == [0.0; 3] && face.cull_face != Direction::Invalid {
                        let (dx, dy, dz) = face.cull_face.get_offset();
                        let neighbor = [pos[0] + dx, pos[1] + dy, pos[2] + dz];
                        if let Some(id) = store.block_state_id(position(neighbor)) {
                            let neighbor_state =
                                store.catalog().state(id).map_err(|e| e.to_string())?;
                            if !is_air(neighbor_state) && model_offset(id, neighbor)? == [0.0; 3] {
                                let other =
                                    prepare(store, neighbor, models, &mut prepared, &mut alphas)?;
                                if other.opaque_faces & (1 << opposite(face.cull_face).index()) != 0
                                {
                                    continue;
                                }
                            }
                        }
                    }
                    let color = tints
                        .color(store, &models.read(), state, position(pos), face.tint_index)
                        .unwrap_or_else(|| {
                            mesh.missing_tint_states.insert(state_id);
                            [255; 3]
                        });
                    let (dx, dy, dz) = if offset == [0.0; 3] && face_on_boundary(face, face.facing)
                    {
                        face.facing.get_offset()
                    } else {
                        (0, 0, 0)
                    };
                    let light = sample_light(store, [pos[0] + dx, pos[1] + dy, pos[2] + dz]);
                    if light.0.is_none() || light.1.is_none() {
                        mesh.missing_light_samples += 1;
                    }
                    let shade = if face.shade {
                        match face.facing {
                            Direction::Down => 0.5,
                            Direction::Up => 1.0,
                            Direction::North | Direction::South => 0.8,
                            Direction::West | Direction::East => 0.6,
                            _ => 1.0,
                        }
                    } else {
                        1.0
                    };
                    if (mesh.solid_buffer.len() + mesh.trans_buffer.len()) / 40
                        + face.vertices.len()
                        > MAX_VERTICES
                    {
                        return Err("modern section mesh exceeds vertex limit".into());
                    }
                    let (buffer, count) = if model.translucent[face_index] {
                        (&mut mesh.trans_buffer, &mut mesh.trans_count)
                    } else {
                        (&mut mesh.solid_buffer, &mut mesh.solid_count)
                    };
                    for vertex in &face.vertices {
                        let mut vertex = vertex.clone();
                        vertex.x += x as f32 + offset[0] as f32;
                        vertex.y += y as f32 + offset[1] as f32;
                        vertex.z += z as f32 + offset[2] as f32;
                        vertex.r = (color[0] as f32 * shade) as u8;
                        vertex.g = (color[1] as f32 * shade) as u8;
                        vertex.b = (color[2] as f32 * shade) as u8;
                        vertex.block_light = u16::from(light.0.unwrap_or(0)) * 4000;
                        vertex.sky_light = u16::from(light.1.unwrap_or(0)) * 4000;
                        vertex.write(buffer);
                    }
                    *count += face.indices;
                }
            }
        }
    }
    Ok(mesh)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fluid {
    Water,
    Lava,
}
impl Fluid {
    fn from_state(state: &NamedState) -> Option<Self> {
        match state.name() {
            "minecraft:water" => Some(Self::Water),
            "minecraft:lava" => Some(Self::Lava),
            _ => None,
        }
    }
    fn name(self) -> &'static str {
        if self == Self::Water {
            "water"
        } else {
            "lava"
        }
    }
}
fn fluid_level(store: &NativeChunkStore, pos: [i32; 3], fluid: Fluid) -> Option<u8> {
    let state = store.named_state(position(pos)).ok().flatten()?;
    if Fluid::from_state(state) != Some(fluid) {
        return None;
    }
    state
        .properties()
        .get("level")?
        .parse::<u8>()
        .ok()
        .filter(|&level| level <= 15)
}
fn own_fluid_height(level: u8) -> f32 {
    if level >= 8 {
        8.0 / 9.0
    } else {
        (8 - level) as f32 / 9.0
    }
}

fn fluid_height(
    store: &NativeChunkStore,
    pos: [i32; 3],
    fluid: Fluid,
    models: &Arc<RwLock<Factory>>,
    cache: &mut HashMap<[i32; 3], Arc<Prepared>>,
    alphas: &mut HashMap<String, Alpha>,
    mesh: &mut NativeMesh,
) -> Result<f32, String> {
    if let Some(level) = fluid_level(store, pos, fluid) {
        if fluid_level(store, [pos[0], pos[1] + 1, pos[2]], fluid).is_some() {
            return Ok(1.0);
        }
        return Ok(own_fluid_height(level));
    }
    let Some(state) = store
        .named_state(position(pos))
        .map_err(|e| e.to_string())?
    else {
        return Ok(0.0);
    };
    if is_air(state) || Fluid::from_state(state).is_some() {
        return Ok(0.0);
    }
    let model = prepare(store, pos, models, cache, alphas)?;
    if model.opaque_faces == 0b111111 {
        Ok(-1.0)
    } else {
        // Material-solid / partial collision metadata is not inferred from a
        // texture or model. Retain the state as an unresolved fluid boundary.
        mesh.unsupported_states.insert(state.id());
        Ok(0.0)
    }
}
fn weighted_fluid_corner(current: f32, first: f32, second: f32, diagonal: f32) -> f32 {
    if first >= 1.0 || second >= 1.0 || ((first > 0.0 || second > 0.0) && diagonal >= 1.0) {
        return 1.0;
    }
    let mut sum = 0.0;
    let mut weight = 0.0;
    for height in [current, first, second]
        .iter()
        .copied()
        .chain((first > 0.0 || second > 0.0).then_some(diagonal))
    {
        if height >= 0.0 {
            let w = if height >= 0.8 { 10.0 } else { 1.0 };
            sum += height * w;
            weight += w;
        }
    }
    if weight == 0.0 {
        0.0
    } else {
        sum / weight
    }
}

#[allow(clippy::too_many_arguments)]
fn render_fluid(
    store: &NativeChunkStore,
    pos: [i32; 3],
    state: &NamedState,
    fluid: Fluid,
    models: &Arc<RwLock<Factory>>,
    tints: &BiomeTints,
    cache: &mut HashMap<[i32; 3], Arc<Prepared>>,
    alphas: &mut HashMap<String, Alpha>,
    mesh: &mut NativeMesh,
) -> Result<(), String> {
    let level = fluid_level(store, pos, fluid).ok_or("invalid vanilla fluid level")?;
    let above = [pos[0], pos[1] + 1, pos[2]];
    let own = own_fluid_height(level);
    let full = fluid_level(store, above, fluid).is_some();
    let mut heights = [1.0; 4];
    let mut sides = [0.0; 4]; // north, south, west, east
    let directions = [[0, -1], [0, 1], [-1, 0], [1, 0]];
    if !full {
        for (i, [dx, dz]) in directions.iter().enumerate() {
            sides[i] = fluid_height(
                store,
                [pos[0] + dx, pos[1], pos[2] + dz],
                fluid,
                models,
                cache,
                alphas,
                mesh,
            )?;
        }
        for (i, (dx, dz, horizontal, vertical)) in
            [(-1, -1, 2, 0), (1, -1, 3, 0), (-1, 1, 2, 1), (1, 1, 3, 1)]
                .iter()
                .copied()
                .enumerate()
        {
            let diagonal = if sides[horizontal] > 0.0 || sides[vertical] > 0.0 {
                fluid_height(
                    store,
                    [pos[0] + dx, pos[1], pos[2] + dz],
                    fluid,
                    models,
                    cache,
                    alphas,
                    mesh,
                )?
            } else {
                0.0
            };
            heights[i] = weighted_fluid_corner(own, sides[horizontal], sides[vertical], diagonal);
        }
    }
    // The horizontal part of FlowingFluid.getFlow controls top-sprite rotation.
    let mut flow = [0.0f32; 2];
    for (i, [dx, dz]) in directions.iter().enumerate() {
        let neighbor = [pos[0] + dx, pos[1], pos[2] + dz];
        if store
            .named_state(position(neighbor))
            .map_err(|e| e.to_string())?
            .and_then(Fluid::from_state)
            .is_some_and(|other| other != fluid)
        {
            continue;
        }
        let difference = if let Some(level) = fluid_level(store, neighbor, fluid) {
            own - own_fluid_height(level)
        } else if sides[i] >= 0.0 {
            fluid_level(store, [neighbor[0], neighbor[1] - 1, neighbor[2]], fluid)
                .map(|level| own - (own_fluid_height(level) - 8.0 / 9.0))
                .unwrap_or(0.0)
        } else {
            0.0
        };
        flow[0] += *dx as f32 * difference;
        flow[1] += *dz as f32 * difference;
    }
    let (still, flowing) = {
        let textures = { models.read().textures.clone() };
        let still_name = format!("minecraft:block/{}_still", fluid.name());
        let flow_name = format!("minecraft:block/{}_flow", fluid.name());
        texture_alpha(models, &still_name)?;
        texture_alpha(models, &flow_name)?;
        (
            crate::render::Renderer::get_texture(&textures, &still_name),
            crate::render::Renderer::get_texture(&textures, &flow_name),
        )
    };
    let color = if fluid == Fluid::Lava {
        [255; 3]
    } else {
        tints
            .color(store, &models.read(), state, position(pos), 0)
            .unwrap_or_else(|| {
                mesh.missing_tint_states.insert(state.id());
                [255; 3]
            })
    };
    let here_light = sample_light(store, pos);
    let above_light = sample_light(store, above);
    let light = (
        max_light(here_light.0, above_light.0),
        max_light(here_light.1, above_light.1),
    );
    for direction in Direction::all() {
        let (dx, dy, dz) = direction.get_offset();
        let neighbor = [pos[0] + dx, pos[1] + dy, pos[2] + dz];
        if fluid_level(store, neighbor, fluid).is_some() {
            continue;
        }
        if let Some(other) = store
            .named_state(position(neighbor))
            .map_err(|e| e.to_string())?
        {
            if !is_air(other) && Fluid::from_state(other).is_none() {
                let model = prepare(store, neighbor, models, cache, alphas)?;
                if model.opaque_faces & (1 << opposite(direction).index()) != 0 {
                    continue;
                }
            }
        }
        let moving_top = direction == Direction::Up && (flow[0] != 0.0 || flow[1] != 0.0);
        let texture = if direction == Direction::Down || (direction == Direction::Up && !moving_top)
        {
            &still
        } else {
            &flowing
        };
        let shade = match direction {
            Direction::Down => 0.5,
            Direction::West | Direction::East => 0.6,
            Direction::North | Direction::South => 0.8,
            _ => 1.0,
        };
        let face_light = if direction == Direction::Down {
            let below = sample_light(store, neighbor);
            (
                max_light(below.0, here_light.0),
                max_light(below.1, here_light.1),
            )
        } else {
            light
        };
        if face_light.0.is_none() || face_light.1.is_none() {
            mesh.missing_light_samples += 1;
        }
        let mut vertices = Vec::with_capacity(4);
        for source in BlockVertex::face_by_direction(direction) {
            let mut vertex = source.clone();
            let corner = (source.x as usize) + (source.z as usize) * 2;
            let height = (heights[corner] - 0.001).max(0.0);
            let (u, v) = if moving_top {
                let angle = flow[1].atan2(flow[0]) - std::f32::consts::FRAC_PI_2;
                let s = angle.sin() * 0.25;
                let c = angle.cos() * 0.25;
                match corner {
                    0 => (0.5 - c - s, 0.5 - c + s),
                    1 => (0.5 + c - s, 0.5 - c - s),
                    2 => (0.5 - c + s, 0.5 + c + s),
                    _ => (0.5 + c + s, 0.5 + c - s),
                }
            } else if direction == Direction::Up || direction == Direction::Down {
                (source.toffsetx as f32, source.toffsety as f32)
            } else {
                (
                    source.toffsetx as f32 * 0.5,
                    if source.y == 0.0 {
                        0.5
                    } else {
                        (1.0 - height) * 0.5
                    },
                )
            };
            vertex.x += (pos[0] & 15) as f32;
            vertex.z += (pos[2] & 15) as f32;
            vertex.y = (pos[1] & 15) as f32 + if source.y == 0.0 { 0.001 } else { height };
            match direction {
                Direction::North => vertex.z += 0.001,
                Direction::South => vertex.z -= 0.001,
                Direction::West => vertex.x += 0.001,
                Direction::East => vertex.x -= 0.001,
                _ => {}
            }
            vertex.tx = texture.get_x() as u16;
            vertex.ty = texture.get_y() as u16;
            vertex.tw = texture.get_width() as u16;
            vertex.th = texture.get_height() as u16;
            vertex.tatlas = texture.atlas as i16;
            vertex.toffsetx = (u * texture.get_width() as f32 * 16.0) as i16;
            vertex.toffsety = (v * texture.get_height() as f32 * 16.0) as i16;
            vertex.r = (color[0] as f32 * shade) as u8;
            vertex.g = (color[1] as f32 * shade) as u8;
            vertex.b = (color[2] as f32 * shade) as u8;
            vertex.block_light = u16::from(face_light.0.unwrap_or(0)) * 4000;
            vertex.sky_light = u16::from(face_light.1.unwrap_or(0)) * 4000;
            vertices.push(vertex);
        }
        if (mesh.solid_buffer.len() + mesh.trans_buffer.len()) / 40 + 8 > MAX_VERTICES {
            return Err("modern fluid mesh exceeds vertex limit".into());
        }
        let (buffer, count) = if fluid == Fluid::Water {
            (&mut mesh.trans_buffer, &mut mesh.trans_count)
        } else {
            (&mut mesh.solid_buffer, &mut mesh.solid_count)
        };
        for vertex in &vertices {
            vertex.write(buffer);
        }
        *count += 6;
        // Fluid surfaces are visible from inside the fluid as well as outside.
        if direction != Direction::Down {
            for i in [0, 2, 1, 3] {
                vertices[i].write(buffer);
            }
            *count += 6;
        }
    }
    Ok(())
}
fn max_light(a: Option<u8>, b: Option<u8>) -> Option<u8> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        _ => None,
    }
}

fn is_air(state: &NamedState) -> bool {
    matches!(
        state.name(),
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}
fn position(pos: [i32; 3]) -> Position {
    Position::new(pos[0], pos[1], pos[2])
}
fn opposite(direction: Direction) -> Direction {
    match direction {
        Direction::Up => Direction::Down,
        Direction::Down => Direction::Up,
        Direction::North => Direction::South,
        Direction::South => Direction::North,
        Direction::East => Direction::West,
        Direction::West => Direction::East,
        _ => Direction::Invalid,
    }
}

fn prepare(
    store: &NativeChunkStore,
    pos: [i32; 3],
    models: &Arc<RwLock<Factory>>,
    cache: &mut HashMap<[i32; 3], Arc<Prepared>>,
    alphas: &mut HashMap<String, Alpha>,
) -> Result<Arc<Prepared>, String> {
    if let Some(model) = cache.get(&pos) {
        return Ok(model.clone());
    }
    let state = store
        .named_state(position(pos))
        .map_err(|e| e.to_string())?
        .ok_or("model requested outside loaded world")?;
    // Stable coordinate-local choices prevent neighbor updates changing an entire
    // section's random variants. Exact Java weighted RNG parity remains separate.
    let seed = (pos[0] as i64).wrapping_mul(3129871)
        ^ (pos[2] as i64).wrapping_mul(116129781)
        ^ pos[1] as i64;
    let seed = seed
        .wrapping_mul(seed)
        .wrapping_mul(42317861)
        .wrapping_add(seed.wrapping_mul(11));
    let mut rng = rand_pcg::Pcg32::seed_from_u64((seed >> 16) as u64);
    let model = Factory::get_named_state_model(models, state, &mut rng)?;
    let mut opaque_faces = 0;
    let mut translucent = Vec::with_capacity(model.model.faces.len());
    for face in &model.model.faces {
        let mut opaque = true;
        let mut trans = false;
        for texture in &face.vertices_texture {
            let alpha = if let Some(alpha) = alphas.get(&texture.name) {
                *alpha
            } else {
                let alpha = texture_alpha(models, &texture.name)?;
                alphas.insert(texture.name.clone(), alpha);
                alpha
            };
            opaque &= alpha.opaque;
            trans |= alpha.translucent;
        }
        if opaque && full_boundary_face(face) {
            opaque_faces |= 1 << face.facing.index();
        }
        translucent.push(trans);
    }
    let result = Arc::new(Prepared {
        model,
        opaque_faces,
        translucent,
    });
    cache.insert(pos, result.clone());
    Ok(result)
}

fn texture_alpha(models: &Arc<RwLock<Factory>>, name: &str) -> Result<Alpha, String> {
    let cached = { models.read().native_texture_alpha.get(name).copied() };
    if let Some(alpha) = cached {
        return Ok(alpha);
    }
    let (namespace, path) = super::definition::resource_location(name, "minecraft")?;
    let resources = { models.read().resources.clone() };
    let mut bytes = Vec::new();
    resources
        .read()
        .open(namespace, &format!("textures/{}.png", path))
        .ok_or_else(|| format!("missing named-state texture {}", name))?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("model texture exceeds bound".into());
    }
    let image = image::load_from_memory(&bytes)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let alpha = Alpha {
        opaque: image.pixels().all(|pixel| pixel.0[3] == 255),
        translucent: image
            .pixels()
            .any(|pixel| pixel.0[3] > 0 && pixel.0[3] < 255),
    };
    models
        .write()
        .native_texture_alpha
        .insert(name.to_owned(), alpha);
    Ok(alpha)
}

fn face_on_boundary(face: &Face, direction: Direction) -> bool {
    let (axis, value) = match direction {
        Direction::West => (0, 0.0),
        Direction::East => (0, 1.0),
        Direction::Down => (1, 0.0),
        Direction::Up => (1, 1.0),
        Direction::North => (2, 0.0),
        Direction::South => (2, 1.0),
        _ => return false,
    };
    !face.vertices.is_empty()
        && face
            .vertices
            .iter()
            .all(|v| ([v.x, v.y, v.z][axis] - value).abs() < 0.00001)
}
fn full_boundary_face(face: &Face) -> bool {
    if face.vertices.len() != 4 || !face_on_boundary(face, face.facing) {
        return false;
    }
    let axes = match face.facing {
        Direction::West | Direction::East => [1, 2],
        Direction::Down | Direction::Up => [0, 2],
        _ => [0, 1],
    };
    let mut corners = 0u8;
    for v in &face.vertices {
        let xyz = [v.x, v.y, v.z];
        let mut corner = 0;
        for (bit, axis) in axes.iter().enumerate() {
            if (xyz[*axis] - 1.0).abs() < 0.00001 {
                corner |= 1 << bit;
            } else if xyz[*axis].abs() >= 0.00001 {
                return false;
            }
        }
        corners |= 1 << corner;
    }
    corners == 0b1111
}

fn sample_light(store: &NativeChunkStore, pos: [i32; 3]) -> (Option<u8>, Option<u8>) {
    let Some(chunk) = store.chunk(pos[0].div_euclid(16), pos[2].div_euclid(16)) else {
        return (None, None);
    };
    let index = ((pos[1] & 15) * 256 + (pos[2] & 15) * 16 + (pos[0] & 15)) as usize;
    (
        light_nibble(&chunk.light, pos[1].div_euclid(16), index, false),
        if store.context().dimension.has_skylight {
            light_nibble(&chunk.light, pos[1].div_euclid(16), index, true)
        } else {
            Some(0)
        },
    )
}
fn light_nibble(light: &LightData, section_y: i32, index: usize, sky: bool) -> Option<u8> {
    let bit = section_y.checked_sub(light.min_section_y)? as usize;
    if bit >= light.section_count || index >= 4096 {
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
    let word = bit / 64;
    let flag = 1u64 << (bit % 64);
    if empty.get(word).copied().unwrap_or(0) & flag != 0 {
        return Some(0);
    }
    if mask.get(word).copied().unwrap_or(0) & flag == 0 {
        return None;
    }
    let preceding: usize = mask
        .iter()
        .take(word)
        .map(|word| word.count_ones() as usize)
        .sum();
    let preceding = preceding + (mask[word] & (flag - 1)).count_ones() as usize;
    let byte = *arrays.get(preceding)?.get(index / 2)?;
    Some((byte >> ((index & 1) * 4)) & 15)
}

#[derive(Debug)]
enum Scalar {
    Number(f64),
    Text(String),
}

// NetworkNbt has already validated lengths/depth/types. This bounded traversal
// collects only biome color fields and does not use the legacy NBT parser's
// unchecked UTF-8 conversion or replace unknown nested data with defaults.
fn read_biome_fields(bytes: &[u8]) -> Result<BTreeMap<String, Scalar>, String> {
    struct Reader<'a> {
        data: &'a [u8],
        offset: usize,
        fields: BTreeMap<String, Scalar>,
    }
    impl Reader<'_> {
        fn take(&mut self, n: usize) -> Result<&[u8], String> {
            let end = self.offset.checked_add(n).ok_or("NBT length overflow")?;
            let data = self
                .data
                .get(self.offset..end)
                .ok_or("truncated biome NBT")?;
            self.offset = end;
            Ok(data)
        }
        fn byte(&mut self) -> Result<u8, String> {
            Ok(self.take(1)?[0])
        }
        fn int(&mut self) -> Result<i32, String> {
            let b = self.take(4)?;
            Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        }
        fn string(&mut self) -> Result<Vec<u8>, String> {
            let b = self.take(2)?;
            let len = u16::from_be_bytes([b[0], b[1]]) as usize;
            Ok(self.take(len)?.to_vec())
        }
        fn value(&mut self, tag: u8, path: &str, depth: usize) -> Result<(), String> {
            if depth > 64 {
                return Err("biome NBT too deep".into());
            }
            let wanted = matches!(
                path,
                "temperature"
                    | "downfall"
                    | "effects/grass_color"
                    | "effects/foliage_color"
                    | "effects/water_color"
                    | "effects/grass_color_modifier"
            );
            let scalar = match tag {
                1 => Some(Scalar::Number(self.byte()? as i8 as f64)),
                2 => {
                    let b = self.take(2)?;
                    Some(Scalar::Number(i16::from_be_bytes([b[0], b[1]]) as f64))
                }
                3 => Some(Scalar::Number(self.int()? as f64)),
                4 | 6 => {
                    let b = self.take(8)?;
                    let raw = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
                    Some(Scalar::Number(if tag == 4 {
                        i64::from_be_bytes(raw) as f64
                    } else {
                        f64::from_be_bytes(raw)
                    }))
                }
                5 => {
                    let b = self.take(4)?;
                    Some(Scalar::Number(
                        f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64
                    ))
                }
                8 => {
                    let bytes = self.string()?;
                    if wanted {
                        Some(Scalar::Text(
                            String::from_utf8(bytes)
                                .map_err(|_| "non-UTF8 biome color modifier")?,
                        ))
                    } else {
                        None
                    }
                }
                7 | 11 | 12 => {
                    let len = self.int()?;
                    if len < 0 {
                        return Err("negative NBT array".into());
                    }
                    let size = if tag == 7 {
                        1
                    } else if tag == 11 {
                        4
                    } else {
                        8
                    };
                    self.take((len as usize).checked_mul(size).ok_or("array overflow")?)?;
                    None
                }
                9 => {
                    let child = self.byte()?;
                    let len = self.int()?;
                    if len < 0 {
                        return Err("negative NBT list".into());
                    }
                    for _ in 0..len {
                        self.value(child, "[]", depth + 1)?;
                    }
                    None
                }
                10 => {
                    loop {
                        let child = self.byte()?;
                        if child == 0 {
                            break;
                        }
                        let bytes = self.string()?;
                        let key = std::str::from_utf8(&bytes).unwrap_or("<unknown>");
                        let child_path = if path.is_empty() {
                            key.to_owned()
                        } else {
                            format!("{}/{}", path, key)
                        };
                        self.value(child, &child_path, depth + 1)?;
                    }
                    None
                }
                _ => return Err("unexpected biome NBT tag".into()),
            };
            if wanted {
                if let Some(scalar) = scalar {
                    if self.fields.insert(path.to_owned(), scalar).is_some() {
                        return Err("duplicate biome color field".into());
                    }
                }
            }
            Ok(())
        }
    }
    let mut reader = Reader {
        data: bytes,
        offset: 0,
        fields: BTreeMap::new(),
    };
    if reader.byte()? != 10 {
        return Err("biome root is not a compound".into());
    }
    reader.value(10, "", 0)?;
    if reader.offset != bytes.len() {
        return Err("trailing biome NBT".into());
    }
    Ok(reader.fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use leafish_blocks::catalog::StateCatalog;
    use leafish_protocol::protocol::chunk767::ChunkSection767;
    use leafish_protocol::protocol::configuration::NetworkNbt;
    use leafish_protocol::protocol::play767::{
        BlockUpdate, ChunkWithLight, Dimension, WorldContext,
    };
    use std::io::Cursor;

    struct TestPack(HashMap<String, Vec<u8>>);
    impl crate::resources::Pack for TestPack {
        fn open(&self, name: &str) -> Option<Box<dyn Read>> {
            self.0
                .get(name)
                .map(|data| Box::new(Cursor::new(data.clone())) as Box<dyn Read>)
        }
    }
    fn fixture() -> (NativeChunkStore, Arc<RwLock<Factory>>, BiomeTints) {
        let catalog = Arc::new(StateCatalog::from_json(br#"{
            "minecraft:air":{"states":[{"id":0,"default":true}]},
            "test:cube":{"states":[{"id":1,"default":true}]},
            "test:entity":{"states":[{"id":2,"default":true}]},
            "test:glass":{"states":[{"id":3,"default":true}]},
            "minecraft:water":{"properties":{"level":["0","7","8"]},"states":[{"id":4,"default":true,"properties":{"level":"0"}},{"id":5,"properties":{"level":"7"}},{"id":6,"properties":{"level":"8"}}]},
            "minecraft:lava":{"properties":{"level":["0"]},"states":[{"id":7,"default":true,"properties":{"level":"0"}}]}
        }"#).unwrap());
        let mut store = NativeChunkStore::new(
            WorldContext {
                dimension: Dimension {
                    registry_id: 0,
                    id: "test:signed".into(),
                    min_y: -16,
                    height: 16,
                    has_skylight: true,
                },
                block_state_count: 8,
                biome_count: 1,
            },
            catalog,
        )
        .unwrap();
        store
            .insert_chunk(ChunkWithLight {
                x: -1,
                z: 2,
                heightmaps: NetworkNbt::from_bytes(&[10, 0]).unwrap(),
                sections: vec![ChunkSection767 {
                    section_y: -1,
                    non_empty_block_count: 0,
                    block_states: vec![0; 4096],
                    biomes: vec![0; 64],
                }],
                block_entities: vec![],
                light: LightData {
                    min_section_y: -2,
                    section_count: 3,
                    sky_mask: vec![7],
                    block_mask: vec![],
                    empty_sky_mask: vec![],
                    empty_block_mask: vec![7],
                    sky_arrays: vec![vec![255; 2048]; 3],
                    block_arrays: vec![],
                },
            })
            .unwrap();
        let mut assets = HashMap::new();
        for (name, alpha) in [("cube", 255), ("glass", 128)] {
            let state =
                serde_json::json!({"variants":{"":{"model":format!("test:block/{}",name)}}});
            assets.insert(
                format!("assets/test/blockstates/{}.json", name),
                serde_json::to_vec(&state).unwrap(),
            );
            let faces: serde_json::Map<String, serde_json::Value> = Direction::all()
                .iter()
                .map(|direction| {
                    (
                        direction.as_string().into(),
                        serde_json::json!({"texture":"#all","cullface":direction.as_string()}),
                    )
                })
                .collect();
            let model = serde_json::json!({"textures":{"all":format!("test:block/{}",name)},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":faces}]});
            assets.insert(
                format!("assets/test/models/block/{}.json", name),
                serde_json::to_vec(&model).unwrap(),
            );
            let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                16,
                16,
                image::Rgba([255, 255, 255, alpha]),
            ));
            let mut png = Cursor::new(Vec::new());
            image.write_to(&mut png, image::ImageFormat::Png).unwrap();
            assets.insert(
                format!("assets/test/textures/block/{}.png", name),
                png.into_inner(),
            );
        }
        assets.insert(
            "assets/test/blockstates/entity.json".into(),
            br#"{"variants":{"":{"model":"test:block/entity"}}}"#.to_vec(),
        );
        assets.insert(
            "assets/test/models/block/entity.json".into(),
            br#"{"elements":[]}"#.to_vec(),
        );
        for name in ["water", "lava"] {
            assets.insert(
                format!("assets/minecraft/blockstates/{}.json", name),
                br#"{"variants":{"":{"model":"test:block/entity"}}}"#.to_vec(),
            );
            for kind in ["still", "flow"] {
                let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                    16,
                    16,
                    image::Rgba([255, 255, 255, if name == "water" { 180 } else { 255 }]),
                ));
                let mut png = Cursor::new(Vec::new());
                image.write_to(&mut png, image::ImageFormat::Png).unwrap();
                assets.insert(
                    format!("assets/minecraft/textures/block/{}_{}.png", name, kind),
                    png.into_inner(),
                );
            }
        }
        let resources = Arc::new(RwLock::new(crate::resources::Manager::from_packs(vec![
            Box::new(TestPack(assets)),
        ])));
        let (textures, _, _) = crate::render::TextureManager::new(resources.clone());
        let factory = Arc::new(RwLock::new(Factory::new(
            resources,
            Arc::new(RwLock::new(textures)),
        )));
        let tints = BiomeTints {
            biomes: vec![Biome {
                temperature: 0.8,
                downfall: 0.4,
                grass: Some(0x123456),
                foliage: None,
                water: 0x3f76e4,
                grass_modifier: "none".into(),
            }],
        };
        (store, factory, tints)
    }

    #[test]
    fn actual_named_mesh_culls_only_opaque_neighbor_and_preserves_local_geometry_light() {
        let (mut store, models, tints) = fixture();
        store
            .apply_block_updates(&[
                BlockUpdate {
                    position: [-15, -15, 33],
                    state_id: 1,
                },
                BlockUpdate {
                    position: [-14, -15, 33],
                    state_id: 1,
                },
                BlockUpdate {
                    position: [-10, -15, 33],
                    state_id: 2,
                },
            ])
            .unwrap();
        let mesh = build_section(&store, (-1, -1, 2), &models, &tints).unwrap();
        assert_eq!((mesh.solid_count, mesh.trans_count), (60, 0));
        assert_eq!(mesh.solid_buffer.len(), 40 * 40);
        assert_eq!(mesh.unsupported_states, [2].iter().copied().collect());
        assert_eq!(mesh.missing_light_samples, 0);
        for v in mesh.solid_buffer.chunks_exact(40) {
            let x = f32::from_ne_bytes([v[0], v[1], v[2], v[3]]);
            let y = f32::from_ne_bytes([v[4], v[5], v[6], v[7]]);
            assert!((1.0..=3.0).contains(&x) && (1.0..=2.0).contains(&y));
            assert_eq!(u16::from_ne_bytes([v[34], v[35]]), 60000);
        }
        assert_eq!(models.read().native_texture_alpha.len(), 1);
        store
            .apply_block_updates(&[BlockUpdate {
                position: [-14, -15, 33],
                state_id: 3,
            }])
            .unwrap();
        let mesh = build_section(&store, (-1, -1, 2), &models, &tints).unwrap();
        assert_eq!((mesh.solid_count, mesh.trans_count), (36, 30));
        assert_eq!(models.read().native_texture_alpha.len(), 2);
        models.write().version_change();
        assert!(models.read().native_texture_alpha.is_empty());
    }

    #[test]
    fn rendered_position_offsets_match_selection_boxes_and_disable_full_face_culling() {
        let (mut store, models, tints) = fixture();
        let hash = "a".repeat(64);
        let states: Vec<_> = store.catalog().states().iter().map(|state| {
            let shifted = state.id() == 1;
            serde_json::json!({
                "id":state.id(),"name":state.name(),"properties":state.properties(),
                "dynamic":shifted,"has_offset":shifted,
                "collision":if shifted {vec![[0.,0.,0.,1.,1.,1.]]} else {vec![]},
                "outline":if shifted {vec![[0.,0.,0.,1.,1.,1.]]} else {vec![]},
                "collision_offset":shifted,"outline_offset":shifted,
                "collision_context":"independent",
                "offset":if shifted {serde_json::json!({"kind":"xyz","max_horizontal":0.25,"max_vertical":0.2})} else {serde_json::Value::Null},
                "movement":{"friction":0.6,"speed_factor":1.0,"jump_factor":1.0}
            })
        }).collect();
        let report = serde_json::json!({"schema_version":2,"minecraft_version":"1.21.1","block_catalog_sha256":hash,"states":states});
        let shapes = crate::modern_shapes::ShapeCatalog::from_json(
            &serde_json::to_vec(&report).unwrap(),
            store.catalog(),
            &hash,
        )
        .unwrap();
        let pos = [-15, -15, 33];
        store
            .apply_block_updates(&[
                BlockUpdate {
                    position: pos,
                    state_id: 1,
                },
                BlockUpdate {
                    position: [-14, -15, 33],
                    state_id: 1,
                },
            ])
            .unwrap();
        let mesh =
            build_section_with_shapes(&store, (-1, -1, 2), &models, &tints, &shapes).unwrap();
        assert_eq!((mesh.solid_count, mesh.trans_count), (72, 0));
        let bounds = shapes.outline_at(1, pos).unwrap()[0];
        assert_ne!(shapes.offset_for(1, pos).unwrap(), [0.0; 3]);
        for axis in 0..3 {
            let coordinates: Vec<_> = mesh
                .solid_buffer
                .chunks_exact(40)
                .take(24)
                .map(|v| {
                    let i = axis * 4;
                    f32::from_ne_bytes([v[i], v[i + 1], v[i + 2], v[i + 3]]) as f64
                })
                .collect();
            let min = coordinates.iter().copied().fold(f64::INFINITY, f64::min);
            let max = coordinates
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            assert!((min - (1.0 + bounds.min[axis])).abs() < 1e-6);
            assert!((max - (1.0 + bounds.max[axis])).abs() < 1e-6);
        }
        // The compatibility entry point retains its original unshifted behavior.
        assert_eq!(
            build_section(&store, (-1, -1, 2), &models, &tints)
                .unwrap()
                .solid_count,
            60
        );
    }

    #[test]
    fn exact_water_and_lava_levels_render_installed_sprites_in_separate_layers() {
        let (mut store, models, tints) = fixture();
        for (id, water) in [(4, true), (5, true), (6, true), (7, false)] {
            store
                .apply_block_updates(&[BlockUpdate {
                    position: [-15, -15, 33],
                    state_id: id,
                }])
                .unwrap();
            let mesh = build_section(&store, (-1, -1, 2), &models, &tints).unwrap();
            assert!(mesh.unsupported_states.is_empty());
            assert!(mesh.missing_tint_states.is_empty());
            assert_eq!(mesh.missing_light_samples, 0);
            assert_eq!(
                (mesh.solid_count, mesh.trans_count),
                if water { (0, 66) } else { (66, 0) }
            );
            let data = if water {
                &mesh.trans_buffer
            } else {
                &mesh.solid_buffer
            };
            assert_eq!(data.len(), 44 * 40);
            assert!(data
                .chunks_exact(40)
                .any(|v| v[28..31] == if water { [63, 118, 228] } else { [255; 3] }));
            let highest = data
                .chunks_exact(40)
                .map(|v| f32::from_ne_bytes([v[4], v[5], v[6], v[7]]))
                .fold(f32::NEG_INFINITY, f32::max);
            let own = if id == 5 { 1.0 / 9.0 } else { 8.0 / 9.0 };
            let expected = 1.0 + weighted_fluid_corner(own, 0.0, 0.0, 0.0) - 0.001;
            assert!((highest - expected).abs() < 0.00001);
        }
    }

    #[test]
    fn fluid_height_reference_cases_include_falling_and_weighted_sources() {
        assert_eq!(own_fluid_height(0), 8.0 / 9.0);
        assert_eq!(own_fluid_height(7), 1.0 / 9.0);
        assert_eq!(own_fluid_height(15), 8.0 / 9.0);
        assert_eq!(weighted_fluid_corner(8.0 / 9.0, 1.0, 0.0, 0.0), 1.0);
        // Weighting by ten and dividing again in f32 can differ from the input
        // by one representable value, as in the reference's float arithmetic.
        assert!(
            (weighted_fluid_corner(8.0 / 9.0, -1.0, -1.0, 0.0) - 8.0 / 9.0).abs() <= f32::EPSILON
        );
        assert!((weighted_fluid_corner(8.0 / 9.0, 0.0, 0.0, 0.0) - 20.0 / 27.0).abs() < 0.000001);
    }

    #[test]
    fn light_masks_preserve_signed_boundary_sections_and_nibble_order() {
        let mut data = LightData {
            min_section_y: -5,
            section_count: 70,
            sky_mask: vec![1, 2],
            block_mask: vec![],
            empty_sky_mask: vec![2],
            empty_block_mask: vec![4],
            sky_arrays: vec![vec![0x3a; 2048], vec![0x71; 2048]],
            block_arrays: vec![],
        };
        assert_eq!(light_nibble(&data, -5, 0, true), Some(10));
        assert_eq!(light_nibble(&data, -5, 1, true), Some(3));
        assert_eq!(light_nibble(&data, 60, 4095, true), Some(7));
        assert_eq!(light_nibble(&data, -4, 0, true), Some(0));
        assert_eq!(light_nibble(&data, -3, 0, false), Some(0));
        assert_eq!(light_nibble(&data, -2, 0, true), None);
        data.sky_arrays.clear();
        assert_eq!(light_nibble(&data, -5, 0, true), None);
    }

    #[test]
    fn only_complete_boundary_geometry_can_occlude_neighbor() {
        let mut face = Face {
            cull_face: Direction::Up,
            facing: Direction::Up,
            vertices: BlockVertex::face_by_direction(Direction::Up).to_vec(),
            vertices_texture: vec![],
            indices: 6,
            shade: true,
            tint_index: -1,
        };
        assert!(full_boundary_face(&face));
        face.vertices[0].x = 0.25;
        assert!(!full_boundary_face(&face));
        face.vertices = BlockVertex::face_by_direction(Direction::Up).to_vec();
        for v in &mut face.vertices {
            v.y = 0.5;
        }
        assert!(!full_boundary_face(&face));
    }

    #[test]
    fn biome_registry_parser_reads_climate_nested_colors_and_rejects_truncation() {
        let mut bytes = vec![10, 5, 0, 11];
        bytes.extend(b"temperature");
        bytes.extend(0.8f32.to_be_bytes());
        bytes.extend([5, 0, 8]);
        bytes.extend(b"downfall");
        bytes.extend(0.4f32.to_be_bytes());
        bytes.extend([10, 0, 7]);
        bytes.extend(b"effects");
        bytes.extend([3, 0, 11]);
        bytes.extend(b"water_color");
        bytes.extend(0x3f76e4i32.to_be_bytes());
        bytes.extend([0, 0]);
        let nbt =
            leafish_protocol::protocol::configuration::NetworkNbt::from_bytes(&bytes).unwrap();
        let tints = BiomeTints::from_registry(&[RegistryEntry {
            id: "test:climate".into(),
            data: Some(nbt),
        }])
        .unwrap();
        assert_eq!(tints.biomes.len(), 1);
        assert_eq!(tints.biomes[0].water, 0x3f76e4);
        assert!((tints.biomes[0].temperature - 0.8).abs() < 0.000001);
        assert!(read_biome_fields(&bytes[..bytes.len() - 1]).is_err());
    }
}
