//! Stats from the HenrikDev API: rank, recent matches, and my own finished match.
//!
//! Rules this file keeps:
//! - The key comes from the `HENRIKDEV_API_KEY` user variable only. It is sent as a header marked
//!   sensitive, never put in a URL, never printed, and wiped when dropped.
//! - Only GET requests, only to api.henrikdev.xyz, only for ids the caller passes in. Hidden
//!   players have no id, so they cannot be passed in.
//! - A finished match names all ten players, as the game's own end screen does. Those who were
//!   hidden live are marked, so the rest of the app can keep treating them differently: they
//!   are shown in that match's scoreboard and never looked up.

use super::{Budget, SourceError, StatsSource};
use crate::model::{FullMatch, Game, MatchPlayer, MyDetail, PlayerId, Rank, Side, Uuid};
use reqwest::header::HeaderValue;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use std::time::Duration;
use tokio::time::Instant;

const HOST: &str = "https://api.henrikdev.xyz";
/// How many recent competitive matches are asked for per player.
const HISTORY_SIZE: u8 = 10;
/// How many of my own stored matches one page of the full history holds.
pub const FULL_PAGE: usize = 100;

/// The stats key. No `Debug`, no `Display`, no `Clone`: it can only be turned into a header.
pub struct ApiKey(String);

impl ApiKey {
    /// From this process's environment, or from the user's stored variables.
    pub fn load() -> Option<Self> {
        const NAME: &str = "HENRIKDEV_API_KEY";
        let value = std::env::var(NAME).ok().filter(|v| !v.trim().is_empty()).or_else(|| crate::win::user_env(NAME))?;
        Some(Self(value.trim().to_string()))
    }

    fn header(&self) -> Option<HeaderValue> {
        let mut value = HeaderValue::from_str(&self.0).ok()?;
        value.set_sensitive(true);
        Some(value)
    }
}

impl Drop for ApiKey {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
    }
}

pub struct Henrik {
    key: ApiKey,
    http: Option<reqwest::Client>,
    /// Where the account plays, read from the game's log when first needed.
    affinity: Option<String>,
    limit: u32,
    remaining: u32,
    reset_at: Option<Instant>,
}

impl Henrik {
    pub fn new(key: ApiKey) -> Self {
        Self { key, http: None, affinity: None, limit: 30, remaining: 30, reset_at: None }
    }

    fn affinity(&mut self) -> Result<String, SourceError> {
        if self.affinity.is_none() {
            self.affinity = super::local::read_affinity();
        }
        self.affinity.clone().ok_or(SourceError::Region)
    }

    fn note_limits(&mut self, headers: &reqwest::header::HeaderMap) {
        let number = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u32>().ok());
        if let Some(limit) = number("x-ratelimit-limit") {
            self.limit = limit.max(1);
        }
        if let Some(remaining) = number("x-ratelimit-remaining") {
            self.remaining = remaining;
        }
        if let Some(seconds) = number("x-ratelimit-reset").or_else(|| number("retry-after")) {
            self.reset_at = Some(Instant::now() + Duration::from_secs(u64::from(seconds.min(120)) + 1));
        }
    }

    async fn get<T: DeserializeOwned>(&mut self, path: &str, what: &'static str) -> Result<T, SourceError> {
        if self.http.is_none() {
            self.http = super::local::remote_http();
        }
        let http = self.http.clone().ok_or(SourceError::Network)?;
        let key = self.key.header().ok_or(SourceError::Auth)?;
        // Errors are mapped to plain kinds; a reqwest error is never formatted (it can hold a URL).
        let resp = http
            .get(format!("{HOST}{path}"))
            .header(reqwest::header::AUTHORIZATION, key)
            .timeout(Duration::from_secs(25))
            .send()
            .await
            .map_err(|_| SourceError::Network)?;
        self.note_limits(resp.headers());
        match resp.status().as_u16() {
            200..=299 => {
                let bytes = resp.bytes().await.map_err(|_| SourceError::Network)?;
                serde_json::from_slice::<Envelope<T>>(&bytes).map(|e| e.data).map_err(|_| SourceError::Shape(what))
            }
            401 | 403 => Err(SourceError::Auth),
            429 => {
                self.remaining = 0;
                if self.reset_at.is_none() {
                    self.reset_at = Some(Instant::now() + Duration::from_secs(60));
                }
                Err(SourceError::Http(429))
            }
            other => Err(SourceError::Http(other)),
        }
    }
}

impl StatsSource for Henrik {
    async fn rank(&mut self, player: PlayerId) -> Result<Rank, SourceError> {
        let affinity = self.affinity()?;
        let data: MmrData = self.get(&format!("/valorant/v3/by-puuid/mmr/{affinity}/pc/{player}"), "rank").await?;
        Ok(rank_from(data))
    }

    async fn history(&mut self, player: PlayerId) -> Result<Vec<Game>, SourceError> {
        let affinity = self.affinity()?;
        let path = format!(
            "/valorant/v1/by-puuid/stored-matches/{affinity}/{player}?mode=competitive&size={HISTORY_SIZE}"
        );
        let data: Vec<Stored> = self.get(&path, "match history").await?;
        Ok(data.into_iter().filter_map(game_from).collect())
    }

    async fn full_history(&mut self, player: PlayerId, page: u32) -> Result<Vec<Game>, SourceError> {
        let affinity = self.affinity()?;
        let path = format!(
            "/valorant/v1/by-puuid/stored-matches/{affinity}/{player}?mode=competitive&size={FULL_PAGE}&page={page}"
        );
        let data: Vec<Stored> = self.get(&path, "match history").await?;
        Ok(data.into_iter().filter_map(game_from).collect())
    }

    async fn last_match(&mut self, me: PlayerId, hidden: &[(Side, Uuid)]) -> Result<Option<FullMatch>, SourceError> {
        let affinity = self.affinity()?;
        let path = format!("/valorant/v4/by-puuid/matches/{affinity}/pc/{me}?size=1");
        let mut data: Vec<Full> = self.get(&path, "match").await?;
        Ok(data.pop().and_then(|full| full_match(full, me, hidden)))
    }

    fn budget(&self) -> Budget {
        let now = Instant::now();
        match self.reset_at {
            Some(at) if at > now => Budget { remaining: self.remaining, limit: self.limit, reset_in: at - now },
            // The window has rolled over (or nothing was sent yet): the full allowance is back.
            _ => Budget { remaining: self.limit, limit: self.limit, reset_in: Duration::ZERO },
        }
    }

    fn sleep(&mut self) {
        self.http = None;
        self.affinity = None;
    }
}

// ---- answers, as the API writes them -------------------------------------------------------

/// A missing field and an explicit `null` both read as the default.
fn lenient<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Deserialize, Default)]
struct Tier {
    #[serde(default, deserialize_with = "lenient")]
    id: i32,
}

#[derive(Deserialize, Default)]
struct Named {
    #[serde(default, deserialize_with = "lenient")]
    id: String,
    #[serde(default, deserialize_with = "lenient")]
    name: String,
}

#[derive(Deserialize)]
struct MmrData {
    #[serde(default, deserialize_with = "lenient")]
    current: MmrCurrent,
    #[serde(default, deserialize_with = "lenient")]
    peak: MmrPeak,
    #[serde(default, deserialize_with = "lenient")]
    seasonal: Vec<Seasonal>,
    #[serde(default, deserialize_with = "lenient")]
    latest_update: LatestUpdate,
}

#[derive(Deserialize, Default)]
struct MmrCurrent {
    #[serde(default, deserialize_with = "lenient")]
    tier: Tier,
    #[serde(default, deserialize_with = "lenient")]
    rr: i32,
}

#[derive(Deserialize, Default)]
struct MmrPeak {
    #[serde(default, deserialize_with = "lenient")]
    tier: Tier,
}

#[derive(Deserialize, Default)]
struct Seasonal {
    #[serde(default, deserialize_with = "lenient")]
    games: i32,
}

#[derive(Deserialize, Default)]
struct LatestUpdate {
    #[serde(default, deserialize_with = "lenient")]
    match_id: String,
    #[serde(default, deserialize_with = "lenient")]
    last_change: i32,
}

fn tier(id: i32) -> u8 {
    id.clamp(0, 27) as u8
}

fn count(n: i32) -> u32 {
    n.max(0) as u32
}

fn rank_from(data: MmrData) -> Rank {
    let current = tier(data.current.tier.id);
    Rank {
        tier: current,
        rr: data.current.rr,
        peak: tier(data.peak.tier.id).max(current),
        last_change: data.latest_update.last_change,
        last_match: Uuid::parse(&data.latest_update.match_id),
        games: data.seasonal.iter().map(|s| count(s.games)).sum(),
        fetched_at: 0,
    }
}

#[derive(Deserialize)]
struct Stored {
    #[serde(default, deserialize_with = "lenient")]
    meta: StoredMeta,
    #[serde(default, deserialize_with = "lenient")]
    stats: StoredStats,
    #[serde(default, deserialize_with = "lenient")]
    teams: StoredTeams,
}

#[derive(Deserialize, Default)]
struct StoredMeta {
    #[serde(default, deserialize_with = "lenient")]
    id: String,
    #[serde(default, deserialize_with = "lenient")]
    map: Named,
    #[serde(default, deserialize_with = "lenient")]
    started_at: String,
}

#[derive(Deserialize, Default)]
struct StoredStats {
    #[serde(default, deserialize_with = "lenient")]
    team: String,
    #[serde(default, deserialize_with = "lenient")]
    character: Named,
    #[serde(default, deserialize_with = "lenient")]
    tier: i32,
    #[serde(default, deserialize_with = "lenient")]
    score: i32,
    #[serde(default, deserialize_with = "lenient")]
    kills: i32,
    #[serde(default, deserialize_with = "lenient")]
    deaths: i32,
    #[serde(default, deserialize_with = "lenient")]
    assists: i32,
    #[serde(default, deserialize_with = "lenient")]
    shots: Shots,
    #[serde(default, deserialize_with = "lenient")]
    damage: StoredDamage,
}

#[derive(Deserialize, Default)]
struct Shots {
    #[serde(default, deserialize_with = "lenient")]
    head: i32,
    #[serde(default, deserialize_with = "lenient")]
    body: i32,
    #[serde(default, deserialize_with = "lenient")]
    leg: i32,
}

#[derive(Deserialize, Default)]
struct StoredDamage {
    #[serde(default, deserialize_with = "lenient")]
    made: i32,
}

#[derive(Deserialize, Default)]
struct StoredTeams {
    #[serde(default, deserialize_with = "lenient")]
    red: i32,
    #[serde(default, deserialize_with = "lenient")]
    blue: i32,
}

fn outcome(mine: u32, theirs: u32) -> u8 {
    match mine.cmp(&theirs) {
        std::cmp::Ordering::Greater => 1,
        std::cmp::Ordering::Less => 0,
        std::cmp::Ordering::Equal => 2,
    }
}

fn game_from(s: Stored) -> Option<Game> {
    let match_id = Uuid::parse(&s.meta.id)?;
    let started_at = unix_from_iso(&s.meta.started_at)?;
    let (red, blue) = (count(s.teams.red), count(s.teams.blue));
    let (mine, theirs) = if s.stats.team.eq_ignore_ascii_case("red") { (red, blue) } else { (blue, red) };
    if red + blue == 0 {
        return None;
    }
    Some(Game {
        match_id,
        started_at,
        map: s.meta.map.name,
        agent: s.stats.character.name,
        won: outcome(mine, theirs),
        rounds: red + blue,
        kills: count(s.stats.kills),
        deaths: count(s.stats.deaths),
        assists: count(s.stats.assists),
        score: count(s.stats.score),
        damage: count(s.stats.damage.made),
        head: count(s.stats.shots.head),
        body: count(s.stats.shots.body),
        leg: count(s.stats.shots.leg),
        tier: tier(s.stats.tier),
    })
}

#[derive(Deserialize)]
struct Full {
    #[serde(default, deserialize_with = "lenient")]
    metadata: FullMeta,
    #[serde(default, deserialize_with = "lenient")]
    players: Vec<FullPlayer>,
    #[serde(default, deserialize_with = "lenient")]
    teams: Vec<FullTeam>,
    #[serde(default, deserialize_with = "lenient")]
    rounds: Vec<FullRound>,
    #[serde(default, deserialize_with = "lenient")]
    kills: Vec<FullKill>,
}

#[derive(Deserialize, Default)]
struct FullMeta {
    #[serde(default, deserialize_with = "lenient")]
    match_id: String,
    #[serde(default, deserialize_with = "lenient")]
    map: Named,
    #[serde(default, deserialize_with = "lenient")]
    started_at: String,
    #[serde(default, deserialize_with = "lenient")]
    queue: Named,
    #[serde(default, deserialize_with = "lenient")]
    is_completed: bool,
}

#[derive(Deserialize, Default)]
struct FullPlayer {
    #[serde(default, deserialize_with = "lenient")]
    puuid: String,
    #[serde(default, deserialize_with = "lenient")]
    name: String,
    #[serde(default, deserialize_with = "lenient")]
    tag: String,
    #[serde(default, deserialize_with = "lenient")]
    team_id: String,
    #[serde(default, deserialize_with = "lenient")]
    agent: Named,
    #[serde(default, deserialize_with = "lenient")]
    stats: FullStats,
    #[serde(default, deserialize_with = "lenient")]
    tier: Tier,
}

#[derive(Deserialize, Default)]
struct FullStats {
    #[serde(default, deserialize_with = "lenient")]
    score: i32,
    #[serde(default, deserialize_with = "lenient")]
    kills: i32,
    #[serde(default, deserialize_with = "lenient")]
    deaths: i32,
    #[serde(default, deserialize_with = "lenient")]
    assists: i32,
    #[serde(default, deserialize_with = "lenient")]
    headshots: i32,
    #[serde(default, deserialize_with = "lenient")]
    bodyshots: i32,
    #[serde(default, deserialize_with = "lenient")]
    legshots: i32,
    #[serde(default, deserialize_with = "lenient")]
    damage: FullDamage,
}

#[derive(Deserialize, Default)]
struct FullDamage {
    #[serde(default, deserialize_with = "lenient")]
    dealt: i32,
}

#[derive(Deserialize, Default)]
struct FullTeam {
    #[serde(default, deserialize_with = "lenient")]
    team_id: String,
    #[serde(default, deserialize_with = "lenient")]
    rounds: TeamRounds,
    #[serde(default, deserialize_with = "lenient")]
    won: bool,
}

#[derive(Deserialize, Default)]
struct TeamRounds {
    #[serde(default, deserialize_with = "lenient")]
    won: i32,
    #[serde(default, deserialize_with = "lenient")]
    lost: i32,
}

#[derive(Deserialize, Default)]
struct FullRound {
    #[serde(default, deserialize_with = "lenient")]
    id: i32,
    #[serde(default, deserialize_with = "lenient")]
    winning_team: String,
    #[serde(default, deserialize_with = "lenient")]
    winning_team_role: String,
}

#[derive(Deserialize, Default)]
struct FullKill {
    #[serde(default, deserialize_with = "lenient")]
    round: i32,
    #[serde(default, deserialize_with = "lenient")]
    time_in_round_in_ms: i64,
    #[serde(default, deserialize_with = "lenient")]
    killer: RoundPlayer,
    #[serde(default, deserialize_with = "lenient")]
    victim: RoundPlayer,
}

#[derive(Deserialize, Default)]
struct RoundPlayer {
    #[serde(default, deserialize_with = "lenient")]
    puuid: String,
    #[serde(default, deserialize_with = "lenient")]
    team: String,
}

/// Turns a full match into what Sightline keeps. `hidden` lists the slots that were hidden live,
/// as side and agent; players in those slots are marked, never me.
fn full_match(full: Full, me: PlayerId, hidden: &[(Side, Uuid)]) -> Option<FullMatch> {
    if !full.metadata.is_completed {
        return None;
    }
    let match_id = Uuid::parse(&full.metadata.match_id)?;
    let started_at = unix_from_iso(&full.metadata.started_at)?;
    let my_team = full.players.iter().find(|p| Uuid::parse(&p.puuid) == Some(me))?.team_id.clone();
    let team = full.teams.iter().find(|t| t.team_id == my_team)?;
    let (rounds_won, rounds_lost) = (count(team.rounds.won), count(team.rounds.lost));
    let rounds = rounds_won + rounds_lost;
    // Modes without rounds still name a winner, so the team's own flag is believed first.
    let other_won = full.teams.iter().any(|t| t.team_id != my_team && t.won);
    let result = if team.won {
        1
    } else if other_won {
        0
    } else {
        outcome(rounds_won, rounds_lost)
    };
    let map = full.metadata.map.name.clone();

    let size = |team: &str| full.players.iter().filter(|p| p.team_id == team).count() as i32;
    let (my_size, their_size) = (size(&my_team), full.players.len() as i32 - size(&my_team));
    let me_text = me.to_string();

    // First kill, first death, and "last one standing" rounds, from the kill order of each round.
    let mut detail = MyDetail::default();
    for round in &full.rounds {
        let i_won = round.winning_team == my_team;
        match (round.winning_team_role.to_ascii_lowercase().as_str(), i_won) {
            ("attacker", true) | ("defender", false) => {
                detail.atk_rounds += 1;
                detail.atk_won += u32::from(i_won);
            }
            ("defender", true) | ("attacker", false) => {
                detail.def_rounds += 1;
                detail.def_won += u32::from(i_won);
            }
            _ => {}
        }
        let mut kills: Vec<&FullKill> = full.kills.iter().filter(|k| k.round == round.id).collect();
        kills.sort_by_key(|k| k.time_in_round_in_ms);
        if let Some(first) = kills.first() {
            detail.first_kills += u32::from(first.killer.puuid == me_text && first.victim.team != my_team);
            detail.first_deaths += u32::from(first.victim.puuid == me_text);
        }
        let (mut mine_up, mut theirs_up, mut i_am_up, mut tried) = (my_size, their_size, true, false);
        for kill in kills {
            if kill.victim.team == my_team {
                mine_up -= 1;
                i_am_up &= kill.victim.puuid != me_text;
            } else {
                theirs_up -= 1;
            }
            tried |= i_am_up && mine_up == 1 && theirs_up >= 1 && my_size > 1;
        }
        if tried {
            detail.clutch_tries += 1;
            detail.clutch_wins += u32::from(i_won);
        }
    }

    let mut mine = None;
    let mut players = Vec::with_capacity(full.players.len());
    for p in full.players {
        let Some(id) = Uuid::parse(&p.puuid) else { continue };
        let ally = p.team_id == my_team;
        let side = if ally { Side::Ally } else { Side::Enemy };
        let was_hidden = id != me && Uuid::parse(&p.agent.id).is_some_and(|agent| hidden.contains(&(side, agent)));
        let game = Game {
            match_id,
            started_at,
            map: map.clone(),
            agent: p.agent.name,
            won: match (ally, result) {
                (true, r) => r,
                (false, 1) => 0,
                (false, 0) => 1,
                (false, r) => r,
            },
            rounds,
            kills: count(p.stats.kills),
            deaths: count(p.stats.deaths),
            assists: count(p.stats.assists),
            score: count(p.stats.score),
            damage: count(p.stats.damage.dealt),
            head: count(p.stats.headshots),
            body: count(p.stats.bodyshots),
            leg: count(p.stats.legshots),
            tier: tier(p.tier.id),
        };
        if id == me {
            mine = Some(game.clone());
        }
        players.push(MatchPlayer { id, name: p.name, tag: p.tag, ally, was_hidden, game });
    }

    Some(FullMatch {
        match_id,
        started_at,
        map,
        queue: full.metadata.queue.id.to_ascii_lowercase(),
        rounds_won,
        rounds_lost,
        result,
        mine: mine?,
        detail,
        players,
    })
}

/// Parses a finished match from its JSON text. Used by the made-up data, so it goes through the
/// same reading and the same hidden-player rule as a real answer.
pub fn parse_full_match(json: &str, me: PlayerId, hidden: &[(Side, Uuid)]) -> Option<FullMatch> {
    full_match(serde_json::from_str(json).ok()?, me, hidden)
}

/// `2026-07-10T12:34:56Z` (with or without fractions) to Unix seconds. UTC is assumed.
pub fn unix_from_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |from: usize, to: usize| s.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (hour, minute, second) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // Days since 1970-01-01 for a proleptic Gregorian date.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "00000000-0000-4000-8000-000000000001";
    const JETT: &str = "add6443a-41bd-e414-f6ad-e58d267f4e95";
    const SOVA: &str = "320b2a48-4d9b-a075-30f1-1f93a9b638fa";

    fn id(s: &str) -> Uuid {
        Uuid::parse(s).unwrap()
    }

    /// Calls the real stats API for my own account. Needs the key and a game log on this PC.
    /// Prints shapes and counts only: no key, no id, no names.
    /// Run with `cargo test -- --ignored real_stats --nocapture`.
    #[test]
    #[ignore]
    fn real_stats_for_my_own_account() {
        let Some(key) = ApiKey::load() else {
            println!("no key on this PC: skipped");
            return;
        };
        let log = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap()).join(r"VALORANT\Saved\Logs\ShooterGame.log");
        let text = String::from_utf8_lossy(&std::fs::read(log).expect("game log")).into_owned();
        let me = text
            .lines()
            .filter_map(|l| l.split_once("Logged in user changed: "))
            .filter_map(|(_, rest)| Uuid::parse(rest.trim().get(..36)?))
            .last()
            .expect("own id in the game log");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut henrik = Henrik::new(key);
        rt.block_on(async {
            let rank = henrik.rank(me).await.expect("rank");
            let after = henrik.budget();
            println!("rank: ranked={} peak>=tier={} games>0={} names last match={}", rank.tier >= 3, rank.peak >= rank.tier, rank.games > 0, rank.last_match.is_some());
            println!("allowance after one call: {}/{}", after.remaining, after.limit);
            let games = henrik.history(me).await.expect("history");
            println!(
                "history: {} matches, all with a map={} an agent={} rounds>=5={} newest first={}",
                games.len(),
                games.iter().all(|g| !g.map.is_empty()),
                games.iter().all(|g| !g.agent.is_empty()),
                games.iter().all(|g| g.rounds >= 5),
                games.windows(2).all(|w| w[0].started_at >= w[1].started_at)
            );
            let last = henrik.last_match(me, &[]).await.expect("match").expect("a finished match");
            let d = last.detail;
            println!(
                "last match: queue={} players={} rounds={} sides add up={} first kills+deaths<=rounds={} clutch wins<=tries={}",
                last.queue,
                last.players.len(),
                last.rounds_won + last.rounds_lost,
                d.atk_rounds + d.def_rounds == last.rounds_won + last.rounds_lost,
                d.first_kills + d.first_deaths <= last.rounds_won + last.rounds_lost,
                d.clutch_wins <= d.clutch_tries
            );
            for page in [1u32, 2, 3, 4] {
                let games = henrik.full_history(me, page).await.expect("full history page");
                let newest_first = games.windows(2).all(|w| w[0].started_at >= w[1].started_at);
                println!("full history page {page}: {} matches, newest first={newest_first}", games.len());
                if games.is_empty() {
                    break;
                }
            }
            let end = henrik.budget();
            println!("allowance at the end: {}/{}", end.remaining, end.limit);
            assert!(!games.is_empty());
        });
    }

    #[test]
    fn iso_times() {
        assert_eq!(unix_from_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_from_iso("2000-03-01T00:00:00.000Z"), Some(951_868_800));
        assert_eq!(unix_from_iso("2026-07-10T12:34:56Z"), Some(1_783_686_896));
        assert_eq!(unix_from_iso("2026-07-10"), None);
        assert_eq!(unix_from_iso("not a date at all!!"), None);
    }

    #[test]
    fn rank_answer() {
        let json = r#"{"status":200,"data":{"account":{"name":"x","tag":"y","puuid":"z"},
            "peak":{"tier":{"id":24,"name":"Immortal 1"},"rr":10},
            "current":{"tier":{"id":22,"name":"Ascendant 2"},"rr":61,"last_change":-14,"leaderboard_placement":null},
            "seasonal":[{"games":40,"wins":21},{"games":12}],
            "latest_update":{"match_id":"00000000-0000-4000-8000-0000000000a1","last_change":21}}}"#;
        let rank = rank_from(serde_json::from_str::<Envelope<MmrData>>(json).unwrap().data);
        assert_eq!((rank.tier, rank.rr, rank.peak, rank.games, rank.last_change), (22, 61, 24, 52, 21));
        assert_eq!(rank.last_match, Uuid::parse("00000000-0000-4000-8000-0000000000a1"));
    }

    #[test]
    fn an_unranked_or_sparse_rank_answer_still_reads() {
        let json = r#"{"status":200,"data":{"current":{"tier":null,"rr":null},"peak":null,"seasonal":null}}"#;
        let rank = rank_from(serde_json::from_str::<Envelope<MmrData>>(json).unwrap().data);
        assert_eq!((rank.tier, rank.peak, rank.games), (0, 0, 0));
        assert!(rank.last_match.is_none());
    }

    #[test]
    fn stored_history_answer() {
        let json = r#"{"status":200,"results":{"total":2,"returned":2},"data":[
            {"meta":{"id":"00000000-0000-4000-8000-0000000000b1","map":{"id":"m","name":"Ascent"},
                     "mode":"Competitive","started_at":"2026-07-10T12:34:56Z"},
             "stats":{"puuid":"p","team":"Blue","level":150,"character":{"id":"a","name":"Jett"},"tier":22,
                      "score":5000,"kills":20,"deaths":15,"assists":5,"shots":{"head":10,"body":40,"leg":5},
                      "damage":{"made":3200,"received":2800}},
             "teams":{"red":9,"blue":13}},
            {"meta":{"id":"broken","map":null,"started_at":null},"stats":null,"teams":null}]}"#;
        let games: Vec<Game> =
            serde_json::from_str::<Envelope<Vec<Stored>>>(json).unwrap().data.into_iter().filter_map(game_from).collect();
        assert_eq!(games.len(), 1, "an unreadable entry is skipped, not fatal");
        let g = &games[0];
        assert_eq!((g.won, g.rounds, g.kills, g.deaths, g.damage, g.head, g.tier), (1, 22, 20, 15, 3200, 10, 22));
        assert_eq!((g.map.as_str(), g.agent.as_str()), ("Ascent", "Jett"));
    }

    /// A 2v2 with three rounds. Round 0: I get the first kill and win it alone after my
    /// teammate dies. Round 1: I die first. Round 2: nobody on my team dies.
    fn tiny_match() -> String {
        let p = |n: u8, team: &str, agent: &str, name: &str| {
            format!(
                r#"{{"puuid":"00000000-0000-4000-8000-00000000000{n}","name":"{name}","tag":"t","team_id":"{team}",
                    "party_id":"p{n}","agent":{{"id":"{agent}","name":"A{n}"}},"tier":{{"id":20,"name":"x"}},
                    "stats":{{"score":100,"kills":{n},"deaths":1,"assists":0,"headshots":2,"bodyshots":6,"legshots":0,
                              "damage":{{"dealt":400,"received":300}}}}}}"#
            )
        };
        let k = |round: u8, t: u32, killer: u8, kt: &str, victim: u8, vt: &str| {
            format!(
                r#"{{"round":{round},"time_in_round_in_ms":{t},
                    "killer":{{"puuid":"00000000-0000-4000-8000-00000000000{killer}","name":"n","tag":"t","team":"{kt}"}},
                    "victim":{{"puuid":"00000000-0000-4000-8000-00000000000{victim}","name":"n","tag":"t","team":"{vt}"}},
                    "player_locations":[{{"player":{{"puuid":"x"}},"view_radians":1.5,"location":{{"x":1,"y":2}}}}]}}"#
            )
        };
        format!(
            r#"{{"metadata":{{"match_id":"00000000-0000-4000-8000-0000000000a1","map":{{"id":"m","name":"Ascent"}},
                 "started_at":"2026-07-10T12:34:56Z","is_completed":true,"queue":{{"id":"competitive","name":"Competitive"}}}},
               "players":[{},{},{},{}],
               "teams":[{{"team_id":"Red","rounds":{{"won":1,"lost":2}},"won":false}},
                        {{"team_id":"Blue","rounds":{{"won":2,"lost":1}},"won":true}}],
               "rounds":[{{"id":0,"winning_team":"Blue","winning_team_role":"Attacker","result":"Elimination"}},
                         {{"id":1,"winning_team":"Red","winning_team_role":"Defender","result":"Elimination"}},
                         {{"id":2,"winning_team":"Blue","winning_team_role":"Attacker","result":"Elimination"}}],
               "kills":[{},{},{},{},{},{}]}}"#,
            p(1, "Blue", "8e253930-4c05-31dd-1b6c-968525494517", "me"),
            p(2, "Blue", JETT, "friend"),
            p(3, "Red", SOVA, "must stay hidden"),
            p(4, "Red", JETT, "enemy"),
            k(0, 9000, 3, "Red", 2, "Blue"),
            k(0, 4000, 1, "Blue", 4, "Red"),
            k(0, 15000, 1, "Blue", 3, "Red"),
            k(1, 3000, 4, "Red", 1, "Blue"),
            k(2, 5000, 2, "Blue", 3, "Red"),
            k(2, 8000, 1, "Blue", 4, "Red"),
        )
    }

    #[test]
    fn a_full_match_gives_result_sides_first_kills_and_clutches() {
        let m = parse_full_match(&tiny_match(), id(ME), &[]).unwrap();
        assert_eq!((m.result, m.rounds_won, m.rounds_lost), (1, 2, 1));
        assert_eq!((m.map.as_str(), m.queue.as_str()), ("Ascent", "competitive"));
        assert_eq!(m.players.len(), 4);
        assert_eq!((m.mine.kills, m.mine.rounds, m.mine.won), (1, 3, 1));
        let d = m.detail;
        // Kills are sorted by time, so my 4 s kill in round 0 is the first one.
        assert_eq!((d.first_kills, d.first_deaths), (1, 1));
        assert_eq!((d.clutch_tries, d.clutch_wins), (1, 1));
        assert_eq!((d.atk_rounds, d.atk_won, d.def_rounds, d.def_won), (3, 2, 0, 0));
        let enemy = m.players.iter().find(|p| !p.ally).unwrap();
        assert_eq!(enemy.game.won, 0, "the other team lost the match I won");
    }

    #[test]
    fn a_player_who_was_hidden_live_is_marked_not_dropped() {
        let hidden = [(Side::Enemy, id(SOVA))];
        let m = parse_full_match(&tiny_match(), id(ME), &hidden).unwrap();
        assert_eq!(m.players.len(), 4, "the finished match lists everyone, as the game's end screen does");
        let marked: Vec<&MatchPlayer> = m.players.iter().filter(|p| p.was_hidden).collect();
        assert_eq!(marked.len(), 1);
        assert_eq!(marked[0].id, id("00000000-0000-4000-8000-000000000003"));
        assert_eq!(marked[0].name, "must stay hidden", "named after the match, as the game does");
        // The same agent on the other side is a different slot and is not marked.
        let ally_side = [(Side::Ally, id(SOVA))];
        assert!(parse_full_match(&tiny_match(), id(ME), &ally_side).unwrap().players.iter().all(|p| !p.was_hidden));
        // I am never marked, whatever the list says.
        let mine = [(Side::Ally, id("8e253930-4c05-31dd-1b6c-968525494517"))];
        let me = parse_full_match(&tiny_match(), id(ME), &mine).unwrap().players.into_iter().find(|p| p.id == id(ME)).unwrap();
        assert!(!me.was_hidden && me.name == "me");
    }

    #[test]
    fn a_mode_without_rounds_takes_the_winner_from_the_team_flag() {
        let no_rounds = tiny_match()
            .replace(r#""rounds":{"won":1,"lost":2}"#, r#""rounds":{"won":0,"lost":0}"#)
            .replace(r#""rounds":{"won":2,"lost":1}"#, r#""rounds":{"won":0,"lost":0}"#);
        let m = parse_full_match(&no_rounds, id(ME), &[]).unwrap();
        assert_eq!((m.result, m.rounds_won, m.rounds_lost), (1, 0, 0));
        assert_eq!(m.players.iter().find(|p| !p.ally).unwrap().game.won, 0);
    }

    #[test]
    fn an_unfinished_or_foreign_match_is_not_used() {
        let unfinished = tiny_match().replace(r#""is_completed":true"#, r#""is_completed":false"#);
        assert!(parse_full_match(&unfinished, id(ME), &[]).is_none());
        let stranger = id("00000000-0000-4000-8000-00000000000f");
        assert!(parse_full_match(&tiny_match(), stranger, &[]).is_none());
    }
}
