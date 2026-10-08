//! Native controls use Minecraft wire angles: yaw zero faces +Z; positive
//! yaw turns toward -X; positive pitch looks down. The controllers cover
//! ordinary dry walking/jumping and Creative free-air flight. World queries,
//! collision shapes, fluids, effects, riding and corrections belong to the
//! session; no visual-model bounds are used as collision geometry here.
use crate::settings::Actionkey;
use leafish_protocol::protocol::play767::PlayerAbilities;

pub const MAX_FRAME_SECONDS: f64 = 0.1;
const TICK_SECONDS: f64 = 0.05;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Axes {
    pub forward: f64,
    pub right: f64,
    pub up: f64,
    pub sprint: bool,
}

impl Axes {
    fn bounded(self) -> Option<Self> {
        if ![self.forward, self.right, self.up]
            .iter()
            .all(|v| v.is_finite())
        {
            return None;
        }
        Some(Self {
            forward: self.forward.clamp(-1.0, 1.0),
            right: self.right.clamp(-1.0, 1.0),
            up: self.up.clamp(-1.0, 1.0),
            sprint: self.sprint,
        })
    }
}

#[derive(Debug, Default)]
pub struct Input {
    forward: bool,
    backward: bool,
    right: bool,
    left: bool,
    up: bool,
    down: bool,
    sprint: bool,
}

impl Input {
    pub fn set(&mut self, action: Actionkey, pressed: bool) {
        let target = match action {
            Actionkey::Forward => &mut self.forward,
            Actionkey::Backward => &mut self.backward,
            Actionkey::Right => &mut self.right,
            Actionkey::Left => &mut self.left,
            Actionkey::Jump => &mut self.up,
            Actionkey::Sneak => &mut self.down,
            Actionkey::Sprint => &mut self.sprint,
            _ => return,
        };
        *target = pressed;
    }

    pub fn axes(&self) -> Axes {
        Axes {
            forward: self.forward as u8 as f64 - self.backward as u8 as f64,
            right: self.right as u8 as f64 - self.left as u8 as f64,
            up: self.up as u8 as f64 - self.down as u8 as f64,
            sprint: self.sprint,
        }
    }

    /// Call on focus loss and pause so a missed key-release cannot keep moving.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

pub fn renderer_angles(rotation: [f32; 2]) -> (f64, f64) {
    (
        -(rotation[0] as f64).to_radians(),
        std::f64::consts::PI - (rotation[1] as f64).to_radians(),
    )
}

pub fn view_direction(rotation: [f32; 2]) -> [f64; 3] {
    let yaw = (rotation[0] as f64).to_radians();
    let pitch = (rotation[1] as f64).to_radians();
    [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ]
}

/// Mouse deltas have screen coordinates: positive X turns right and positive
/// Y looks down. Sensitivity is degrees per count, explicitly not radians.
pub fn apply_look(rotation: [f32; 2], mouse_delta: [f64; 2], degrees_per_count: f64) -> [f32; 2] {
    if !rotation.iter().all(|v| v.is_finite())
        || !mouse_delta.iter().all(|v| v.is_finite())
        || !degrees_per_count.is_finite()
        || degrees_per_count < 0.0
    {
        return rotation;
    }
    let yaw = rotation[0] as f64 + mouse_delta[0] * degrees_per_count;
    let pitch = rotation[1] as f64 + mouse_delta[1] * degrees_per_count;
    if !yaw.is_finite() || !pitch.is_finite() {
        return rotation;
    }
    [
        ((yaw + 180.0).rem_euclid(360.0) - 180.0) as f32,
        pitch.clamp(-90.0, 90.0) as f32,
    ]
}

/// Bounded stateless movement helper. `speed` is blocks per second, NOT the
/// server's flight acceleration coefficient. This helper has no inertia;
/// use FlightController for the creative free-air integration below.
pub fn move_flying(
    position: [f64; 3],
    rotation: [f32; 2],
    axes: Axes,
    speed: f64,
    dt: f64,
) -> [f64; 3] {
    let axes = match axes.bounded() {
        Some(axes) => axes,
        None => return position,
    };
    if !position.iter().all(|v| v.is_finite())
        || !rotation.iter().all(|v| v.is_finite())
        || !speed.is_finite()
        || speed < 0.0
        || !dt.is_finite()
    {
        return position;
    }
    let yaw = (rotation[0] as f64).to_radians();
    let length = (axes.forward * axes.forward + axes.right * axes.right + axes.up * axes.up)
        .sqrt()
        .max(1.0);
    let distance =
        speed * dt.clamp(0.0, MAX_FRAME_SECONDS) * if axes.sprint { 2.0 } else { 1.0 } / length;
    let result = [
        position[0] + (-yaw.sin() * axes.forward - yaw.cos() * axes.right) * distance,
        position[1] + axes.up * distance,
        position[2] + (yaw.cos() * axes.forward - yaw.sin() * axes.right) * distance,
    ];
    if result.iter().all(|v| v.is_finite()) {
        result
    } else {
        position
    }
}

/// Fixed 20 Hz creative free-air subset of Player/LocalPlayer/LivingEntity:
/// advertised horizontal acceleration (twice when sprinting), 0.91 horizontal
/// drag, vertical input at three times flight speed, and 0.6 vertical drag.
/// This does not perform or approximate block collision with visual models.
#[derive(Debug, Default)]
pub struct FlightController {
    flight_speed: Option<f32>,
    velocity: [f64; 3],
    accumulated_seconds: f64,
}

impl FlightController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Permission must come from both the joined creative mode and the
    /// server's abilities. Requesting flight alone does not enable movement.
    pub fn set_abilities(&mut self, creative: bool, abilities: &PlayerAbilities) -> bool {
        let allowed = creative
            && abilities.instant_build
            && abilities.may_fly
            && abilities.flying
            && abilities.flying_speed.is_finite()
            && abilities.flying_speed >= 0.0;
        self.flight_speed = if allowed {
            Some(abilities.flying_speed)
        } else {
            self.reset_motion();
            None
        };
        allowed
    }

    /// Call after server teleports, on focus loss and when entering pause.
    pub fn reset_motion(&mut self) {
        self.velocity = [0.0; 3];
        self.accumulated_seconds = 0.0;
    }

    /// Velocity is measured in blocks per game tick, after the prior tick's drag.
    pub fn velocity(&self) -> [f64; 3] {
        self.velocity
    }

    /// Preserve momentum when changing movement modes; reject invalid input.
    pub fn set_velocity(&mut self, velocity: [f64; 3]) -> bool {
        if velocity.iter().all(|v| v.is_finite()) {
            self.velocity = velocity;
            true
        } else {
            false
        }
    }

    /// Remove momentum only on axes clipped by authoritative collision shapes.
    /// Perpendicular motion and the fixed-tick remainder remain unchanged.
    pub fn clip_motion(&mut self, blocked: [bool; 3]) {
        for axis in 0..3 {
            if blocked[axis] {
                self.velocity[axis] = 0.0;
            }
        }
    }

    pub fn advance(
        &mut self,
        mut position: [f64; 3],
        rotation: [f32; 2],
        axes: Axes,
        dt: f64,
    ) -> [f64; 3] {
        let speed = match self.flight_speed {
            Some(speed) => speed,
            None => return position,
        };
        let axes = match axes.bounded() {
            Some(axes) => axes,
            None => {
                self.reset_motion();
                return position;
            }
        };
        if !position.iter().all(|v| v.is_finite())
            || !rotation.iter().all(|v| v.is_finite())
            || !dt.is_finite()
            || dt < 0.0
        {
            self.reset_motion();
            return position;
        }
        self.accumulated_seconds += dt.min(MAX_FRAME_SECONDS);
        let yaw = (rotation[0] as f64).to_radians();
        // LivingEntity scales travel input before Entity normalizes it.
        let forward = axes.forward * 0.98_f32 as f64;
        let right = axes.right * 0.98_f32 as f64;
        let length = (forward * forward + right * right).sqrt().max(1.0);
        let acceleration = (speed * if axes.sprint { 2.0 } else { 1.0 }) as f64;
        while self.accumulated_seconds + 1e-12 >= TICK_SECONDS {
            self.accumulated_seconds = (self.accumulated_seconds - TICK_SECONDS).max(0.0);
            for component in &mut self.velocity {
                if component.abs() < 0.003 {
                    *component = 0.0;
                }
            }
            self.velocity[0] += (-yaw.sin() * forward - yaw.cos() * right) / length * acceleration;
            self.velocity[2] += (yaw.cos() * forward - yaw.sin() * right) / length * acceleration;
            self.velocity[1] += (axes.up as f32 * speed * 3.0) as f64;
            for axis in 0..3 {
                position[axis] += self.velocity[axis];
            }
            self.velocity[0] *= 0.91_f32 as f64;
            self.velocity[2] *= 0.91_f32 as f64;
            self.velocity[1] *= 0.6;
        }
        position
    }
}

/// LocalPlayer.aiStep's Creative flight gesture. Call exactly once per game
/// tick, using the held jump state (not OS key-repeat events). The session sends
/// the returned ability change and supplies the current server-permitted mode.
#[derive(Debug, Default)]
pub struct FlightToggle {
    previous_jump: bool,
    remaining_ticks: u8,
}

impl FlightToggle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn tick(&mut self, jump: bool, may_fly: bool, flying: bool) -> Option<bool> {
        let mut change = None;
        if !may_fly {
            self.remaining_ticks = 0;
        } else if jump && !self.previous_jump {
            if self.remaining_ticks == 0 {
                self.remaining_ticks = 7;
            } else {
                self.remaining_ticks = 0;
                change = Some(!flying);
            }
        }
        self.previous_jump = jump;
        // Player.aiStep runs after LocalPlayer checks the gesture, including
        // on the tick that first arms the seven-tick counter.
        self.remaining_ticks = self.remaining_ticks.saturating_sub(1);
        change
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GroundInput {
    pub axes: Axes,
    pub jump: bool,
}

/// Unmodified player attributes. These defaults are not a substitute for
/// decoding server attribute updates when effects/equipment are supported.
#[derive(Clone, Copy, Debug)]
pub struct GroundSettings {
    pub movement_speed: f32,
    pub gravity: f64,
    pub jump_strength: f32,
}

impl Default for GroundSettings {
    fn default() -> Self {
        Self {
            movement_speed: 0.1,
            gravity: 0.08,
            jump_strength: 0.42,
        }
    }
}

/// The session resolves the supporting/current block and supplies exact
/// Block movement factors. Friction and jump_factor apply before movement;
/// speed_factor applies at the destination, after collision.
#[derive(Clone, Copy, Debug)]
pub struct GroundEnvironment {
    pub grounded: bool,
    pub friction: f32,
    pub speed_factor: f32,
    pub jump_factor: f32,
}

impl GroundEnvironment {
    fn valid(self) -> bool {
        self.friction.is_finite()
            && self.friction > 0.0
            && self.speed_factor.is_finite()
            && self.speed_factor >= 0.0
            && self.jump_factor.is_finite()
            && self.jump_factor >= 0.0
    }
}

/// Collision must describe this tick's requested displacement, including
/// static step handling where supported. `grounded` is support after movement,
/// not a flag inferred from the prior position or from an upward ceiling hit.
#[derive(Clone, Copy, Debug)]
pub struct GroundCollision {
    pub position: [f64; 3],
    pub blocked: [bool; 3],
    pub grounded: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct GroundAdvance {
    pub position: [f64; 3],
    pub grounded: bool,
    pub sprinting: bool,
    pub ticks: u8,
}

/// Dry, standing-player subset of 1.21.1 LocalPlayer/LivingEntity/Entity.
/// Each fixed 20 Hz step resolves collision before applying gravity and drag.
/// There is no fluid/effect/climbing/crouching/block-callback approximation.
#[derive(Clone, Debug, Default)]
pub struct GroundController {
    velocity: [f64; 3],
    accumulated_seconds: f64,
    grounded: bool,
    no_jump_delay: u8,
    sprinting: bool,
    sprint_trigger_time: u8,
    previous_forward: bool,
    major_horizontal_collision: bool,
}

impl GroundController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset_motion(&mut self) {
        *self = Self::default();
    }

    /// Restore the session's current sprint latch after a movement-mode reset.
    pub fn set_sprinting(&mut self, sprinting: bool) {
        self.sprinting = sprinting;
    }

    pub fn velocity(&self) -> [f64; 3] {
        self.velocity
    }

    pub fn set_velocity(&mut self, velocity: [f64; 3]) -> bool {
        if velocity.iter().all(|v| v.is_finite()) {
            self.velocity = velocity;
            true
        } else {
            false
        }
    }

    fn stationary(&self, position: [f64; 3]) -> GroundAdvance {
        GroundAdvance {
            position,
            grounded: self.grounded,
            sprinting: self.sprinting,
            ticks: 0,
        }
    }

    /// Environment/collision callbacks are pure queries, called separately for
    /// every simulated tick. An error leaves controller state uncommitted, so
    /// the session can retain the original position and display that error.
    /// Invalid numeric input resets momentum and returns stationary movement.
    pub fn advance<E>(
        &mut self,
        position: [f64; 3],
        rotation: [f32; 2],
        input: GroundInput,
        settings: GroundSettings,
        dt: f64,
        mut environment: impl FnMut([f64; 3]) -> Result<GroundEnvironment, E>,
        mut collide: impl FnMut([f64; 3], [f64; 3]) -> Result<GroundCollision, E>,
    ) -> Result<GroundAdvance, E> {
        let Some(axes) = input.axes.bounded() else {
            self.reset_motion();
            return Ok(self.stationary(position));
        };
        if !position.iter().all(|v| v.is_finite())
            || !rotation.iter().all(|v| v.is_finite())
            || !dt.is_finite()
            || dt < 0.0
            || !settings.movement_speed.is_finite()
            || settings.movement_speed < 0.0
            || !settings.gravity.is_finite()
            || !settings.jump_strength.is_finite()
            || settings.jump_strength < 0.0
        {
            self.reset_motion();
            return Ok(self.stationary(position));
        }
        let mut next = self.clone();
        let mut result = self.stationary(position);
        next.accumulated_seconds += dt.min(MAX_FRAME_SECONDS);
        while next.accumulated_seconds + 1e-12 >= TICK_SECONDS {
            let before = environment(result.position)?;
            if !before.valid() {
                self.reset_motion();
                return Ok(self.stationary(position));
            }
            next.accumulated_seconds = (next.accumulated_seconds - TICK_SECONDS).max(0.0);
            next.no_jump_delay = next.no_jump_delay.saturating_sub(1);
            next.sprint_trigger_time = next.sprint_trigger_time.saturating_sub(1);
            let forward = axes.forward >= 0.8;
            if !next.sprinting && forward {
                if before.grounded && !next.previous_forward {
                    if next.sprint_trigger_time == 0 && !axes.sprint {
                        next.sprint_trigger_time = 7;
                    } else {
                        next.sprinting = true;
                    }
                }
                if axes.sprint {
                    next.sprinting = true;
                }
            }
            if axes.forward <= 1e-5 || next.major_horizontal_collision {
                next.sprinting = false;
            }
            next.previous_forward = forward;
            for value in &mut next.velocity {
                if value.abs() < 0.003 {
                    *value = 0.0;
                }
            }
            if input.jump {
                if before.grounded && next.no_jump_delay == 0 {
                    let power = settings.jump_strength * before.jump_factor;
                    if power > 1e-5 {
                        next.velocity[1] = power as f64;
                        if next.sprinting {
                            let (sin, cos) = movement_yaw(rotation[0]);
                            next.velocity[0] -= sin * 0.2;
                            next.velocity[2] += cos * 0.2;
                        }
                    }
                    next.no_jump_delay = 10;
                }
            } else {
                next.no_jump_delay = 0;
            }
            // Attribute modifiers use double arithmetic; Player.getSpeed then
            // converts the result to float before friction scaling.
            let speed = (settings.movement_speed as f64
                * if next.sprinting {
                    1.0 + 0.3_f32 as f64
                } else {
                    1.0
                }) as f32;
            let acceleration = if before.grounded {
                speed * (0.21600002_f32 / (before.friction * before.friction * before.friction))
            } else if next.sprinting {
                0.025999999_f32
            } else {
                0.02_f32
            };
            let relative = ground_relative(axes, rotation[0]);
            let (left, forward) = ground_input(axes);
            let length_squared = left * left + forward * forward;
            if length_squared >= 1e-7 {
                // Entity.getInputVector normalizes/scales the local vector
                // before rotating with Mth's float trigonometry values.
                let divisor = length_squared.sqrt().max(1.0);
                let left = left / divisor * acceleration as f64;
                let forward = forward / divisor * acceleration as f64;
                let (sin, cos) = movement_yaw(rotation[0]);
                next.velocity[0] += left * cos - forward * sin;
                next.velocity[2] += forward * cos + left * sin;
            }
            let collision = collide(result.position, next.velocity)?;
            if !collision.position.iter().all(|v| v.is_finite()) {
                self.reset_motion();
                return Ok(self.stationary(position));
            }
            let after = environment(collision.position)?;
            if !after.valid() {
                self.reset_motion();
                return Ok(self.stationary(position));
            }
            let applied = std::array::from_fn(|a| collision.position[a] - result.position[a]);
            next.major_horizontal_collision = (collision.blocked[0] || collision.blocked[2])
                && !minor_collision(relative, applied);
            for axis in 0..3 {
                if collision.blocked[axis] {
                    next.velocity[axis] = 0.0;
                }
            }
            let drag = if before.grounded {
                before.friction * 0.91_f32
            } else {
                0.91_f32
            };
            next.velocity[0] *= after.speed_factor as f64;
            next.velocity[2] *= after.speed_factor as f64;
            next.velocity[0] *= drag as f64;
            next.velocity[2] *= drag as f64;
            next.velocity[1] = (next.velocity[1] - settings.gravity) * 0.98_f32 as f64;
            next.grounded = collision.grounded;
            result = GroundAdvance {
                position: collision.position,
                grounded: collision.grounded,
                sprinting: next.sprinting,
                ticks: result.ticks + 1,
            };
        }
        *self = next;
        Ok(result)
    }
}

// Mth uses a 65,536-entry float sine table. Evaluate the selected entries
// directly with the table's initialization formula rather than allocating it.
pub(crate) fn movement_yaw(yaw: f32) -> (f64, f64) {
    let radians = yaw * (std::f64::consts::PI / 180.0) as f32;
    let lookup = |index: i32| {
        (((index & 65535) as f64 * std::f64::consts::PI * 2.0 / 65536.0).sin() as f32) as f64
    };
    (
        lookup((radians * 10430.378_f32) as i32),
        lookup((radians * 10430.378_f32 + 16384.0_f32) as i32),
    )
}

fn ground_relative(axes: Axes, yaw: f32) -> [f64; 3] {
    let (left, forward) = ground_input(axes);
    let (sin, cos) = movement_yaw(yaw);
    [left * cos - forward * sin, 0.0, forward * cos + left * sin]
}

fn ground_input(axes: Axes) -> (f64, f64) {
    (
        (-axes.right as f32 * 0.98_f32) as f64,
        (axes.forward as f32 * 0.98_f32) as f64,
    )
}

fn minor_collision(relative: [f64; 3], applied: [f64; 3]) -> bool {
    let input_length = relative[0] * relative[0] + relative[2] * relative[2];
    let applied_length = applied[0] * applied[0] + applied[2] * applied[2];
    if input_length < 1e-5_f32 as f64 || applied_length < 1e-5_f32 as f64 {
        return false;
    }
    let dot = relative[0] * applied[0] + relative[2] * applied[2];
    (dot / (input_length * applied_length).sqrt()).acos() < 0.13962634_f32 as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: [f64; 3], expected: [f64; 3]) {
        for axis in 0..3 {
            assert!(
                (actual[axis] - expected[axis]).abs() < 1e-7,
                "{:?} != {:?}",
                actual,
                expected
            );
        }
    }
    fn abilities() -> PlayerAbilities {
        PlayerAbilities {
            invulnerable: true,
            flying: true,
            may_fly: true,
            instant_build: true,
            flying_speed: 0.05,
            walking_speed: 0.1,
        }
    }

    #[test]
    fn wire_cardinal_directions_and_renderer_angles_agree() {
        for (yaw, expected) in [
            (0.0, [0.0, 0.0, 1.0]),
            (90.0, [-1.0, 0.0, 0.0]),
            (180.0, [0.0, 0.0, -1.0]),
            (-90.0, [1.0, 0.0, 0.0]),
        ] {
            close(view_direction([yaw, 0.0]), expected);
            let (yaw, pitch) = renderer_angles([yaw, 0.0]);
            close(
                [
                    (yaw - std::f64::consts::FRAC_PI_2).cos() * -pitch.cos(),
                    -pitch.sin(),
                    -(yaw - std::f64::consts::FRAC_PI_2).sin() * -pitch.cos(),
                ],
                expected,
            );
        }
    }

    #[test]
    fn mouse_right_and_down_are_not_inverted_and_pitch_is_clamped() {
        let rotation = apply_look([0.0, 0.0], [20.0, 10.0], 0.5);
        assert_eq!(rotation, [10.0, 5.0]);
        let direction = view_direction(rotation);
        assert!(direction[0] < 0.0 && direction[1] < 0.0);
        assert_eq!(apply_look(rotation, [0.0, 1000.0], 1.0)[1], 90.0);
        assert_eq!(apply_look(rotation, [0.0, -1000.0], 1.0)[1], -90.0);
        assert_eq!(apply_look([179.0, 0.0], [2.0, 0.0], 1.0)[0], -179.0);
    }

    #[test]
    fn stateless_diagonals_have_no_speed_boost_and_pause_gap_is_bounded() {
        let axes = Axes {
            forward: 1.0,
            right: 1.0,
            up: 1.0,
            sprint: false,
        };
        let position = move_flying([0.0; 3], [0.0; 2], axes, 10.0, 10.0);
        let distance = position.iter().map(|v| v * v).sum::<f64>().sqrt();
        assert!((distance - 1.0).abs() < 1e-12);
        assert!(position[0] < 0.0 && position[1] > 0.0 && position[2] > 0.0);
    }

    #[test]
    fn held_opposites_cancel_and_focus_reset_clears_every_axis() {
        let mut input = Input::default();
        input.set(Actionkey::Forward, true);
        input.set(Actionkey::Backward, true);
        input.set(Actionkey::Jump, true);
        input.set(Actionkey::Sprint, true);
        assert_eq!(input.axes().forward, 0.0);
        assert_eq!(input.axes().up, 1.0);
        input.clear();
        assert_eq!(input.axes(), Axes::default());
    }

    #[test]
    fn only_explicit_server_permitted_creative_flight_moves() {
        let mut flight = FlightController::new();
        let axes = Axes {
            forward: 1.0,
            ..Axes::default()
        };
        assert_eq!(flight.advance([0.0; 3], [0.0; 2], axes, 0.1), [0.0; 3]);
        assert!(!flight.set_abilities(false, &abilities()));
        let mut permission = abilities();
        permission.flying = false;
        assert!(!flight.set_abilities(true, &permission));
        assert!(flight.set_abilities(true, &abilities()));
        assert!(flight.advance([0.0; 3], [0.0; 2], axes, 0.05)[2] > 0.0);
        permission.may_fly = false;
        assert!(!flight.set_abilities(true, &permission));
        assert_eq!(flight.advance([0.0; 3], [0.0; 2], axes, 0.1), [0.0; 3]);
    }

    #[test]
    fn free_air_tick_uses_advertised_speed_and_preserves_drag() {
        let mut flight = FlightController::new();
        flight.set_abilities(true, &abilities());
        let first = flight.advance(
            [0.0; 3],
            [0.0; 2],
            Axes {
                forward: 1.0,
                up: 1.0,
                ..Axes::default()
            },
            0.05,
        );
        close(
            first,
            [
                0.0,
                (0.05_f32 * 3.0) as f64,
                0.05_f32 as f64 * 0.98_f32 as f64,
            ],
        );
        let second = flight.advance(first, [0.0; 2], Axes::default(), 0.05);
        close(
            second,
            [0.0, first[1] * 1.6, first[2] * (1.0 + 0.91_f32 as f64)],
        );
        flight.reset_motion();
        close(
            flight.advance(second, [0.0; 2], Axes::default(), 0.05),
            second,
        );
    }

    #[test]
    fn fixed_step_result_is_independent_of_frame_subdivision() {
        let mut first = FlightController::new();
        let mut second = FlightController::new();
        first.set_abilities(true, &abilities());
        second.set_abilities(true, &abilities());
        let axes = Axes {
            forward: 1.0,
            right: 1.0,
            sprint: true,
            ..Axes::default()
        };
        let expected = first.advance([0.0; 3], [45.0, 0.0], axes, 0.1);
        let mut actual = [0.0; 3];
        for _ in 0..10 {
            actual = second.advance(actual, [45.0, 0.0], axes, 0.01);
        }
        close(actual, expected);
        assert!(expected[0].abs() > 0.0 && expected[2].abs() < 1e-7);
    }

    #[test]
    fn collision_clears_blocked_axis_and_preserves_sliding_and_tick_remainder() {
        let mut flight = FlightController::new();
        flight.set_abilities(true, &abilities());
        let axes = Axes {
            forward: 1.0,
            right: 1.0,
            ..Axes::default()
        };
        let first = flight.advance([0.0; 3], [0.0; 2], axes, 0.075);
        flight.clip_motion([true, false, false]);
        let second = flight.advance(first, [0.0; 2], Axes::default(), 0.025);
        assert_eq!(second[0], first[0]);
        assert!(second[2] > first[2]);
        assert!((second[2] - first[2] * (1.0 + 0.91_f32 as f64)).abs() < 1e-12);
    }

    fn ordinary_ground(position: [f64; 3]) -> Result<GroundEnvironment, &'static str> {
        Ok(GroundEnvironment {
            grounded: position[1] <= 0.0,
            friction: 0.6,
            speed_factor: 1.0,
            jump_factor: 1.0,
        })
    }

    fn floor_collision(
        position: [f64; 3],
        delta: [f64; 3],
    ) -> Result<GroundCollision, &'static str> {
        let mut next = std::array::from_fn(|a| position[a] + delta[a]);
        let hit_floor = next[1] < 0.0;
        next[1] = next[1].max(0.0);
        Ok(GroundCollision {
            position: next,
            blocked: [false, hit_floor, false],
            grounded: next[1] == 0.0,
        })
    }

    fn ground_tick(
        controller: &mut GroundController,
        position: [f64; 3],
        input: GroundInput,
    ) -> GroundAdvance {
        controller
            .advance(
                position,
                [0.0; 2],
                input,
                GroundSettings::default(),
                0.05,
                ordinary_ground,
                floor_collision,
            )
            .unwrap()
    }

    #[test]
    fn flight_gesture_requires_two_edges_and_the_reference_tick_window() {
        let mut toggle = FlightToggle::new();
        assert_eq!(toggle.tick(true, true, false), None);
        for _ in 0..20 {
            assert_eq!(toggle.tick(true, true, false), None);
        }
        toggle.reset();
        assert_eq!(toggle.tick(true, true, false), None); // tick 0
        for _ in 1..6 {
            assert_eq!(toggle.tick(false, true, false), None);
        }
        assert_eq!(toggle.tick(true, true, false), Some(true)); // tick 6
        assert_eq!(toggle.tick(true, true, true), None); // held
        toggle.reset();
        toggle.tick(true, true, true);
        for _ in 1..7 {
            toggle.tick(false, true, true);
        }
        assert_eq!(toggle.tick(true, true, true), None); // tick 7: expired
        toggle.tick(false, true, true);
        assert_eq!(toggle.tick(true, true, true), Some(false));
        toggle.reset();
        toggle.tick(true, false, false);
        toggle.tick(false, false, false);
        assert_eq!(toggle.tick(true, false, false), None);
        assert_eq!(toggle.tick(true, true, false), None); // permission while held
    }

    #[test]
    fn ordinary_walking_acceleration_and_terminal_speed_match_reference() {
        let mut walking = GroundController::new();
        let input = GroundInput {
            axes: Axes {
                forward: 1.0,
                ..Axes::default()
            },
            jump: false,
        };
        let first = ground_tick(&mut walking, [0.0; 3], input);
        close(first.position, [0.0, 0.0, 0.09800000336766246]);
        let mut position = first.position;
        let mut displacement = 0.0;
        for _ in 1..100 {
            let next = ground_tick(&mut walking, position, input);
            displacement = next.position[2] - position[2];
            position = next.position;
        }
        // Original runtime Player attributes + LivingEntity ground acceleration;
        // dry scalar recurrence captured privately from the 1.21.1 reference.
        assert!((displacement * 20.0 - 4.317181368163077).abs() < 1e-10);
        assert!(walking.grounded);
    }

    #[test]
    fn sprint_latches_forward_then_stops_and_does_not_boost_strafing() {
        let mut controller = GroundController::new();
        let mut input = GroundInput {
            axes: Axes {
                forward: 1.0,
                sprint: true,
                ..Axes::default()
            },
            jump: false,
        };
        let first = ground_tick(&mut controller, [0.0; 3], input);
        assert!(first.sprinting);
        assert!((first.position[2] - 0.12740001240968724).abs() < 1e-12);
        input.axes.sprint = false;
        assert!(ground_tick(&mut controller, first.position, input).sprinting);
        input.axes.forward = 0.0;
        input.axes.right = 1.0;
        input.axes.sprint = true;
        assert!(!ground_tick(&mut controller, first.position, input).sprinting);
    }

    #[test]
    fn jump_apex_landing_and_air_gravity_match_reference() {
        let mut controller = GroundController::new();
        let first = ground_tick(
            &mut controller,
            [0.0; 3],
            GroundInput {
                jump: true,
                ..GroundInput::default()
            },
        );
        assert_eq!(first.position[1], 0.42_f32 as f64);
        assert!(!first.grounded);
        let mut position = first.position;
        let mut apex = position[1];
        for _ in 1..12 {
            let next = ground_tick(&mut controller, position, GroundInput::default());
            position = next.position;
            apex = apex.max(position[1]);
        }
        assert!((apex - 1.2522033402537238).abs() < 1e-12);
        assert_eq!(position[1], 0.0);
        assert!(controller.grounded);
        assert!((controller.velocity()[1] + 0.0784000015258789).abs() < 1e-12);
    }

    #[test]
    fn jump_release_resets_cooldown_and_sprinting_adds_forward_impulse() {
        let mut controller = GroundController::new();
        let sprint_jump = GroundInput {
            axes: Axes {
                forward: 1.0,
                sprint: true,
                ..Axes::default()
            },
            jump: true,
        };
        let first = ground_tick(&mut controller, [0.0; 3], sprint_jump);
        assert!((first.position[2] - 0.32740001240968724).abs() < 1e-12);
        // Simulate an immediate ceiling/floor return with the same controller.
        let held = ground_tick(&mut controller, [0.0; 3], sprint_jump);
        assert!(held.position[1] < first.position[1]);
        ground_tick(&mut controller, [0.0; 3], GroundInput::default());
        let next = ground_tick(&mut controller, [0.0; 3], sprint_jump);
        assert_eq!(next.position[1], 0.42_f32 as f64);
    }

    #[test]
    fn destination_speed_factor_and_each_catchup_collision_are_applied() {
        let mut controller = GroundController::new();
        let mut calls = 0;
        let result = controller
            .advance(
                [0.0; 3],
                [0.0; 2],
                GroundInput {
                    axes: Axes {
                        forward: 1.0,
                        ..Axes::default()
                    },
                    jump: false,
                },
                GroundSettings::default(),
                0.1,
                |position| {
                    let mut env = ordinary_ground(position)?;
                    if position[2] > 0.0 {
                        env.speed_factor = 0.4;
                    }
                    Ok::<_, &'static str>(env)
                },
                |position, delta| {
                    calls += 1;
                    floor_collision(position, delta)
                },
            )
            .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(result.ticks, 2);
        assert!((result.position[2] - 0.21740320904049987).abs() < 1e-7);
        // An unresolved second step must not commit partial velocity/timers.
        let original = controller.clone();
        let mut calls = 0;
        let failed = controller.advance(
            result.position,
            [0.0; 2],
            GroundInput::default(),
            GroundSettings::default(),
            0.1,
            ordinary_ground,
            |position, delta| {
                calls += 1;
                if calls == 2 {
                    Err("unloaded next cell")
                } else {
                    floor_collision(position, delta)
                }
            },
        );
        assert_eq!(failed.unwrap_err(), "unloaded next cell");
        assert_eq!(controller.velocity(), original.velocity());
        assert_eq!(controller.accumulated_seconds, original.accumulated_seconds);
    }

    #[test]
    fn ground_fixed_steps_are_frame_independent_and_preserve_wall_sliding() {
        let mut first = GroundController::new();
        let mut second = GroundController::new();
        let input = GroundInput {
            axes: Axes {
                forward: 1.0,
                right: 1.0,
                ..Axes::default()
            },
            jump: false,
        };
        let collision = |position: [f64; 3], delta: [f64; 3]| {
            let mut result = floor_collision(position, delta)?;
            result.position[0] = position[0];
            result.blocked[0] = delta[0] != 0.0;
            Ok::<_, &'static str>(result)
        };
        let expected = first
            .advance(
                [0.0; 3],
                [0.0; 2],
                input,
                GroundSettings::default(),
                0.1,
                ordinary_ground,
                collision,
            )
            .unwrap();
        let mut actual = [0.0; 3];
        for _ in 0..10 {
            actual = second
                .advance(
                    actual,
                    [0.0; 2],
                    input,
                    GroundSettings::default(),
                    0.01,
                    ordinary_ground,
                    collision,
                )
                .unwrap()
                .position;
        }
        close(actual, expected.position);
        assert_eq!(actual[0], 0.0);
        assert!(actual[2] > 0.1);
        assert_eq!(second.velocity()[0], 0.0);
    }

    #[test]
    fn diagonal_rotation_matches_original_runtime_input_vector_samples() {
        // Original Entity.getInputVector(.98-scaled input,.1f,yaw), invoked
        // through official 1.21.1 mappings. These values include Mth's float
        // sine-table quantization and normalization before rotation.
        for (yaw, expected) in [
            (0.0, [-0.07071067917232597, 0.0, 0.07071067917232597]),
            (30.0, [-0.09659175478356266, 0.0, 0.025884991053521697]),
            (45.0, [-0.099999999778689, 0.0, 0.0]),
            (90.0, [-0.07071067917232599, 0.0, -0.07071067917232596]),
            (-90.0, [0.07071067917232597, 0.0, 0.07071067917232597]),
            (179.8, [0.07045939878518818, 0.0, -0.07096106604626203]),
        ] {
            let mut controller = GroundController::new();
            let result = controller
                .advance(
                    [0.0; 3],
                    [yaw, 0.0],
                    GroundInput {
                        axes: Axes {
                            forward: 1.0,
                            right: 1.0,
                            ..Axes::default()
                        },
                        jump: false,
                    },
                    GroundSettings::default(),
                    0.05,
                    ordinary_ground,
                    floor_collision,
                )
                .unwrap();
            for axis in 0..3 {
                assert!(
                    (result.position[axis] - expected[axis]).abs() < 1e-12,
                    "yaw={yaw}: {:?} != {:?}",
                    result.position,
                    expected
                );
            }
        }
    }
}
