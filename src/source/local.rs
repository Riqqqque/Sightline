//! The running game client: lockfile, local API, local event socket, and the two Riot endpoints
//! that say who is in my lobby or match.
//!
//! Rules this file keeps:
//! - Read-only. Only GET requests, plus the one name lookup Riot exposes as PUT.
//! - Tokens and the lockfile password live in memory only, are never printed or written, and are
//!   wiped when the game goes away.
//! - Certificate checks are relaxed for 127.0.0.1 only (the client's certificate is self-signed).
//! - Nothing is done while the Riot Client is not running, apart from looking for the lockfile.

use super::assets::{self, Catalog};
use super::wire::{self, Region};
use super::{DataSource, SourceError};
use crate::model::{Name, Phase, PlayerId, Roster, Signal, Uuid};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

/// How often the lockfile is looked for while nothing is running.
const CLOSED_POLL: Duration = Duration::from_secs(5);
const SOCKET_RETRY: Duration = Duration::from_secs(3);

/// Required by Riot's lobby endpoints. Describes the platform as "PC, Windows".
const CLIENT_PLATFORM: &str = concat!(
    "ew0KCSJwbGF0Zm9ybVR5cGUiOiAiUEMiLA0KCSJwbGF0Zm9ybU9TIjog",
    "IldpbmRvd3MiLA0KCSJwbGF0Zm9ybU9TVmVyc2lvbiI6ICIxMC4wLjE5",
    "MDQyLjEuMjU2LjY0Yml0IiwNCgkicGxhdGZvcm1DaGlwc2V0IjogIlVua25vd24iDQp9"
);

fn wipe(s: &mut String) {
    let mut bytes = std::mem::take(s).into_bytes();
    bytes.fill(0);
    std::hint::black_box(&bytes);
}

/// Where the local API listens. No `Debug`: the password must never reach a log or a panic message.
#[derive(Clone)]
struct Lockfile {
    port: u16,
    password: String,
}

impl Drop for Lockfile {
    fn drop(&mut self) {
        wipe(&mut self.password);
    }
}

fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

fn parse_lockfile(text: &str) -> Option<Lockfile> {
    // name:pid:port:password:protocol
    let mut parts = text.trim().split(':');
    let (_name, _pid) = (parts.next()?, parts.next()?);
    let port: u16 = parts.next()?.parse().ok()?;
    let password = parts.next()?;
    let ok = !password.is_empty() && password.len() <= 128 && password.bytes().all(|c| c.is_ascii_graphic());
    (port != 0 && ok).then(|| Lockfile { port, password: password.to_string() })
}

fn read_lockfile() -> Option<Lockfile> {
    let path = local_app_data()?.join(r"Riot Games\Riot Client\Config\lockfile");
    parse_lockfile(&std::fs::read_to_string(path).ok()?)
}

fn read_region() -> Option<Region> {
    let path = local_app_data()?.join(r"VALORANT\Saved\Logs\ShooterGame.log");
    let mut reader = BufReader::new(std::fs::File::open(path).ok()?);
    let mut buf = Vec::with_capacity(512);
    let lines = std::iter::from_fn(move || {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(n) if n > 0 => Some(String::from_utf8_lossy(&buf).into_owned()),
            _ => None,
        }
    });
    wire::parse_region(lines)
}

/// Which HenrikDev region the account plays in, from the game's own log: `na`, `eu`, `ap`, `kr`,
/// `latam` or `br`. The log names the game server group as e.g. `na-1`.
pub fn read_affinity() -> Option<String> {
    let region = read_region()?;
    let name = region.glz.split('-').next().unwrap_or("").to_ascii_lowercase();
    matches!(name.as_str(), "na" | "eu" | "ap" | "kr" | "latam" | "br").then_some(name)
}

fn basic_auth(lock: &Lockfile) -> String {
    let mut raw = format!("riot:{}", lock.password);
    let out = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(&raw));
    wipe(&mut raw);
    out
}

/// The only way a local URL is built: the host is fixed.
fn local_url(lock: &Lockfile, path: &str) -> String {
    format!("https://127.0.0.1:{}{}", lock.port, path)
}

/// Client for the Riot Client's own API. Loopback only, so its self-signed certificate is accepted
/// and no proxy is ever used.
fn loopback_http() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .tls_danger_accept_invalid_hostnames(true)
        .no_proxy()
        .https_only(true)
        .http1_only()
        .pool_max_idle_per_host(1)
        .timeout(Duration::from_secs(5))
        .build()
        .ok()
}

/// Sent to remote hosts. Plain and generic: it does not claim to be the game.
const USER_AGENT: &str = "desktop-companion/0.1";

/// Client for everything that is not this machine. Full certificate verification.
pub(crate) fn remote_http() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .https_only(true)
        .http1_only()
        .pool_max_idle_per_host(2)
        .pool_idle_timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .ok()
}

async fn local_get<T: DeserializeOwned>(
    http: &reqwest::Client,
    lock: &Lockfile,
    path: &str,
    what: &'static str,
) -> Result<T, SourceError> {
    let resp = http
        .get(local_url(lock, path))
        .header("Authorization", basic_auth(lock))
        .send()
        .await
        .map_err(|_| SourceError::ClientUnavailable)?;
    if !resp.status().is_success() {
        return Err(SourceError::NotReady);
    }
    let bytes = resp.bytes().await.map_err(|_| SourceError::ClientUnavailable)?;
    serde_json::from_slice(&bytes).map_err(|_| SourceError::Shape(what))
}

/// What the watcher and the request side both need to know.
#[derive(Default)]
struct Shared {
    lock: Mutex<Option<Lockfile>>,
    me: Mutex<Option<Uuid>>,
    seen: Mutex<Seen>,
}

/// Last things observed, for the status line. No identities.
#[derive(Default, Clone)]
struct Seen {
    /// How far the watcher got: which link is up.
    stage: &'static str,
    frames: u32,
    unreadable: u32,
    loop_state: String,
    party_state: String,
    layout: &'static str,
    roster: String,
}

impl Shared {
    fn lock(&self) -> Option<Lockfile> {
        self.lock.lock().ok()?.clone()
    }
    fn me(&self) -> Option<Uuid> {
        *self.me.lock().ok()?
    }
    fn stage(&self, stage: &'static str) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.stage = stage;
        }
    }

    fn count_frame(&self, readable: bool) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.frames = seen.frames.saturating_add(1);
            if !readable {
                seen.unreadable = seen.unreadable.saturating_add(1);
            }
        }
    }

    fn forget(&self) {
        if let Ok(mut l) = self.lock.lock() {
            *l = None;
        }
        if let Ok(mut m) = self.me.lock() {
            *m = None;
        }
        if let Ok(mut s) = self.seen.lock() {
            *s = Seen::default();
        }
    }
}

async fn resolve_me(http: &reqwest::Client, lock: &Lockfile, shared: &Shared) -> Option<Uuid> {
    if let Some(me) = shared.me() {
        return Some(me);
    }
    let session: wire::ChatSession = local_get(http, lock, "/chat/v1/session", "chat session").await.ok()?;
    let me = Uuid::parse(&session.puuid)?;
    if let Ok(mut slot) = shared.me.lock() {
        *slot = Some(me);
    }
    Some(me)
}

/// Sends a signal only when it differs from the last one. Returns false once nobody listens.
async fn emit(tx: &mpsc::Sender<Signal>, last: &mut Option<Signal>, signal: Signal) -> bool {
    if last.as_ref() == Some(&signal) {
        return true;
    }
    *last = Some(signal.clone());
    tx.send(signal).await.is_ok()
}

fn note_presence(shared: &Shared, info: &wire::PresenceInfo) {
    if let Ok(mut seen) = shared.seen.lock() {
        seen.loop_state = info.loop_state.clone();
        seen.party_state = info.party_state.clone();
        seen.layout = if info.nested { "nested" } else { "flat" };
    }
}

/// Watches for the Riot Client and reports game-state changes. Event-driven: while the client is
/// up it waits on the socket; while it is not, it checks for the lockfile every few seconds.
async fn watcher(shared: Arc<Shared>, tx: mpsc::Sender<Signal>) {
    let mut last: Option<Signal> = None;
    loop {
        let Some(lock) = read_lockfile() else {
            shared.forget();
            shared.stage("no Riot Client");
            if !emit(&tx, &mut last, Signal::ClientGone).await {
                return;
            }
            tokio::time::sleep(CLOSED_POLL).await;
            continue;
        };
        if let Ok(mut slot) = shared.lock.lock() {
            *slot = Some(lock.clone());
        }
        shared.stage("Riot Client found, connecting");
        if matches!(last, None | Some(Signal::ClientGone)) && !emit(&tx, &mut last, Signal::ClientOnly).await {
            return;
        }
        if !socket(&lock, &shared, &tx, &mut last).await {
            return;
        }
        // The socket ended: the client is closing, restarting, or was not ready yet.
        tokio::time::sleep(SOCKET_RETRY).await;
    }
}

/// Runs one socket session. Returns false once nobody listens for signals any more.
async fn socket(lock: &Lockfile, shared: &Shared, tx: &mpsc::Sender<Signal>, last: &mut Option<Signal>) -> bool {
    let Some(http) = loopback_http() else { return true };
    let Ok(tls) = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()
    else {
        return true;
    };
    let Ok(mut request) = format!("wss://127.0.0.1:{}", lock.port).into_client_request() else { return true };
    let Ok(auth) = HeaderValue::from_str(&basic_auth(lock)) else { return true };
    request.headers_mut().insert("Authorization", auth);
    let connector = tokio_tungstenite::Connector::NativeTls(tls);
    let Ok((mut ws, _)) = tokio_tungstenite::connect_async_tls_with_config(request, None, true, Some(connector)).await
    else {
        shared.stage("Riot Client found, event socket refused");
        return true;
    };
    if ws.send(Message::text(r#"[5, "OnJsonApiEvent_chat_v4_presences"]"#)).await.is_err() {
        shared.stage("Riot Client found, event socket closed");
        return true;
    }
    shared.stage("event socket connected");
    // The round score is sent on its own, and only when it changes.
    let mut last_score: Option<(u32, u32)> = None;

    // Where things stand right now (the app may have been started mid-match).
    if resolve_me(&http, lock, shared).await.is_none() {
        shared.stage("event socket connected, not signed in yet");
    }
    if let Some(me) = resolve_me(&http, lock, shared).await {
        if let Ok(list) = local_get::<wire::PresenceList>(&http, lock, "/chat/v4/presences", "presences").await {
            match wire::my_presence(&list, me) {
                Some(info) => {
                    note_presence(shared, &info);
                    if let Some(signal) = info.signal() {
                        if !emit(tx, last, signal).await {
                            return false;
                        }
                    }
                    if let Some((ours, theirs)) = info.score {
                        last_score = info.score;
                        if tx.send(Signal::Score(ours, theirs)).await.is_err() {
                            return false;
                        }
                    }
                }
                None => {
                    if !emit(tx, last, Signal::ClientOnly).await {
                        return false;
                    }
                }
            }
        }
    }

    while let Some(Ok(message)) = ws.next().await {
        let Message::Text(text) = message else { continue };
        if text.len() < 10 {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<wire::Frame>(text.as_str()) else {
            shared.count_frame(false);
            continue;
        };
        shared.count_frame(true);
        let Some(list) = frame.2.data else { continue };
        let Some(me) = resolve_me(&http, lock, shared).await else {
            shared.stage("event socket connected, not signed in yet");
            continue;
        };
        shared.stage("event socket connected");
        let mine = list.presences.iter().any(|p| p.is_valorant() && p.id() == Some(me));
        if !mine {
            continue;
        }
        let mut score = None;
        let signal = if frame.2.event_type == "Delete" {
            Some(Signal::ClientOnly)
        } else {
            wire::my_presence(&list, me).and_then(|info| {
                note_presence(shared, &info);
                score = info.score;
                info.signal()
            })
        };
        if let Some(signal) = signal {
            if !emit(tx, last, signal).await {
                return false;
            }
        }
        if let Some((ours, theirs)) = score.filter(|s| Some(*s) != last_score) {
            last_score = score;
            if tx.send(Signal::Score(ours, theirs)).await.is_err() {
                return false;
            }
        }
    }
    shared.stage("event socket ended, reconnecting");
    true
}

/// The session the running game client holds. Memory only; wiped on drop.
struct Session {
    access: String,
    entitlement: String,
}

impl Drop for Session {
    fn drop(&mut self) {
        wipe(&mut self.access);
        wipe(&mut self.entitlement);
    }
}

pub struct LocalClient {
    shared: Arc<Shared>,
    local: Option<reqwest::Client>,
    remote: Option<reqwest::Client>,
    session: Option<Session>,
    region: Option<Region>,
    /// The lobby or match found for the current phase, so each refresh is one request, not two.
    found: Option<(Phase, Uuid)>,
}

impl LocalClient {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared::default()),
            local: None,
            remote: None,
            session: None,
            region: None,
            found: None,
        }
    }

    fn local(&mut self) -> Result<reqwest::Client, SourceError> {
        if self.local.is_none() {
            self.local = loopback_http();
        }
        self.local.clone().ok_or(SourceError::ClientUnavailable)
    }

    fn remote(&mut self) -> Result<reqwest::Client, SourceError> {
        if self.remote.is_none() {
            self.remote = remote_http();
        }
        self.remote.clone().ok_or(SourceError::Network)
    }

    fn region(&mut self) -> Result<Region, SourceError> {
        if self.region.is_none() {
            self.region = read_region();
        }
        self.region.clone().ok_or(SourceError::Region)
    }

    async fn open_session(&mut self) -> Result<(), SourceError> {
        if self.session.is_some() {
            return Ok(());
        }
        let lock = self.shared.lock().ok_or(SourceError::ClientUnavailable)?;
        let http = self.local()?;
        let mut e: wire::Entitlements = local_get(&http, &lock, "/entitlements/v1/token", "session").await?;
        if e.access_token.is_empty() || e.token.is_empty() {
            return Err(SourceError::NotReady);
        }
        if let (Some(me), Ok(mut slot)) = (Uuid::parse(&e.subject), self.shared.me.lock()) {
            *slot = Some(me);
        }
        self.session =
            Some(Session { access: std::mem::take(&mut e.access_token), entitlement: std::mem::take(&mut e.token) });
        Ok(())
    }

    /// One request to a Riot host with the client's session. Retries once with a fresh session
    /// if the old one was rejected.
    async fn riot<T: DeserializeOwned>(
        &mut self,
        put_body: Option<&[String]>,
        url: &str,
        what: &'static str,
    ) -> Result<T, SourceError> {
        let region = self.region()?;
        let http = self.remote()?;
        for attempt in 0..2 {
            self.open_session().await?;
            let Some(session) = self.session.as_ref() else { return Err(SourceError::NotReady) };
            let request = match put_body {
                Some(body) => http.put(url).json(body),
                None => http.get(url),
            };
            let resp = request
                .bearer_auth(&session.access)
                .header("X-Riot-Entitlements-JWT", &session.entitlement)
                .header("X-Riot-ClientPlatform", CLIENT_PLATFORM)
                .header("X-Riot-ClientVersion", &region.version)
                .send()
                .await
                .map_err(|_| SourceError::Network)?;
            let status = resp.status().as_u16();
            match status {
                200..=299 => {
                    let bytes = resp.bytes().await.map_err(|_| SourceError::Network)?;
                    return serde_json::from_slice(&bytes).map_err(|_| SourceError::Shape(what));
                }
                404 => return Err(SourceError::NotReady),
                // An expired session is refreshed once. A refusal is not argued with.
                400 | 401 if attempt == 0 => self.session = None,
                400 | 401 => return Err(SourceError::Auth),
                other => return Err(SourceError::Http(other)),
            }
        }
        Err(SourceError::Auth)
    }
}

impl DataSource for LocalClient {
    fn watch(&mut self) -> mpsc::Receiver<Signal> {
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(watcher(self.shared.clone(), tx));
        rx
    }

    async fn roster(&mut self, phase: Phase) -> Result<Roster, SourceError> {
        let kind = match phase {
            Phase::AgentSelect => "pregame",
            Phase::InGame => "core-game",
            _ => return Err(SourceError::NotReady),
        };
        let lock = self.shared.lock().ok_or(SourceError::ClientUnavailable)?;
        let region = self.region()?;
        self.open_session().await?;
        let me = self.shared.me().ok_or(SourceError::NotReady)?;
        let glz = format!("https://glz-{}.{}.a.pvp.net", region.glz, region.shard);

        let match_id = match self.found {
            Some((p, id)) if p == phase => id,
            _ => {
                self.found = None;
                let player: wire::PlayerRef =
                    self.riot(None, &format!("{glz}/{kind}/v1/players/{me}"), "lobby lookup").await?;
                // Parsed and re-printed, so only a well-formed id ever goes into a URL.
                let id = Uuid::parse(&player.match_id).ok_or(SourceError::Shape("match id"))?;
                self.found = Some((phase, id));
                id
            }
        };
        let url = format!("{glz}/{kind}/v1/matches/{match_id}");

        // Parties come from the local presence feed: no extra request to Riot.
        let http = self.local()?;
        let parties = match local_get::<wire::PresenceList>(&http, &lock, "/chat/v4/presences", "presences").await {
            Ok(list) => wire::party_ids(&list),
            Err(_) => Default::default(),
        };

        let read = if phase == Phase::AgentSelect {
            self.riot::<wire::PregameMatch>(None, &url, "lobby").await.map(|m| wire::pregame_roster(m, me, &parties))
        } else {
            self.riot::<wire::CoreMatch>(None, &url, "match").await.map(|m| wire::core_roster(m, me, &parties))
        };
        let roster = match read {
            Ok(roster) => roster.ok_or(SourceError::Shape("roster"))?,
            Err(e) => {
                // The lobby is gone or changed: look it up again next time.
                self.found = None;
                return Err(e);
            }
        };

        if let Ok(mut seen) = self.shared.seen.lock() {
            let allies = roster.slots.iter().filter(|s| s.side == crate::model::Side::Ally).count();
            seen.roster = format!(
                "{kind} {} + {} players, {} hidden{}",
                allies,
                roster.slots.len() - allies,
                roster.hidden,
                if roster.flag_missing > 0 { ", identity flag missing" } else { "" }
            );
        }
        Ok(roster)
    }

    async fn names(&mut self, players: &[PlayerId]) -> Result<Vec<Name>, SourceError> {
        if players.is_empty() {
            return Ok(Vec::new());
        }
        let region = self.region()?;
        let body: Vec<String> = players.iter().map(|p| p.to_string()).collect();
        let url = format!("https://pd.{}.a.pvp.net/name-service/v2/players", region.shard);
        let raw: Vec<wire::RawName> = self.riot(Some(&body), &url, "names").await?;
        Ok(raw
            .into_iter()
            .filter_map(|n| {
                let id = Uuid::parse(&n.subject)?;
                // Only what was asked for comes back out.
                players.contains(&id).then_some(Name { id, name: n.game_name, tag: n.tag_line })
            })
            .collect())
    }

    async fn catalog(&mut self) -> Result<Catalog, SourceError> {
        let http = self.remote()?;
        assets::fetch(&http).await
    }

    fn sleep(&mut self) {
        self.found = None;
        self.session = None;
        self.local = None;
        self.remote = None;
        self.region = None;
        crate::win::trim_working_set();
    }

    fn diagnostics(&self) -> String {
        let Ok(seen) = self.shared.seen.lock() else { return String::new() };
        let mut out = seen.stage.to_string();
        if seen.frames > 0 {
            out.push_str(&format!(" · {} events", seen.frames));
            if seen.unreadable > 0 {
                out.push_str(&format!(" ({} unreadable)", seen.unreadable));
            }
        }
        if !seen.loop_state.is_empty() {
            out.push_str(&format!(" · presence {} / {} ({})", seen.loop_state, seen.party_state, seen.layout));
        }
        if !seen.roster.is_empty() {
            if !out.is_empty() {
                out.push_str(" · ");
            }
            out.push_str(&seen.roster);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfile_parses() {
        let l = parse_lockfile("Riot Client:12345:54321:AbC-123_xyz:https").unwrap();
        assert_eq!(l.port, 54321);
        assert_eq!(l.password, "AbC-123_xyz");
        assert_eq!(local_url(&l, "/chat/v1/session"), "https://127.0.0.1:54321/chat/v1/session");
    }

    #[test]
    fn lockfile_rejects_bad_input() {
        assert!(parse_lockfile("").is_none());
        assert!(parse_lockfile("Riot Client:1:notaport:pw:https").is_none());
        assert!(parse_lockfile("Riot Client:1:0:pw:https").is_none());
        assert!(parse_lockfile("Riot Client:1:5000::https").is_none());
        assert!(parse_lockfile("Riot Client:1:5000:pw\r\nHost\tx:https").is_none());
    }

    /// Reads this machine's real game log. Run with `cargo test -- --ignored real_`.
    #[test]
    #[ignore]
    fn real_game_log_gives_a_region() {
        let region = read_region().expect("region and version in ShooterGame.log");
        println!("shard={} glz={} version={}", region.shard, region.glz, region.version);
    }

    /// Fetches the real agent and map names. Static content, no player data, no key.
    #[test]
    #[ignore]
    fn real_catalog_fetch() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let http = remote_http().unwrap();
        let catalog = rt.block_on(assets::fetch(&http)).expect("catalog");
        let jett = Uuid::parse("add6443a-41bd-e414-f6ad-e58d267f4e95").unwrap();
        println!("jett={:?} ascent={:?}", catalog.agent(&jett), catalog.map("/Game/Maps/Ascent/Ascent"));
        assert_eq!(catalog.map("/Game/Maps/Ascent/Ascent"), "Ascent");
        assert!(catalog.agent(&jett).is_some());
    }

    /// Reads the real lockfile if the Riot Client is running; prints only whether it parsed.
    #[test]
    #[ignore]
    fn real_lockfile_state() {
        println!("lockfile present and valid: {}", read_lockfile().is_some());
    }

    #[test]
    fn wipe_empties_the_string() {
        let mut s = String::from("secret");
        wipe(&mut s);
        assert!(s.is_empty());
    }

    #[test]
    fn client_platform_is_plain_platform_json() {
        let json = base64::engine::general_purpose::STANDARD.decode(CLIENT_PLATFORM).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(v["platformType"], "PC");
        assert_eq!(v["platformOS"], "Windows");
    }
}
