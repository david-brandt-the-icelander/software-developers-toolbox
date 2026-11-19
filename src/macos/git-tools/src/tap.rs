// Counts quick repeated presses of an arrow key (double and triple taps) and tells them apart from a key held down.

// Longest pause between two presses that still counts as the same multi-tap.
pub const TAP_MS: u64 = 250;
// Presses closer together than this are a held key's auto-repeat, not taps.
pub const REPEAT_MS: u64 = 60;

// The arrow directions taps are counted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

// The tap state: the last direction, when it was pressed, how many taps so far, and whether the key is being held.
#[derive(Debug, Default)]
pub struct Taps {
    last: Option<(Dir, u64)>,
    count: u8,
    held: bool,
}

impl Taps {
    // Records a press and returns the tap count (1 to 3); held keys and auto-repeats always count as 1.
    pub fn press(&mut self, dir: Dir, now_ms: u64, repeat: bool) -> u8 {
        let gap = match self.last {
            Some((d, t)) if d == dir => Some(now_ms.saturating_sub(t)),
            _ => None,
        };
        self.last = Some((dir, now_ms));
        if repeat || gap.is_some_and(|g| g < REPEAT_MS) {
            self.held = true;
            self.count = 1;
            return 1;
        }
        match gap {
            Some(g) if g <= TAP_MS && !self.held => self.count = (self.count + 1).min(3),
            _ => {
                self.held = false;
                self.count = 1;
            }
        }
        self.count
    }

    // Forgets the taps so far, after any other key.
    pub fn reset(&mut self) {
        *self = Taps::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checks single, double and triple taps, and that more taps stay at three.
    #[test]
    fn test_taps_count_up_to_three() {
        let mut t = Taps::default();
        assert_eq!(t.press(Dir::Left, 1000, false), 1);
        assert_eq!(t.press(Dir::Left, 1150, false), 2);
        assert_eq!(t.press(Dir::Left, 1300, false), 3);
        assert_eq!(t.press(Dir::Left, 1450, false), 3);
        assert_eq!(t.press(Dir::Left, 2000, false), 1);
    }

    // Checks that a change of direction or a reset starts again at one.
    #[test]
    fn test_taps_direction_change_and_reset() {
        let mut t = Taps::default();
        t.press(Dir::Up, 0, false);
        assert_eq!(t.press(Dir::Down, 100, false), 1);
        assert_eq!(t.press(Dir::Down, 200, false), 2);
        t.reset();
        assert_eq!(t.press(Dir::Down, 300, false), 1);
    }

    // Checks that auto-repeat, reported or inferred from fast presses, is not a tap until the key is released.
    #[test]
    fn test_taps_held_key() {
        let mut t = Taps::default();
        assert_eq!(t.press(Dir::Right, 0, false), 1);
        assert_eq!(t.press(Dir::Right, 30, false), 1);
        assert_eq!(t.press(Dir::Right, 200, false), 1);
        assert_eq!(t.press(Dir::Right, 600, false), 1);
        assert_eq!(t.press(Dir::Right, 700, false), 2);
        assert_eq!(t.press(Dir::Right, 750, true), 1);
    }
}
