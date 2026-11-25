# Renderer check

Status: measured 2026-11-23. Verdict: **Slint's software renderer passes. No GPU renderer needed.**
One finding needs a decision: memory at full 4K width.

## Setup

- Test app: `spikes/renderer-test` (throwaway, not app code). One screen, 10 player cards in two
  columns, dummy data, 14 placeholder icons generated locally, header with a rank-gap bar.
- Slint 1.19.0, winit backend, **software renderer only**. `default-features = false`; the build
  links no FemtoVG, OpenGL, wgpu or Skia (checked with `cargo tree`).
- Rust 1.100.0. Release profile exactly as specified: `lto="fat"`, `codegen-units=1`,
  `opt-level="s"`, `strip=true`, `panic="abort"`. Binary: 7.3 MB.
- Machine: Ryzen 7 9800X3D. Window on the second monitor: 3840x2160 at 150% scale, 120 Hz.
  The window's real client size was confirmed from a DPI-aware process for each run.
- Three window widths, all 2112 px tall (physical pixels):
  narrow 720 (480 logical), half 1920 (1280 logical), full 3840 (2560 logical).
- Three modes: idle (static screen), shimmer (all ten cards showing animated skeleton loaders, the
  realistic worst case), full repaint (every pixel redrawn every frame, a stress case).
- Frames are counted by a probe that only the renderer evaluates, once per drawn frame.
  CPU is process CPU time over an 8 s window, sampled from outside the process.

## Results

| Width | Mode | Private Bytes | Working Set | CPU, one core | CPU, whole machine | Frames/s | CPU ms per frame | GPU |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Narrow 720 | idle | 10.0 MB | 25.4 MB | 0.00% | 0.00% | 0 (nothing to draw) | n/a | 0% |
| Narrow 720 | shimmer | 9.5 MB | 24.9 MB | 8.7% | 0.5% | 122.6 | 0.71 | 0% |
| Narrow 720 | full repaint | 9.5 MB | 24.9 MB | 20.9% | 1.3% | 122.7 | 1.70 | 0% |
| Half 1920 | idle | 19.9 MB | 35.3 MB | 0.00% | 0.00% | 0 | n/a | 0% |
| Half 1920 | shimmer | 19.3 MB | 34.7 MB | 16.3% | 1.0% | 122.8 | 1.33 | 0% |
| Half 1920 | full repaint | 19.3 MB | 34.8 MB | 36.1% | 2.3% | 122.9 | 2.94 | 0% |
| Full 3840 | idle | 35.3 MB | 50.7 MB | 0.00% | 0.00% | 0 | n/a | 0% |
| Full 3840 | shimmer | 34.8 MB | 50.2 MB | 19.6% | 1.2% | 122.7 | 1.59 | 0% |
| Full 3840 | full repaint | 34.7 MB | 50.2 MB | 57.8% | 3.6% | 122.6 | 4.72 | 0% |

Start: 209 to 232 ms from process launch to the first drawn frame, of which 15 to 19 ms is inside
`main`. These are warm starts (the binary was already in the file cache). A true cold start after a
reboot was not measured.

## Reading the numbers

- **Frame rate.** Slint paces redraws to the monitor's refresh rate, so about 122 frames/s is the
  cap on this 120 Hz monitor, and every mode at every width reached it. The slowest case costs
  4.72 ms of CPU per frame against a 16.7 ms budget at 60 fps: 3.5x headroom. On a 60 Hz monitor
  the CPU percentages above would roughly halve.
- **Idle.** A static screen draws nothing: 0 frames, 0% CPU, 0% GPU.
- **Animation cost.** Skeleton shimmer on all ten cards costs 9 to 20% of one core (about 1% of the
  machine) at 120 Hz. It only runs while data is loading, and capping animations at 60 fps halves
  it.
- **GPU.** The process had no GPU engine activity in any run.
- **Memory is the frame buffer.** Private Bytes follow the window's pixel count almost exactly:
  4 bytes per physical pixel (6 MB narrow, 16 MB half, 32 MB full) on top of about 3.5 MB for
  everything else in this test. Working Set adds about 15 MB of shared system DLL pages that every
  windowed process maps.

## The finding that needs a decision

The target is under 40 MB in-match. Projected for the real app, adding the data layer (HTTP client,
TLS, SQLite with a 4 MB page cache, decoded images) at roughly 8 to 10 MB:

| Window | Private Bytes, projected | Working Set, projected |
| --- | --- | --- |
| Narrow, 480 logical wide | about 20 MB | about 35 MB |
| Half of the 4K monitor | about 30 MB | about 45 MB |
| Full 4K monitor | about 45 MB | about 60 MB |

The projection is an estimate; the data layer does not exist yet. Private Bytes is the honest
figure for what the app costs. On that measure narrow and half width meet the target and the full
4K width does not, purely because a 3840x2112 frame buffer is 32 MB on its own. On Working Set only
the narrow width is under 40 MB.

Options:

1. **Cap the content width** (recommended). The two-column layout does not need more than about
   1280 logical px. Cap the window's maximum width there; "full" density then means "as wide as the
   layout uses". The app stays near 30 MB private at any size.
2. Accept about 45 MB private when the window is maximised on the 4K monitor.
3. Measure Slint's GPU renderer for comparison. It moves the frame buffer to the GPU but loads a
   graphics driver into the process, which usually costs more RAM than it saves and breaks the
   "GPU near 0%" target. The spec says to consider this only if 60 fps cannot be held, and it can,
   so this was not measured.

## Things learned for the real app

- A window moved to a monitor with a different scale gets rescaled by Windows. Position the window
  first, then size it.
- Density switching driven by window width must not feed back into the window's own size limits.
  Slint reports this as a binding loop. Fix: explicit geometry on the root layout and explicit
  minimum sizes on the window.
- The software renderer has no rendering-notifier hook, so frame timing has to be measured with a
  render-time probe like the one in the test.
- Slint 1.19 added rounded clipping and drop shadows to the software renderer. Rotation and scaling
  transforms are still unsupported there; the design does not need them.

## Reproduce

```powershell
cd spikes\renderer-test
cargo build --release
powershell -NoProfile -ExecutionPolicy Bypass -File .\measure.ps1 | Format-Table -AutoSize
```

`measure.ps1` takes `-OriginX`, `-OriginY` (top-left of the target monitor in physical pixels),
`-Sizes` and `-Modes`. Each run opens the test window for 16 seconds.
