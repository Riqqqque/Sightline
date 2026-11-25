//! Updating from the project's GitHub releases.
//!
//! What it does, and no more:
//! - Asks GitHub which release is the newest. One request, no account, nothing about the user or
//!   the game in it.
//! - When the user says so, downloads that release's `Sightline.exe` and its published SHA-256,
//!   checks one against the other, swaps the file in next to the running one, and restarts.
//! - Only ever talks to this project's own release addresses, over HTTPS.
//!
//! It never updates by itself: the download starts from a click in Settings.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where releases are published.
pub const REPO: &str = "Riqqqque/Sightline";
const EXE: &str = "Sightline.exe";
const SUM: &str = "Sightline.exe.sha256";
/// Left behind by the swap and removed at the next start.
const OLD: &str = "Sightline.old.exe";
const NEW: &str = "Sightline.update";
/// Nothing larger than this is accepted as an update.
const MAX_BYTES: usize = 100 * 1024 * 1024;

pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A release newer than the running app.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Release {
    pub version: String,
    exe_url: String,
    sum_url: String,
}

#[derive(Deserialize)]
struct Latest {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    browser_download_url: String,
}

/// "v0.3.1" or "0.3.1" as numbers. Anything else is not a version.
fn parse(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let triple = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    parts.next().is_none().then_some(triple)
}

pub fn newer(latest: &str, running: &str) -> bool {
    matches!((parse(latest), parse(running)), (Some(a), Some(b)) if a > b)
}

/// Only this project's own release files are ever downloaded.
fn trusted(url: &str) -> bool {
    url.starts_with(&format!("https://github.com/{REPO}/releases/download/"))
}

fn release_from(latest: Latest, running: &str) -> Option<Release> {
    if !newer(&latest.tag_name, running) {
        return None;
    }
    let url = |name: &str| latest.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());
    let (exe_url, sum_url) = (url(EXE)?, url(SUM)?);
    (trusted(&exe_url) && trusted(&sum_url))
        .then(|| Release { version: latest.tag_name.trim_start_matches('v').to_string(), exe_url, sum_url })
}

/// The newest release, if it is newer than `running` and has both files. `Ok(None)` means up to
/// date (or nothing published yet).
pub async fn check(http: &reqwest::Client, running: &str) -> Result<Option<Release>, &'static str> {
    let resp = http
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| "could not reach GitHub")?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        403 | 429 => return Err("GitHub asked to slow down; try again later"),
        _ => return Err("GitHub did not answer as expected"),
    }
    let bytes = resp.bytes().await.map_err(|_| "could not reach GitHub")?;
    let latest: Latest = serde_json::from_slice(&bytes).map_err(|_| "GitHub did not answer as expected")?;
    Ok(release_from(latest, running))
}

async fn fetch(http: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, &'static str> {
    if !trusted(url) {
        return Err("the update is not from this project's releases");
    }
    let resp = http.get(url).timeout(Duration::from_secs(180)).send().await.map_err(|_| "the download did not start")?;
    if !resp.status().is_success() {
        return Err("the download was refused");
    }
    if resp.content_length().is_some_and(|n| n as usize > limit) {
        return Err("the download is larger than an update should be");
    }
    let bytes = resp.bytes().await.map_err(|_| "the download broke off")?;
    if bytes.len() > limit {
        return Err("the download is larger than an update should be");
    }
    Ok(bytes.to_vec())
}

/// SHA-256 through Windows' own implementation: no extra code to carry or to trust.
pub fn sha256(data: &[u8]) -> Option<[u8; 32]> {
    use windows_sys::Win32::Security::Cryptography::BCryptHash;
    // The system's ready-made SHA-256 provider (BCRYPT_SHA256_ALG_HANDLE).
    let algorithm = 0x41usize as *mut core::ffi::c_void;
    let mut out = [0u8; 32];
    let length = u32::try_from(data.len()).ok()?;
    // SAFETY: the input and output pointers are valid for the lengths passed with them.
    let status = unsafe { BCryptHash(algorithm, std::ptr::null(), 0, data.as_ptr(), length, out.as_mut_ptr(), 32) };
    (status >= 0).then_some(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The 64 hex digits at the start of a checksum file ("<digest>  Sightline.exe").
fn published_sum(text: &str) -> Option<String> {
    let digest = text.split_whitespace().next()?.to_ascii_lowercase();
    (digest.len() == 64 && digest.bytes().all(|c| c.is_ascii_hexdigit())).then_some(digest)
}

/// True when the file is a Windows program and its SHA-256 is the published one.
fn verified(exe: &[u8], sum_file: &str) -> bool {
    let Some(want) = published_sum(sum_file) else { return false };
    exe.len() > 1024 * 1024 && exe.starts_with(b"MZ") && sha256(exe).is_some_and(|got| hex(&got) == want)
}

fn folder() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// Puts the new program where the running one is. The running file can be renamed while it
/// runs, so it steps aside as `Sightline.old.exe` and is removed at the next start.
fn swap(dir: &Path, running: &Path, bytes: &[u8]) -> Result<(), &'static str> {
    let (new, old) = (dir.join(NEW), dir.join(OLD));
    std::fs::write(&new, bytes).map_err(|_| "this folder cannot be written to")?;
    let _ = std::fs::remove_file(&old);
    if std::fs::rename(running, &old).is_err() {
        let _ = std::fs::remove_file(&new);
        return Err("the running program could not be moved aside");
    }
    if std::fs::rename(&new, running).is_err() {
        // Put things back as they were.
        let _ = std::fs::rename(&old, running);
        let _ = std::fs::remove_file(&new);
        return Err("the new program could not be put in place");
    }
    Ok(())
}

/// Downloads the release and checks it against its published checksum. Returns the program only
/// if the check passes.
async fn download(http: &reqwest::Client, release: &Release) -> Result<Vec<u8>, &'static str> {
    let sum = fetch(http, &release.sum_url, 4096).await?;
    let exe = fetch(http, &release.exe_url, MAX_BYTES).await?;
    if !verified(&exe, &String::from_utf8_lossy(&sum)) {
        return Err("the download does not match its published checksum; nothing was changed");
    }
    Ok(exe)
}

/// Downloads the release, checks it, and swaps it in. The caller restarts afterwards. Nothing on
/// disk changes unless the check passes.
pub async fn install(http: &reqwest::Client, release: &Release) -> Result<(), &'static str> {
    let running = std::env::current_exe().map_err(|_| "the running program could not be found")?;
    let dir = folder().ok_or("the running program could not be found")?;
    let exe = download(http, release).await?;
    swap(&dir, &running, &exe)
}

/// Starts the program again and ends this one.
pub fn restart() -> ! {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0)
}

/// Removes what the last update left next to the program.
pub fn tidy() {
    if let Some(dir) = folder() {
        let _ = std::fs::remove_file(dir.join(OLD));
        let _ = std::fs::remove_file(dir.join(NEW));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("v0.2.0", "0.2.0"));
        assert!(!newer("0.1.9", "0.2.0"));
        // Anything that is not three numbers is never "newer".
        assert!(!newer("latest", "0.2.0"));
        assert!(!newer("v1.0", "0.2.0"));
        assert!(!newer("v9.9.9-beta", "0.2.0"));
        assert!(!newer("", "0.2.0"));
    }

    #[test]
    fn only_this_projects_release_files_are_trusted() {
        assert!(trusted("https://github.com/Riqqqque/Sightline/releases/download/v0.3.0/Sightline.exe"));
        assert!(!trusted("http://github.com/Riqqqque/Sightline/releases/download/v0.3.0/Sightline.exe"));
        assert!(!trusted("https://github.com/someone-else/Sightline/releases/download/v9/Sightline.exe"));
        assert!(!trusted("https://github.com.evil.example/Riqqqque/Sightline/releases/download/v9/Sightline.exe"));
        assert!(!trusted("https://example.com/Sightline.exe"));
    }

    fn latest(tag: &str, assets: &[(&str, &str)]) -> Latest {
        Latest {
            tag_name: tag.to_string(),
            assets: assets.iter().map(|(n, u)| Asset { name: n.to_string(), browser_download_url: u.to_string() }).collect(),
        }
    }

    #[test]
    fn a_release_counts_only_when_newer_complete_and_from_here() {
        let base = "https://github.com/Riqqqque/Sightline/releases/download/v99.0.0/";
        let (exe, sum) = (format!("{base}Sightline.exe"), format!("{base}Sightline.exe.sha256"));
        let good = release_from(latest("v99.0.0", &[("Sightline.exe", &exe), ("Sightline.exe.sha256", &sum)]), "0.2.0").unwrap();
        assert_eq!(good.version, "99.0.0");
        // Not newer.
        assert!(release_from(latest("v0.0.1", &[("Sightline.exe", &exe), ("Sightline.exe.sha256", &sum)]), "0.2.0").is_none());
        // The same release is offered to an older install and not to a newer one.
        assert!(release_from(latest("v0.3.0", &[("Sightline.exe", &exe), ("Sightline.exe.sha256", &sum)]), "0.2.9").is_some());
        assert!(release_from(latest("v0.3.0", &[("Sightline.exe", &exe), ("Sightline.exe.sha256", &sum)]), "0.3.0").is_none());
        // No checksum published: not offered.
        assert!(release_from(latest("v99.0.0", &[("Sightline.exe", &exe)]), "0.2.0").is_none());
        // Files that point somewhere else: not offered.
        let elsewhere = "https://example.com/Sightline.exe";
        assert!(release_from(latest("v99.0.0", &[("Sightline.exe", elsewhere), ("Sightline.exe.sha256", &sum)]), "0.2.0").is_none());
    }

    #[test]
    fn sha256_matches_the_standard_test_value() {
        let digest = sha256(b"abc").unwrap();
        assert_eq!(hex(&digest), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn a_download_is_accepted_only_with_its_own_checksum() {
        let mut exe = vec![0u8; 2 * 1024 * 1024];
        exe[..2].copy_from_slice(b"MZ");
        let sum = format!("{}  Sightline.exe\n", hex(&sha256(&exe).unwrap()));
        assert!(verified(&exe, &sum));
        assert!(verified(&exe, &sum.to_uppercase()), "upper-case digests are the same digest");
        // One changed byte.
        let mut tampered = exe.clone();
        tampered[4096] ^= 1;
        assert!(!verified(&tampered, &sum));
        // Not a program, too small, or no usable checksum.
        let mut not_exe = exe.clone();
        not_exe[0] = b'X';
        assert!(!verified(&not_exe, &format!("{}  x", hex(&sha256(&not_exe).unwrap()))));
        assert!(!verified(b"MZ", &format!("{}  x", hex(&sha256(b"MZ").unwrap()))));
        assert!(!verified(&exe, ""));
        assert!(!verified(&exe, "not-a-digest  Sightline.exe"));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sightline-update-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_swap_puts_the_new_program_in_place_and_keeps_the_old_one_aside() {
        let dir = scratch("swap");
        let running = dir.join("Sightline.exe");
        std::fs::write(&running, b"old program").unwrap();
        swap(&dir, &running, b"new program").unwrap();
        assert_eq!(std::fs::read(&running).unwrap(), b"new program");
        assert_eq!(std::fs::read(dir.join(OLD)).unwrap(), b"old program");
        assert!(!dir.join(NEW).exists(), "no half-finished file is left");
        // A second update replaces the copy that was set aside by the first.
        swap(&dir, &running, b"newer program").unwrap();
        assert_eq!(std::fs::read(&running).unwrap(), b"newer program");
        assert_eq!(std::fs::read(dir.join(OLD)).unwrap(), b"new program");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_swap_that_cannot_happen_changes_nothing() {
        let dir = scratch("noswap");
        // There is no running program at this path, so it cannot be moved aside.
        let running = dir.join("Sightline.exe");
        assert!(swap(&dir, &running, b"new program").is_err());
        assert!(!running.exists() && !dir.join(NEW).exists() && !dir.join(OLD).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Asks the real GitHub for the newest release as an old install would, downloads both
    /// files and checks one against the other. Nothing is installed. Run with
    /// `cargo test -- --ignored real_update --nocapture`.
    #[test]
    #[ignore]
    fn real_update_download_verifies() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let http = crate::source::local::remote_http().unwrap();
        rt.block_on(async {
            match check(&http, current()).await {
                Ok(Some(release)) => println!("this build ({}) would be offered {}", current(), release.version),
                Ok(None) => println!("this build ({}) is up to date", current()),
                Err(why) => println!("check failed: {why}"),
            }
            let release = check(&http, "0.0.1").await.expect("check").expect("an old install is offered the newest release");
            println!("an install at 0.0.1 is offered {}", release.version);
            let exe = download(&http, &release).await.expect("download and checksum");
            println!("downloaded {} bytes, checksum matches, starts with MZ: {}", exe.len(), exe.starts_with(b"MZ"));
        });
    }
}
