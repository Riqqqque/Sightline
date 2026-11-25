//! Draws the window into a picture file without opening a window. For checking the look of the
//! app while something else has the screen: nothing appears, nothing takes focus.
//!
//! It runs the made-up game for a few seconds, takes the last view, lays the real window out at
//! the asked size and scale, and writes what the software renderer produces as a .bmp file.

use crate::engine::{self, StatsLink};
use crate::model::View;
use crate::source::fixture::{self, Fixture, FixtureStats};
use crate::store::Store;
use crate::{pair, stats, together, ui_bridge, value, AppWindow};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// A platform with exactly one window that exists only in memory.
struct Offscreen {
    window: Rc<MinimalSoftwareWindow>,
    started: Instant,
}

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.started.elapsed()
    }
}

/// The last view the made-up game produces within `seconds`.
fn last_view(mode: &str, seconds: u64) -> Option<View> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    let store = Store::open(None)?;
    fixture::seed(&store);
    let last: Rc<RefCell<Option<View>>> = Rc::default();
    let sink = last.clone();
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (insight_tx, insight_rx) = mpsc::unbounded_channel();
    let link = StatsLink { commands: command_tx, insights: insight_rx };
    let stats = (mode != "nokey").then(FixtureStats::new);
    runtime.block_on(async {
        let app = together(
            engine::run_with(Fixture::new(mode), Some(link), move |view| *sink.borrow_mut() = Some(view)),
            stats::run(stats, &store, command_rx, move |insight| {
                let _ = insight_tx.send(insight);
            }),
        );
        // The made-up game never ends by itself; the clock ends the run.
        let _ = tokio::time::timeout(Duration::from_secs(seconds), app).await;
    });
    last.take()
}

/// Writes 24-bit pixels as a .bmp file: a 54-byte header, then rows bottom-up, each padded to
/// four bytes.
fn write_bmp(path: &Path, width: usize, height: usize, pixels: &[slint::Rgb8Pixel]) -> std::io::Result<()> {
    let row = (width * 3).div_ceil(4) * 4;
    let size = 54 + row * height;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for y in (0..height).rev() {
        for p in &pixels[y * width..(y + 1) * width] {
            out.extend_from_slice(&[p.b, p.g, p.r]);
        }
        out.resize(out.len() + row - width * 3, 0);
    }
    std::fs::write(path, out)
}

pub fn to_file(args: &[String], mode: &str, out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height) = value(args, "--size").and_then(|v| pair(v, 'x')).unwrap_or((1920, 1290));
    let (width, height) = (width.clamp(64, 8192) as u32, height.clamp(64, 8192) as u32);
    let scale: f32 = value(args, "--scale").and_then(|v| v.parse().ok()).unwrap_or(1.5);
    let seconds: u64 = value(args, "--exit-after").and_then(|v| v.parse().ok()).unwrap_or(5);

    let view = last_view(mode, seconds).ok_or("the made-up game produced nothing")?;

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen { window: window.clone(), started: Instant::now() }))?;
    let ui = AppWindow::new()?;
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
    window.set_size(slint::PhysicalSize::new(width, height));
    ui.set_key_found(mode != "nokey");
    ui.set_app_version(crate::update::current().into());
    if args.iter().any(|a| a == "--settings") {
        ui.set_page(1);
    }
    if let Some(row) = value(args, "--select") {
        ui.set_sel_key(row.as_str().into());
    }
    ui.show()?;
    ui_bridge::apply(&ui, view);

    slint::platform::update_timers_and_animations();
    window.request_redraw();
    let mut pixels = vec![slint::Rgb8Pixel { r: 0, g: 0, b: 0 }; (width * height) as usize];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width as usize);
    });
    write_bmp(out, width as usize, height as usize, &pixels)?;
    Ok(())
}
