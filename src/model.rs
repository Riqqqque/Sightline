//! Plain data shared by every layer. Nothing here knows where the data came from.

use std::fmt;

/// A 128-bit id as Riot writes them (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`).
/// Used for players, matches, agents and maps.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Uuid([u8; 16]);

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Ids stay out of debug output: only enough to tell two apart.
        write!(f, "Uuid(..{:02x}{:02x})", self.0[14], self.0[15])
    }
}

impl Uuid {
    pub fn parse(s: &str) -> Option<Self> {
        let mut out = [0u8; 16];
        let mut n = 0;
        let mut hi: Option<u8> = None;
        for c in s.bytes() {
            if c == b'-' {
                continue;
            }
            let v = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return None,
            };
            match hi.take() {
                None => hi = Some(v),
                Some(h) => {
                    if n == 16 {
                        return None;
                    }
                    out[n] = (h << 4) | v;
                    n += 1;
                }
            }
        }
        (n == 16 && hi.is_none()).then_some(Self(out))
    }

    pub fn bytes(&self) -> &[u8; 16] {
        &self.0
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(Self(bytes.try_into().ok()?))
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, b) in self.0.iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// A player we are allowed to know. Hidden players never get one.
pub type PlayerId = Uuid;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Closed,
    Menu,
    Queue,
    AgentSelect,
    InGame,
    PostGame,
}

/// What the game client reports about itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Signal {
    /// No Riot Client on this machine right now.
    ClientGone,
    /// Riot Client is running but VALORANT is not (or is not signed in yet).
    ClientOnly,
    /// VALORANT is running.
    Game { stage: Stage, matchmaking: bool, queue: String },
    /// The round score of the match in progress changed: my team, then the other.
    Score(u32, u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Menus,
    Pregame,
    Ingame,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Side {
    Ally,
    Enemy,
}

/// One slot in the lobby or match.
///
/// `id` is `None` for a hidden player. That is the only representation of "hidden":
/// there is no id to pass to any lookup, so no later code can look one up by mistake.
#[derive(Clone, PartialEq, Eq)]
pub struct Slot {
    pub id: Option<PlayerId>,
    pub is_me: bool,
    pub side: Side,
    pub agent: Option<Uuid>,
    pub locked: bool,
    /// `None` when the player hides it, or is hidden.
    pub level: Option<u32>,
    /// 0 = solo, otherwise an index shared by party members in this match.
    pub party: u8,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Roster {
    pub match_id: Uuid,
    pub map_url: String,
    pub queue: String,
    pub slots: Vec<Slot>,
    /// Counts for the status line; no identities.
    pub hidden: u8,
    pub flag_missing: u8,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Name {
    pub id: PlayerId,
    pub name: String,
    pub tag: String,
}

/// What an agent is for on a team. Comes with the agent names; says nothing about a player.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Role {
    #[default]
    Unknown,
    Duelist,
    Initiator,
    Controller,
    Sentinel,
}

impl Role {
    pub fn parse(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "duelist" => Self::Duelist,
            "initiator" => Self::Initiator,
            "controller" => Self::Controller,
            "sentinel" => Self::Sentinel,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "",
            Self::Duelist => "Duelist",
            Self::Initiator => "Initiator",
            Self::Controller => "Controller",
            Self::Sentinel => "Sentinel",
        }
    }
}

/// What the UI shows for one player. Complete and self-contained.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Row {
    pub name: String,
    pub tag: String,
    pub hidden: bool,
    pub is_me: bool,
    /// The name lookup finished and gave nothing. Different from "still loading".
    pub unnamed: bool,
    /// Empty when no agent is chosen yet, or when the agent names could not be loaded.
    pub agent: String,
    pub role: &'static str,
    /// An agent is chosen (hovered or locked), whether or not its name is known.
    pub picked: bool,
    pub locked: bool,
    pub level: Option<u32>,
    pub party: u8,
    /// Rank, recent form, seen-before and notes. Empty for hidden players, always.
    pub card: Card,
}

/// A player's competitive rank at one moment.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rank {
    /// Riot's tier number: 0 unranked, 3 Iron 1 ... 27 Radiant.
    pub tier: u8,
    pub rr: i32,
    pub peak: u8,
    /// RR gained or lost in the most recent competitive match, and which match that was.
    pub last_change: i32,
    pub last_match: Option<Uuid>,
    /// Competitive matches played across all acts.
    pub games: u32,
    /// Unix seconds.
    pub fetched_at: i64,
}

/// One player's line in one finished match.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Game {
    pub match_id: Uuid,
    /// Unix seconds.
    pub started_at: i64,
    pub map: String,
    pub agent: String,
    /// 1 win, 0 loss, 2 draw.
    pub won: u8,
    pub rounds: u32,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub score: u32,
    pub damage: u32,
    pub head: u32,
    pub body: u32,
    pub leg: u32,
    pub tier: u8,
}

/// A visible player in a finished match of mine.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MatchPlayer {
    pub id: PlayerId,
    pub name: String,
    pub tag: String,
    pub ally: bool,
    /// Their name was hidden while the match was live. The finished match names them, as the
    /// game's own end screen does; nothing is ever looked up about them.
    pub was_hidden: bool,
    pub game: Game,
}

/// What only a full match can tell about my own play.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MyDetail {
    pub first_kills: u32,
    pub first_deaths: u32,
    /// Rounds in which I was the last one alive on my team with an enemy still up.
    pub clutch_tries: u32,
    pub clutch_wins: u32,
    pub atk_rounds: u32,
    pub atk_won: u32,
    pub def_rounds: u32,
    pub def_won: u32,
}

/// A finished match of mine, with all of its players.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FullMatch {
    pub match_id: Uuid,
    pub started_at: i64,
    pub map: String,
    pub queue: String,
    pub rounds_won: u32,
    pub rounds_lost: u32,
    /// 1 win, 0 loss, 2 draw.
    pub result: u8,
    pub mine: Game,
    pub detail: MyDetail,
    pub players: Vec<MatchPlayer>,
}

/// Display-ready extras for one player row. Every field is text or a flag the UI shows as is.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Card {
    /// Stats are looked up for this player at all (key present, lookups allowed, not hidden).
    pub stats_on: bool,
    pub rank_done: bool,
    /// "Asc 2", "Unranked", or empty when unknown.
    pub rank: String,
    /// Which rank colour to use: 0 none, 1 Iron ... 9 Radiant.
    pub rank_tint: u8,
    /// The rank as the game numbers it (3 Iron 1 ... 27 Radiant), for the badge; 0 unknown.
    pub tier: u8,
    pub rr: String,
    pub peak: String,
    pub stats_done: bool,
    /// Newest first: 1 win, 0 loss, 2 draw.
    pub dots: Vec<u8>,
    /// "KD 1.21 · HS 26% · ADR 152"
    pub stats: String,
    /// "With 3x (2-1) · 4d ago"
    pub seen: String,
    pub smurf: bool,
    pub signals: String,
    pub mains: String,
    pub on_map: String,
    pub note: String,
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct MapRow {
    pub name: String,
    /// Win rate, 0 to 100.
    pub pct: u8,
    pub line: String,
}

/// Everything on screen that is not a player row. Built by the stats worker.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Panel {
    pub my_rank: String,
    pub session_title: String,
    pub session_line: String,
    pub session_extra: String,
    pub session_dots: Vec<u8>,
    pub maps: Vec<MapRow>,
    /// 0 none, 1 win, 2 loss, 3 draw.
    pub recap_result: u8,
    pub recap_title: String,
    pub recap_line: String,
    pub recap_extra: String,
    pub nudge: String,
    pub map_line: String,
    /// My win rate on this map, "70%"; empty when unknown.
    pub map_win: String,
    /// Where the rank-gap marker sits, 0 to 100; -1 when there is no bar.
    pub gap_pct: i32,
    pub gap_label: String,
    pub stats_status: String,
    pub store_status: String,
    /// The version a click would update to; empty when up to date or not checked.
    pub update: String,
    /// What the updater last did, in words.
    pub update_status: String,
    /// Settings as stored: lookup, smurf, estimate, seen, nudge.
    pub settings: Vec<(String, String)>,
}

/// One full screen's worth of state, sent from the worker to the UI.
#[derive(Clone, PartialEq, Eq)]
pub struct View {
    pub phase: Phase,
    pub status: String,
    pub detail: String,
    pub map: String,
    pub queue: String,
    pub allies: Vec<Row>,
    pub enemies: Vec<Row>,
    /// The rows are the finished match's scoreboard, not the live roster: they show that match's
    /// numbers and cannot be selected.
    pub board: bool,
    /// The round score while a match is on, "7 - 5"; empty otherwise.
    pub score: String,
    /// The roster on screen was read from the running match and has nobody on the other side:
    /// a deathmatch, or a custom game alone. Never true while the match roster is still loading.
    pub one_list: bool,
    pub panel: Panel,
}

impl View {
    pub fn empty(phase: Phase) -> Self {
        Self {
            phase,
            status: String::new(),
            detail: String::new(),
            map: String::new(),
            queue: String::new(),
            allies: Vec::new(),
            enemies: Vec::new(),
            board: false,
            score: String::new(),
            one_list: false,
            panel: Panel { gap_pct: -1, ..Panel::default() },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_round_trip() {
        let s = "0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9";
        let u = Uuid::parse(s).unwrap();
        assert_eq!(u.to_string(), s);
        assert!(Uuid::parse(&s.to_uppercase()) == Some(u));
    }

    #[test]
    fn uuid_rejects_bad_input() {
        assert!(Uuid::parse("").is_none());
        assert!(Uuid::parse("0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f").is_none());
        assert!(Uuid::parse("0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9aa").is_none());
        assert!(Uuid::parse("zz1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9").is_none());
    }
}
