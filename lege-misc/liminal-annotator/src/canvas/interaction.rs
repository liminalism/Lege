//! The canvas interaction state machine.
//!
//! Pointer-down decides the action, pointer-move updates a preview, and
//! pointer-up commits exactly one undoable edit. Tentative geometry lives in
//! the [`ToolState`] and never in the document until release.

use liminalkit_shell::MouseButton;

use crate::annotation::commands::{Annotation, Edit, PageDoc, new_annotation_id};
use crate::annotation::geometry::{Box2, DragLimits, Handle, Point, apply_drag, apply_edge_drag};

use super::hit_test::pick;
use super::pointer::{PointerEvent, PointerPhase};
use super::viewport::{CanvasRect, ViewTransform, ZoomMode};

#[derive(Clone, Debug, PartialEq)]
pub enum ToolState {
    Idle,
    Drawing {
        class_id: u8,
        /// Image coordinates.
        origin: Point,
        current: Point,
    },
    Editing {
        annotation_id: String,
        handle: Handle,
        original: Box2,
        /// Image coordinate of the press.
        pointer_start: Point,
        /// Screen position of the press, for the click/drag threshold.
        screen_start: Point,
        preview: Box2,
        /// Set once the pointer has travelled past the drag threshold.
        dragging: bool,
    },
    Panning {
        button: MouseButton,
        screen_start: Point,
        original_origin: Point,
    },
}

/// Interaction tuning, all in physical screen pixels.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// Half-width of the edge hit band.
    pub slop: f64,
    /// Pointer travel below which a press on a box is a click (select only).
    pub drag_threshold: f64,
    /// Smallest new box, per side, that a draw commits.
    pub min_draw: f64,
    /// Smallest size a resize may shrink a box to.
    pub min_size: f64,
}

impl Params {
    pub fn for_dpi(scale_factor: f64, slop_logical: f64) -> Params {
        Params {
            slop: slop_logical * scale_factor,
            drag_threshold: 3.0 * scale_factor,
            min_draw: 3.0 * scale_factor,
            min_size: 3.0 * scale_factor,
        }
    }
}

/// What a processed event changed, so the app knows what to redraw/save.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The document changed (one committed edit).
    pub edited: bool,
    /// The view transform changed (image layer must be re-rendered).
    pub view_changed: bool,
    /// Something visible changed (overlay redraw).
    pub redraw: bool,
}

impl Outcome {
    pub fn merge(&mut self, o: Outcome) {
        self.edited |= o.edited;
        self.view_changed |= o.view_changed;
        self.redraw |= o.redraw;
    }
}

#[derive(Debug)]
pub struct Editor {
    pub state: ToolState,
    pub selected: Option<String>,
    pub active_class: u8,
    /// Box and handle the pointer would grab now (hover feedback).
    pub hover: Option<(String, Handle)>,
    /// Space is held: left-drag pans.
    pub space_held: bool,
    pub show_outlines: bool,
    pub zoom_mode: ZoomMode,
    pub params: Params,
    /// Last pointer position (screen).
    pub pointer: Point,
}

impl Editor {
    pub fn new(params: Params) -> Editor {
        Editor {
            state: ToolState::Idle,
            selected: None,
            active_class: 0,
            hover: None,
            space_held: false,
            show_outlines: true,
            zoom_mode: ZoomMode::Fit,
            params,
            pointer: Point::default(),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.state != ToolState::Idle
    }

    /// Forget per-page state on navigation.
    pub fn reset_for_page(&mut self) {
        self.state = ToolState::Idle;
        self.selected = None;
        self.hover = None;
    }

    fn limits(&self, doc: &PageDoc, view: &ViewTransform) -> DragLimits {
        DragLimits {
            min_size: (self.params.min_size / view.scale).max(1.0),
            bounds_w: doc.width,
            bounds_h: doc.height,
        }
    }

    /// The geometry to draw for annotation `id` right now (preview while
    /// it is being dragged).
    pub fn preview_for(&self, id: &str) -> Option<Box2> {
        match &self.state {
            ToolState::Editing {
                annotation_id,
                preview,
                ..
            } if annotation_id == id => Some(*preview),
            _ => None,
        }
    }

    /// The rubber band of a box being drawn, in image coordinates.
    pub fn drawing_box(&self, doc: &PageDoc) -> Option<(u8, Box2)> {
        match &self.state {
            ToolState::Drawing {
                class_id,
                origin,
                current,
            } => Some((
                *class_id,
                Box2::from_corners(*origin, *current).clamp_to(doc.width, doc.height),
            )),
            _ => None,
        }
    }

    /// Handle one pointer event. `doc` is `None` when no page is loaded
    /// (only panning/zooming is then meaningful — and not even that).
    pub fn pointer(
        &mut self,
        ev: &PointerEvent,
        doc: &mut PageDoc,
        view: &mut ViewTransform,
        canvas: CanvasRect,
        image_ok: bool,
    ) -> Outcome {
        let screen = Point::new(ev.position.0, ev.position.1);
        self.pointer = screen;
        let mut out = Outcome::default();
        match ev.phase {
            PointerPhase::Down => {
                if self.state != ToolState::Idle {
                    return out; // a second button during a gesture is ignored
                }
                let button = ev.button.unwrap_or(MouseButton::Left);
                let pan = button == MouseButton::Middle
                    || (button == MouseButton::Left && self.space_held);
                if pan {
                    self.state = ToolState::Panning {
                        button,
                        screen_start: screen,
                        original_origin: view.origin,
                    };
                    return out;
                }
                if button != MouseButton::Left || !image_ok {
                    return out;
                }
                let image_pt = view.screen_to_image(screen);
                let hit = if self.show_outlines && !ev.modifiers.shift {
                    pick(
                        &doc.annotations,
                        self.selected.as_deref(),
                        view,
                        screen,
                        self.params.slop,
                    )
                } else {
                    None
                };
                match hit {
                    Some(hit) => {
                        let a = &doc.annotations[hit.index];
                        if self.selected.as_deref() != Some(a.id.as_str()) {
                            out.redraw = true;
                        }
                        self.selected = Some(a.id.clone());
                        self.state = ToolState::Editing {
                            annotation_id: a.id.clone(),
                            handle: hit.handle,
                            original: a.bbox,
                            pointer_start: image_pt,
                            screen_start: screen,
                            preview: a.bbox,
                            dragging: false,
                        };
                    }
                    None => {
                        if self.selected.take().is_some() {
                            out.redraw = true;
                        }
                        let p = Point::new(
                            image_pt.x.clamp(0.0, doc.width),
                            image_pt.y.clamp(0.0, doc.height),
                        );
                        self.state = ToolState::Drawing {
                            class_id: self.active_class,
                            origin: p,
                            current: p,
                        };
                    }
                }
            }
            PointerPhase::Move => {
                let limits = self.limits(doc, view);
                match &mut self.state {
                    ToolState::Idle => {
                        let hover = if self.show_outlines && image_ok && canvas.contains(screen) {
                            pick(
                                &doc.annotations,
                                self.selected.as_deref(),
                                view,
                                screen,
                                self.params.slop,
                            )
                            .map(|h| (doc.annotations[h.index].id.clone(), h.handle))
                        } else {
                            None
                        };
                        if hover != self.hover {
                            self.hover = hover;
                            out.redraw = true;
                        }
                    }
                    ToolState::Drawing { current, .. } => {
                        *current = view.screen_to_image(screen);
                        out.redraw = true;
                    }
                    ToolState::Editing {
                        handle,
                        original,
                        pointer_start,
                        screen_start,
                        preview,
                        dragging,
                        ..
                    } => {
                        if !*dragging {
                            let d = ((screen.x - screen_start.x).powi(2)
                                + (screen.y - screen_start.y).powi(2))
                            .sqrt();
                            if d >= self.params.drag_threshold {
                                *dragging = true;
                            }
                        }
                        if *dragging {
                            let now = view.screen_to_image(screen);
                            let delta =
                                Point::new(now.x - pointer_start.x, now.y - pointer_start.y);
                            let next = if ev.modifiers.alt && handle.is_corner() {
                                // Alt: move only the edge whose axis the drag favours.
                                let axis_x = delta.x.abs() >= delta.y.abs();
                                apply_edge_drag(*original, *handle, delta, axis_x, limits)
                            } else {
                                apply_drag(*original, *handle, delta, limits)
                            };
                            if next != *preview {
                                *preview = next;
                                out.redraw = true;
                            }
                        }
                    }
                    ToolState::Panning {
                        screen_start,
                        original_origin,
                        ..
                    } => {
                        view.origin = Point::new(
                            original_origin.x + screen.x - screen_start.x,
                            original_origin.y + screen.y - screen_start.y,
                        );
                        view.constrain(canvas, doc.width, doc.height, 64.0);
                        self.zoom_mode = ZoomMode::Free;
                        out.view_changed = true;
                        out.redraw = true;
                    }
                }
            }
            PointerPhase::Up => {
                let state = std::mem::replace(&mut self.state, ToolState::Idle);
                match state {
                    ToolState::Drawing {
                        class_id, origin, ..
                    } => {
                        // The release position is authoritative, even if no
                        // move arrived between press and release.
                        let current = view.screen_to_image(screen);
                        let b = Box2::from_corners(origin, current).clamp_to(doc.width, doc.height);
                        let min = self.params.min_draw / view.scale;
                        if b.width() >= min && b.height() >= min {
                            let ann = Annotation {
                                id: new_annotation_id(),
                                class_id,
                                bbox: b,
                                is_ignore: false,
                                provenance_json: None,
                            };
                            self.selected = Some(ann.id.clone());
                            let index = doc.annotations.len();
                            out.edited = doc.apply(Edit::Add { index, ann });
                        }
                        out.redraw = true;
                    }
                    ToolState::Editing {
                        annotation_id,
                        handle,
                        original,
                        pointer_start,
                        screen_start,
                        dragging,
                        ..
                    } => {
                        let travel = ((screen.x - screen_start.x).powi(2)
                            + (screen.y - screen_start.y).powi(2))
                        .sqrt();
                        if dragging || travel >= self.params.drag_threshold {
                            let now = view.screen_to_image(screen);
                            let delta =
                                Point::new(now.x - pointer_start.x, now.y - pointer_start.y);
                            let limits = self.limits(doc, view);
                            let finalb = if ev.modifiers.alt && handle.is_corner() {
                                let axis_x = delta.x.abs() >= delta.y.abs();
                                apply_edge_drag(original, handle, delta, axis_x, limits)
                            } else {
                                apply_drag(original, handle, delta, limits)
                            };
                            if let Some(before) = doc.get(&annotation_id).cloned() {
                                let mut after = before.clone();
                                after.bbox = finalb;
                                out.edited = doc.apply(Edit::Modify { before, after });
                            }
                        }
                        out.redraw = true;
                    }
                    ToolState::Panning { .. } | ToolState::Idle => {}
                }
            }
            PointerPhase::Cancel => {
                if self.cancel() {
                    out.redraw = true;
                }
            }
        }
        out
    }

    /// Escape or focus loss: abandon the gesture with no edit.
    pub fn cancel(&mut self) -> bool {
        let was_busy = self.state != ToolState::Idle;
        self.state = ToolState::Idle;
        was_busy
    }

    /// Number key: reclassify the selection (one edit) and make the class
    /// the active drawing class.
    pub fn choose_class(&mut self, class_id: u8, doc: &mut PageDoc) -> Outcome {
        let mut out = Outcome {
            redraw: true,
            ..Outcome::default()
        };
        self.active_class = class_id;
        if let ToolState::Drawing { class_id: c, .. } = &mut self.state {
            *c = class_id;
        }
        if let Some(id) = &self.selected {
            if let Some(before) = doc.get(id).cloned() {
                let mut after = before.clone();
                after.class_id = class_id;
                out.edited = doc.apply(Edit::Modify { before, after });
            }
        }
        out
    }

    pub fn delete_selected(&mut self, doc: &mut PageDoc) -> Outcome {
        let mut out = Outcome::default();
        if self.is_busy() {
            return out;
        }
        if let Some(id) = self.selected.take() {
            if let Some(index) = doc.index_of(&id) {
                let ann = doc.annotations[index].clone();
                out.edited = doc.apply(Edit::Remove { index, ann });
                out.redraw = true;
            }
        }
        self.hover = None;
        out
    }

    pub fn toggle_ignore(&mut self, doc: &mut PageDoc) -> Outcome {
        let mut out = Outcome::default();
        if let Some(id) = &self.selected {
            if let Some(before) = doc.get(id).cloned() {
                let mut after = before.clone();
                after.is_ignore = !after.is_ignore;
                out.edited = doc.apply(Edit::Modify { before, after });
                out.redraw = true;
            }
        }
        out
    }

    /// Select the next/previous box in reading order (top-to-bottom,
    /// left-to-right), for keyboard-only review.
    pub fn cycle_selection(&mut self, doc: &PageDoc, forward: bool) -> Outcome {
        if doc.annotations.is_empty() || self.is_busy() {
            return Outcome::default();
        }
        let mut order: Vec<usize> = (0..doc.annotations.len()).collect();
        order.sort_by(|&a, &b| {
            let (pa, pb) = (&doc.annotations[a].bbox, &doc.annotations[b].bbox);
            pa.y0.total_cmp(&pb.y0).then(pa.x0.total_cmp(&pb.x0))
        });
        let cur = self
            .selected
            .as_ref()
            .and_then(|id| order.iter().position(|&i| &doc.annotations[i].id == id));
        let n = order.len();
        let next = match (cur, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(c), true) => (c + 1) % n,
            (Some(c), false) => (c + n - 1) % n,
        };
        self.selected = Some(doc.annotations[order[next]].id.clone());
        Outcome {
            redraw: true,
            ..Outcome::default()
        }
    }

    /// Nudge the selection by whole image pixels (arrow keys with Ctrl), or
    /// grow/shrink its bottom-right corner with Shift. One edit per press.
    pub fn nudge(&mut self, doc: &mut PageDoc, dx: f64, dy: f64, resize: bool) -> Outcome {
        let mut out = Outcome::default();
        if self.is_busy() {
            return out;
        }
        let Some(id) = &self.selected else { return out };
        let Some(before) = doc.get(id).cloned() else {
            return out;
        };
        let limits = DragLimits {
            min_size: 1.0,
            bounds_w: doc.width,
            bounds_h: doc.height,
        };
        let handle = if resize {
            Handle::BottomRight
        } else {
            Handle::Move
        };
        let mut after = before.clone();
        after.bbox = apply_drag(before.bbox, handle, Point::new(dx, dy), limits);
        out.edited = doc.apply(Edit::Modify { before, after });
        out.redraw = true;
        out
    }

    /// Drop a selection/hover that no longer exists (after undo/redo).
    pub fn reconcile(&mut self, doc: &PageDoc) {
        if let Some(id) = &self.selected {
            if doc.index_of(id).is_none() {
                self.selected = None;
            }
        }
        if let Some((id, _)) = &self.hover {
            if doc.index_of(id).is_none() {
                self.hover = None;
            }
        }
        if let ToolState::Editing { annotation_id, .. } = &self.state {
            if doc.index_of(annotation_id).is_none() {
                self.state = ToolState::Idle;
            }
        }
    }
}
