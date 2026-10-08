//! World-dependent inputs for the ordinary, dry-ground player controller.
use crate::modern_controls::{GroundCollision, GroundEnvironment};
use crate::modern_shapes::{self, ShapeCatalog};
use crate::shared::Position;
use crate::world::native::NativeChunkStore;

pub fn environment(
    store: &NativeChunkStore,
    shapes: &ShapeCatalog,
    position: [f64; 3],
) -> Result<GroundEnvironment, String> {
    let state_at = |y: f64| {
        let cell = Position::new(
            position[0].floor() as i32,
            y.floor() as i32,
            position[2].floor() as i32,
        );
        store
            .block_state_id(cell)
            .ok_or_else(|| "Movement reached unloaded terrain".to_owned())
    };
    let body = state_at(position[1])?;
    if store.catalog().state(body).is_ok_and(|s| {
        matches!(
            s.name(),
            "minecraft:water" | "minecraft:lava" | "minecraft:bubble_column"
        )
    }) {
        return Err("Swimming physics is unfinished; double-tap Space to fly".into());
    }
    // Entity.getBlockPosBelowThatAffectsMyMovement uses 0.500001f.
    let below = state_at(position[1] - 0.500001_f32 as f64)?;
    let body_factors = shapes.movement_factors(body).map_err(|e| e.to_string())?;
    let below_factors = shapes.movement_factors(below).map_err(|e| e.to_string())?;
    let probe = modern_shapes::move_player(store, shapes, position, [0.0, -1e-5, 0.0])
        .map_err(|e| e.to_string())?;
    let is_water = store
        .catalog()
        .state(body)
        .is_ok_and(|s| matches!(s.name(), "minecraft:water" | "minecraft:bubble_column"));
    Ok(GroundEnvironment {
        grounded: probe.blocked[1],
        friction: below_factors.friction as f32,
        speed_factor: if body_factors.speed_factor != 1.0 || is_water {
            body_factors.speed_factor
        } else {
            below_factors.speed_factor
        } as f32,
        jump_factor: if body_factors.jump_factor != 1.0 {
            body_factors.jump_factor
        } else {
            below_factors.jump_factor
        } as f32,
    })
}

pub fn collide(
    store: &NativeChunkStore,
    shapes: &ShapeCatalog,
    position: [f64; 3],
    delta: [f64; 3],
) -> Result<GroundCollision, String> {
    let grounded = modern_shapes::move_player(store, shapes, position, [0.0, -1e-5, 0.0])
        .map_err(|e| e.to_string())?
        .blocked[1];
    let result =
        modern_shapes::move_player_ground(store, shapes, position, delta, grounded, 0.6_f32)
            .map_err(|e| e.to_string())?;
    Ok(GroundCollision {
        position: result.position,
        blocked: result.blocked,
        grounded: delta[1] < 0.0 && result.blocked[1],
    })
}
