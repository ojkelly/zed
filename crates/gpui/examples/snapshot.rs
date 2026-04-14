//! Example demonstrating visual snapshot testing in GPUI.
//!
//! This example shows how to capture a rendered window as a PNG image.
//!
//! Run with: cargo run -p gpui --example snapshot --features snapshots

use gpui::{
    App, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions, div, point,
    prelude::*, px, rgb, size,
};
use gpui_platform::application;
use std::time::Duration;

struct SnapshotDemo {
    text: SharedString,
}

impl Render for SnapshotDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .bg(rgb(0x2e3440))
            .size_full()
            .justify_center()
            .items_center()
            .text_xl()
            .text_color(rgb(0xeceff4))
            .child(format!("Snapshot Demo: {}", &self.text))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().size_8().bg(rgb(0xbf616a)).rounded_md())
                    .child(div().size_8().bg(rgb(0xd08770)).rounded_md())
                    .child(div().size_8().bg(rgb(0xebcb8b)).rounded_md())
                    .child(div().size_8().bg(rgb(0xa3be8c)).rounded_md())
                    .child(div().size_8().bg(rgb(0x5e81ac)).rounded_md()),
            )
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::new(point(px(100.), px(100.)), size(px(400.), px(300.)));
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_window, cx| {
                    cx.new(|_| SnapshotDemo {
                        text: "Hello World".into(),
                    })
                },
            )
            .unwrap();

        // Schedule a snapshot after render
        cx.spawn(async move |cx| {
            // Wait for the window to render
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;

            // Take snapshot directly on the window handle
            match window.take_snapshot(cx, "snapshot_demo") {
                Ok(path) => println!("Snapshot saved: {}", path.display()),
                Err(e) => eprintln!("Snapshot failed: {}", e),
            }

            // Quit after snapshot
            cx.update(|cx| cx.quit());
        })
        .detach();

        cx.activate(true);
    });
}
