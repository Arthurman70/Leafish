//! Creative held-button timing from Minecraft.tick and MultiPlayerGameMode.
//! Break delay advances only with a block under the crosshair; use delay ticks
//! regardless. World mutation and acknowledgments remain the session's job.
#[derive(Default)]
pub struct HeldInteraction {
    attack: bool,
    use_item: bool,
    destroy_delay: u8,
    use_delay: u8,
    accumulated: f64,
}

impl HeldInteraction {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn set(&mut self, place: bool, down: bool) {
        if place {
            self.use_item = down;
        } else {
            self.attack = down;
        }
    }

    pub fn started(&mut self, place: bool, sent: bool) {
        if place {
            self.use_delay = 4;
        } else if sent {
            self.destroy_delay = 5;
        }
    }

    pub fn frame_ticks(&mut self, seconds: f64) -> usize {
        if !seconds.is_finite() || seconds < 0.0 {
            return 0;
        }
        self.accumulated += seconds.min(0.1);
        let count = ((self.accumulated + 1e-12) / 0.05).floor() as usize;
        self.accumulated = (self.accumulated - count as f64 * 0.05).max(0.0);
        count
    }

    /// Return [break, use] attempts. Call `started` after each attempt.
    pub fn tick(&mut self, has_block_target: bool) -> [bool; 2] {
        self.use_delay = self.use_delay.saturating_sub(1);
        let place = self.use_item && self.use_delay == 0;
        let attack = if self.attack && has_block_target {
            if self.destroy_delay > 0 {
                self.destroy_delay -= 1;
                false
            } else {
                true
            }
        } else {
            false
        };
        [attack, place]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn creative_break_waits_five_continuations_and_use_repeats_every_four_ticks() {
        let mut input = HeldInteraction::default();
        input.set(false, true);
        input.set(true, true);
        input.started(false, true);
        input.started(true, true);
        for tick in 1..=12 {
            let actions = input.tick(true);
            assert_eq!(actions[0], tick % 6 == 0);
            assert_eq!(actions[1], tick % 4 == 0);
            if actions[0] {
                input.started(false, true);
            }
            if actions[1] {
                input.started(true, true);
            }
        }
    }
    #[test]
    fn miss_does_not_consume_destroy_delay_and_focus_reset_releases_both_buttons() {
        let mut input = HeldInteraction::default();
        input.set(false, true);
        input.started(false, true);
        for _ in 0..10 {
            assert_eq!(input.tick(false), [false, false]);
        }
        assert_eq!(input.destroy_delay, 5);
        input.set(true, true);
        input.clear();
        assert_eq!(input.tick(true), [false, false]);
    }
    #[test]
    fn timer_carries_fractional_frames_without_unbounded_catchup() {
        let mut input = HeldInteraction::default();
        assert_eq!(input.frame_ticks(0.02), 0);
        assert_eq!(input.frame_ticks(0.03), 1);
        assert_eq!(input.frame_ticks(50.0), 2);
        assert_eq!(input.frame_ticks(f64::NAN), 0);
    }
}
