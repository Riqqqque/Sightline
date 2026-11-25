//! The local database: what Sightline remembers between matches. One SQLite file, opened by the
//! stats worker only. The UI thread never touches it.
//!
//! What is never written here: the stats key, Riot tokens, and anything about a hidden player.
//! A hidden slot is remembered as side + agent only, so the finished match can be cleaned.

use crate::model::{FullMatch, Game, PlayerId, Rank, Side, Uuid};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta     (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;

-- Every non-hidden player seen in a lobby or looked up.
CREATE TABLE IF NOT EXISTS players (
  puuid       BLOB PRIMARY KEY,
  name        TEXT,
  tag         TEXT,
  tier        INTEGER,
  rr          INTEGER,
  peak        INTEGER,
  games       INTEGER,
  last_change INTEGER,
  last_match  BLOB,
  rank_at     INTEGER,
  history_at  INTEGER,
  first_seen  INTEGER NOT NULL,
  last_seen   INTEGER NOT NULL,
  note        TEXT
) WITHOUT ROWID;

-- One player's line in one finished competitive match. Never refetched.
CREATE TABLE IF NOT EXISTS games (
  puuid      BLOB NOT NULL,
  match_id   BLOB NOT NULL,
  started_at INTEGER NOT NULL,
  map        TEXT NOT NULL,
  agent      TEXT NOT NULL,
  won        INTEGER NOT NULL,
  rounds     INTEGER NOT NULL,
  kills      INTEGER NOT NULL,
  deaths     INTEGER NOT NULL,
  assists    INTEGER NOT NULL,
  score      INTEGER NOT NULL,
  damage     INTEGER NOT NULL,
  head       INTEGER NOT NULL,
  body       INTEGER NOT NULL,
  leg        INTEGER NOT NULL,
  tier       INTEGER NOT NULL,
  PRIMARY KEY (puuid, match_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS games_recent ON games(puuid, started_at);

-- My own matches: session tracker, estimate log, recap.
CREATE TABLE IF NOT EXISTS my_games (
  match_id     BLOB PRIMARY KEY,
  seen_at      INTEGER NOT NULL,
  map          TEXT,
  queue        TEXT,
  agent        TEXT,
  ally_avg     INTEGER,
  enemy_avg    INTEGER,
  estimate     INTEGER,
  result       INTEGER,
  rr_change    INTEGER,
  rounds_won   INTEGER,
  rounds_lost  INTEGER,
  kills INTEGER, deaths INTEGER, assists INTEGER, damage INTEGER, rounds INTEGER,
  head INTEGER, shots INTEGER,
  first_kills INTEGER, first_deaths INTEGER, clutch_tries INTEGER, clutch_wins INTEGER,
  atk_rounds INTEGER, atk_won INTEGER, def_rounds INTEGER, def_won INTEGER
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS my_games_seen ON my_games(seen_at);

-- Seen before: one row per non-hidden player per match of mine.
CREATE TABLE IF NOT EXISTS encounters (
  puuid    BLOB NOT NULL,
  match_id BLOB NOT NULL,
  relation INTEGER NOT NULL,          -- 0 with me, 1 against me
  agent    TEXT,
  party    INTEGER NOT NULL,
  PRIMARY KEY (puuid, match_id)
) WITHOUT ROWID;

-- The scoreboard of my finished matches, everyone in it, as the game's end screen shows it.
CREATE TABLE IF NOT EXISTS board (
  match_id   BLOB NOT NULL,
  slot       INTEGER NOT NULL,
  name       TEXT NOT NULL,
  tag        TEXT NOT NULL,
  ally       INTEGER NOT NULL,
  me         INTEGER NOT NULL,
  was_hidden INTEGER NOT NULL,
  agent      TEXT NOT NULL,
  tier       INTEGER NOT NULL,
  kills      INTEGER NOT NULL,
  deaths     INTEGER NOT NULL,
  assists    INTEGER NOT NULL,
  score      INTEGER NOT NULL,
  damage     INTEGER NOT NULL,
  head       INTEGER NOT NULL,
  shots      INTEGER NOT NULL,
  rounds     INTEGER NOT NULL,
  PRIMARY KEY (match_id, slot)
) WITHOUT ROWID;

-- Hidden players in a match of mine while it is live: side and agent only, never an id or a name.
CREATE TABLE IF NOT EXISTS hidden_slots (
  match_id BLOB NOT NULL,
  side     INTEGER NOT NULL,
  agent    BLOB NOT NULL,
  PRIMARY KEY (match_id, side, agent)
) WITHOUT ROWID;
";

pub struct Store {
    db: Connection,
    path: Option<PathBuf>,
}

/// Recent form of one player, from the stored match lines.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Summary {
    pub games: u32,
    pub wins: u32,
    pub rounds: u32,
    pub kills: u32,
    pub deaths: u32,
    pub damage: u32,
    pub head: u32,
    pub shots: u32,
    /// Newest first: 1 win, 0 loss, 2 draw.
    pub last5: Vec<u8>,
    /// Agent, matches, wins. Most played first.
    pub agents: Vec<(String, u32, u32)>,
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Tally {
    pub games: u32,
    pub wins: u32,
    pub losses: u32,
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Seen {
    pub with: Tally,
    pub against: Tally,
    /// Unix seconds of the most recent match together; 0 when never.
    pub last_at: i64,
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct MapStat {
    pub name: String,
    pub games: u32,
    pub wins: u32,
    pub kills: u32,
    pub deaths: u32,
    pub damage: u32,
    pub rounds: u32,
}

/// One line of a finished match's scoreboard.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct BoardLine {
    pub name: String,
    pub tag: String,
    pub ally: bool,
    pub me: bool,
    pub was_hidden: bool,
    pub agent: String,
    pub tier: u8,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub score: u32,
    pub damage: u32,
    pub head: u32,
    pub shots: u32,
    pub rounds: u32,
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct MyGame {
    pub seen_at: i64,
    pub map: String,
    pub queue: String,
    pub estimate: Option<i32>,
    pub result: Option<u8>,
    pub rr_change: Option<i32>,
    pub rounds_won: u32,
    pub rounds_lost: u32,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub damage: u32,
    pub rounds: u32,
    pub head: u32,
    pub shots: u32,
    pub first_kills: u32,
    pub first_deaths: u32,
    pub clutch_tries: u32,
    pub clutch_wins: u32,
}

fn side_number(side: Side) -> i64 {
    match side {
        Side::Ally => 0,
        Side::Enemy => 1,
    }
}

fn blob(id: &Uuid) -> &[u8] {
    &id.bytes()[..]
}

type Res<T> = rusqlite::Result<T>;

impl Store {
    /// `None` keeps everything in memory: used by tests and by made-up data, so neither can
    /// reach the real file.
    pub fn open(path: Option<&Path>) -> Option<Self> {
        let db = match path {
            Some(path) => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).ok()?;
                }
                Connection::open(path).ok()?
            }
            None => Connection::open_in_memory().ok()?,
        };
        let _ = db.pragma_update(None, "journal_mode", "WAL");
        let _ = db.pragma_update(None, "synchronous", "NORMAL");
        let _ = db.pragma_update(None, "cache_size", -1024);
        db.set_prepared_statement_cache_capacity(32);
        db.execute_batch(SCHEMA).ok()?;
        Some(Self { db, path: path.map(Path::to_path_buf) })
    }

    /// The real file: `%LOCALAPPDATA%\Sightline\sightline.db`.
    pub fn default_path() -> Option<PathBuf> {
        Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Sightline").join("sightline.db"))
    }

    pub fn size_bytes(&self) -> u64 {
        self.path.as_ref().and_then(|p| std::fs::metadata(p).ok()).map_or(0, |m| m.len())
    }

    /// Settles defaults that depend on whether this is a new install. "Names after the match" is
    /// off for a new one; a database that already holds finished matches comes from before the
    /// setting existed, when names were shown, and keeps that.
    pub fn adopt(&self) {
        if self.setting("after").is_none() {
            let existing = self.db.query_row("SELECT COUNT(*) FROM my_games WHERE result IS NOT NULL", [], |r| r.get::<_, u32>(0)).unwrap_or(0) > 0;
            self.set_setting("after", if existing { "on" } else { "off" });
        }
    }

    /// Gives cached pages back while nothing is going on.
    pub fn rest(&self) {
        let _ = self.db.execute_batch("PRAGMA shrink_memory;");
    }

    // ---- small key/value tables ----

    fn value(&self, table: &str, key: &str) -> Option<String> {
        self.db
            .prepare_cached(&format!("SELECT value FROM {table} WHERE key = ?1"))
            .and_then(|mut q| q.query_row([key], |r| r.get(0)).optional())
            .ok()
            .flatten()
    }

    fn set_value(&self, table: &str, key: &str, value: &str) {
        let _ = self.db.execute(
            &format!("INSERT INTO {table}(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2"),
            params![key, value],
        );
    }

    pub fn setting(&self, key: &str) -> Option<String> {
        self.value("settings", key)
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        self.set_value("settings", key, value);
    }

    /// My own player id, remembered so the idle screen works before the first lobby.
    pub fn me(&self) -> Option<PlayerId> {
        Uuid::parse(&self.value("meta", "me")?)
    }

    pub fn set_me(&self, me: PlayerId) {
        self.set_value("meta", "me", &me.to_string());
    }

    // ---- players ----

    pub fn touch(&self, id: PlayerId, now: i64) {
        let _ = self.db.execute(
            "INSERT INTO players(puuid, first_seen, last_seen) VALUES(?1, ?2, ?2)
             ON CONFLICT(puuid) DO UPDATE SET last_seen = ?2",
            params![blob(&id), now],
        );
    }

    pub fn set_name(&self, id: PlayerId, name: &str, tag: &str, now: i64) {
        self.touch(id, now);
        let _ = self.db.execute("UPDATE players SET name = ?2, tag = ?3 WHERE puuid = ?1", params![blob(&id), name, tag]);
    }

    pub fn save_rank(&self, id: PlayerId, rank: &Rank, now: i64) {
        self.touch(id, now);
        let _ = self.db.execute(
            "UPDATE players SET tier = ?2, rr = ?3, peak = ?4, games = ?5, last_change = ?6, last_match = ?7, rank_at = ?8
             WHERE puuid = ?1",
            params![
                blob(&id),
                rank.tier,
                rank.rr,
                rank.peak,
                rank.games,
                rank.last_change,
                rank.last_match.as_ref().map(blob),
                now
            ],
        );
    }

    pub fn rank(&self, id: PlayerId) -> Option<Rank> {
        self.db
            .prepare_cached(
                "SELECT tier, rr, peak, games, last_change, last_match, rank_at FROM players
                 WHERE puuid = ?1 AND rank_at IS NOT NULL",
            )
            .and_then(|mut q| {
                q.query_row([blob(&id)], |r| {
                    Ok(Rank {
                        tier: r.get::<_, Option<u8>>(0)?.unwrap_or(0),
                        rr: r.get::<_, Option<i32>>(1)?.unwrap_or(0),
                        peak: r.get::<_, Option<u8>>(2)?.unwrap_or(0),
                        games: r.get::<_, Option<u32>>(3)?.unwrap_or(0),
                        last_change: r.get::<_, Option<i32>>(4)?.unwrap_or(0),
                        last_match: r.get::<_, Option<Vec<u8>>>(5)?.and_then(|b| Uuid::from_bytes(&b)),
                        fetched_at: r.get(6)?,
                    })
                })
                .optional()
            })
            .ok()
            .flatten()
    }

    pub fn mark_history(&self, id: PlayerId, now: i64) {
        self.touch(id, now);
        let _ = self.db.execute("UPDATE players SET history_at = ?2 WHERE puuid = ?1", params![blob(&id), now]);
    }

    pub fn history_at(&self, id: PlayerId) -> Option<i64> {
        self.db
            .prepare_cached("SELECT history_at FROM players WHERE puuid = ?1")
            .and_then(|mut q| q.query_row([blob(&id)], |r| r.get::<_, Option<i64>>(0)).optional())
            .ok()
            .flatten()
            .flatten()
    }

    pub fn note(&self, id: PlayerId) -> String {
        self.db
            .prepare_cached("SELECT note FROM players WHERE puuid = ?1")
            .and_then(|mut q| q.query_row([blob(&id)], |r| r.get::<_, Option<String>>(0)).optional())
            .ok()
            .flatten()
            .flatten()
            .unwrap_or_default()
    }

    pub fn set_note(&self, id: PlayerId, text: &str, now: i64) {
        self.touch(id, now);
        let text = text.trim();
        let value = (!text.is_empty()).then_some(text);
        let _ = self.db.execute("UPDATE players SET note = ?2 WHERE puuid = ?1", params![blob(&id), value]);
    }

    // ---- match lines ----

    fn insert_game(&self, id: PlayerId, g: &Game) -> Res<usize> {
        self.db
            .prepare_cached(
                "INSERT OR IGNORE INTO games(puuid, match_id, started_at, map, agent, won, rounds, kills, deaths,
                                             assists, score, damage, head, body, leg, tier)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )?
            .execute(params![
                blob(&id),
                blob(&g.match_id),
                g.started_at,
                g.map,
                g.agent,
                g.won,
                g.rounds,
                g.kills,
                g.deaths,
                g.assists,
                g.score,
                g.damage,
                g.head,
                g.body,
                g.leg,
                g.tier
            ])
    }

    /// Stores one player's match lines. Lines already known are left alone.
    pub fn save_games(&self, id: PlayerId, games: &[Game]) -> usize {
        let _ = self.db.execute_batch("BEGIN");
        let added = games.iter().filter_map(|g| self.insert_game(id, g).ok()).sum();
        let _ = self.db.execute_batch("COMMIT");
        added
    }

    pub fn summary(&self, id: PlayerId, limit: u32) -> Summary {
        let mut out = Summary::default();
        let Ok(mut q) = self.db.prepare_cached(
            "SELECT won, rounds, kills, deaths, damage, head, body, leg, agent FROM games
             WHERE puuid = ?1 ORDER BY started_at DESC LIMIT ?2",
        ) else {
            return out;
        };
        let rows = q.query_map(params![blob(&id), limit], |r| {
            Ok((
                r.get::<_, u8>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, u32>(2)?,
                r.get::<_, u32>(3)?,
                r.get::<_, u32>(4)?,
                r.get::<_, u32>(5)?,
                r.get::<_, u32>(6)?,
                r.get::<_, u32>(7)?,
                r.get::<_, String>(8)?,
            ))
        });
        let Ok(rows) = rows else { return out };
        for (won, rounds, kills, deaths, damage, head, body, leg, agent) in rows.flatten() {
            out.games += 1;
            out.wins += u32::from(won == 1);
            out.rounds += rounds;
            out.kills += kills;
            out.deaths += deaths;
            out.damage += damage;
            out.head += head;
            out.shots += head + body + leg;
            if out.last5.len() < 5 {
                out.last5.push(won);
            }
            match out.agents.iter_mut().find(|(name, _, _)| *name == agent) {
                Some(entry) => {
                    entry.1 += 1;
                    entry.2 += u32::from(won == 1);
                }
                None => out.agents.push((agent, 1, u32::from(won == 1))),
            }
        }
        out.agents.sort_by(|a, b| b.1.cmp(&a.1));
        out
    }

    /// How one player has done per map over their recent matches, most played first.
    pub fn maps(&self, id: PlayerId, limit: u32) -> Vec<MapStat> {
        let Ok(mut q) = self.db.prepare_cached(
            "SELECT map, COUNT(*), SUM(won = 1), SUM(kills), SUM(deaths), SUM(damage), SUM(rounds)
             FROM (SELECT * FROM games WHERE puuid = ?1 ORDER BY started_at DESC LIMIT ?2)
             GROUP BY map ORDER BY COUNT(*) DESC, map",
        ) else {
            return Vec::new();
        };
        q.query_map(params![blob(&id), limit], |r| {
            Ok(MapStat {
                name: r.get(0)?,
                games: r.get(1)?,
                wins: r.get(2)?,
                kills: r.get(3)?,
                deaths: r.get(4)?,
                damage: r.get(5)?,
                rounds: r.get(6)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// Agents one player has used on a map: name, matches, wins.
    pub fn agents_on_map(&self, id: PlayerId, map: &str) -> Vec<(String, u32, u32)> {
        let Ok(mut q) = self.db.prepare_cached(
            "SELECT agent, COUNT(*), SUM(won = 1) FROM games WHERE puuid = ?1 AND map = ?2
             GROUP BY agent ORDER BY COUNT(*) DESC, agent",
        ) else {
            return Vec::new();
        };
        q.query_map(params![blob(&id), map], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// My attack and defense rounds on a map: (attack played, attack won, defense played, defense won).
    pub fn sides(&self, map: &str) -> (u32, u32, u32, u32) {
        self.db
            .prepare_cached(
                "SELECT COALESCE(SUM(atk_rounds), 0), COALESCE(SUM(atk_won), 0),
                        COALESCE(SUM(def_rounds), 0), COALESCE(SUM(def_won), 0)
                 FROM my_games WHERE map = ?1 AND result IN (0, 1, 2) AND COALESCE(queue, '') != 'deathmatch'",
            )
            .and_then(|mut q| q.query_row([map], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))))
            .unwrap_or_default()
    }

    // ---- my own matches ----

    /// Called when a match of mine loads. Remembers who was in it: visible players as
    /// encounters, hidden slots as side + agent. Safe to call again for the same match.
    pub fn begin_game(
        &self,
        match_id: Uuid,
        now: i64,
        map: &str,
        queue: &str,
        agent: &str,
        hidden: &[(Side, Uuid)],
        others: &[(PlayerId, Side, String, u8)],
    ) {
        let _ = self.db.execute_batch("BEGIN");
        let _ = self.db.execute(
            "INSERT OR IGNORE INTO my_games(match_id, seen_at, map, queue, agent) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![blob(&match_id), now, map, queue, agent],
        );
        for (side, agent) in hidden {
            let _ = self.db.execute(
                "INSERT OR IGNORE INTO hidden_slots(match_id, side, agent) VALUES(?1, ?2, ?3)",
                params![blob(&match_id), side_number(*side), blob(agent)],
            );
        }
        for (id, side, agent, party) in others {
            self.touch(*id, now);
            let _ = self.db.execute(
                "INSERT OR IGNORE INTO encounters(puuid, match_id, relation, agent, party) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![blob(id), blob(&match_id), side_number(*side), agent, party],
            );
        }
        let _ = self.db.execute_batch("COMMIT");
    }

    pub fn set_estimate(&self, match_id: Uuid, ally_avg: i32, enemy_avg: i32, percent: Option<i32>) {
        let _ = self.db.execute(
            "UPDATE my_games SET ally_avg = ?2, enemy_avg = ?3, estimate = ?4 WHERE match_id = ?1 AND result IS NULL",
            params![blob(&match_id), ally_avg, enemy_avg, percent],
        );
    }

    /// The newest match of mine that has no result yet, if it was seen after `since`.
    pub fn open_game(&self, since: i64) -> Option<(Uuid, i64)> {
        self.db
            .prepare_cached(
                "SELECT match_id, seen_at FROM my_games
                 WHERE result IS NULL AND seen_at >= ?1 AND COALESCE(queue, '') != 'deathmatch'
                 ORDER BY seen_at DESC LIMIT 1",
            )
            .and_then(|mut q| q.query_row([since], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))).optional())
            .ok()
            .flatten()
            .and_then(|(id, at)| Some((Uuid::from_bytes(&id)?, at)))
    }

    pub fn hidden_slots(&self, match_id: Uuid) -> Vec<(Side, Uuid)> {
        let Ok(mut q) = self.db.prepare_cached("SELECT side, agent FROM hidden_slots WHERE match_id = ?1") else {
            return Vec::new();
        };
        q.query_map([blob(&match_id)], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
            .map(|rows| {
                rows.flatten()
                    .filter_map(|(side, agent)| {
                        Some((if side == 0 { Side::Ally } else { Side::Enemy }, Uuid::from_bytes(&agent)?))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn has_game(&self, match_id: Uuid) -> bool {
        self.db
            .prepare_cached("SELECT 1 FROM my_games WHERE match_id = ?1")
            .and_then(|mut q| q.exists([blob(&match_id)]))
            .unwrap_or(false)
    }

    /// Stores my finished match. `live` is the match Sightline watched, if it did: only then are
    /// the other players kept (their line in this match, their name, and, with `remember`, that
    /// we met). For a match it never saw live, my own line is the only one stored.
    ///
    /// Players who were hidden live are kept like the others here, from this one match only: the
    /// game itself names them once the match is over. Nothing is ever looked up about them. With
    /// `name_hidden` off they stay nameless on the scoreboard and nothing about them is kept.
    pub fn finish_game(&self, m: &FullMatch, me: PlayerId, live: Option<Uuid>, remember: bool, name_hidden: bool) {
        let _ = self.db.execute_batch("BEGIN");
        let watched = match live {
            Some(live) if live != m.match_id && self.has_game(live) && !self.has_game(m.match_id) => {
                // The lobby and the finished match were given different ids: carry the rows over.
                for table in ["my_games", "encounters", "hidden_slots"] {
                    let _ = self.db.execute(
                        &format!("UPDATE {table} SET match_id = ?2 WHERE match_id = ?1"),
                        params![blob(&live), blob(&m.match_id)],
                    );
                }
                true
            }
            _ => self.has_game(m.match_id),
        };
        let _ = self.db.execute(
            "INSERT OR IGNORE INTO my_games(match_id, seen_at, map, queue, agent) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![blob(&m.match_id), m.started_at, m.map, m.queue, m.mine.agent],
        );
        let (g, d) = (&m.mine, &m.detail);
        let _ = self.db.execute(
            "UPDATE my_games SET map = ?2, queue = ?3, agent = ?4, result = ?5, rounds_won = ?6, rounds_lost = ?7,
                kills = ?8, deaths = ?9, assists = ?10, damage = ?11, rounds = ?12, head = ?13, shots = ?14,
                first_kills = ?15, first_deaths = ?16, clutch_tries = ?17, clutch_wins = ?18,
                atk_rounds = ?19, atk_won = ?20, def_rounds = ?21, def_won = ?22
             WHERE match_id = ?1",
            params![
                blob(&m.match_id),
                m.map,
                m.queue,
                g.agent,
                m.result,
                m.rounds_won,
                m.rounds_lost,
                g.kills,
                g.deaths,
                g.assists,
                g.damage,
                g.rounds,
                g.head,
                g.head + g.body + g.leg,
                d.first_kills,
                d.first_deaths,
                d.clutch_tries,
                d.clutch_wins,
                d.atk_rounds,
                d.atk_won,
                d.def_rounds,
                d.def_won
            ],
        );
        if m.queue == "competitive" {
            for p in m.players.iter().filter(|p| p.id == me || (watched && (name_hidden || !p.was_hidden))) {
                let _ = self.insert_game(p.id, &p.game);
            }
        }
        if watched {
            let mut order: Vec<&crate::model::MatchPlayer> = m.players.iter().collect();
            order.sort_by(|a, b| b.game.score.cmp(&a.game.score));
            for (slot, p) in order.iter().enumerate() {
                let g = &p.game;
                // Kept nameless when the setting says so: the line has numbers and an agent only.
                let named = name_hidden || !p.was_hidden;
                let (name, tag) = if named { (p.name.as_str(), p.tag.as_str()) } else { ("", "") };
                let _ = self.db.execute(
                    "INSERT OR REPLACE INTO board(match_id, slot, name, tag, ally, me, was_hidden, agent, tier, kills,
                                                  deaths, assists, score, damage, head, shots, rounds)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                    params![
                        blob(&m.match_id),
                        slot as i64,
                        name,
                        tag,
                        p.ally,
                        p.id == me,
                        p.was_hidden,
                        g.agent,
                        g.tier,
                        g.kills,
                        g.deaths,
                        g.assists,
                        g.score,
                        g.damage,
                        g.head,
                        g.head + g.body + g.leg,
                        g.rounds
                    ],
                );
                if p.id != me && named && !p.name.is_empty() {
                    self.set_name(p.id, &p.name, &p.tag, m.started_at);
                }
                // Someone who was hidden live could not be written down as met when the match
                // started; now that the match has named them, they can.
                if remember && named && p.was_hidden {
                    let _ = self.db.execute(
                        "INSERT OR IGNORE INTO encounters(puuid, match_id, relation, agent, party) VALUES(?1, ?2, ?3, ?4, 0)",
                        params![blob(&p.id), blob(&m.match_id), i64::from(!p.ally), g.agent],
                    );
                }
            }
            // Scoreboards are kept for the thirty newest matches only.
            let _ = self.db.execute(
                "DELETE FROM board WHERE match_id NOT IN (SELECT match_id FROM my_games ORDER BY seen_at DESC LIMIT 30)",
                [],
            );
        }
        let _ = self.db.execute_batch("COMMIT");
    }

    /// The scoreboard of my newest finished match, best score first, if it was seen after `since`.
    pub fn latest_board(&self, since: i64) -> Vec<BoardLine> {
        let Ok(mut q) = self.db.prepare_cached(
            "SELECT name, tag, ally, me, was_hidden, agent, tier, kills, deaths, assists, score, damage, head, shots, rounds
             FROM board WHERE match_id = (
                 SELECT match_id FROM my_games WHERE result IN (0, 1, 2) AND seen_at >= ?1
                 AND COALESCE(queue, '') != 'deathmatch' ORDER BY seen_at DESC LIMIT 1)
             ORDER BY slot",
        ) else {
            return Vec::new();
        };
        q.query_map([since], |r| {
            Ok(BoardLine {
                name: r.get(0)?,
                tag: r.get(1)?,
                ally: r.get(2)?,
                me: r.get(3)?,
                was_hidden: r.get(4)?,
                agent: r.get(5)?,
                tier: r.get(6)?,
                kills: r.get(7)?,
                deaths: r.get(8)?,
                assists: r.get(9)?,
                score: r.get(10)?,
                damage: r.get(11)?,
                head: r.get(12)?,
                shots: r.get(13)?,
                rounds: r.get(14)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// The result of a watched match never became available. It stops counting as open, and
    /// stays out of the session numbers.
    pub fn close_unknown(&self, match_id: Uuid) {
        let _ = self.db.execute("UPDATE my_games SET result = 3 WHERE match_id = ?1 AND result IS NULL", [blob(&match_id)]);
    }

    /// Fills in the RR change of a finished match once the rank answer names it.
    pub fn apply_rr(&self, match_id: Uuid, change: i32) {
        let _ = self.db.execute(
            "UPDATE my_games SET rr_change = ?2 WHERE match_id = ?1 AND rr_change IS NULL AND queue = 'competitive'",
            params![blob(&match_id), change],
        );
    }

    /// Finished matches of mine, newest first.
    pub fn my_games(&self, limit: u32) -> Vec<MyGame> {
        let Ok(mut q) = self.db.prepare_cached(
            "SELECT seen_at, map, queue, estimate, result, rr_change, rounds_won, rounds_lost, kills, deaths, assists,
                    damage, rounds, head, shots, first_kills, first_deaths, clutch_tries, clutch_wins
             FROM my_games WHERE result IN (0, 1, 2) AND COALESCE(queue, '') != 'deathmatch'
             ORDER BY seen_at DESC LIMIT ?1",
        ) else {
            return Vec::new();
        };
        let n = |r: &rusqlite::Row<'_>, i: usize| r.get::<_, Option<u32>>(i).map(|v| v.unwrap_or(0));
        q.query_map([limit], |r| {
            Ok(MyGame {
                seen_at: r.get(0)?,
                map: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                queue: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                estimate: r.get(3)?,
                result: r.get(4)?,
                rr_change: r.get(5)?,
                rounds_won: n(r, 6)?,
                rounds_lost: n(r, 7)?,
                kills: n(r, 8)?,
                deaths: n(r, 9)?,
                assists: n(r, 10)?,
                damage: n(r, 11)?,
                rounds: n(r, 12)?,
                head: n(r, 13)?,
                shots: n(r, 14)?,
                first_kills: n(r, 15)?,
                first_deaths: n(r, 16)?,
                clutch_tries: n(r, 17)?,
                clutch_wins: n(r, 18)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// How often I have met this player before, not counting the match named in `except`.
    pub fn seen(&self, id: PlayerId, except: Option<Uuid>) -> Seen {
        let mut out = Seen::default();
        let Ok(mut q) = self.db.prepare_cached(
            "SELECT e.relation, g.result, g.seen_at FROM encounters e JOIN my_games g ON g.match_id = e.match_id
             WHERE e.puuid = ?1 AND e.match_id != ?2 AND COALESCE(g.queue, '') != 'deathmatch'
             ORDER BY g.seen_at DESC LIMIT 100",
        ) else {
            return out;
        };
        let except = except.map(|id| id.bytes().to_vec()).unwrap_or_default();
        let rows = q.query_map(params![blob(&id), except], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<u8>>(1)?, r.get::<_, i64>(2)?))
        });
        let Ok(rows) = rows else { return out };
        for (relation, result, at) in rows.flatten() {
            let tally = if relation == 0 { &mut out.with } else { &mut out.against };
            tally.games += 1;
            tally.wins += u32::from(result == Some(1));
            tally.losses += u32::from(result == Some(0));
            out.last_at = out.last_at.max(at);
        }
        out
    }

    pub fn counts(&self) -> (u32, u32) {
        let one = |sql: &str| self.db.query_row(sql, [], |r| r.get::<_, u32>(0)).unwrap_or(0);
        (one("SELECT COUNT(*) FROM players"), one("SELECT COUNT(DISTINCT match_id) FROM games"))
    }

    /// Every piece of text and every player id in the database. Tests use it to prove that
    /// nothing about a hidden player was written.
    #[cfg(test)]
    pub fn dump(&self) -> (String, Vec<Uuid>) {
        let mut text = String::new();
        let mut ids = Vec::new();
        for sql in [
            "SELECT COALESCE(name, '') || ' ' || COALESCE(tag, '') || ' ' || COALESCE(note, '') FROM players",
            "SELECT map || ' ' || agent FROM games",
            "SELECT COALESCE(agent, '') FROM encounters",
            "SELECT key || ' ' || value FROM meta",
        ] {
            let mut q = self.db.prepare(sql).unwrap();
            for line in q.query_map([], |r| r.get::<_, String>(0)).unwrap().flatten() {
                text.push_str(&line);
                text.push('\n');
            }
        }
        for sql in ["SELECT puuid FROM players", "SELECT puuid FROM games", "SELECT puuid FROM encounters"] {
            let mut q = self.db.prepare(sql).unwrap();
            ids.extend(q.query_map([], |r| r.get::<_, Vec<u8>>(0)).unwrap().flatten().filter_map(|b| Uuid::from_bytes(&b)));
        }
        (text, ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MatchPlayer, MyDetail};

    fn id(n: u8) -> Uuid {
        Uuid::parse(&format!("00000000-0000-4000-8000-0000000000{n:02x}")).unwrap()
    }

    fn game(n: u8, at: i64, map: &str, agent: &str, won: u8) -> Game {
        Game {
            match_id: id(100 + n),
            started_at: at,
            map: map.into(),
            agent: agent.into(),
            won,
            rounds: 20,
            kills: 20,
            deaths: 10,
            assists: 4,
            score: 5000,
            damage: 3000,
            head: 10,
            body: 28,
            leg: 2,
            tier: 21,
        }
    }

    #[test]
    fn settings_and_identity_round_trip() {
        let s = Store::open(None).unwrap();
        assert_eq!(s.setting("lookup"), None);
        s.set_setting("lookup", "off");
        s.set_setting("lookup", "on");
        assert_eq!(s.setting("lookup").as_deref(), Some("on"));
        assert_eq!(s.me(), None);
        s.set_me(id(1));
        assert_eq!(s.me(), Some(id(1)));
    }

    #[test]
    fn a_new_install_keeps_hidden_players_nameless_and_an_existing_one_keeps_its_choice() {
        let fresh = Store::open(None).unwrap();
        fresh.adopt();
        assert_eq!(fresh.setting("after").as_deref(), Some("off"));
        let used = Store::open(None).unwrap();
        used.finish_game(&finished(id(60), 2000, 1), id(1), None, true, true);
        used.adopt();
        assert_eq!(used.setting("after").as_deref(), Some("on"));
        // A choice already made is never overwritten.
        used.set_setting("after", "off");
        used.adopt();
        assert_eq!(used.setting("after").as_deref(), Some("off"));
    }

    #[test]
    fn rank_and_notes_round_trip() {
        let s = Store::open(None).unwrap();
        assert!(s.rank(id(2)).is_none());
        s.touch(id(2), 10);
        assert!(s.rank(id(2)).is_none(), "seen, but never ranked");
        let rank = Rank { tier: 22, rr: 61, peak: 24, last_change: -14, last_match: Some(id(9)), games: 300, fetched_at: 0 };
        s.save_rank(id(2), &rank, 500);
        assert_eq!(s.rank(id(2)), Some(Rank { fetched_at: 500, ..rank }));
        s.set_note(id(2), "  shot caller  ", 600);
        assert_eq!(s.note(id(2)), "shot caller");
        s.set_note(id(2), "   ", 700);
        assert_eq!(s.note(id(2)), "");
        assert_eq!(s.history_at(id(2)), None);
        s.mark_history(id(2), 800);
        assert_eq!(s.history_at(id(2)), Some(800));
    }

    #[test]
    fn match_lines_are_stored_once_and_summed() {
        let s = Store::open(None).unwrap();
        let games = [
            game(1, 300, "Ascent", "Jett", 1),
            game(2, 200, "Ascent", "Omen", 0),
            game(3, 100, "Bind", "Jett", 1),
        ];
        assert_eq!(s.save_games(id(2), &games), 3);
        assert_eq!(s.save_games(id(2), &games), 0, "a known match is never written twice");
        let sum = s.summary(id(2), 10);
        assert_eq!((sum.games, sum.wins, sum.rounds, sum.kills, sum.deaths), (3, 2, 60, 60, 30));
        assert_eq!((sum.head, sum.shots, sum.damage), (30, 120, 9000));
        assert_eq!(sum.last5, [1, 0, 1], "newest first");
        assert_eq!(sum.agents[0], ("Jett".to_string(), 2, 2));
        let maps = s.maps(id(2), 50);
        assert_eq!((maps[0].name.as_str(), maps[0].games, maps[0].wins), ("Ascent", 2, 1));
        assert_eq!(s.agents_on_map(id(2), "Ascent").len(), 2);
        assert_eq!(s.summary(id(3), 10), Summary::default());
    }

    fn finished(match_id: Uuid, at: i64, result: u8) -> FullMatch {
        let mine = Game { match_id, won: result, ..game(0, at, "Ascent", "Omen", result) };
        let line = |n: u8, ally: bool| MatchPlayer {
            id: id(n),
            name: format!("player {n}"),
            tag: "t".into(),
            ally,
            was_hidden: n == 7,
            game: Game { match_id, score: 4000 + u32::from(n), ..game(0, at, "Ascent", "Jett", 1) },
        };
        FullMatch {
            match_id,
            started_at: at,
            map: "Ascent".into(),
            queue: "competitive".into(),
            rounds_won: 13,
            rounds_lost: 7,
            result,
            mine: mine.clone(),
            detail: MyDetail { first_kills: 3, clutch_tries: 2, clutch_wins: 1, atk_rounds: 12, atk_won: 8, def_rounds: 8, def_won: 5, ..Default::default() },
            players: vec![
                MatchPlayer { id: id(1), name: "me".into(), tag: "t".into(), ally: true, was_hidden: false, game: mine },
                line(2, true),
                line(6, false),
                line(7, false),
            ],
        }
    }

    #[test]
    fn a_watched_match_keeps_encounters_and_results() {
        let s = Store::open(None).unwrap();
        let others = [(id(2), Side::Ally, "Jett".to_string(), 1u8), (id(6), Side::Enemy, "Jett".to_string(), 0u8)];
        let sova = Uuid::parse("320b2a48-4d9b-a075-30f1-1f93a9b638fa").unwrap();
        s.begin_game(id(50), 1000, "Ascent", "competitive", "Omen", &[(Side::Enemy, sova)], &others);
        s.begin_game(id(50), 1001, "Ascent", "competitive", "Omen", &[(Side::Enemy, sova)], &others);
        assert_eq!(s.open_game(0), Some((id(50), 1000)));
        assert_eq!(s.hidden_slots(id(50)), [(Side::Enemy, sova)]);
        // Before the result is known the meeting counts, without a win or a loss.
        assert_eq!(s.seen(id(2), None).with, Tally { games: 1, wins: 0, losses: 0 });
        assert_eq!(s.seen(id(2), Some(id(50))), Seen::default(), "the current match is not 'before'");

        // The finished match arrives under a different id: the rows follow it.
        s.set_estimate(id(50), 2100, 2050, Some(56));
        s.finish_game(&finished(id(51), 1005, 1), id(1), Some(id(50)), true, true);
        assert_eq!(s.open_game(0), None);
        // The scoreboard has everyone, best score first, and names the player who was hidden live.
        let board = s.latest_board(0);
        assert_eq!(board.len(), 4);
        assert_eq!(board[0].name, "me", "my 5000 is the best score here");
        assert_eq!((board[1].name.as_str(), board[1].was_hidden, board[1].ally), ("player 7", true, false));
        assert!(board.iter().any(|l| l.me && l.name == "me"));
        // Now that the match has named them, they count as met, like everyone else in it.
        assert_eq!(s.seen(id(7), None).against, Tally { games: 1, wins: 1, losses: 0 });
        assert_eq!(s.hidden_slots(id(51)), [(Side::Enemy, sova)]);
        assert_eq!(s.seen(id(6), None).against, Tally { games: 1, wins: 1, losses: 0 });
        s.apply_rr(id(51), 21);
        s.apply_rr(id(51), 99);
        let games = s.my_games(10);
        assert_eq!(games.len(), 1);
        assert_eq!((games[0].result, games[0].rr_change, games[0].estimate), (Some(1), Some(21), Some(56)));
        assert_eq!((games[0].clutch_tries, games[0].clutch_wins, games[0].first_kills), (2, 1, 3));
        assert_eq!(s.sides("Ascent"), (12, 8, 8, 5));
        // Because the match was watched, the visible players' lines were kept too.
        assert_eq!(s.summary(id(6), 10).games, 1);
    }

    #[test]
    fn with_names_after_the_match_off_a_hidden_player_stays_nameless() {
        let s = Store::open(None).unwrap();
        s.begin_game(id(50), 1000, "Ascent", "competitive", "Omen", &[], &[]);
        s.finish_game(&finished(id(50), 1005, 1), id(1), Some(id(50)), true, false);
        let board = s.latest_board(0);
        assert_eq!(board.len(), 4, "their numbers are still on the scoreboard");
        let nameless = board.iter().find(|l| l.was_hidden).unwrap();
        assert!(nameless.name.is_empty() && nameless.tag.is_empty() && nameless.kills > 0);
        // Nothing about them is kept: no name, no match line, not "met".
        let (text, _) = s.dump();
        assert!(!text.contains("player 7"));
        assert_eq!(s.summary(id(7), 10).games, 0);
        assert_eq!(s.seen(id(7), None), Seen::default());
        // The visible players are unaffected.
        assert_eq!(s.summary(id(6), 10).games, 1);
    }

    #[test]
    fn a_match_never_watched_stores_only_my_line() {
        let s = Store::open(None).unwrap();
        s.finish_game(&finished(id(60), 2000, 0), id(1), None, true, true);
        assert_eq!(s.my_games(10).len(), 1);
        assert!(s.latest_board(0).is_empty(), "no scoreboard for a match that was not watched");
        assert_eq!(s.seen(id(7), None), Seen::default());
        assert_eq!(s.summary(id(1), 10).games, 1);
        assert_eq!(s.summary(id(2), 10).games, 0, "who was hidden there is unknown, so nobody else is kept");
        assert_eq!(s.summary(id(6), 10).games, 0);
        assert_eq!(s.seen(id(6), None), Seen::default());
    }
}
