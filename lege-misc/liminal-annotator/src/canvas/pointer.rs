//! An ordered pointer-event stream with explicit capture.
//!
//! LiminalKit's high-level `Input` keeps the held state and one unclaimed
//! press, which cannot reconstruct a press and release that both happen
//! between two frames. The host's `PixelApp` callbacks *do* arrive in order,
//! so this module turns them into a queue of `Down / Move / Up / Cancel`
//! events that the application drains once per frame. Nothing is coalesced
//! except consecutive moves with no button transition between them.
//!
//! Designed to be lifted into LiminalKit unchanged: it knows nothing about
//! annotations.

use std::collections::VecDeque;

use liminalkit_shell::{Modifiers, MouseButton};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerPhase {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEvent {
    pub phase: PointerPhase,
    /// Physical buffer pixels.
    pub position: (f64, f64),
    pub button: Option<MouseButton>,
    pub modifiers: Modifiers,
    pub pointer_id: u64,
}

/// Which surface owns the pointer between a press and its release.
#[derive(Clone, Copy, Debug, Default)]
pub struct PointerCapture {
    owner: Option<u64>,
}

impl PointerCapture {
    pub fn capture(&mut self, surface_id: u64) -> bool {
        match self.owner {
            None => {
                self.owner = Some(surface_id);
                true
            }
            Some(id) => id == surface_id,
        }
    }

    pub fn release(&mut self, surface_id: u64) {
        if self.owner == Some(surface_id) {
            self.owner = None;
        }
    }

    pub fn owns(&self, surface_id: u64) -> bool {
        self.owner == Some(surface_id)
    }

    pub fn owner(&self) -> Option<u64> {
        self.owner
    }

    pub fn clear(&mut self) {
        self.owner = None;
    }
}

/// Collects host callbacks into ordered pointer events.
#[derive(Debug, Default)]
pub struct PointerQueue {
    events: VecDeque<PointerEvent>,
    position: (f64, f64),
    modifiers: Modifiers,
    /// Buttons currently held, so focus loss can cancel exactly those.
    held: [bool; 3],
    inside: bool,
}

const MOUSE_ID: u64 = 0;

fn button_index(b: MouseButton) -> usize {
    match b {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
    }
}

impl PointerQueue {
    pub fn new() -> PointerQueue {
        PointerQueue::default()
    }

    pub fn position(&self) -> (f64, f64) {
        self.position
    }

    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    pub fn inside(&self) -> bool {
        self.inside
    }

    pub fn any_held(&self) -> bool {
        self.held.iter().any(|&h| h)
    }

    pub fn set_modifiers(&mut self, m: Modifiers) {
        self.modifiers = m;
    }

    pub fn moved(&mut self, x: f64, y: f64) {
        self.position = (x, y);
        self.inside = true;
        // Coalesce only a move that directly follows another move: the
        // latest position is all a consumer of a pure move needs, and a
        // button transition in between is never merged away.
        if let Some(last) = self.events.back_mut() {
            if last.phase == PointerPhase::Move {
                last.position = (x, y);
                last.modifiers = self.modifiers;
                return;
            }
        }
        self.push(PointerPhase::Move, None);
    }

    pub fn left(&mut self) {
        self.inside = false;
    }

    pub fn button(&mut self, button: MouseButton, pressed: bool) {
        let i = button_index(button);
        if pressed {
            if self.held[i] {
                return; // a duplicate press without release: ignore
            }
            self.held[i] = true;
            self.push(PointerPhase::Down, Some(button));
        } else {
            if !self.held[i] {
                return; // release of a press we never saw (e.g. from another window)
            }
            self.held[i] = false;
            self.push(PointerPhase::Up, Some(button));
        }
    }

    /// Focus loss or explicit cancellation: every held button is released
    /// as a `Cancel`, so no owner is left waiting for an `Up`.
    pub fn cancel_all(&mut self) {
        for (i, held) in self.held.iter_mut().enumerate() {
            if *held {
                *held = false;
                let button = [MouseButton::Left, MouseButton::Right, MouseButton::Middle][i];
                self.events.push_back(PointerEvent {
                    phase: PointerPhase::Cancel,
                    position: self.position,
                    button: Some(button),
                    modifiers: self.modifiers,
                    pointer_id: MOUSE_ID,
                });
            }
        }
    }

    fn push(&mut self, phase: PointerPhase, button: Option<MouseButton>) {
        self.events.push_back(PointerEvent {
            phase,
            position: self.position,
            button,
            modifiers: self.modifiers,
            pointer_id: MOUSE_ID,
        });
    }

    pub fn drain(&mut self) -> impl Iterator<Item = PointerEvent> + '_ {
        self.events.drain(..)
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_and_release_within_one_frame_survive() {
        let mut q = PointerQueue::new();
        q.moved(10.0, 10.0);
        q.button(MouseButton::Left, true);
        q.moved(20.0, 20.0);
        q.moved(30.0, 30.0);
        q.button(MouseButton::Left, false);
        let phases: Vec<_> = q.drain().map(|e| (e.phase, e.position)).collect();
        assert_eq!(
            phases,
            vec![
                (PointerPhase::Move, (10.0, 10.0)),
                (PointerPhase::Down, (10.0, 10.0)),
                (PointerPhase::Move, (30.0, 30.0)),
                (PointerPhase::Up, (30.0, 30.0)),
            ]
        );
    }

    #[test]
    fn focus_loss_cancels_held_buttons() {
        let mut q = PointerQueue::new();
        q.button(MouseButton::Left, true);
        q.drain().count();
        q.cancel_all();
        let e: Vec<_> = q.drain().collect();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].phase, PointerPhase::Cancel);
        // A late release is ignored.
        q.button(MouseButton::Left, false);
        assert!(q.is_empty());
    }

    #[test]
    fn capture_is_exclusive() {
        let mut c = PointerCapture::default();
        assert!(c.capture(1));
        assert!(!c.capture(2));
        assert!(c.capture(1));
        c.release(2);
        assert!(c.owns(1));
        c.release(1);
        assert!(c.capture(2));
    }
}
