# Toolchain and dependencies

Status: versions confirmed against crates.io and the release pages on 2026-11-23.
Exact versions are pinned with `=` in `Cargo.toml`, and `Cargo.lock` is kept.

The whole set below was resolved together in `spikes/deps-check` (`cargo tree -e features`), which
proves the versions are compatible and every feature name exists. The resolved TLS stack is
Schannel only (`native-tls` 0.2.18, `schannel` 0.1.30, `hyper-tls`, `tokio-native-tls`): no rustls,
ring, aws-lc or OpenSSL, and no HTTP/2. Only Slint has been compiled and run so far.

## Toolchain

| Tool | Version | Notes |
| --- | --- | --- |
| Rust | 1.100.0 (2026-11-09), `x86_64-pc-windows-msvc` | Current stable. Pinned in `rust-toolchain.toml`. Installed side by side; the machine's default toolchain (1.94.1) was not changed. Slint 1.19 needs Rust 1.95 or newer. |
| Slint | 1.19.0 (2026-11-16) | Current release on crates.io. A 1.19.1 entry is in the changelog but was not published when checked. |

## Direct dependencies

| Crate | Version | Features | Why it is needed |
| --- | --- | --- | --- |
| `slint` | =1.19.0 | `std`, `backend-winit`, `renderer-software`, `compat-1-2`; default features off | The UI. Defaults are off to keep the OpenGL renderer, accessibility tree and tray icon out of the binary. Measured in `renderer-check.md`. |
| `slint-build` | =1.19.0 | build dependency | Compiles `.slint` files at build time. |
| `winresource` | =0.1.34 | build dependency; default features off | Puts the icon and the file description into the exe, so Explorer, pinned shortcuts and Task Manager show them. Runs the Windows SDK resource compiler at build time; nothing of the crate ends up in the binary. Default features are off (the default adds a TOML parser); adding it this way put exactly one new package in `Cargo.lock`. Added 2026-11-24. |
| `tokio` | =1.54.1 | `rt`, `net`, `time`, `sync` | The `current_thread` runtime for all I/O, on one worker thread. No `macros`, no multi-thread scheduler. |
| `reqwest` | =0.13.5 | `native-tls`, `json`; default features off | The shared HTTP client with connection reuse. `native-tls` selects Windows Schannel. Default features are off because the default TLS is now rustls, and HTTP/2, charset decoding and system proxy lookup are not needed. |
| `tokio-tungstenite` | =0.30.0 | `connect`, `handshake`, `native-tls` | The local Riot Client websocket, which is what makes the state machine event-driven instead of polling. |
| `native-tls` | =0.2.18 | none | Needed directly to build the TLS connector for the localhost websocket. Already in the tree through `reqwest`. |
| `futures-util` | =0.3.34 | `sink`, `std`; default features off | Reading from and writing to the websocket stream. Already in the tree. |
| `rusqlite` | =0.43.0 | `bundled` | SQLite with its own compiled copy, (`libsqlite3-sys` 0.40.0), so the app does not depend on a system DLL. |
| `serde` | =1.0.229 | `derive` | Typed JSON decoding. |
| `serde_json` | =1.0.154 | none | JSON parsing, straight from the response bytes into small structs. |
| `base64` | =0.23.1 | none | Decoding the `private` blob in presence data. Already in the tree through `reqwest`, so it adds nothing. |
| `windows-sys` | =0.61.2 | `Win32_Foundation`, `Win32_System_Threading`, `Win32_System_ProcessStatus`, `Win32_System_Registry`, `Win32_Security`, `Win32_Security_Cryptography` | Trimming the working set when the game closes, and reading the `HENRIKDEV_API_KEY` user variable from where Windows stores it (a process started from another app does not always inherit it), and checking a downloaded update against its published SHA-256 with Windows' own hash function. Already in the tree through `tokio` and `winit`. |

Twelve direct dependencies, eleven at runtime. Five of them are already pulled in by another one.

`rusqlite` and `windows-sys` were added on 2026-11-24 with steps 2 to 8; the manifest now holds
all of the crates above. Bundled SQLite compiled with the MSVC toolchain here without changes. Tests add `tokio`'s `test-util` feature (paused clock) as a
dev-dependency; it is not part of the release build.

## Deliberately not used

| Left out | Instead |
| --- | --- |
| `image`, `fast_image_resize` | Slint already links PNG and JPEG decoders. Images are downscaled once with a small box filter and cached at display size. |
| `chrono`, `time` | Timestamps are Unix seconds. Session reset is a gap in seconds, so no calendar or time zone is needed. |
| `anyhow`, `thiserror` | One hand-written error enum. |
| `log`, `tracing` | A fixed-size in-memory status ring shown in Settings. Nothing is written to disk, which also guarantees no token ever lands in a log file. |
| `toml`, `config`, `dirs` | Settings live in the SQLite `settings` table. The data folder comes from the `LOCALAPPDATA` variable. |
| `uuid` | PUUIDs and match ids are parsed to 16 bytes with a few lines of code. |
| `notify` | The lockfile is checked with a cheap timer while the game is closed. |
| An API wrapper crate for HenrikDev | None is maintained, and the app uses five endpoints. |
| HTTP/2, gzip, cookies, proxies | Not needed by either API. |

## Licences

| Component | Licence | Obligation |
| --- | --- | --- |
| Slint | Royalty-free Desktop, Mobile and Web Applications License 2.0 (one of three options) | Show the `AboutSlint` widget in an About screen. The licence ties this to distribution; Sightline is not distributed, but the widget goes in anyway. |
| VALORANT-rank-yoinker | ISC | Notice kept in `NOTICE`. It is used as a reference for endpoints; no code is copied. |
| Rust crates above | MIT or MIT/Apache-2.0 | None for a private build. |
| SQLite (via `rusqlite` bundled) | Public domain | None. |
| Font | To choose. Inter (SIL OFL 1.1) is the suggestion: one variable file, strong digits, free to bundle. | Keep the OFL text next to the font file. Not downloaded yet. |

## Still to confirm at first build of the real app

- That bundled SQLite compiles with the MSVC toolchain here (resolved, not yet compiled).
- `tokio-tungstenite`'s API for passing a custom `native-tls` connector to a `wss://127.0.0.1` URL.
- Whether the local Riot Client certificate can be trusted by pinning Riot's own root certificate
  instead of turning verification off for localhost. If not, verification is relaxed for
  `127.0.0.1` only, on a separate client, and never for any remote host.
