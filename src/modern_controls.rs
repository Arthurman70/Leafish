//! Native controls use Minecraft wire angles: yaw zero faces +Z; positive
//! yaw turns toward -X; positive pitch looks down. Flight here covers only
//! creative movement through free air. Collision, fluids, effects, walking,
//! riding and server corrections belong to the session, not this controller.
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
}
