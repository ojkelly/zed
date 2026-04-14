# Visual Snapshot Testing in GPUI

This guide covers how to use GPUI's snapshot capability to capture screenshots of your UI during development and testing.

## Quick Start

Enable the `snapshots` feature in your `Cargo.toml`:

```toml
[dependencies]
gpui = { path = "../gpui", features = ["snapshots"] }
```

Take a snapshot from a `WindowHandle`:

```rust
window.take_snapshot(cx, "my_component")?;
// Saves to .snapshots/my_component.png
```

## API Reference

### WindowHandle Methods

```rust
// Simple API - saves to .snapshots/{name}.png
window_handle.take_snapshot(cx, "name")?;
```

### Window Methods

When you have direct access to `&Window`:

```rust
// Save to .snapshots/{name}.png
window.take_snapshot("name")?;

// Save to a custom path
window.save_snapshot("path/to/screenshot.png")?;

// Get raw pixel data (width, height, RGBA bytes)
let (width, height, rgba_data) = window.snapshot()?;
```

### Event-Driven Snapshots

For capturing snapshots in response to events:

```rust
use gpui::{TakeSnapshot, SnapshotTrigger};

// Create the trigger component
let trigger = cx.new(|_| SnapshotTrigger::new());

// Subscribe to snapshot events
cx.subscribe_in(&trigger, window, |trigger, _, event: &TakeSnapshot, window, _cx| {
    match trigger.capture(&event.prefix, window) {
        Ok(path) => log::info!("Saved: {}", path.display()),
        Err(e) => log::error!("Failed: {}", e),
    }
}).detach();

// Trigger a snapshot from anywhere
trigger.update(cx, |_, cx| {
    cx.emit(TakeSnapshot { prefix: "after_click".into() });
});
```

## Development Workflow

### Iterating on a Component

1. **Set up a snapshot point** in your component:

```rust
impl MyComponent {
    fn on_some_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // ... your logic ...

        #[cfg(feature = "snapshots")]
        window.take_snapshot("my_component_after_action").ok();
    }
}
```

2. **Run your app** and trigger the action. Check `.snapshots/` for the captured image.

3. **Compare iterations** by naming snapshots descriptively:

```rust
window.take_snapshot("button_v1_hover_state")?;
// Make changes, run again
window.take_snapshot("button_v2_hover_state")?;
```

### Capturing at Specific Moments

Use a timer to capture after animations complete:

```rust
cx.spawn(async move |cx| {
    // Wait for animation
    cx.background_executor().timer(Duration::from_millis(300)).await;

    window_handle.take_snapshot(cx, "after_animation").ok();
}).detach();
```

### Conditional Snapshots

Capture only in debug builds or when a flag is set:

```rust
#[cfg(all(debug_assertions, feature = "snapshots"))]
if std::env::var("CAPTURE_SNAPSHOTS").is_ok() {
    window.take_snapshot("debug_state")?;
}
```

## Writing Visual Tests

Create a test that captures snapshots for manual or automated comparison:

```rust
#[cfg(feature = "snapshots")]
#[test]
fn test_button_states() {
    let app = Application::new().unwrap();

    app.run(|cx| {
        let window = cx.open_window(WindowOptions::default(), |_, cx| {
            cx.new(|_| MyButton::new("Click me"))
        }).unwrap();

        cx.spawn(async move |cx| {
            // Capture default state
            window.take_snapshot(cx, "button_default").ok();

            // Simulate hover (update component state)
            window.update(cx, |button, _, cx| {
                button.set_hovered(true);
                cx.notify();
            }).ok();

            // Wait for render
            cx.background_executor().timer(Duration::from_millis(100)).await;

            // Capture hover state
            window.take_snapshot(cx, "button_hover").ok();

            cx.update(|cx| cx.quit()).ok();
        }).detach();
    });
}
```

## Output Location

By default, snapshots save to `.snapshots/` in the current working directory. The directory is created automatically if it doesn't exist.

For custom locations, use `window.save_snapshot()` or `SnapshotTrigger::with_output_dir()`:

```rust
// Custom path
window.save_snapshot("tests/fixtures/expected_output.png")?;

// Custom directory for event-driven snapshots
let trigger = cx.new(|_| SnapshotTrigger::with_output_dir("test_output"));
```

## Tips

- **Name snapshots descriptively**: Include component name, state, and version in the filename
- **Clean up old snapshots**: The `.snapshots/` directory can grow large during development
- **Add to .gitignore**: Consider ignoring `.snapshots/` or selectively committing only reference images
- **Use for debugging**: Capture state before and after problematic operations to visualize issues
