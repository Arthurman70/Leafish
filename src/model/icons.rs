//! Bounded CPU rendering of installed item models into the existing UI atlas.
//! Geometry, material UVs and GUI transforms come from the resource pack. This
//! does not substitute a placed block for an unsupported item/entity renderer.
use super::{definition, Factory};
use crate::{render::TextureManager, resources};
use image::{DynamicImage, Rgba, RgbaImage};
use leafish_blocks::catalog::StateCatalog;
use parking_lot::RwLock;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

const SIZE: u32 = 32;
const CACHE_LIMIT: usize = 128;
const MAX_FACES: usize = 768;
static NEXT_CACHE: AtomicU64 = AtomicU64::new(1);
type Result<T> = std::result::Result<T, String>;

struct Entry {
    texture: Option<String>,
    used: u64,
}

pub struct IconCache {
    resources: Arc<RwLock<resources::Manager>>,
    models: Arc<RwLock<Factory>>,
    textures: Arc<RwLock<TextureManager>>,
    entries: HashMap<String, Entry>,
    resource_version: usize,
    identity: u64,
    clock: u64,
}

impl IconCache {
    pub fn new(resources: Arc<RwLock<resources::Manager>>, models: Arc<RwLock<Factory>>) -> Self {
        let textures = models.read().textures.clone();
        let resource_version = resources.read().version();
        Self {
            resources,
            models,
            textures,
            entries: HashMap::new(),
            resource_version,
            identity: NEXT_CACHE.fetch_add(1, Ordering::Relaxed),
            clock: 0,
        }
    }

    /// The caller supplies names from its exact item registry. A catalog default
    /// state must also exist: this cache is for block items, not arbitrary items.
    pub fn hotbar_textures(
        &mut self,
        catalog: &StateCatalog,
        names: [Option<&str>; 9],
    ) -> [Option<String>; 9] {
        let version = self.resources.read().version();
        if version != self.resource_version {
            self.clear();
            self.resource_version = version;
        }
        std::array::from_fn(|i| names[i].and_then(|name| self.texture(catalog, name)))
    }

    fn texture(&mut self, catalog: &StateCatalog, name: &str) -> Option<String> {
        // Exact identity validation happens before any resource access.
        catalog.default_state(name).ok()?;
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(name) {
            entry.used = self.clock;
            return entry.texture.clone();
        }
        if self.entries.len() >= CACHE_LIMIT {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
                .unwrap();
            if let Some(texture) = self.entries.remove(&oldest).unwrap().texture {
                self.textures
                    .write()
                    .remove_dynamic(texture.strip_prefix("leafish-dynamic:").unwrap());
            }
        }
        let texture = match render_item(&self.resources, &self.models, name) {
            Ok(image) => {
                let key = format!("native-item-{}-{}", self.identity, self.clock);
                self.textures
                    .write()
                    .put_dynamic(&key, DynamicImage::ImageRgba8(image));
                Some(format!("leafish-dynamic:{key}"))
            }
            Err(reason) => {
                log::debug!("Text fallback for item {}: {}", name, reason);
                None
            }
        };
        self.entries.insert(
            name.to_owned(),
            Entry {
                texture: texture.clone(),
                used: self.clock,
            },
        );
        texture
    }

    fn clear(&mut self) {
        let mut textures = self.textures.write();
        for (_, entry) in self.entries.drain() {
            if let Some(name) = entry.texture {
                textures.remove_dynamic(name.strip_prefix("leafish-dynamic:").unwrap());
            }
        }
    }
}
impl Drop for IconCache {
    fn drop(&mut self) {
        self.clear();
    }
}

fn read(
    resources: &Arc<RwLock<resources::Manager>>,
    namespace: &str,
    path: &str,
) -> Result<Vec<u8>> {
    let reader = resources
        .read()
        .open(namespace, path)
        .ok_or_else(|| format!("missing {namespace}:{path}"))?;
    let mut bytes = Vec::new();
    reader
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("icon asset exceeds byte bound".into());
    }
    Ok(bytes)
}
fn json(resources: &Arc<RwLock<resources::Manager>>, namespace: &str, path: &str) -> Result<Value> {
    serde_json::from_slice(&read(resources, namespace, &format!("models/{path}.json"))?)
        .map_err(|e| e.to_string())
}

#[derive(Clone, Debug)]
struct Gui {
    rotation: [f64; 3],
    translation: [f64; 3],
    scale: [f64; 3],
}
impl Default for Gui {
    fn default() -> Self {
        Self {
            rotation: [0.0; 3],
            translation: [0.0; 3],
            scale: [1.0; 3],
        }
    }
}
impl Gui {
    fn parse(value: &Value) -> Result<Self> {
        if !value.is_object() || value.get("right_rotation").is_some() {
            return Err("unsupported GUI transform extension".into());
        }
        let vector = |key: &str, default: [f64; 3]| -> Result<[f64; 3]> {
            let Some(value) = value.get(key) else {
                return Ok(default);
            };
            let values = value
                .as_array()
                .filter(|v| v.len() == 3)
                .ok_or("invalid GUI vector")?;
            let mut out = [0.0; 3];
            for i in 0..3 {
                out[i] = values[i]
                    .as_f64()
                    .filter(|n| n.is_finite())
                    .ok_or("invalid GUI coordinate")?;
            }
            Ok(out)
        };
        Ok(Self {
            rotation: vector("rotation", [0.0; 3])?.map(f64::to_radians),
            translation: vector("translation", [0.0; 3])?.map(|n| (n / 16.0).clamp(-5.0, 5.0)),
            scale: vector("scale", [1.0; 3])?.map(|n| n.clamp(-4.0, 4.0)),
        })
    }
    fn transform(&self, point: [f64; 3]) -> [f64; 3] {
        let mut point = std::array::from_fn(|i| (point[i] - 0.5) * self.scale[i]);
        // ItemTransform's Quaternion.rotationXYZ: rightmost Z acts first.
        for axis in [2, 1, 0] {
            point = rotate(point, axis, self.rotation[axis]);
        }
        std::array::from_fn(|i| point[i] + self.translation[i])
    }
}
fn rotate(mut p: [f64; 3], axis: usize, angle: f64) -> [f64; 3] {
    let (a, b) = match axis {
        0 => (1, 2),
        1 => (2, 0),
        _ => (0, 1),
    };
    let (s, c) = angle.sin_cos();
    let x = p[a];
    let y = p[b];
    p[a] = x * c - y * s;
    p[b] = x * s + y * c;
    p
}

fn gui_transform<F>(namespace: &str, path: &str, load: &F) -> Result<Gui>
where
    F: Fn(&str, &str) -> Result<Value>,
{
    let mut id = format!("{namespace}:{path}");
    let mut seen = Vec::new();
    for _ in 0..64 {
        if seen.contains(&id) {
            return Err("cyclic GUI transform inheritance".into());
        }
        seen.push(id.clone());
        let (namespace, path) = definition::resource_location(&id, "minecraft")?;
        if namespace == "minecraft" && path.starts_with("builtin/") {
            return Ok(Gui::default());
        }
        let model = load(namespace, path)?;
        if let Some(gui) = model.get("display").and_then(|v| v.get("gui")) {
            return Gui::parse(gui);
        }
        let Some(parent) = model.get("parent").and_then(Value::as_str) else {
            return Ok(Gui::default());
        };
        id = parent.to_owned();
    }
    Err("GUI inheritance exceeds bound".into())
}

fn texture_image(resources: &Arc<RwLock<resources::Manager>>, name: &str) -> Result<RgbaImage> {
    let (namespace, path) = definition::resource_location(name, "minecraft")?;
    let bytes = read(resources, namespace, &format!("textures/{path}.png"))?;
    let (width, height) =
        image::io::Reader::with_format(Cursor::new(&bytes), image::ImageFormat::Png)
            .into_dimensions()
            .map_err(|e| e.to_string())?;
    if width == 0
        || height == 0
        || width > 1024
        || height > 4096
        || width as u64 * height as u64 > 1024 * 1024
    {
        return Err("icon texture exceeds decoded pixel bound".into());
    }
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let metadata = resources
        .read()
        .open(namespace, &format!("textures/{path}.png.mcmeta"));
    let Some(metadata) = metadata else {
        return Ok(image);
    };
    let value = super::read_asset_json(metadata)?;
    let Some(animation) = value.get("animation") else {
        return Ok(image);
    };
    let fw = animation
        .get("width")
        .map(|v| v.as_u64().ok_or("invalid frame width"))
        .transpose()?;
    let fh = animation
        .get("height")
        .map(|v| v.as_u64().ok_or("invalid frame height"))
        .transpose()?;
    let (fw, fh) = match (fw, fh) {
        (None, None) => {
            let size = width.min(height) as u64;
            (size, size)
        }
        (w, h) => (w.unwrap_or(width as u64), h.unwrap_or(height as u64)),
    };
    if fw == 0
        || fh == 0
        || fw > width as u64
        || fh > height as u64
        || width as u64 % fw != 0
        || height as u64 % fh != 0
    {
        return Err("invalid animated texture dimensions".into());
    }
    let frame = match animation
        .get("frames")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        Some(frame) => frame
            .as_u64()
            .or_else(|| frame.get("index").and_then(Value::as_u64))
            .ok_or("invalid first texture frame")?,
        None => 0,
    };
    let columns = width as u64 / fw;
    if frame >= columns * (height as u64 / fh) {
        return Err("animation frame outside texture".into());
    }
    // Icons are static snapshots of the first declared animation frame.
    Ok(image::imageops::crop_imm(
        &image,
        ((frame % columns) * fw) as u32,
        ((frame / columns) * fh) as u32,
        fw as u32,
        fh as u32,
    )
    .to_image())
}

fn item_tint(
    name: &str,
    index: i32,
    resources: &Arc<RwLock<resources::Manager>>,
) -> Result<[u8; 3]> {
    if index < 0 {
        return Ok([255; 3]);
    }
    let color: i32 = match name {
        "minecraft:grass_block"
        | "minecraft:short_grass"
        | "minecraft:fern"
        | "minecraft:tall_grass"
        | "minecraft:large_fern" => {
            let map = texture_image(resources, "minecraft:colormap/grass")?;
            if map.dimensions() != (256, 256) {
                return Err("invalid grass item colormap".into());
            }
            let p = map.get_pixel(127, 127).0;
            return Ok([p[0], p[1], p[2]]);
        }
        "minecraft:spruce_leaves" => -10380959,
        "minecraft:birch_leaves" => -8345771,
        "minecraft:oak_leaves"
        | "minecraft:jungle_leaves"
        | "minecraft:acacia_leaves"
        | "minecraft:dark_oak_leaves"
        | "minecraft:vine" => -12012264,
        "minecraft:mangrove_leaves" => -7158200,
        "minecraft:lily_pad" => -9321636,
        name if name.starts_with("minecraft:") => -1,
        _ => return Err("custom item color handler is unsupported".into()),
    };
    Ok([(color >> 16) as u8, (color >> 8) as u8, color as u8])
}

#[derive(Clone, Copy)]
struct Vertex {
    p: [f64; 3],
    uv: [f64; 2],
}
struct Quad {
    vertices: [Vertex; 4],
    texture: String,
    tint: [u8; 3],
    brightness: f64,
}

fn render_item(
    resources: &Arc<RwLock<resources::Manager>>,
    models: &Arc<RwLock<Factory>>,
    name: &str,
) -> Result<RgbaImage> {
    let (namespace, path) = definition::resource_location(name, "minecraft")?;
    let path = format!("item/{path}");
    let load = |namespace: &str, path: &str| json(resources, namespace, path);
    let resolved = definition::resolve_model(namespace, &path, &load)?;
    if resolved
        .get("overrides")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
    {
        return Err("conditional item overrides need item component support".into());
    }
    let gui = gui_transform(namespace, &path, &load)?;
    let mut images = HashMap::new();
    let mut quads = Vec::new();
    let generated = resolved.get("parent").and_then(Value::as_str) == Some("builtin/generated");
    if resolved
        .get("parent")
        .and_then(Value::as_str)
        .is_some_and(|p| p.starts_with("builtin/") && p != "builtin/generated")
    {
        return Err("item requires a builtin entity renderer".into());
    }
    if generated {
        // Front-on generated sprites do not require extruded edge geometry.
        // A rotated generated item keeps text until that geometry is implemented.
        if gui.rotation.iter().any(|r| r.abs() > 1e-9) {
            return Err("rotated generated item requires edge extrusion".into());
        }
        let variables = resolved
            .get("textures")
            .and_then(Value::as_object)
            .ok_or("missing generated textures")?;
        for layer in 0..5 {
            let key = format!("layer{layer}");
            if !variables.contains_key(&key) {
                break;
            }
            let texture = definition::resolve_texture(variables, &format!("#{key}"))?;
            images.insert(texture.clone(), texture_image(resources, &texture)?);
            let tint = item_tint(name, layer, resources)?;
            let points = [
                [0.0, 0.0, 0.5],
                [0.0, 1.0, 0.5],
                [1.0, 0.0, 0.5],
                [1.0, 1.0, 0.5],
            ];
            let uv = [[0.0, 1.0], [0.0, 0.0], [1.0, 1.0], [1.0, 0.0]];
            quads.push(Quad {
                vertices: std::array::from_fn(|i| {
                    let mut p = gui.transform(points[i]);
                    p[2] += layer as f64 * 0.0001;
                    Vertex { p, uv: uv[i] }
                }),
                texture,
                tint,
                brightness: 1.0,
            });
        }
    } else {
        let factory = models.read();
        let raw = factory
            .parse_model(&resolved)
            .ok_or("item geometry did not parse")?;
        if raw.elements.is_empty() {
            return Err("item has no baked geometry".into());
        }
        if raw.elements.len() * 6 > MAX_FACES {
            return Err("item geometry exceeds icon work bound".into());
        }
        // Validate every actual material before baking can request a missing-texture fallback.
        for element in &raw.elements {
            for face in element.faces.iter().flatten() {
                let texture = raw.lookup_texture(&face.texture);
                if !images.contains_key(&texture) {
                    if images.len() >= 32 {
                        return Err("item material count exceeds bound".into());
                    }
                    images.insert(texture.clone(), texture_image(resources, &texture)?);
                }
            }
        }
        let model = factory.process_model(raw);
        drop(factory);
        let front_light = resolved.get("gui_light").and_then(Value::as_str) == Some("front");
        for face in model.faces {
            if face.vertices.len() != 4 || face.vertices_texture.len() != 4 {
                return Err("unsupported baked item primitive".into());
            }
            let vertices = std::array::from_fn(|i| {
                let v = &face.vertices[i];
                Vertex {
                    p: gui.transform([v.x as f64, v.y as f64, v.z as f64]),
                    uv: [
                        v.toffsetx as f64 / (v.tw as f64 * 16.0),
                        v.toffsety as f64 / (v.th as f64 * 16.0),
                    ],
                }
            });
            quads.push(Quad {
                texture: face.vertices_texture[0].name.clone(),
                tint: item_tint(name, face.tint_index, resources)?,
                brightness: if front_light {
                    1.0
                } else {
                    brightness(&vertices)
                },
                vertices,
            });
        }
    }
    if quads.is_empty() {
        return Err("item produced no visible geometry".into());
    }
    rasterize(&quads, &images)
}

fn brightness(vertices: &[Vertex; 4]) -> f64 {
    let a: [f64; 3] = std::array::from_fn(|i| vertices[2].p[i] - vertices[0].p[i]);
    let b: [f64; 3] = std::array::from_fn(|i| vertices[1].p[i] - vertices[0].p[i]);
    let normal = normalize([
        a[1] * b[2] - a[2] * b[1],
        -(a[2] * b[0] - a[0] * b[2]),
        a[0] * b[1] - a[1] * b[0],
    ]);
    let mut amount = 0.4;
    for mut light in [[0.2, 1.0, -0.7], [-0.2, 1.0, 0.7]] {
        light = normalize(light);
        // GlStateManager.setupGui3DDiffuseLighting's two YXZ rotations.
        light = rotate(light, 0, std::f64::consts::PI * 3.0 / 4.0);
        light = rotate(light, 1, -std::f64::consts::PI / 8.0);
        light = rotate(light, 0, 3.2375858);
        light = rotate(light, 1, 1.0821041);
        light[1] = -light[1];
        amount += (0..3).map(|i| normal[i] * light[i]).sum::<f64>().max(0.0) * 0.6;
    }
    amount.min(1.0)
}
fn normalize(v: [f64; 3]) -> [f64; 3] {
    let length = v.iter().map(|n| n * n).sum::<f64>().sqrt();
    if length < 1e-12 {
        [0.0; 3]
    } else {
        v.map(|n| n / length)
    }
}
fn edge(a: [f64; 3], b: [f64; 3], p: [f64; 3]) -> f64 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

fn rasterize(quads: &[Quad], images: &HashMap<String, RgbaImage>) -> Result<RgbaImage> {
    let mut fragments: Vec<Vec<(f64, [f64; 4])>> = vec![Vec::new(); (SIZE * SIZE) as usize];
    for quad in quads {
        let image = images
            .get(&quad.texture)
            .ok_or("baked item refers to an unvalidated material")?;
        let vertices = quad.vertices.map(|v| Vertex {
            p: [
                (v.p[0] + 0.5) * SIZE as f64,
                (0.5 - v.p[1]) * SIZE as f64,
                v.p[2],
            ],
            uv: v.uv,
        });
        for y in 0..SIZE {
            for x in 0..SIZE {
                let sample = [x as f64 + 0.5, y as f64 + 0.5, 0.0];
                for indices in [[0, 1, 2], [2, 1, 3]] {
                    let [a, b, c] = indices.map(|i| vertices[i]);
                    let area = edge(a.p, b.p, c.p);
                    if area <= 1e-10 {
                        continue;
                    } // GUI front faces, no guessed two-sided model.
                    let weights = [
                        edge(b.p, c.p, sample) / area,
                        edge(c.p, a.p, sample) / area,
                        edge(a.p, b.p, sample) / area,
                    ];
                    if weights.iter().any(|n| *n < -1e-9) {
                        continue;
                    }
                    let v = [a, b, c];
                    let uv: [f64; 2] = std::array::from_fn(|axis| {
                        (0..3).map(|i| weights[i] * v[i].uv[axis]).sum()
                    });
                    let px = (uv[0].clamp(0.0, 1.0 - f64::EPSILON) * image.width() as f64) as u32;
                    let py = (uv[1].clamp(0.0, 1.0 - f64::EPSILON) * image.height() as f64) as u32;
                    let pixel = image.get_pixel(px, py).0;
                    if pixel[3] != 0 {
                        let samples = &mut fragments[(y * SIZE + x) as usize];
                        if samples.len() >= 64 {
                            return Err("icon transparency exceeds fragment bound".into());
                        }
                        let depth = (0..3).map(|i| weights[i] * v[i].p[2]).sum();
                        samples.push((
                            depth,
                            [
                                pixel[0] as f64 * quad.tint[0] as f64 / 255.0 * quad.brightness,
                                pixel[1] as f64 * quad.tint[1] as f64 / 255.0 * quad.brightness,
                                pixel[2] as f64 * quad.tint[2] as f64 / 255.0 * quad.brightness,
                                pixel[3] as f64 / 255.0,
                            ],
                        ));
                    }
                    break; // Shared triangle edge must never double-blend a face.
                }
            }
        }
    }
    let mut output = RgbaImage::new(SIZE, SIZE);
    for (i, samples) in fragments.iter_mut().enumerate() {
        samples.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut rgb = [0.0; 3];
        let mut alpha = 0.0;
        for (_, pixel) in samples {
            for c in 0..3 {
                rgb[c] = pixel[c] * pixel[3] + rgb[c] * (1.0 - pixel[3]);
            }
            alpha = pixel[3] + alpha * (1.0 - pixel[3]);
        }
        if alpha > 0.0 {
            output.put_pixel(
                i as u32 % SIZE,
                i as u32 / SIZE,
                Rgba([
                    (rgb[0] / alpha).round() as u8,
                    (rgb[1] / alpha).round() as u8,
                    (rgb[2] / alpha).round() as u8,
                    (alpha * 255.0).round() as u8,
                ]),
            );
        }
    }
    if !output.pixels().any(|pixel| pixel[3] != 0) {
        return Err("item model has no visible pixels in GUI view".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Pack(HashMap<String, Vec<u8>>);
    impl resources::Pack for Pack {
        fn open(&self, name: &str) -> Option<Box<dyn Read>> {
            self.0
                .get(name)
                .map(|data| Box::new(Cursor::new(data.clone())) as Box<dyn Read>)
        }
    }
    fn fixture() -> (
        Arc<RwLock<resources::Manager>>,
        Arc<RwLock<Factory>>,
        StateCatalog,
    ) {
        let mut assets = HashMap::new();
        for (path, model) in [
            (
                "item/slab",
                serde_json::json!({"parent":"fixture:block/icon","display":{"ground":{"scale":[0.1,0.1,0.1]}}}),
            ),
            (
                "block/icon",
                serde_json::json!({"gui_light":"front","display":{"gui":{"rotation":[0,0,0]}},"textures":{"face":"fixture:block/pattern"},"elements":[{"from":[0,8,0],"to":[16,16,16],"faces":{"south":{"texture":"#face"}}}]}),
            ),
            (
                "item/flat",
                serde_json::json!({"parent":"minecraft:builtin/generated","textures":{"layer0":"fixture:block/pattern"}}),
            ),
            (
                "item/entity",
                serde_json::json!({"parent":"minecraft:builtin/entity"}),
            ),
            (
                "item/conditional",
                serde_json::json!({"parent":"fixture:block/icon","overrides":[{"predicate":{"custom_model_data":1},"model":"fixture:block/other"}]}),
            ),
        ] {
            let namespace = if path == "item/flat" {
                "minecraft"
            } else {
                "fixture"
            };
            assets.insert(
                format!("assets/{namespace}/models/{path}.json"),
                serde_json::to_vec(&model).unwrap(),
            );
        }
        let mut pattern = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 255, 255]));
        for y in 0..2 {
            for x in 0..4 {
                pattern.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        }
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(pattern)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        assets.insert(
            "assets/fixture/textures/block/pattern.png".into(),
            png.into_inner(),
        );
        let resources = Arc::new(RwLock::new(resources::Manager::from_packs(vec![Box::new(
            Pack(assets),
        )])));
        let (textures, _, _) = TextureManager::new(resources.clone());
        let models = Arc::new(RwLock::new(Factory::new(
            resources.clone(),
            Arc::new(RwLock::new(textures)),
        )));
        let catalog = StateCatalog::from_json(
            br#"{
            "fixture:slab":{"states":[{"id":0,"default":true}]},
            "minecraft:flat":{"states":[{"id":1,"default":true}]},
            "fixture:entity":{"states":[{"id":2,"default":true}]},
            "fixture:conditional":{"states":[{"id":3,"default":true}]}
        }"#,
        )
        .unwrap();
        (resources, models, catalog)
    }

    #[test]
    fn exact_item_parent_geometry_uv_and_flat_sprite_are_preserved() {
        let (resources, models, _) = fixture();
        let slab = render_item(&resources, &models, "fixture:slab").unwrap();
        assert_eq!(slab.get_pixel(16, 8).0, [255, 0, 0, 255]);
        assert_eq!(slab.get_pixel(16, 24).0, [0, 0, 0, 0]);
        assert_eq!(slab.pixels().filter(|p| p[3] != 0).count(), 512);
        let flat = render_item(&resources, &models, "minecraft:flat").unwrap();
        assert_eq!(flat.get_pixel(16, 8).0, [255, 0, 0, 255]);
        assert_eq!(flat.get_pixel(16, 24).0, [0, 0, 255, 255]);
        assert!(render_item(&resources, &models, "fixture:entity").is_err());
        assert!(render_item(&resources, &models, "fixture:conditional").is_err());
    }

    #[test]
    fn gui_inherits_its_context_and_applies_scale_rotation_then_translation() {
        let models = HashMap::from([
            (
                "item/child",
                serde_json::json!({"parent":"test:block/parent","display":{"ground":{"scale":[0,0,0]}}}),
            ),
            (
                "block/parent",
                serde_json::json!({"display":{"gui":{"rotation":[0,0,90],"translation":[4,0,0],"scale":[0.5,0.5,0.5]}}}),
            ),
        ]);
        let gui = gui_transform("test", "item/child", &|_, path| Ok(models[path].clone())).unwrap();
        let point = gui.transform([1.0, 0.5, 0.5]);
        assert!(
            (point[0] - 0.25).abs() < 1e-9
                && (point[1] - 0.25).abs() < 1e-9
                && point[2].abs() < 1e-9
        );
        assert!(Gui::parse(&serde_json::json!({"rotation":[1,2]})).is_err());
    }

    #[test]
    fn transparency_depth_and_shared_triangle_edge_are_correct() {
        let vertices = [
            Vertex {
                p: [-0.5, -0.5, 0.0],
                uv: [0.0, 1.0],
            },
            Vertex {
                p: [-0.5, 0.5, 0.0],
                uv: [0.0, 0.0],
            },
            Vertex {
                p: [0.5, -0.5, 0.0],
                uv: [1.0, 1.0],
            },
            Vertex {
                p: [0.5, 0.5, 0.0],
                uv: [1.0, 0.0],
            },
        ];
        let front = vertices.map(|mut v| {
            v.p[2] = 0.1;
            v
        });
        let images = HashMap::from([
            (
                "red".into(),
                RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 255])),
            ),
            (
                "green".into(),
                RgbaImage::from_pixel(1, 1, Rgba([0, 255, 0, 128])),
            ),
        ]);
        let quads = [
            Quad {
                vertices: front,
                texture: "green".into(),
                tint: [255; 3],
                brightness: 1.0,
            },
            Quad {
                vertices,
                texture: "red".into(),
                tint: [255; 3],
                brightness: 1.0,
            },
        ];
        let image = rasterize(&quads, &images).unwrap();
        assert!(image.pixels().all(|p| p.0 == [127, 128, 0, 255]));
    }

    #[test]
    fn cache_requires_catalog_identity_reuses_upload_and_releases_atlas_slots() {
        let (resources, models, catalog) = fixture();
        let mut cache = IconCache::new(resources, models);
        let first = cache.texture(&catalog, "fixture:slab").unwrap();
        assert_eq!(cache.texture(&catalog, "fixture:slab"), Some(first));
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.texture(&catalog, "fixture:missing").is_none());
        assert_eq!(cache.entries.len(), 1);
        for i in 0..CACHE_LIMIT - 1 {
            cache.entries.insert(
                format!("test:failure{i}"),
                Entry {
                    texture: None,
                    used: cache.clock + 1,
                },
            );
        }
        assert!(cache.texture(&catalog, "fixture:entity").is_none());
        assert_eq!(cache.entries.len(), CACHE_LIMIT);
        assert!(!cache.entries.contains_key("fixture:slab"));
        let replacement = cache.texture(&catalog, "minecraft:flat").unwrap();
        assert!(replacement.starts_with("leafish-dynamic:native-item-"));
        cache.clear();
        assert!(cache.entries.is_empty());
    }

    #[test]
    #[ignore = "requires caller-owned LEAFISH_CLIENT_JAR; optional LEAFISH_ICON_PREVIEW output"]
    fn installed_hotbar_items_use_their_real_assets() {
        let path = std::env::var_os("LEAFISH_CLIENT_JAR").unwrap();
        let (resources, _) =
            resources::Manager::from_local_sources(std::path::Path::new(&path), None, &[]).unwrap();
        let resources = Arc::new(RwLock::new(resources));
        let (textures, _, _) = TextureManager::new(resources.clone());
        let models = Arc::new(RwLock::new(Factory::new(
            resources.clone(),
            Arc::new(RwLock::new(textures)),
        )));
        let names = [
            "stone",
            "dirt",
            "oak_planks",
            "glass",
            "bricks",
            "cobblestone",
            "oak_log",
            "sand",
            "torch",
            "grass_block",
            "oak_leaves",
            "oak_stairs",
        ];
        let mut preview = RgbaImage::new(SIZE * names.len() as u32, SIZE);
        for (slot, name) in names.iter().enumerate() {
            let image = render_item(&resources, &models, &format!("minecraft:{name}"))
                .unwrap_or_else(|error| panic!("{}: {}", name, error));
            assert!(
                image.pixels().filter(|p| p[3] != 0).count() > 16,
                "{name} has too few visible pixels"
            );
            image::imageops::replace(&mut preview, &image, slot as i64 * SIZE as i64, 0);
        }
        assert!(render_item(&resources, &models, "minecraft:chest").is_err());
        if let Some(path) = std::env::var_os("LEAFISH_ICON_PREVIEW") {
            preview.save(path).unwrap();
        }
    }
}
