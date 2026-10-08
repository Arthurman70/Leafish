//! Local vanilla 1.21.1 creative session using exact native identities.
//! This path is separate from the legacy client and never authenticates remotely.
use crate::model::{icons::IconCache, native_mesh, Factory};
use crate::modern_controls;
use crate::modern_hud::{Hud, PickerView, PICKER_ROWS};
use crate::modern_interaction::HeldInteraction;
use crate::modern_motion;
use crate::modern_shapes::{self, ShapeCatalog};
use crate::render::{ChunkBuffer, Renderer};
use crate::server::modern::{LocalTransform, ModernConnection, ModernEvent, ModernEvents};
use crate::settings::{Actionkey, FloatSetting, IntSetting};
use crate::shared::Position;
use crate::ui;
use crate::world::native::NativeChunkStore;
use crate::Game;
use leafish_blocks::catalog::StateCatalog;
use leafish_protocol::protocol::play767::{self as play, PlainItemStack};
use parking_lot::RwLock;
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::event::{DeviceEvent, ElementState, Event, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey, PhysicalKey};
use winit::window::{CursorGrabMode, Window};

type Result<T> = std::result::Result<T, String>;
type Section = (i32, i32, i32);
// Vanilla 1.21.1 block_interaction_range 4.5 + Creative modifier 0.5.
const CREATIVE_REACH: f64 = 5.0;
// A single block item is valid even for signs and unstackable shulker boxes.
// Creative placement does not consume it; item stack-limit metadata is pending.
const PICKER_COUNT: u8 = 1;
const CHECK_PICKER_ITEMS: [&str; 3] = [
    "minecraft:oak_sign",
    "minecraft:shulker_box",
    "minecraft:oak_planks",
];
#[derive(Default)]
struct PlayCheck {
    stage: u8,
    stage_started: Option<Instant>,
    origin: Option<[f64; 3]>,
    target: Option<[i32; 3]>,
    initial_state: Option<u32>,
    picker_probe: usize,
    completed: bool,
}
const HOTBAR: [&str; 9] = [
    "stone",
    "dirt",
    "oak_planks",
    "glass",
    "bricks",
    "cobblestone",
    "oak_log",
    "sand",
    "torch",
];

#[derive(Default)]
struct MotionCheck {
    stage: u8,
    stage_started: Option<Instant>,
    origin: Option<[f64; 3]>,
    step_peak: f64,
    jump_base: f64,
    jump_peak: f64,
    completed: bool,
}

struct BlockPicker {
    query: String,
    matches: Vec<usize>,
    cursor: usize,
}
impl BlockPicker {
    fn new(choices: &[String]) -> Self {
        Self {
            query: String::new(),
            matches: (0..choices.len()).collect(),
            cursor: 0,
        }
    }
    fn filter(&mut self, choices: &[String]) {
        self.matches = filter_blocks(choices, &self.query);
        self.cursor = 0;
    }
    fn move_cursor(&mut self, delta: i32) {
        self.cursor = (self.cursor as i64 + delta as i64)
            .clamp(0, self.matches.len().saturating_sub(1) as i64) as usize;
    }
    fn offset(&self) -> usize {
        self.cursor / PICKER_ROWS * PICKER_ROWS
    }
}

fn selectable_blocks(catalog: &StateCatalog, items: &[String]) -> Vec<String> {
    let items: BTreeSet<&str> = items.iter().map(String::as_str).collect();
    let mut choices: Vec<String> = catalog
        .states()
        .iter()
        .filter(|state| state.is_default() && !is_air(state.name()) && items.contains(state.name()))
        .map(|state| state.name().to_owned())
        .collect();
    choices.sort_unstable();
    choices.dedup();
    choices
}

fn filter_blocks(choices: &[String], query: &str) -> Vec<usize> {
    let query = query.to_ascii_lowercase().replace('_', " ");
    let words: Vec<&str> = query.split_whitespace().collect();
    choices
        .iter()
        .enumerate()
        .filter_map(|(index, name)| {
            let name = name.replace('_', " ");
            words
                .iter()
                .all(|word| name.contains(word))
                .then_some(index)
        })
        .collect()
}

pub struct ModernSession {
    connection: Arc<ModernConnection>,
    events: ModernEvents,
    catalog: Arc<StateCatalog>,
    shapes: ShapeCatalog,
    items: Arc<Vec<String>>,
    store: Option<NativeChunkStore>,
    models: Arc<RwLock<Factory>>,
    icons: IconCache,
    tints: native_mesh::BiomeTints,
    buffers: BTreeMap<Section, Arc<RwLock<ChunkBuffer>>>,
    dirty: BTreeSet<Section>,
    transform: Option<LocalTransform>,
    abilities: Option<play::PlayerAbilities>,
    hotbar: [Option<PlainItemStack>; 9],
    selected: u8,
    provisioned: bool,
    inventory_received: bool,
    flight_requested: bool,
    flight: modern_controls::FlightController,
    ground: modern_controls::GroundController,
    flight_toggle: modern_controls::FlightToggle,
    motion_clock: f64,
    grounded: bool,
    entity_id: Option<i32>,
    sprinting: bool,
    interactions: HeldInteraction,
    pressed: [bool; 7],
    paused: bool,
    window_focused: bool,
    last_move: Instant,
    started: Instant,
    error: Option<String>,
    notice: Option<String>,
    hud: Hud,
    crosshair: ui::TextRef,
    block_choices: Vec<String>,
    picker: Option<BlockPicker>,
    pending_choice: Option<(u8, String)>,
    choices_confirmed: usize,
    unsupported: BTreeSet<u32>,
    pub movement_sent: usize,
    pub block_updates: usize,
    pub action_acks: usize,
    check: Option<PlayCheck>,
    motion_check: Option<MotionCheck>,
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Reference report exceeds 32 MiB".into());
    }
    Ok(bytes)
}

pub fn load_items(path: &Path) -> Result<Vec<String>> {
    let bytes = read_bounded(path)?;
    if format!("{:x}", Sha1::digest(&bytes)) != "7875827e253106b8922a19e751cd278963007120" {
        return Err("Item report differs from the pinned official 1.21.1 reference".into());
    }
    let report: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let entries = report["minecraft:item"]["entries"]
        .as_object()
        .ok_or("Missing exact item registry")?;
    if entries.is_empty() || entries.len() > 65536 {
        return Err("Invalid item registry size".into());
    }
    let mut items = vec![String::new(); entries.len()];
    for (name, entry) in entries {
        let id = entry["protocol_id"]
            .as_u64()
            .ok_or("Missing item protocol ID")? as usize;
        if id >= items.len() || !items[id].is_empty() || !name.starts_with("minecraft:") {
            return Err("Invalid vanilla item registry identity".into());
        }
        items[id] = name.clone();
    }
    if items.iter().any(String::is_empty) {
        return Err("Item registry is not contiguous".into());
    }
    Ok(items)
}

impl ModernSession {
    pub fn connect(
        address: SocketAddr,
        catalog_path: &Path,
        item_path: &Path,
        shape_path: &Path,
        renderer: &Arc<Renderer>,
        ui: &mut ui::Container,
    ) -> Result<Self> {
        let catalog_bytes = read_bounded(catalog_path)?;
        if format!("{:x}", Sha1::digest(&catalog_bytes))
            != "929c7b8f9ca1cd5b4d86588c81b0f5891f79b2e1"
        {
            return Err("Block report differs from the pinned official 1.21.1 reference".into());
        }
        let catalog = Arc::new(StateCatalog::from_json(&catalog_bytes).map_err(|e| e.to_string())?);
        if catalog.len() != 26684 {
            return Err("Expected the exact vanilla 1.21.1 catalog".into());
        }
        let items = Arc::new(load_items(item_path)?);
        let shapes = ShapeCatalog::from_json(
            &read_bounded(shape_path)?,
            &catalog,
            "0dde7f869588905763ea6ff2e7e01bec1db740a58d27477fd27c2f07fb029f73",
        )
        .map_err(|e| e.to_string())?;
        let (connection, events) = ModernConnection::connect_loopback_with_items(
            address,
            "ReformedLocal",
            catalog.clone(),
            items.clone(),
        )
        .map_err(|e| e.to_string())?;
        let tints = native_mesh::BiomeTints::from_registry(connection.biome_registry_entries())?;
        let hud = Hud::new(ui);
        let block_choices = selectable_blocks(&catalog, &items);
        let crosshair = ui::TextBuilder::new()
            .text("+")
            .alignment(ui::VAttach::Middle, ui::HAttach::Center)
            .draw_index(10)
            .create(ui);
        let models = Arc::new(RwLock::new(Factory::new(
            renderer.resources.clone(),
            renderer.get_textures(),
        )));
        let icons = IconCache::new(renderer.resources.clone(), models.clone());
        Ok(Self {
            connection,
            events,
            catalog,
            shapes,
            items,
            store: None,
            models,
            icons,
            tints,
            buffers: BTreeMap::new(),
            dirty: BTreeSet::new(),
            transform: None,
            abilities: None,
            hotbar: Default::default(),
            selected: 0,
            provisioned: false,
            inventory_received: false,
            flight_requested: false,
            flight: modern_controls::FlightController::new(),
            ground: modern_controls::GroundController::new(),
            flight_toggle: modern_controls::FlightToggle::default(),
            motion_clock: 0.0,
            grounded: false,
            entity_id: None,
            sprinting: false,
            interactions: HeldInteraction::default(),
            pressed: [false; 7],
            paused: false,
            window_focused: true,
            last_move: Instant::now(),
            started: Instant::now(),
            error: None,
            notice: None,
            hud,
            crosshair,
            block_choices,
            picker: None,
            pending_choice: None,
            choices_confirmed: 0,
            unsupported: BTreeSet::new(),
            movement_sent: 0,
            block_updates: 0,
            action_acks: 0,
            check: None,
            motion_check: None,
        })
    }

    fn fail(&mut self, error: impl ToString) {
        let message = error.to_string();
        log::error!("Local 1.21.1 session: {}", message);
        self.error = Some(message);
        self.paused = true;
        self.pressed = [false; 7];
        self.flight.reset_motion();
        self.ground.reset_motion();
        self.flight_toggle.reset();
        self.interactions.clear();
    }

    fn mark_chunk(&mut self, x: i32, z: i32) {
        if let Some(store) = &self.store {
            for (cx, cz) in [(x, z), (x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)] {
                if let Some(chunk) = store.chunk(cx, cz) {
                    for section in &chunk.sections {
                        // A current empty section can still own an old GPU mesh.
                        if section.block_states.iter().any(|&id| {
                            store
                                .catalog()
                                .state(id)
                                .map_or(true, |s| !is_air(s.name()))
                        }) || self.buffers.contains_key(&(cx, section.section_y, cz))
                        {
                            self.dirty.insert((cx, section.section_y, cz));
                        }
                    }
                }
            }
        }
    }

    fn apply_event(&mut self, event: ModernEvent) -> Result<()> {
        match event {
            ModernEvent::Joined { join, context } => {
                if join.game_mode != 1 {
                    return Err(
                        "This local client milestone requires the isolated Creative world".into(),
                    );
                }
                self.entity_id = Some(join.entity_id);
                self.store = Some(
                    NativeChunkStore::new(context, self.catalog.clone())
                        .map_err(|e| e.to_string())?,
                );
                log::info!(
                    "Joined native 1.21.1 Creative world {}",
                    join.dimension_name
                );
            }
            ModernEvent::Teleported { transform, .. } => {
                self.transform = Some(transform);
                self.flight.reset_motion();
                self.ground.reset_motion();
                self.flight_toggle.reset();
                self.motion_clock = 0.0;
            }
            ModernEvent::Chunk(chunk) => {
                let (x, z) = (chunk.x, chunk.z);
                self.store
                    .as_mut()
                    .ok_or("Chunk before join")?
                    .insert_chunk(chunk)
                    .map_err(|e| e.to_string())?;
                self.mark_chunk(x, z);
            }
            ModernEvent::Unload { x, z } => {
                if let Some(store) = self.store.as_mut() {
                    store.unload_chunk(x, z);
                }
                self.buffers.retain(|&(cx, _, cz), _| cx != x || cz != z);
                self.dirty.retain(|&(cx, _, cz)| cx != x || cz != z);
                self.mark_chunk(x, z);
            }
            ModernEvent::Light { x, z, data } => {
                let store = self.store.as_mut().ok_or("Light before join")?;
                if store.chunk(x, z).is_some() {
                    store.apply_light(x, z, data).map_err(|e| e.to_string())?;
                    self.mark_chunk(x, z);
                }
            }
            ModernEvent::BlockChanges(updates) => {
                let store = self.store.as_mut().ok_or("Block updates before join")?;
                let updates: Vec<_> = updates
                    .into_iter()
                    .filter(|u| {
                        store
                            .chunk(u.position[0] >> 4, u.position[2] >> 4)
                            .is_some()
                    })
                    .collect();
                store
                    .apply_block_updates(&updates)
                    .map_err(|e| e.to_string())?;
                self.block_updates += updates.len();
                for update in updates {
                    let [x, y, z] = update.position;
                    for (dx, dy, dz) in [
                        (0, 0, 0),
                        (-1, 0, 0),
                        (1, 0, 0),
                        (0, -1, 0),
                        (0, 1, 0),
                        (0, 0, -1),
                        (0, 0, 1),
                    ] {
                        self.dirty
                            .insert(((x + dx) >> 4, (y + dy).div_euclid(16), (z + dz) >> 4));
                    }
                }
            }
            ModernEvent::Abilities(abilities) => {
                self.flight.set_abilities(true, &abilities);
                self.abilities = Some(abilities);
            }
            ModernEvent::BlockActionAcknowledged(_) => {
                self.action_acks += 1;
            }
            ModernEvent::InventoryContent(content) => {
                if content.container_id == 0 {
                    self.inventory_received = true;
                    for i in 0..9 {
                        self.hotbar[i] = content.slots.get(36 + i).cloned().flatten();
                    }
                    self.confirm_chosen_block(None);
                }
            }
            ModernEvent::InventorySlot(slot) => {
                let i = if slot.container_id == 0 {
                    slot.slot - 36
                } else if slot.container_id == -2 {
                    slot.slot
                } else {
                    -1
                };
                if (0..9).contains(&i) {
                    self.hotbar[i as usize] = slot.item;
                    self.confirm_chosen_block(Some(i as u8));
                }
            }
            ModernEvent::SelectedSlot(slot) => {
                self.selected = slot;
            }
            ModernEvent::UnsupportedInventory { .. } => {
                log::warn!("Inventory component patch retained but not applied by the local Creative client");
            }
            ModernEvent::ConformanceCheckpoint { .. } => {}
        }
        Ok(())
    }

    fn prepare_creative(&mut self) -> Result<()> {
        if self.transform.is_none() {
            return Ok(());
        }
        let Some(abilities) = &self.abilities else {
            return Ok(());
        };
        if !abilities.may_fly || !abilities.instant_build {
            return Err("Local server has not granted Creative abilities".into());
        }
        if !self.flight_requested {
            self.flight_requested = true;
            if self.check.is_some() {
                self.set_flying(true)?;
            }
        }
        if !self.provisioned && self.inventory_received {
            // Seed a new empty profile only. Reopening a saved world preserves
            // the player's server-owned hotbar and selection.
            if self.hotbar.iter().all(Option::is_none) {
                for (i, item) in HOTBAR.iter().enumerate() {
                    self.connection
                        .set_creative_hotbar_slot(i as u8, &format!("minecraft:{item}"), 64)
                        .map_err(|e| e.to_string())?;
                }
                self.connection
                    .send(play::Serverbound::SelectedSlot(0))
                    .map_err(|e| e.to_string())?;
            }
            self.provisioned = true;
        }
        Ok(())
    }

    fn set_flying(&mut self, flying: bool) -> Result<()> {
        let Some(mut abilities) = self.abilities.clone() else {
            return Ok(());
        };
        if flying && !abilities.may_fly {
            return Ok(());
        }
        if flying == abilities.flying {
            return Ok(());
        }
        let mut velocity = if flying {
            self.ground.velocity()
        } else {
            self.flight.velocity()
        };
        if flying && self.grounded {
            if let (Some(store), Some(transform)) = (&self.store, &self.transform) {
                let environment =
                    modern_motion::environment(store, &self.shapes, transform.position)?;
                velocity[1] = (0.42_f32 * environment.jump_factor) as f64;
                if self.sprinting {
                    let (sin, cos) = modern_controls::movement_yaw(transform.rotation[0]);
                    velocity[0] -= sin * 0.2;
                    velocity[2] += cos * 0.2;
                }
            }
        }
        self.connection
            .send(play::Serverbound::Flying(flying))
            .map_err(|e| e.to_string())?;
        abilities.flying = flying;
        self.flight.set_abilities(true, &abilities);
        if flying {
            self.flight.reset_motion();
            self.flight.set_velocity(velocity);
        } else {
            self.ground.reset_motion();
            self.ground.set_velocity(velocity);
            self.ground.set_sprinting(self.sprinting);
        }
        self.abilities = Some(abilities);
        Ok(())
    }

    fn move_local(&mut self, seconds: f64) -> Result<()> {
        if let Some(authoritative) = self.connection.local_transform() {
            if self.transform.as_ref().map(|t| t.generation) != Some(authoritative.generation) {
                self.transform = Some(authoritative);
                self.flight.reset_motion();
                self.ground.reset_motion();
                self.flight_toggle.reset();
                self.motion_clock = 0.0;
            }
        }
        let Some(mut transform) = self.transform.clone() else {
            return Ok(());
        };
        if !self.paused && self.window_focused && self.flight_requested {
            self.motion_clock += seconds.min(0.1);
            while self.motion_clock + 1e-12 >= 0.05 {
                self.motion_clock = (self.motion_clock - 0.05).max(0.0);
                let active = self.picker.is_none();
                let forward = if active {
                    self.pressed[0] as u8 as f64 - self.pressed[1] as u8 as f64
                } else {
                    0.0
                };
                let axes = modern_controls::Axes {
                    forward,
                    right: if active {
                        self.pressed[3] as u8 as f64 - self.pressed[2] as u8 as f64
                    } else {
                        0.0
                    },
                    up: if active {
                        self.pressed[4] as u8 as f64 - self.pressed[5] as u8 as f64
                    } else {
                        0.0
                    },
                    sprint: active && self.pressed[6] && forward > 0.0,
                };
                if let Some(abilities) = &self.abilities {
                    if let Some(flying) = self.flight_toggle.tick(
                        active && self.pressed[4],
                        abilities.may_fly,
                        abilities.flying,
                    ) {
                        self.transform = Some(transform.clone());
                        self.set_flying(flying)?;
                    }
                }
                let flying = self.abilities.as_ref().is_some_and(|a| a.flying);
                let mut actual_sprint = axes.sprint;
                if let Some(store) = &self.store {
                    let moved = if flying {
                        let mut flight_axes = axes;
                        flight_axes.sprint = axes.forward > 0.0 && (axes.sprint || self.sprinting);
                        actual_sprint = flight_axes.sprint;
                        let candidate = self.flight.advance(
                            transform.position,
                            transform.rotation,
                            flight_axes,
                            0.05,
                        );
                        let delta = std::array::from_fn(|a| candidate[a] - transform.position[a]);
                        modern_shapes::move_player(store, &self.shapes, transform.position, delta)
                            .map(|movement| {
                                self.flight.clip_motion(movement.blocked);
                                self.grounded = delta[1] < 0.0 && movement.blocked[1];
                                movement.position
                            })
                            .map_err(|e| e.to_string())
                    } else {
                        let shapes = &self.shapes;
                        self.ground
                            .advance(
                                transform.position,
                                transform.rotation,
                                modern_controls::GroundInput {
                                    axes,
                                    jump: active && self.pressed[4],
                                },
                                modern_controls::GroundSettings::default(),
                                0.05,
                                |p| modern_motion::environment(store, shapes, p),
                                |p, d| modern_motion::collide(store, shapes, p, d),
                            )
                            .map(|movement| {
                                self.grounded = movement.grounded;
                                actual_sprint = movement.sprinting;
                                movement.position
                            })
                    };
                    match moved {
                        Ok(position) => {
                            transform.position = position;
                            self.notice = None;
                            if let Some(check) = self.motion_check.as_mut() {
                                if check.stage == 1 {
                                    check.step_peak = check.step_peak.max(position[1]);
                                }
                                if matches!(check.stage, 3 | 4) {
                                    check.jump_peak = check.jump_peak.max(position[1]);
                                }
                            }
                        }
                        Err(error) => {
                            self.flight.reset_motion();
                            self.ground.reset_motion();
                            self.grounded = false;
                            self.notice = Some(error);
                        }
                    }
                }
                if self.sprinting != actual_sprint {
                    if let Some(entity_id) = self.entity_id {
                        self.connection
                            .send(play::Serverbound::PlayerCommand {
                                entity_id,
                                action: if actual_sprint { 3 } else { 4 },
                            })
                            .map_err(|e| e.to_string())?;
                    }
                    self.sprinting = actual_sprint;
                }
                if flying && self.grounded {
                    self.transform = Some(transform.clone());
                    self.set_flying(false)?;
                }
            }
        } else {
            self.motion_clock = 0.0;
        }
        self.transform = Some(transform.clone());
        if let Err(error) = self.connection.update_local_transform(transform.clone()) {
            if let Some(current) = self.connection.local_transform() {
                self.transform = Some(current);
                return Ok(());
            }
            return Err(error.to_string());
        }
        if self.last_move.elapsed() >= Duration::from_millis(50) {
            self.last_move = Instant::now();
            if let Err(error) = self.connection.move_player(transform, self.grounded) {
                if let Some(current) = self.connection.local_transform() {
                    self.transform = Some(current);
                    return Ok(());
                }
                return Err(error.to_string());
            }
            self.movement_sent += 1;
        }
        Ok(())
    }

    pub fn frame(&mut self, window: &Window, game: &Game, ui: &mut ui::Container, seconds: f64) {
        let frame_started = Instant::now();
        if self.error.is_none() {
            for _ in 0..128 {
                match self.events.try_recv() {
                    Ok(event) => {
                        if let Err(error) = self.apply_event(event) {
                            self.fail(error);
                            break;
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(_) => {
                        self.fail(
                            self.connection
                                .stats()
                                .close_reason
                                .unwrap_or_else(|| "Connection closed".into()),
                        );
                        break;
                    }
                }
            }
            if self.error.is_none() {
                if let Err(error) = self
                    .advance_check()
                    .and_then(|_| self.advance_motion_check())
                {
                    self.fail(error);
                }
                if let Err(error) = self
                    .prepare_creative()
                    .and_then(|_| self.move_local(seconds))
                {
                    self.fail(error);
                }
            }
            self.repeat_interactions(seconds);
            let mesh_start = Instant::now();
            for _ in 0..4 {
                let Some(store) = &self.store else {
                    break;
                };
                let center = self.transform.as_ref().map_or([0.0; 3], |t| t.position);
                let next = self
                    .dirty
                    .iter()
                    .min_by_key(|&&(x, y, z)| {
                        let dx = x as f64 * 16.0 + 8.0 - center[0];
                        let dy = y as f64 * 16.0 + 8.0 - center[1];
                        let dz = z as f64 * 16.0 + 8.0 - center[2];
                        (dx * dx + dy * dy + dz * dz) as i64
                    })
                    .copied();
                let Some(pos) = next else {
                    break;
                };
                self.dirty.remove(&pos);
                if store.chunk(pos.0, pos.2).is_none()
                    || store.bounds().section_index(pos.1).is_none()
                {
                    continue;
                }
                match native_mesh::build_section_with_shapes(
                    store,
                    pos,
                    &self.models,
                    &self.tints,
                    &self.shapes,
                ) {
                    Ok(mesh) => {
                        self.unsupported.extend(mesh.unsupported_states);
                        self.unsupported.extend(mesh.missing_tint_states);
                        let buffer = self
                            .buffers
                            .entry(pos)
                            .or_insert_with(|| Arc::new(RwLock::new(ChunkBuffer::new())))
                            .clone();
                        game.renderer.update_chunk_solid(
                            buffer.clone(),
                            &mesh.solid_buffer,
                            mesh.solid_count,
                        );
                        game.renderer.update_chunk_trans(
                            buffer,
                            &mesh.trans_buffer,
                            mesh.trans_count,
                        );
                    }
                    Err(error) => {
                        self.fail(error);
                        break;
                    }
                }
                if mesh_start.elapsed() > Duration::from_millis(8) {
                    break;
                }
            }
        }
        let physical = window.inner_size();
        if physical.width == 0 || physical.height == 0 {
            return;
        }
        let logical = physical.to_logical::<u32>(window.scale_factor());
        if let Some(transform) = &self.transform {
            let mut camera = game.renderer.camera.lock();
            camera.pos = cgmath::Point3::new(
                transform.position[0],
                transform.position[1] + 1.62,
                transform.position[2],
            );
            (camera.yaw, camera.pitch) = modern_controls::renderer_angles(transform.rotation);
        }
        game.renderer.update_camera(physical.width, physical.height);
        game.renderer.light_data.lock().sky_offset = 1.0;
        let focused = !self.paused
            && self.picker.is_none()
            && self.window_focused
            && self.error.is_none()
            && self.transform.is_some();
        if focused != game.is_focused() {
            if focused {
                if window.set_cursor_grab(CursorGrabMode::Locked).is_err() {
                    let _ = window.set_cursor_grab(CursorGrabMode::Confined);
                }
            } else {
                let _ = window.set_cursor_grab(CursorGrabMode::None);
            }
            window.set_cursor_visible(!focused);
            game.set_focused(focused);
        }
        let status = if let Some(error) = &self.error {
            format!("Local session stopped: {error}")
        } else if self.paused {
            "Paused - Escape to resume; close window to save and quit".to_owned()
        } else if self.pending_choice.is_some() {
            "Waiting for the server to confirm the selected block...".to_owned()
        } else if let Some(notice) = &self.notice {
            format!("Local play: {notice}")
        } else {
            "Minecraft 1.21.1 | Creative | WASD move | Space jump | Double-Space flight | Mouse build"
                .to_owned()
        };
        let inventory = &self.hotbar;
        let items = &self.items;
        let hotbar = std::array::from_fn(|slot| {
            inventory[slot]
                .as_ref()
                .and_then(|stack| items.get(stack.item_id as usize))
                .map(String::as_str)
        });
        let picker_entries: Vec<String> = self.picker.as_ref().map_or_else(Vec::new, |picker| {
            picker
                .matches
                .iter()
                .skip(picker.offset())
                .take(PICKER_ROWS)
                .map(|&index| self.block_choices[index].clone())
                .collect()
        });
        let picker_view = self.picker.as_ref().map(|picker| PickerView {
            query: &picker.query,
            entries: &picker_entries,
            selected: picker.cursor.saturating_sub(picker.offset()),
            offset: picker.offset(),
            total: picker.matches.len(),
        });
        self.crosshair.borrow_mut().colour.3 = if self.picker.is_some() { 0 } else { 255 };
        let icons = self.icons.hotbar_textures(&self.catalog, hotbar);
        let icon_refs = std::array::from_fn(|slot| icons[slot].as_deref());
        self.hud.update(
            hotbar,
            icon_refs,
            self.selected as usize,
            &status,
            picker_view,
        );
        ui.tick(
            game.renderer.clone(),
            seconds * 60.0,
            logical.width as f64,
            logical.height as f64,
        );
        let list: Vec<_> = self
            .buffers
            .iter()
            .map(|(pos, b)| (*pos, b.clone()))
            .collect();
        game.renderer.tick_native(
            &list,
            seconds * 60.0,
            logical.width,
            logical.height,
            physical.width,
            physical.height,
        );
        let cap = game.settings.get_int(IntSetting::MaxFps);
        if cap > 0 {
            let budget = Duration::from_secs_f64(1.0 / cap as f64);
            if let Some(remaining) = budget.checked_sub(frame_started.elapsed()) {
                std::thread::sleep(remaining);
            }
        }
    }

    pub fn ready_for_capture(&self) -> bool {
        self.error.is_some()
            || (self.started.elapsed() > Duration::from_secs(5)
                && self.store.as_ref().is_some_and(|s| s.len() > 0)
                && self.dirty.is_empty()
                && !self.buffers.is_empty()
                && self.check.as_ref().map_or(true, |c| c.completed)
                && self.motion_check.as_ref().map_or(true, |c| c.completed))
    }

    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"protocol":767,"connected":self.connection.is_connected(),"chunks":self.store.as_ref().map_or(0,|s|s.len()),"meshes":self.buffers.len(),"pending_meshes":self.dirty.len(),"movement_packets":self.movement_sent,"block_updates":self.block_updates,"action_acknowledgments":self.action_acks,"unsupported_visual_states":self.unsupported,"error":self.error,"position":self.transform.as_ref().map(|t|t.position),"selected_item":self.hotbar[self.selected as usize].as_ref().and_then(|s|self.items.get(s.item_id as usize)),"full_gameplay_parity":false,
        "selected_slot":self.selected,"flying":self.abilities.as_ref().map(|a|a.flying),"grounded":self.grounded,"motion_check":self.motion_check.as_ref().map(|c|serde_json::json!({"completed":c.completed,"stage":c.stage,"origin":c.origin,"step_peak":c.step_peak,"jump_base":c.jump_base,"jump_peak":c.jump_peak})),
        "creative_block_choices":self.block_choices.len(),"creative_choices_confirmed":self.choices_confirmed,"pending_creative_choice":self.pending_choice,
        "interaction_check":self.check.as_ref().map(|c|serde_json::json!({"completed":c.completed,"stage":c.stage,"initial_position":c.origin,"target":c.target,"initial_state":c.initial_state,"final_state":c.target.and_then(|p|self.store.as_ref().and_then(|s|s.block_state_id(Position::new(p[0],p[1],p[2]))))}))})
    }

    pub fn enable_verification(&mut self) {
        self.check = Some(PlayCheck::default());
    }

    pub fn enable_motion_verification(&mut self) {
        self.motion_check = Some(MotionCheck::default());
    }

    fn advance_motion_check(&mut self) -> Result<()> {
        let Some(mut check) = self.motion_check.take() else {
            return Ok(());
        };
        let result = (|| -> Result<()> {
            if check.completed {
                return Ok(());
            }
            if self.started.elapsed() > Duration::from_secs(90) {
                return Err(format!(
                    "Motion verification timed out at stage {}",
                    check.stage
                ));
            }
            let Some(position) = self.transform.as_ref().map(|t| t.position) else {
                return Ok(());
            };
            let elapsed = check.stage_started.map_or(Duration::ZERO, |t| t.elapsed());
            match check.stage {
                0 if self.started.elapsed() > Duration::from_secs(5)
                    && self.provisioned
                    && self.dirty.is_empty() =>
                {
                    self.set_flying(false)?;
                    self.flight.reset_motion();
                    self.ground.reset_motion();
                    self.flight_toggle.reset();
                    check.origin = Some(position);
                    check.step_peak = position[1];
                    self.pressed = [false; 7];
                    self.pressed[0] = true;
                    check.stage = 1;
                    check.stage_started = Some(Instant::now());
                }
                1 => {
                    check.step_peak = check.step_peak.max(position[1]);
                    if position[2] - check.origin.unwrap()[2] >= 6.0 {
                        if check.step_peak - check.origin.unwrap()[1] < 0.49 {
                            return Err("Walking did not step onto the reference half-slab".into());
                        }
                        self.pressed = [false; 7];
                        self.ground.reset_motion();
                        check.stage = 2;
                        check.stage_started = Some(Instant::now());
                    }
                }
                2 if self.grounded && elapsed > Duration::from_millis(250) => {
                    check.jump_base = position[1];
                    check.jump_peak = position[1];
                    self.pressed[4] = true;
                    check.stage = 3;
                    check.stage_started = Some(Instant::now());
                }
                3 => {
                    check.jump_peak = check.jump_peak.max(position[1]);
                    if elapsed > Duration::from_millis(100) {
                        self.pressed[4] = false;
                        check.stage = 4;
                        check.stage_started = Some(Instant::now());
                    }
                }
                4 => {
                    check.jump_peak = check.jump_peak.max(position[1]);
                    if self.grounded && elapsed > Duration::from_millis(200) {
                        let height = check.jump_peak - check.jump_base;
                        if (height - 1.2522033402537238).abs() > 1e-6 {
                            return Err(format!("Reference jump height mismatch: {height}"));
                        }
                        self.pressed[4] = true;
                        check.stage = 5;
                        check.stage_started = Some(Instant::now());
                    }
                }
                5 if elapsed > Duration::from_millis(100) => {
                    self.pressed[4] = false;
                    check.stage = 6;
                    check.stage_started = Some(Instant::now());
                }
                6 if elapsed > Duration::from_millis(100) => {
                    self.pressed[4] = true;
                    check.stage = 7;
                    check.stage_started = Some(Instant::now());
                }
                7 if self.abilities.as_ref().is_some_and(|a| a.flying)
                    && position[1] - check.jump_base >= 2.0 =>
                {
                    self.pressed = [false; 7];
                    self.flight.reset_motion();
                    self.ground.reset_motion();
                    check.stage = 8;
                    check.stage_started = Some(Instant::now());
                    if let Some(t) = self.transform.as_mut() {
                        t.rotation = [0.0, 35.0];
                    }
                }
                8 if elapsed > Duration::from_millis(300) => {
                    check.completed = true;
                    check.stage = 9;
                    log::info!("Native walking, half-slab step, jump/landing and double-tap flight verified");
                }
                _ => {}
            }
            Ok(())
        })();
        self.motion_check = Some(check);
        result
    }

    /// Opt-in test driver sends the same controls/actions as the game. It never
    /// mutates received blocks; success requires authoritative server updates.
    fn advance_check(&mut self) -> Result<()> {
        let Some(mut check) = self.check.take() else {
            return Ok(());
        };
        let result = (|| -> Result<()> {
            if check.completed {
                return Ok(());
            }
            if self.started.elapsed() > Duration::from_secs(90) {
                return Err(format!(
                    "Local interaction verification timed out at stage {}",
                    check.stage
                ));
            }
            self.paused = false;
            self.window_focused = true;
            match check.stage {
                0 if self.started.elapsed() > Duration::from_secs(5)
                    && self.dirty.is_empty()
                    && !self.buffers.is_empty()
                    && self.hotbar.iter().all(Option::is_some) =>
                {
                    check.origin = self.transform.as_ref().map(|t| t.position);
                    self.pressed[3] = true;
                    self.pressed[4] = true;
                    check.stage_started = Some(Instant::now());
                    check.stage = 1;
                }
                1 if check.stage_started.unwrap().elapsed() > Duration::from_millis(100) => {
                    self.pressed = [false; 7];
                    self.flight.reset_motion();
                    let transform = self.transform.as_mut().ok_or("Player transform missing")?;
                    let origin = check.origin.unwrap();
                    if (transform.position[0] - origin[0]).abs()
                        + (transform.position[2] - origin[2]).abs()
                        < 0.05
                        || transform.position[1] <= origin[1]
                    {
                        return Err("Creative input did not move the player".into());
                    }
                    transform.rotation = [0.0, 89.0];
                    self.connection
                        .move_player(transform.clone(), false)
                        .map_err(|e| e.to_string())?;
                    let store = self.store.as_ref().unwrap();
                    let hit = modern_shapes::raycast(
                        store,
                        &self.shapes,
                        [
                            transform.position[0],
                            transform.position[1] + 1.62,
                            transform.position[2],
                        ],
                        modern_controls::view_direction(transform.rotation),
                        CREATIVE_REACH,
                    )
                    .map_err(|e| e.to_string())?
                    .ok_or("No target under the player")?;
                    check.target = Some(hit.position);
                    check.initial_state = store.block_state_id(Position::new(
                        hit.position[0],
                        hit.position[1],
                        hit.position[2],
                    ));
                    self.interact(false);
                    check.stage = 2;
                }
                2 => {
                    let p = check.target.unwrap();
                    if self
                        .store
                        .as_ref()
                        .unwrap()
                        .named_state(Position::new(p[0], p[1], p[2]))
                        .map_err(|e| e.to_string())?
                        .is_some_and(|s| is_air(s.name()))
                        && self.action_acks >= 1
                    {
                        self.select(0);
                        check.stage = 7;
                    }
                }
                7 => {
                    // Prove small-stack and unstackable items cannot wedge the
                    // real picker before selecting the final placement block.
                    let requested = CHECK_PICKER_ITEMS[check.picker_probe];
                    self.open_picker();
                    self.picker_key(&Key::Character(requested.into()));
                    let picker = self.picker.as_ref().ok_or("Block picker did not open")?;
                    let index = picker
                        .matches
                        .iter()
                        .position(|&candidate| self.block_choices[candidate] == requested)
                        .ok_or_else(|| format!("Block picker search did not find {requested}"))?;
                    for _ in 0..index {
                        self.picker_key(&Key::Named(NamedKey::ArrowDown));
                    }
                    check.stage_started = Some(Instant::now());
                    check.stage = 6;
                }
                6 if check.stage_started.unwrap().elapsed() > Duration::from_millis(250) => {
                    if !self.confirm_picker()? {
                        return Err("Block picker did not submit a selection".into());
                    }
                    check.stage = 5;
                }
                5 if self.pending_choice.is_none()
                    && self.choices_confirmed > check.picker_probe
                    && self.hotbar[self.selected as usize]
                        .as_ref()
                        .filter(|stack| stack.count == PICKER_COUNT as u32)
                        .and_then(|stack| self.items.get(stack.item_id as usize))
                        .is_some_and(|name| name == CHECK_PICKER_ITEMS[check.picker_probe]) =>
                {
                    if check.picker_probe + 1 < CHECK_PICKER_ITEMS.len() {
                        check.picker_probe += 1;
                        check.stage = 7;
                    } else {
                        self.interact(true);
                        check.stage = 3;
                    }
                }
                3 => {
                    let p = check.target.unwrap();
                    if self
                        .store
                        .as_ref()
                        .unwrap()
                        .named_state(Position::new(p[0], p[1], p[2]))
                        .map_err(|e| e.to_string())?
                        .is_some_and(|s| s.name() == "minecraft:oak_planks")
                        && self.action_acks >= 2
                    {
                        check.completed = true;
                        check.stage = 4;
                        if let Some(transform) = self.transform.as_mut() {
                            transform.rotation = [0.0, 40.0];
                        }
                        log::info!("Native Creative movement, selected item, break and place verified by server updates");
                    }
                }
                _ => {}
            }
            Ok(())
        })();
        self.check = Some(check);
        result
    }

    fn select(&mut self, slot: u8) {
        if slot < 9 && self.transform.is_some() {
            match self.connection.send(play::Serverbound::SelectedSlot(slot)) {
                Ok(()) => self.selected = slot,
                Err(e) => self.fail(e),
            }
        }
    }

    /// Requests a registry-verified block item; local inventory changes only in
    /// response to authoritative InventoryContent/InventorySlot events.
    pub fn choose_block(&mut self, name: &str) -> Result<()> {
        if self.pending_choice.is_some() {
            return Err("Waiting for the server to confirm the previous selection".into());
        }
        if self
            .block_choices
            .binary_search_by(|choice| choice.as_str().cmp(name))
            .is_err()
        {
            return Err("Block has no selectable item in the exact vanilla registry".into());
        }
        if !self
            .abilities
            .as_ref()
            .is_some_and(|abilities| abilities.instant_build)
        {
            return Err("Server has not granted Creative inventory control".into());
        }
        if self.hotbar[self.selected as usize]
            .as_ref()
            .is_some_and(|stack| {
                stack.count > 0
                    && self
                        .items
                        .get(stack.item_id as usize)
                        .is_some_and(|item| item == name)
            })
        {
            return Ok(());
        }
        self.connection
            .set_creative_hotbar_slot(self.selected, name, PICKER_COUNT)
            .map_err(|error| error.to_string())?;
        self.pending_choice = Some((self.selected, name.to_owned()));
        Ok(())
    }

    fn confirm_chosen_block(&mut self, updated_slot: Option<u8>) {
        let confirmed = self
            .pending_choice
            .as_ref()
            .is_some_and(|(slot, requested)| {
                updated_slot.map_or(true, |updated| updated == *slot)
                    && self.hotbar[*slot as usize]
                        .as_ref()
                        .filter(|stack| stack.count == PICKER_COUNT as u32)
                        .and_then(|stack| self.items.get(stack.item_id as usize))
                        == Some(requested)
            });
        if confirmed {
            self.pending_choice = None;
            self.choices_confirmed += 1;
        }
    }

    fn open_picker(&mut self) {
        if self.paused
            || self.error.is_some()
            || self.transform.is_none()
            || !self
                .abilities
                .as_ref()
                .is_some_and(|abilities| abilities.instant_build)
        {
            return;
        }
        self.picker = Some(BlockPicker::new(&self.block_choices));
        self.pressed = [false; 7];
        self.flight.reset_motion();
        self.ground.reset_motion();
        self.flight_toggle.reset();
        self.interactions.clear();
    }

    fn close_picker(&mut self) {
        self.picker = None;
        self.pressed = [false; 7];
        self.flight.reset_motion();
        self.ground.reset_motion();
        self.flight_toggle.reset();
        self.interactions.clear();
    }

    fn confirm_picker(&mut self) -> Result<bool> {
        let name = self.picker.as_ref().and_then(|picker| {
            picker
                .matches
                .get(picker.cursor)
                .map(|&index| self.block_choices[index].clone())
        });
        let Some(name) = name else {
            return Ok(false);
        };
        self.choose_block(&name)?;
        self.close_picker();
        Ok(true)
    }

    fn picker_key(&mut self, key: &Key) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        match key {
            Key::Named(NamedKey::Escape) => self.close_picker(),
            Key::Named(NamedKey::ArrowUp) => picker.move_cursor(-1),
            Key::Named(NamedKey::ArrowDown) => picker.move_cursor(1),
            Key::Named(NamedKey::PageUp) => picker.move_cursor(-(PICKER_ROWS as i32)),
            Key::Named(NamedKey::PageDown) => picker.move_cursor(PICKER_ROWS as i32),
            Key::Named(NamedKey::Home) => picker.cursor = 0,
            Key::Named(NamedKey::End) => picker.cursor = picker.matches.len().saturating_sub(1),
            Key::Named(NamedKey::Backspace) => {
                picker.query.pop();
                picker.filter(&self.block_choices);
            }
            Key::Named(NamedKey::Enter) => {
                if let Err(error) = self.confirm_picker() {
                    self.notice = Some(error);
                }
            }
            Key::Named(NamedKey::Space) if picker.query.len() < 48 => {
                picker.query.push(' ');
                picker.filter(&self.block_choices);
            }
            Key::Character(value) => {
                for c in value
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || " _:/.-".contains(*c))
                {
                    if picker.query.len() >= 48 {
                        break;
                    }
                    picker.query.push(c.to_ascii_lowercase());
                }
                picker.filter(&self.block_choices);
            }
            _ => {}
        }
    }

    fn target(&self) -> Result<Option<modern_shapes::Hit>> {
        let (Some(transform), Some(store)) = (&self.transform, &self.store) else {
            return Ok(None);
        };
        let origin = [
            transform.position[0],
            transform.position[1] + 1.62,
            transform.position[2],
        ];
        modern_shapes::raycast(
            store,
            &self.shapes,
            origin,
            modern_controls::view_direction(transform.rotation),
            CREATIVE_REACH,
        )
        .map_err(|e| e.to_string())
    }

    fn interact(&mut self, place: bool) -> bool {
        if self.paused || self.picker.is_some() || self.error.is_some() {
            return false;
        }
        let hit = match self.target() {
            Ok(Some(hit)) => hit,
            Ok(None) => return false,
            Err(error) => {
                self.notice = Some(error);
                return false;
            }
        };
        let result = if place {
            self.connection
                .use_item_on(0, hit.position, hit.face, hit.local, hit.inside)
        } else {
            self.connection.dig(0, hit.position, hit.face)
        };
        if let Err(error) = result {
            self.fail(error);
            false
        } else {
            let _ = self.connection.send(play::Serverbound::Swing { hand: 0 });
            true
        }
    }

    fn repeat_interactions(&mut self, seconds: f64) {
        if self.paused || self.picker.is_some() || !self.window_focused || self.error.is_some() {
            self.interactions.clear();
            return;
        }
        for _ in 0..self.interactions.frame_ticks(seconds) {
            let target = self.target().is_ok_and(|hit| hit.is_some());
            let [attack, place] = self.interactions.tick(target);
            if place {
                let sent = self.interact(true);
                self.interactions.started(true, sent);
            }
            if attack {
                let sent = self.interact(false);
                self.interactions.started(false, sent);
            }
        }
    }
    pub fn event<T>(&mut self, window: &Window, game: &Game, event: Event<T>) -> bool {
        match event {
            Event::AboutToWait => return true,
            Event::DeviceEvent {
                event: DeviceEvent::MouseMotion { delta: (x, y) },
                ..
            } if game.is_focused() && self.picker.is_none() => {
                if let Some(transform) = &mut self.transform {
                    let sensitivity = game.settings.get_float(FloatSetting::MouseSense) * 0.15;
                    transform.rotation =
                        modern_controls::apply_look(transform.rotation, [x, y], sensitivity);
                    // Publish rotations now, before a relative server teleport can arrive.
                    let _ = self.connection.update_local_transform(transform.clone());
                }
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    self.connection.close();
                    game.set_should_close();
                }
                WindowEvent::Focused(focused) => {
                    self.window_focused = focused;
                    if !focused {
                        self.pressed = [false; 7];
                        self.paused = true;
                        self.flight.reset_motion();
                        self.ground.reset_motion();
                        self.flight_toggle.reset();
                        self.interactions.clear();
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    let down = event.state == ElementState::Pressed;
                    let action = if let PhysicalKey::Code(code) = event.physical_key {
                        game.keybinds
                            .get(code, &event.logical_key)
                            .map(|binding| binding.action)
                    } else {
                        None
                    };
                    if down && !event.repeat && event.logical_key == Key::Named(NamedKey::F11) {
                        let full = !game.is_fullscreen();
                        window.set_fullscreen(if full {
                            Some(winit::window::Fullscreen::Borderless(
                                window.current_monitor(),
                            ))
                        } else {
                            None
                        });
                        game.set_full_screen(full);
                    } else if self.picker.is_some() {
                        if down {
                            self.picker_key(&event.logical_key);
                        }
                    } else if down
                        && !event.repeat
                        && event.logical_key == Key::Named(NamedKey::Escape)
                    {
                        self.paused = !self.paused;
                        self.pressed = [false; 7];
                        self.flight.reset_motion();
                        self.ground.reset_motion();
                        self.flight_toggle.reset();
                        self.interactions.clear();
                    } else if down && !event.repeat && action == Some(Actionkey::OpenInv) {
                        self.open_picker();
                    } else {
                        if down && !self.paused {
                            if let Key::Character(value) = &event.logical_key {
                                if let Ok(slot) = value.parse::<u8>() {
                                    if (1..=9).contains(&slot) {
                                        self.select(slot - 1);
                                    }
                                }
                            }
                        }
                        let index = match action {
                            Some(Actionkey::Forward) => Some(0),
                            Some(Actionkey::Backward) => Some(1),
                            Some(Actionkey::Left) => Some(2),
                            Some(Actionkey::Right) => Some(3),
                            Some(Actionkey::Jump) => Some(4),
                            Some(Actionkey::Sneak) => Some(5),
                            Some(Actionkey::Sprint) => Some(6),
                            _ => None,
                        };
                        if let Some(index) = index {
                            self.pressed[index] = down && !self.paused;
                        }
                    }
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    let place = match button {
                        MouseButton::Left => Some(false),
                        MouseButton::Right => Some(true),
                        _ => None,
                    };
                    if let Some(place) = place {
                        let down = state == ElementState::Pressed
                            && game.is_focused()
                            && self.picker.is_none();
                        self.interactions.set(place, down);
                        if down {
                            let sent = self.interact(place);
                            self.interactions.started(place, sent);
                        }
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    let y = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y as f64,
                        MouseScrollDelta::PixelDelta(p) => p.y,
                    };
                    if y != 0.0 {
                        if let Some(picker) = self.picker.as_mut() {
                            picker.move_cursor(if y > 0.0 { -1 } else { 1 });
                        } else if game.is_focused() {
                            self.select(
                                (self.selected as i32 - if y > 0.0 { 1 } else { -1 }).rem_euclid(9)
                                    as u8,
                            );
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
        false
    }
}

fn is_air(name: &str) -> bool {
    matches!(
        name,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_uses_default_blocks_with_real_items_only() {
        let catalog = StateCatalog::from_json(
            br#"{
            "minecraft:air":{"states":[{"id":0,"default":true}]},
            "minecraft:water":{"states":[{"id":1,"default":true}]},
            "minecraft:stone":{"states":[{"id":2,"default":true}]},
            "minecraft:oak_log":{
                "properties":{"axis":["x","y"]},
                "states":[
                    {"id":3,"properties":{"axis":"x"}},
                    {"id":4,"properties":{"axis":"y"},"default":true}
                ]
            }
        }"#,
        )
        .unwrap();
        let items = [
            "minecraft:stone",
            "minecraft:oak_log",
            "minecraft:air",
            "minecraft:stick",
        ]
        .map(str::to_owned);
        assert_eq!(
            selectable_blocks(&catalog, &items),
            ["minecraft:oak_log", "minecraft:stone"]
        );
    }

    #[test]
    fn picker_filters_tokens_and_keeps_navigation_in_page_bounds() {
        let choices = [
            "minecraft:dark_oak_planks",
            "minecraft:oak_log",
            "minecraft:oak_planks",
            "minecraft:stone",
        ]
        .map(str::to_owned);
        assert_eq!(filter_blocks(&choices, "OAK_planks"), [0, 2]);
        assert_eq!(filter_blocks(&choices, "planks dark"), [0]);
        assert_eq!(filter_blocks(&choices, "minecraft:oak_planks"), [2]);

        let choices: Vec<_> = (0..23).map(|i| format!("minecraft:block_{i}")).collect();
        let mut picker = BlockPicker::new(&choices);
        picker.move_cursor(10);
        assert_eq!((picker.cursor, picker.offset()), (10, 10));
        picker.move_cursor(99);
        assert_eq!((picker.cursor, picker.offset()), (22, 20));
        picker.move_cursor(-99);
        assert_eq!((picker.cursor, picker.offset()), (0, 0));
        picker.query = "not present".into();
        picker.filter(&choices);
        picker.move_cursor(10);
        assert!(picker.matches.is_empty());
        assert_eq!((picker.cursor, picker.offset()), (0, 0));
        picker.query.clear();
        picker.filter(&choices);
        assert_eq!(picker.matches.len(), 23);
    }
}
