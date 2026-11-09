//! Keyboard contract (spec §3). Digits use the *physical* top-row key
//! (`Digit1`..`Digit9`), because Shift+1 arrives as `!` on US layouts and as
//! something else on others; letters use the layout's character so Ctrl+Z
//! follows the printed key on QWERTZ/AZERTY, with the physical key as a
//! fallback.

use liminalkit_shell::KeyInput;
use liminalkit_windowing::KeyEvent;

use crate::settings::Resolved;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    PrevPage,
    AcceptAndNext,
    /// Skip forward without marking reviewed.
    NextPage,
    Class(u8),
    Fit,
    Actual,
    ZoomIn,
    ZoomOut,
    Undo,
    Redo,
    Delete,
    ToggleIgnore,
    ToggleExclude,
    ToggleAttention,
    ToggleOutlines,
    ToggleTags,
    ToggleHud,
    CycleCompare,
    CycleQueue,
    Cancel,
    NextBox,
    PrevBox,
    Nudge {
        dx: i8,
        dy: i8,
        resize: bool,
    },
    FirstUnreviewed,
    JumpBack10,
    JumpFwd10,
    Help,
    Fullscreen,
    PerfOverlay,
    SaveNotice,
    RevertPage,
    SpaceDown,
    SpaceUp,
}

fn digit_of(physical: &str) -> Option<usize> {
    let d = physical
        .strip_prefix("Digit")
        .or_else(|| physical.strip_prefix("Numpad"))?;
    let n: usize = d.parse().ok()?;
    (1..=9).contains(&n).then_some(n)
}

fn letter(ev: &KeyEvent) -> Option<char> {
    if let KeyInput::Character(c) = ev.key {
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_lowercase());
        }
    }
    // Ctrl+letter may arrive as a control character; fall back to physical.
    ev.physical
        .strip_prefix("Key")
        .and_then(|s| s.chars().next())
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
}

/// Map a key event to an action. Returns `None` for keys the annotator does
/// not use (and for releases other than Space).
pub fn map_key(ev: &KeyEvent, settings: &Resolved) -> Option<Action> {
    let m = ev.modifiers;
    if ev.physical == "Space" {
        if ev.repeat {
            return None;
        }
        return Some(if ev.pressed {
            Action::SpaceDown
        } else {
            Action::SpaceUp
        });
    }
    if !ev.pressed {
        return None;
    }
    let ctrl = m.control || m.logo;

    // Digits by physical position.
    if let Some(n) = digit_of(&ev.physical) {
        let numpad = ev.physical.starts_with("Numpad");
        if ctrl && !m.shift && n == 1 && !numpad {
            return Some(Action::Actual);
        }
        if ctrl {
            return None;
        }
        let table = if m.shift && !numpad {
            &settings.shift_digit
        } else {
            &settings.digit
        };
        return table[n - 1].map(Action::Class);
    }
    if !settings.raw.physical_digits && !ctrl {
        if let KeyInput::Character(c) = ev.key {
            if let Some(n) = c.to_digit(10).filter(|n| (1..=9).contains(n)) {
                return settings.digit[n as usize - 1].map(Action::Class);
            }
        }
    }

    match ev.key {
        KeyInput::Left if ctrl => {
            return Some(Action::Nudge {
                dx: -1,
                dy: 0,
                resize: m.shift,
            })
        }
        KeyInput::Right if ctrl => {
            return Some(Action::Nudge {
                dx: 1,
                dy: 0,
                resize: m.shift,
            })
        }
        KeyInput::Up if ctrl => {
            return Some(Action::Nudge {
                dx: 0,
                dy: -1,
                resize: m.shift,
            })
        }
        KeyInput::Down if ctrl => {
            return Some(Action::Nudge {
                dx: 0,
                dy: 1,
                resize: m.shift,
            })
        }
        KeyInput::Left => return Some(Action::PrevPage),
        KeyInput::Right if m.shift => return Some(Action::NextPage),
        KeyInput::Right => return Some(Action::AcceptAndNext),
        KeyInput::Delete | KeyInput::Backspace => return Some(Action::Delete),
        KeyInput::Escape => return Some(Action::Cancel),
        KeyInput::Tab if m.shift => return Some(Action::PrevBox),
        KeyInput::Tab => return Some(Action::NextBox),
        KeyInput::Home => return Some(Action::FirstUnreviewed),
        KeyInput::PageUp => return Some(Action::JumpBack10),
        KeyInput::PageDown => return Some(Action::JumpFwd10),
        KeyInput::Function(1) => return Some(Action::Help),
        KeyInput::Function(11) => return Some(Action::Fullscreen),
        KeyInput::Function(12) => return Some(Action::PerfOverlay),
        _ => {}
    }
    if let KeyInput::Character(c) = ev.key {
        match c {
            '+' | '=' if !ctrl => return Some(Action::ZoomIn),
            '-' if !ctrl => return Some(Action::ZoomOut),
            '?' => return Some(Action::Help),
            _ => {}
        }
    }
    let l = letter(ev)?;
    if ctrl {
        return match l {
            'z' if m.shift => Some(Action::Redo),
            'z' => Some(Action::Undo),
            'y' => Some(Action::Redo),
            's' => Some(Action::SaveNotice),
            'c' => Some(Action::RevertPage),
            _ => None,
        };
    }
    if m.alt {
        return None;
    }
    match l {
        'f' => Some(Action::Fit),
        'u' => Some(Action::ToggleIgnore),
        'x' => Some(Action::ToggleExclude),
        'n' => Some(Action::ToggleAttention),
        'v' => Some(Action::ToggleOutlines),
        't' => Some(Action::ToggleTags),
        'h' => Some(Action::ToggleHud),
        'c' => Some(Action::CycleCompare),
        'q' => Some(Action::CycleQueue),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use liminalkit_shell::Modifiers;

    fn ev(key: KeyInput, physical: &str, m: Modifiers) -> KeyEvent {
        KeyEvent {
            key,
            text: String::new(),
            physical: physical.into(),
            pressed: true,
            repeat: false,
            modifiers: m,
        }
    }

    #[test]
    fn shift_digit_uses_physical_key() {
        let (r, _) = Settings::default().resolve();
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        // US layout: Shift+1 produces '!'.
        assert_eq!(
            map_key(&ev(KeyInput::Character('!'), "Digit1", shift), &r),
            Some(Action::Class(3))
        );
        assert_eq!(
            map_key(
                &ev(KeyInput::Character('1'), "Digit1", Modifiers::default()),
                &r
            ),
            Some(Action::Class(0))
        );
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        assert_eq!(
            map_key(&ev(KeyInput::Character('1'), "Digit1", ctrl), &r),
            Some(Action::Actual)
        );
        // AZERTY: unshifted top-row key 2 produces 'é' but is still class 2.
        assert_eq!(
            map_key(
                &ev(KeyInput::Character('é'), "Digit2", Modifiers::default()),
                &r
            ),
            Some(Action::Class(6))
        );
    }

    #[test]
    fn ctrl_z_follows_layout_character() {
        let (r, _) = Settings::default().resolve();
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        // QWERTZ: the key printed Z sits at physical KeyY.
        assert_eq!(
            map_key(&ev(KeyInput::Character('z'), "KeyY", ctrl), &r),
            Some(Action::Undo)
        );
        assert_eq!(
            map_key(&ev(KeyInput::Character('y'), "KeyZ", ctrl), &r),
            Some(Action::Redo)
        );
    }
}
