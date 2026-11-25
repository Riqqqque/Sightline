// Phase 0 renderer check: one dummy lobby screen on Slint's software renderer.
//
// Usage: renderer-test [--mode 0|1|2] [--size WxH] [--pos X,Y] [--seconds N]
//   mode 0  static screen (idle)
//   mode 1  skeleton shimmer on all 10 cards (realistic worst case while loading)
//   mode 2  whole window repainted every frame (stress)
// Sizes and positions are physical pixels.

slint::include_modules!();

use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn pair(s: &str, sep: char) -> Option<(i32, i32)> {
    let (a, b) = s.split_once(sep)?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn unix_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

fn players(seed: usize) -> ModelRc<Player> {
    const NAMES: [&str; 10] = [
        "Rique", "nightowl", "Sova Main 2019", "kz", "TenZ fan", "wholesome jett", "abc", "lurkmaster",
        "one tap only", "quiet",
    ];
    const TAGS: [&str; 10] = ["EUW", "0001", "sova", "kzkz", "NA1", "meow", "123", "lurk", "tap", "shh"];
    let list: Vec<Player> = (0..5)
        .map(|i| {
            let n = seed * 5 + i;
            let wl: Vec<bool> = (0..5).map(|k| (n + k * 3) % 3 != 0).collect();
            let mains: Vec<i32> = (0..3).map(|k| ((n + k * 2) % 6) as i32).collect();
            Player {
                name: SharedString::from(NAMES[n]),
                tag: SharedString::from(TAGS[n]),
                hidden: n == 7,
                level: 20 + (n as i32 * 37) % 400,
                agent: (n % 6) as i32,
                rank: ((n * 3 + 2) % 8) as i32,
                peak: ((n * 3 + 4) % 8) as i32,
                rr: ((n * 23) % 100) as i32,
                kd: SharedString::from(format!("{:.2}", 0.7 + (n as f32) * 0.09)),
                hs: SharedString::from(format!("{}", 14 + (n * 3) % 20)),
                adr: SharedString::from(format!("{}", 110 + (n * 13) % 80)),
                wl: ModelRc::new(VecModel::from(wl)),
                mains: ModelRc::new(VecModel::from(mains)),
                party: if n == 0 || n == 1 { 1 } else if n == 5 || n == 6 || n == 8 { 2 } else { 0 },
                smurf: if n == 6 { 2 } else if n == 3 { 1 } else { 0 },
            }
        })
        .collect();
    ModelRc::new(VecModel::from(list))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let t0 = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    let mode: i32 = arg(&args, "--mode").and_then(|s| s.parse().ok()).unwrap_or(0);
    let seconds: u64 = arg(&args, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(0);

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()?;

    let ui = TestWindow::new()?;
    ui.set_allies(players(0));
    ui.set_enemies(players(1));
    ui.set_mode(mode);
    debug_assert_eq!(ui.get_allies().row_count(), 5);

    // Move to the target monitor first, then size once the window has picked up that monitor's
    // scale factor. Sizing before the move gets rescaled by Windows on the DPI change.
    let size = arg(&args, "--size").and_then(|s| pair(&s, 'x'));
    if let Some((x, y)) = arg(&args, "--pos").and_then(|s| pair(&s, ',')) {
        ui.window().set_position(slint::PhysicalPosition::new(x, y));
    }
    let size_timer = Timer::default();
    if let Some((w, h)) = size {
        let weak = ui.as_weak();
        size_timer.start(TimerMode::SingleShot, Duration::from_millis(400), move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().set_size(slint::PhysicalSize::new(w as u32, h as u32));
            }
        });
    }

    let frames = Rc::new(Cell::new(0u32));
    let first = Rc::new(Cell::new(true));
    ui.on_probe({
        let frames = frames.clone();
        let first = first.clone();
        move |_| {
            frames.set(frames.get() + 1);
            if first.replace(false) {
                println!("first_frame unix_ms={} since_main_ms={:.1}", unix_ms(), t0.elapsed().as_secs_f64() * 1000.0);
            }
            slint::Color::from_argb_u8(0, 0, 0, 0)
        }
    });

    let fps_timer = Timer::default();
    fps_timer.start(TimerMode::Repeated, Duration::from_secs(1), {
        let frames = frames.clone();
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                let s = ui.window().size();
                println!("fps={} size={}x{} scale={}", frames.replace(0), s.width, s.height, ui.window().scale_factor());
            }
        }
    });

    let quit_timer = Timer::default();
    if seconds > 0 {
        quit_timer.start(TimerMode::SingleShot, Duration::from_secs(seconds), || {
            let _ = slint::quit_event_loop();
        });
    }

    println!("ready unix_ms={} since_main_ms={:.1}", unix_ms(), t0.elapsed().as_secs_f64() * 1000.0);
    ui.run()?;
    Ok(())
}
