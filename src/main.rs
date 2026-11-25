// Sightline: a separate window that shows who is in the lobby and match.
// Read-only. No game memory, no injection, no input.
//
// Usage: sightline [--fixture [tour|closed|menu|queue|lobby|match|post|dm|dm12|stress|refused|nokey]]
//                 [--db PATH] [--exit-after SECONDS] [--pos X,Y] [--size WxH]
//   --fixture     run on made-up data without the game or the network; put `show:` in front of
//                 a mode (`show:match`) for the cast used in the README pictures
//   --db          use this database file instead of the default one
//   --exit-after  quit after this many seconds (for measurements)
//   --pos, --size place the window, in physical pixels (for measurements)
//   --settings    open on the settings page; --select a1 opens that row (for pictures)
//   --render FILE draw the window into a .bmp file and quit, without ever opening a window;
//                 needs --fixture; takes --size, --scale 1.5, --settings, --select, --exit-after
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod logic;
mod model;
mod render;
mod source;
mod state;
mod stats;
mod store;
mod ui_bridge;
mod update;
mod win;

slint::include_modules!();

use engine::StatsLink;
use slint::{ComponentHandle, Model};
use source::fixture::{Fixture, FixtureStats};
use source::henrik::{ApiKey, Henrik};
use source::local::LocalClient;
use stats::Command;
use std::future::Future;
use std::path::PathBuf;
use std::task::Poll;
use std::time::Duration;
use store::Store;
use tokio::sync::mpsc;

pub(crate) fn pair(s: &str, sep: char) -> Option<(i32, i32)> {
    let (a, b) = s.split_once(sep)?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

pub(crate) fn value<'a>(args: &'a [String], name: &str) -> Option<&'a String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1))
}

/// Runs the engine and the stats worker side by side on this thread. Ends when the engine does.
pub(crate) async fn together(engine: impl Future<Output = ()>, worker: impl Future<Output = ()>) {
    let mut engine = std::pin::pin!(engine);
    let mut worker = std::pin::pin!(worker);
    let mut worker_done = false;
    std::future::poll_fn(|cx| {
        if engine.as_mut().poll(cx).is_ready() {
            return Poll::Ready(());
        }
        if !worker_done && worker.as_mut().poll(cx).is_ready() {
            worker_done = true;
        }
        Poll::Pending
    })
    .await;
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let fixture = args.iter().position(|a| a == "--fixture").map(|i| {
        args.get(i + 1).filter(|v| !v.starts_with("--")).cloned().unwrap_or_else(|| "tour".to_string())
    });
    let exit_after: Option<u64> = value(&args, "--exit-after").and_then(|v| v.parse().ok());
    let db_path: Option<PathBuf> = value(&args, "--db").map(PathBuf::from);

    if let Some(file) = value(&args, "--render") {
        return render::to_file(&args, fixture.as_deref().unwrap_or("match"), std::path::Path::new(file));
    }

    slint::BackendSelector::new().backend_name("winit".into()).renderer_name("software".into()).select()?;
    let ui = AppWindow::new()?;
    ui.set_app_version(update::current().into());
    if fixture.is_none() {
        // Whatever the last update left next to the program.
        update::tidy();
    }

    // The key is read once, here. Only "found or not" ever reaches the window.
    let key = match fixture.as_deref() {
        Some(_) => None,
        None => ApiKey::load(),
    };
    ui.set_key_found(match fixture.as_deref() {
        Some(mode) => mode != "nokey",
        None => key.is_some(),
    });

    if args.iter().any(|a| a == "--settings") {
        ui.set_page(1);
    }
    // Opens a row's details a few seconds in, once the made-up lobby is on screen.
    let select = slint::Timer::default();
    if let Some(key) = value(&args, "--select").cloned() {
        let weak = ui.as_weak();
        select.start(slint::TimerMode::SingleShot, Duration::from_secs(4), move || {
            let Some(ui) = weak.upgrade() else { return };
            let row = ui.get_allies().iter().chain(ui.get_enemies().iter()).find(|r| r.key == key.as_str());
            if let Some(row) = row {
                ui.set_note_draft(row.note.clone());
                ui.set_sel_key(row.key.clone());
                ui.set_sel_row(row);
            }
        });
    }

    let (command_tx, command_rx) = mpsc::unbounded_channel::<Command>();
    let (insight_tx, insight_rx) = mpsc::unbounded_channel();

    // All I/O happens on one worker thread. The UI thread only draws.
    let weak = ui.as_weak();
    let commands = command_tx.clone();
    std::thread::Builder::new().name("worker".into()).spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        let push = move |view| {
            let _ = weak.upgrade_in_event_loop(move |ui| ui_bridge::apply(&ui, view));
        };
        let emit = move |insight| {
            let _ = insight_tx.send(insight);
        };
        let link = StatsLink { commands, insights: insight_rx };
        match fixture {
            Some(mode) => {
                // Made-up data never reaches the real database: memory only, unless a file is named.
                let Some(store) = Store::open(db_path.as_deref()) else { return };
                source::fixture::seed(&store);
                let stats = (mode != "nokey").then(|| FixtureStats::for_mode(&mode));
                runtime.block_on(together(
                    engine::run_with(Fixture::new(&mode), Some(link), push),
                    stats::run(stats, &store, command_rx, emit),
                ));
            }
            None => {
                let path = db_path.or_else(Store::default_path);
                // Without a usable file the app still runs; it just remembers nothing.
                let Some(store) = Store::open(path.as_deref()).or_else(|| Store::open(None)) else { return };
                store.adopt();
                runtime.block_on(together(
                    engine::run_with(LocalClient::new(), Some(link), push),
                    stats::run_live(key.map(Henrik::new), &store, command_rx, emit),
                ));
            }
        }
    })?;

    let commands = command_tx.clone();
    ui.on_note_edited(move |key, text| {
        if let Some((side, index)) = ui_bridge::place(&key) {
            let _ = commands.send(Command::Note { side, index, text: text.to_string() });
        }
    });
    let commands = command_tx.clone();
    ui.on_setting_changed(move |key, value| {
        let _ = commands.send(Command::Setting { key: key.to_string(), value: value.to_string() });
    });
    let commands = command_tx;
    ui.on_update_now(move || {
        let _ = commands.send(Command::Update);
    });

    // Move to the target monitor first, then size: Windows rescales a window that changes monitor.
    if let Some((x, y)) = value(&args, "--pos").and_then(|v| pair(v, ',')) {
        ui.window().set_position(slint::PhysicalPosition::new(x, y));
    }
    let resize = slint::Timer::default();
    if let Some((w, h)) = value(&args, "--size").and_then(|v| pair(v, 'x')) {
        let weak = ui.as_weak();
        resize.start(slint::TimerMode::SingleShot, Duration::from_millis(400), move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().set_size(slint::PhysicalSize::new(w.max(1) as u32, h.max(1) as u32));
            }
        });
    }

    let quit = slint::Timer::default();
    if let Some(seconds) = exit_after {
        quit.start(slint::TimerMode::SingleShot, Duration::from_secs(seconds), || {
            let _ = slint::quit_event_loop();
        });
    }

    ui.run()?;
    Ok(())
}
