//! A reusable editable-image canvas: viewport mapping, an ordered pointer
//! stream with capture, rectangle hit-testing and the drag state machine.
//! Nothing here knows about COCO or layout classes beyond an annotation's
//! class id, so it can move into LiminalKit later.

pub mod draw;
pub mod hit_test;
pub mod interaction;
pub mod pointer;
pub mod shortcuts;
pub mod viewport;
