//! Snapshot testing support for GPUI.
//!
//! This module provides a component that captures screenshots when triggered by events.
//!
//! # Example
//!
//! ```rust,ignore
//! // Create the snapshot trigger
//! let snapshot_trigger = cx.new(|_| SnapshotTrigger::new());
//!
//! // Subscribe with window access
//! cx.subscribe_in(&snapshot_trigger, window, |trigger, _emitter, event: &TakeSnapshot, window, _cx| {
//!     trigger.capture(&event.prefix, window).ok();
//! }).detach();
//!
//! // Trigger a snapshot
//! snapshot_trigger.update(cx, |_, cx| {
//!     cx.emit(TakeSnapshot { prefix: "my_component".into() });
//! });
//! ```

use crate::{div, Context, EventEmitter, IntoElement, Render, Window};

/// Event to request a snapshot capture.
///
/// The `prefix` field is used as the filename (without extension).
/// For example, `TakeSnapshot { prefix: "my_component".into() }` will save to
/// `.snapshots/my_component.png` (or the configured output directory).
#[derive(Clone, Debug)]
pub struct TakeSnapshot {
    /// The prefix for the snapshot filename (e.g., "my_component" -> ".snapshots/my_component.png")
    pub prefix: String,
}

/// A component that captures screenshots when it receives [`TakeSnapshot`] events.
///
/// Use with `subscribe_in` to get window access directly in the event handler:
///
/// ```rust,ignore
/// let snapshot_trigger = cx.new(|_| SnapshotTrigger::new());
///
/// cx.subscribe_in(&snapshot_trigger, window, |trigger, _emitter, event: &TakeSnapshot, window, _cx| {
///     match trigger.capture(&event.prefix, window) {
///         Ok(path) => log::info!("Snapshot saved: {}", path.display()),
///         Err(e) => log::error!("Snapshot failed: {}", e),
///     }
/// }).detach();
/// ```
pub struct SnapshotTrigger {
    output_dir: String,
}

impl SnapshotTrigger {
    /// Create a new SnapshotTrigger with default output directory (`.snapshots`)
    pub fn new() -> Self {
        Self {
            output_dir: ".snapshots".into(),
        }
    }

    /// Create with a custom output directory
    pub fn with_output_dir(output_dir: impl Into<String>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }

    /// Take a snapshot with the given prefix.
    ///
    /// Saves the screenshot to `{output_dir}/{prefix}.png`.
    /// Creates the output directory if it doesn't exist.
    pub fn capture(&self, prefix: &str, window: &Window) -> anyhow::Result<std::path::PathBuf> {
        let path = format!("{}/{}.png", self.output_dir, prefix);
        window.save_snapshot(&path)
    }
}

impl Default for SnapshotTrigger {
    fn default() -> Self {
        Self::new()
    }
}

impl EventEmitter<TakeSnapshot> for SnapshotTrigger {}

impl Render for SnapshotTrigger {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Invisible component - no visual representation
        div()
    }
}
