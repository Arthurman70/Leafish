//! Split GUI sprites introduced after the legacy `gui/widgets` atlas.
//! Resource metadata describes logical source pixels, independent of PNG resolution.

use super::Region;
use crate::render;
use serde_json::Value;
use std::io::Read;

#[derive(Clone, Copy)]
pub(super) enum Widget {
    Button { disabled: bool, highlighted: bool },
    Slider { highlighted: bool },
    SliderHandle { highlighted: bool },
}

impl Widget {
    fn path(self) -> &'static str {
        match self {
            Self::Button { disabled: true, .. } => "gui/sprites/widget/button_disabled",
            Self::Button {
                highlighted: true, ..
            } => "gui/sprites/widget/button_highlighted",
            Self::Button { .. } => "gui/sprites/widget/button",
            Self::Slider { highlighted: true } => "gui/sprites/widget/slider_highlighted",
            Self::Slider { highlighted: false } => "gui/sprites/widget/slider",
            Self::SliderHandle { highlighted: true } => {
                "gui/sprites/widget/slider_handle_highlighted"
            }
            Self::SliderHandle { highlighted: false } => "gui/sprites/widget/slider_handle",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Scaling {
    Stretch,
    Tile {
        width: u32,
        height: u32,
    },
    NineSlice {
        width: u32,
        height: u32,
        border: [u32; 4],
    },
}

impl Scaling {
    fn from_metadata(data: &[u8]) -> Result<Self, String> {
        let metadata: Value = serde_json::from_slice(data).map_err(|e| e.to_string())?;
        let Some(scaling) = metadata.get("gui").and_then(|gui| gui.get("scaling")) else {
            return Ok(Self::Stretch);
        };
        let number = |value: Option<&Value>| -> Result<u32, String> {
            value
                .and_then(Value::as_u64)
                .filter(|&n| n <= 4096)
                .map(|n| n as u32)
                .ok_or_else(|| "GUI dimensions must be integers in 0..=4096".into())
        };
        let kind = scaling
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("stretch");
        if kind == "stretch" {
            return Ok(Self::Stretch);
        }
        let width = number(scaling.get("width"))?;
        let height = number(scaling.get("height"))?;
        if width == 0 || height == 0 {
            return Err("GUI sprite dimensions must be positive".into());
        }
        match kind {
            "tile" => Ok(Self::Tile { width, height }),
            "nine_slice" => {
                let b = scaling.get("border").ok_or("Missing GUI border")?;
                let border = if b.is_object() {
                    [
                        number(b.get("left"))?,
                        number(b.get("top"))?,
                        number(b.get("right"))?,
                        number(b.get("bottom"))?,
                    ]
                } else {
                    [number(Some(b))?; 4]
                };
                if border[0] + border[2] >= width || border[1] + border[3] >= height {
                    return Err("GUI borders leave no tileable center".into());
                }
                Ok(Self::NineSlice {
                    width,
                    height,
                    border,
                })
            }
            _ => Err(format!("Unsupported GUI scaling type: {}", kind)),
        }
    }

    fn patches(self, width: f64, height: f64) -> Vec<Patch> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Vec::new();
        }
        let (source_width, source_height, xs, ys) = match self {
            Self::Stretch => {
                return vec![Patch {
                    destination: [0.0, 0.0, width, height],
                    uv: [0.0, 0.0, 1.0, 1.0],
                }]
            }
            Self::Tile {
                width: w,
                height: h,
            } => (
                w,
                h,
                tiled_axis(width, w as f64),
                tiled_axis(height, h as f64),
            ),
            Self::NineSlice {
                width: w,
                height: h,
                border: b,
            } => (
                w,
                h,
                sliced_axis(width, w, b[0], b[2]),
                sliced_axis(height, h, b[1], b[3]),
            ),
        };
        // Widget sizes are bounded by the window; also bound hostile pack tiling.
        if xs.len().saturating_mul(ys.len()) > 16384 {
            return Vec::new();
        }
        let mut result = Vec::with_capacity(xs.len() * ys.len());
        for x in xs {
            for y in &ys {
                result.push(Patch {
                    destination: [x.destination, y.destination, x.length, y.length],
                    uv: [
                        x.source / source_width as f64,
                        y.source / source_height as f64,
                        x.length / source_width as f64,
                        y.length / source_height as f64,
                    ],
                });
            }
        }
        result
    }
}

#[derive(Debug)]
struct Patch {
    destination: [f64; 4],
    uv: [f64; 4],
}

struct Axis {
    destination: f64,
    source: f64,
    length: f64,
}

fn tiled_axis(length: f64, tile: f64) -> Vec<Axis> {
    if length / tile > 16384.0 {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut destination = 0.0;
    while destination < length {
        let part = tile.min(length - destination);
        result.push(Axis {
            destination,
            source: 0.0,
            length: part,
        });
        destination += part;
    }
    result
}

fn sliced_axis(length: f64, source: u32, before: u32, after: u32) -> Vec<Axis> {
    let source = source as f64;
    if length == source {
        return vec![Axis {
            destination: 0.0,
            source: 0.0,
            length,
        }];
    }
    // Match Minecraft's clipping of borders when a widget is smaller than its frame.
    let before = (before as f64).min((length / 2.0).floor());
    let after = (after as f64).min((length / 2.0).floor());
    let mut result = Vec::new();
    if before > 0.0 {
        result.push(Axis {
            destination: 0.0,
            source: 0.0,
            length: before,
        });
    }
    for mut part in tiled_axis(length - before - after, source - before - after) {
        part.destination += before;
        part.source = before;
        result.push(part);
    }
    if after > 0.0 {
        result.push(Axis {
            destination: length - after,
            source: source - after,
            length: after,
        });
    }
    result
}

/// Returns false only when the selected modern asset does not exist, allowing
/// older resource packs to retain their original atlas rendering path.
pub(super) fn draw(
    renderer: &render::Renderer,
    widget: Widget,
    region: &Region,
    sw: f64,
    sh: f64,
    width: f64,
    height: f64,
    data: &mut Vec<u8>,
) -> bool {
    let path = widget.path();
    let scaling = {
        let resources = renderer.resources.read();
        if resources
            .open("minecraft", &format!("textures/{}.png", path))
            .is_none()
        {
            return false;
        }
        if let Some(metadata) =
            resources.open("minecraft", &format!("textures/{}.png.mcmeta", path))
        {
            let mut bytes = Vec::new();
            match metadata
                .take(64 * 1024)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())
                .and_then(|_| Scaling::from_metadata(&bytes))
            {
                Ok(scaling) => scaling,
                Err(error) => {
                    log::warn!("Invalid GUI sprite metadata for {}: {}", path, error);
                    Scaling::Stretch
                }
            }
        } else {
            Scaling::Stretch
        }
    };
    let texture = render::Renderer::get_texture(renderer.get_textures_ref(), path);
    // Leafish's UI coordinates use two units per Minecraft GUI pixel.
    let scale_x = sw * 2.0;
    let scale_y = sh * 2.0;
    for patch in scaling.patches(region.w / scale_x, region.h / scale_y) {
        let [x, y, w, h] = patch.destination;
        let [u, v, uw, vh] = patch.uv;
        let part = texture.relative(u as f32, v as f32, uw as f32, vh as f32);
        data.extend(
            render::ui::UIElement::new(
                &part,
                region.x + x * scale_x,
                region.y + y * scale_y,
                w * scale_x,
                h * scale_y,
                0.0,
                0.0,
                1.0,
                1.0,
            )
            .bytes(width, height),
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a user-provided LEAFISH_GUI_REFERENCE_ARCHIVE"]
    fn installed_widget_art_and_metadata_cover_each_state() {
        let path =
            std::env::var_os("LEAFISH_GUI_REFERENCE_ARCHIVE").expect("reference archive path");
        let file = std::fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        for widget in [
            Widget::Button {
                disabled: false,
                highlighted: false,
            },
            Widget::Button {
                disabled: false,
                highlighted: true,
            },
            Widget::Button {
                disabled: true,
                highlighted: false,
            },
            Widget::Slider { highlighted: false },
            Widget::Slider { highlighted: true },
            Widget::SliderHandle { highlighted: false },
            Widget::SliderHandle { highlighted: true },
        ] {
            let name = format!("assets/minecraft/textures/{}.png", widget.path());
            let mut png = Vec::new();
            archive
                .by_name(&name)
                .unwrap()
                .read_to_end(&mut png)
                .unwrap();
            let image = image::load_from_memory(&png).unwrap();
            let mut metadata = Vec::new();
            archive
                .by_name(&format!("{}.mcmeta", name))
                .unwrap()
                .read_to_end(&mut metadata)
                .unwrap();
            let scaling = Scaling::from_metadata(&metadata).unwrap();
            let Scaling::NineSlice { width, height, .. } = scaling else {
                panic!("Expected installed nine-slice metadata")
            };
            assert_eq!(image.width(), width);
            assert_eq!(image.height(), height);
            assert_eq!(
                scaling.patches(width as f64, height as f64)[0].uv,
                [0.0, 0.0, 1.0, 1.0]
            );
            for (w, h) in [(10.0, 20.0), (150.0, 20.0), (300.0, 35.0)] {
                let patches = scaling.patches(w, h);
                assert!(!patches.is_empty());
                assert_eq!(
                    patches
                        .iter()
                        .map(|p| p.destination[2] * p.destination[3])
                        .sum::<f64>(),
                    w * h
                );
            }
        }
    }

    #[test]
    fn widget_states_use_distinct_installed_sprite_names() {
        assert_eq!(
            Widget::Button {
                disabled: true,
                highlighted: true
            }
            .path(),
            "gui/sprites/widget/button_disabled"
        );
        assert_eq!(
            Widget::Button {
                disabled: false,
                highlighted: true
            }
            .path(),
            "gui/sprites/widget/button_highlighted"
        );
        assert_eq!(
            Widget::SliderHandle { highlighted: false }.path(),
            "gui/sprites/widget/slider_handle"
        );
    }

    #[test]
    fn scalar_and_asymmetric_borders_are_read_from_pack_metadata() {
        assert_eq!(
            Scaling::from_metadata(
                br#"{"gui":{"scaling":{"type":"nine_slice","width":200,"height":20,"border":3}}}"#
            )
            .unwrap(),
            Scaling::NineSlice {
                width: 200,
                height: 20,
                border: [3; 4]
            }
        );
        assert_eq!(Scaling::from_metadata(br#"{"gui":{"scaling":{"type":"nine_slice","width":8,"height":20,"border":{"left":2,"top":2,"right":2,"bottom":3}}}}"#).unwrap(),
            Scaling::NineSlice { width: 8, height: 20, border: [2, 2, 2, 3] });
        assert!(Scaling::from_metadata(
            br#"{"gui":{"scaling":{"type":"nine_slice","width":4,"height":4,"border":2}}}"#
        )
        .is_err());
    }

    #[test]
    fn same_size_is_one_exact_image_and_width_resize_keeps_full_height() {
        let scaling = Scaling::NineSlice {
            width: 200,
            height: 20,
            border: [3; 4],
        };
        let exact = scaling.patches(200.0, 20.0);
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].uv, [0.0, 0.0, 1.0, 1.0]);
        let wide = scaling.patches(250.0, 20.0);
        assert_eq!(wide.len(), 4);
        assert_eq!(wide[0].destination, [0.0, 0.0, 3.0, 20.0]);
        assert_eq!(wide[1].destination, [3.0, 0.0, 194.0, 20.0]);
        assert_eq!(wide[2].destination, [197.0, 0.0, 50.0, 20.0]);
        assert_eq!(wide[3].destination, [247.0, 0.0, 3.0, 20.0]);
    }

    #[test]
    fn resized_handle_covers_widget_without_stretching_or_overlapping_borders() {
        let scaling = Scaling::NineSlice {
            width: 8,
            height: 20,
            border: [2, 2, 2, 3],
        };
        for (width, height) in [(10.0, 30.0), (3.0, 3.0), (1.0, 1.0)] {
            let patches = scaling.patches(width, height);
            let area: f64 = patches
                .iter()
                .map(|p| p.destination[2] * p.destination[3])
                .sum();
            assert_eq!(area, width * height);
            for patch in patches {
                let [x, y, w, h] = patch.destination;
                assert!(x >= 0.0 && y >= 0.0 && x + w <= width && y + h <= height);
                let [u, v, uw, vh] = patch.uv;
                assert!(u >= 0.0 && v >= 0.0 && u + uw <= 1.0 && v + vh <= 1.0);
            }
        }
    }

    #[test]
    fn missing_scaling_stretches_and_tile_crops_the_last_repeat() {
        assert_eq!(
            Scaling::from_metadata(br#"{"animation":{}}"#).unwrap(),
            Scaling::Stretch
        );
        let tiles = Scaling::Tile {
            width: 4,
            height: 4,
        }
        .patches(6.0, 3.0);
        assert_eq!(tiles.len(), 2);
        assert_eq!(tiles[1].destination, [4.0, 0.0, 2.0, 3.0]);
        assert_eq!(tiles[1].uv, [0.0, 0.0, 0.5, 0.75]);
    }
}
