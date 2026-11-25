//! Made-up data that looks like the real thing. Used by tests and by `--fixture`, which runs the
//! whole app without the game. It goes through the same parsing and the same hidden-player rule
//! as live data, and never touches the network.

use super::assets::Catalog;
use super::wire;
use super::{Budget, DataSource, SourceError, StatsSource};
use crate::model::{FullMatch, Game, MyDetail, Name, Phase, PlayerId, Rank, Role, Roster, Side, Signal, Stage, Uuid};
use crate::store::Store;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

const PREGAME: &str = include_str!("../../fixtures/pregame.json");
const COREGAME: &str = include_str!("../../fixtures/coregame.json");

const ME: &str = "00000000-0000-4000-8000-000000000001";

const NAMES: [(&str, &str, &str); 10] = [
    ("00000000-0000-4000-8000-000000000001", "Rique", "EUW"),
    ("00000000-0000-4000-8000-000000000002", "nightowl", "0001"),
    ("00000000-0000-4000-8000-000000000003", "must stay hidden", "leak"),
    ("00000000-0000-4000-8000-000000000004", "kz", "kzkz"),
    ("00000000-0000-4000-8000-000000000005", "abc", "123"),
    ("00000000-0000-4000-8000-000000000006", "wholesome jett", "meow"),
    ("00000000-0000-4000-8000-000000000007", "one tap only", "tap"),
    ("00000000-0000-4000-8000-000000000008", "must stay hidden too", "leak"),
    ("00000000-0000-4000-8000-000000000009", "a very long name", "12345"),
    ("00000000-0000-4000-8000-00000000000a", "quiet", "shh"),
];

/// (player, party): me + 2 are a party, 6 + 7 are a duo.
const PARTIES: [(&str, &str); 4] = [
    ("00000000-0000-4000-8000-000000000001", "party-a"),
    ("00000000-0000-4000-8000-000000000002", "party-a"),
    ("00000000-0000-4000-8000-000000000006", "party-b"),
    ("00000000-0000-4000-8000-000000000007", "party-b"),
];

/// The cast for pictures: the same twelve made-up players under names that read like real ones,
/// each on a different agent. Chosen with a `show:` prefix on the mode, as in `show:match`.
/// Name, tag, agent id, agent.
const SHOW: [(&str, &str, &str, &str); 12] = [
    ("Rique", "EUW", "8e253930-4c05-31dd-1b6c-968525494517", "Omen"),
    ("nightowl", "0001", "add6443a-41bd-e414-f6ad-e58d267f4e95", "Jett"),
    ("must stay hidden", "leak", "320b2a48-4d9b-a075-30f1-1f93a9b638fa", "Sova"),
    ("Marlowe", "mrl", "1e58de9c-4950-5125-93e9-a0aee9f98746", "Killjoy"),
    ("tidepool", "wave", "569fdd95-4d10-43ab-ca70-79becc718b46", "Sage"),
    ("glasswing", "gls", "a3bfb853-43b2-7238-a4f1-ad90e9e46bcc", "Reyna"),
    ("Kestrel", "kes", "117ed9e3-49f3-6512-3ccf-0cada7e3823b", "Cypher"),
    ("must stay hidden too", "leak", "5f8d3a7f-467b-97f3-062c-13acf203c006", "Breach"),
    ("paper tiger", "roar", "f94c3b30-42be-e959-889c-5aa313dba261", "Raze"),
    ("duskfall", "dusk", "1dbf2edd-4729-0984-3115-daa5eed44993", "Clove"),
    ("mossback", "4040", "dade69b4-4f5a-8528-247b-219e5a1facd6", "Fade"),
    ("tenfold", "x10", "bb2a4828-46eb-8cd1-e765-15848195d751", "Neon"),
];

/// Splits `show:match` into (true, "match"); any other mode is (false, mode).
fn showcase(mode: &str) -> (bool, &str) {
    match mode.strip_prefix("show:") {
        Some(inner) => (true, inner),
        None => (false, mode),
    }
}

/// Puts the showcase cast's agents on the players of a roster answer. A player who has not
/// picked yet stays that way.
fn recast(players: &mut [wire::RawPlayer]) {
    for p in players {
        // All twelve: the two extra deathmatch players are in the cast as well.
        let cast = Uuid::parse(&p.subject)
            .and_then(|id| (1..=12u8).find(|n| player(*n) == id))
            .map(|n| SHOW[usize::from(n - 1)]);
        if let (Some((_, _, agent, _)), false) = (cast, p.character_id.as_deref().unwrap_or("").is_empty()) {
            p.character_id = Some(agent.to_string());
        }
    }
}

pub struct Fixture {
    mode: String,
    /// The showcase cast is on.
    show: bool,
    pregame_calls: u32,
    /// Every id a name was asked for, so tests can prove hidden players were never asked about.
    pub asked: Vec<PlayerId>,
}

impl Fixture {
    /// `mode`: `tour` (default) walks through every state; `closed`, `menu`, `queue`, `lobby`,
    /// `match` or `post` go there and stay. Two more exist for checking the layout: `stress` is
    /// a match where every name is as wide as a name can be, and `refused` is a lobby whose
    /// second read is refused, which puts the error text in the footer.
    pub fn new(mode: &str) -> Self {
        let (show, mode) = showcase(mode);
        Self { mode: mode.to_string(), show, pregame_calls: 0, asked: Vec::new() }
    }

    fn me() -> Uuid {
        Uuid::parse(ME).expect("fixture id")
    }

    fn parties() -> HashMap<Uuid, String> {
        PARTIES.iter().filter_map(|(id, p)| Some((Uuid::parse(id)?, p.to_string()))).collect()
    }

    #[cfg(test)]
    pub fn pregame(all_locked: bool) -> Roster {
        Self::pregame_cast(all_locked, false)
    }

    fn pregame_cast(all_locked: bool, show: bool) -> Roster {
        let mut m: wire::PregameMatch = serde_json::from_str(PREGAME).expect("pregame fixture");
        if all_locked {
            if let Some(team) = m.ally_team.as_mut() {
                for p in &mut team.players {
                    if p.character_id.as_deref().unwrap_or("").is_empty() {
                        p.character_id = Some("f94c3b30-42be-e959-889c-5aa313dba261".into());
                    }
                    p.selection_state = Some("locked".into());
                }
            }
        }
        if let (true, Some(team)) = (show, m.ally_team.as_mut()) {
            recast(&mut team.players);
        }
        wire::pregame_roster(m, Self::me(), &Self::parties()).expect("pregame roster")
    }

    #[cfg(test)]
    pub fn coregame() -> Roster {
        Self::coregame_cast(false)
    }

    fn coregame_cast(show: bool) -> Roster {
        let mut m: wire::CoreMatch = serde_json::from_str(COREGAME).expect("coregame fixture");
        if show {
            recast(&mut m.players);
        }
        wire::core_roster(m, Self::me(), &Self::parties()).expect("core roster")
    }

    /// The same ten players as a deathmatch: the game gives everyone the same team id there.
    fn deathmatch_cast(twelve: bool, show: bool) -> Roster {
        let mut m: wire::CoreMatch = serde_json::from_str(COREGAME).expect("coregame fixture");
        if twelve {
            // Two more visible players, as a real deathmatch has. There are no stats for them,
            // which also shows what a player without any looks like.
            for (id, agent) in [("0b", "1e58de9c-4950-5125-93e9-a0aee9f98746"), ("0c", "a3bfb853-43b2-7238-a4f1-ad90e9e46bcc")] {
                let json = format!(
                    r#"{{"Subject":"00000000-0000-4000-8000-0000000000{id}","TeamID":"Blue","CharacterID":"{agent}",
                        "PlayerIdentity":{{"AccountLevel":77,"Incognito":false,"HideAccountLevel":false}}}}"#
                );
                m.players.push(serde_json::from_str(&json).expect("extra fixture player"));
            }
        }
        for p in &mut m.players {
            p.team_id = Some("Blue".into());
        }
        if show {
            recast(&mut m.players);
        }
        m.matchmaking = Some(wire::Matchmaking { queue_id: Some("deathmatch".into()) });
        wire::core_roster(m, Self::me(), &Self::parties()).expect("core roster")
    }

    pub fn catalog_data() -> Catalog {
        let mut c = Catalog::default();
        for (id, name, role) in [
            ("8e253930-4c05-31dd-1b6c-968525494517", "Omen", Role::Controller),
            ("add6443a-41bd-e414-f6ad-e58d267f4e95", "Jett", Role::Duelist),
            ("320b2a48-4d9b-a075-30f1-1f93a9b638fa", "Sova", Role::Initiator),
            ("f94c3b30-42be-e959-889c-5aa313dba261", "Raze", Role::Duelist),
            ("569fdd95-4d10-43ab-ca70-79becc718b46", "Sage", Role::Sentinel),
            // Used by the showcase cast and the twelve-player deathmatch.
            ("1e58de9c-4950-5125-93e9-a0aee9f98746", "Killjoy", Role::Sentinel),
            ("a3bfb853-43b2-7238-a4f1-ad90e9e46bcc", "Reyna", Role::Duelist),
            ("117ed9e3-49f3-6512-3ccf-0cada7e3823b", "Cypher", Role::Sentinel),
            ("5f8d3a7f-467b-97f3-062c-13acf203c006", "Breach", Role::Initiator),
            ("1dbf2edd-4729-0984-3115-daa5eed44993", "Clove", Role::Controller),
            ("dade69b4-4f5a-8528-247b-219e5a1facd6", "Fade", Role::Initiator),
            ("bb2a4828-46eb-8cd1-e765-15848195d751", "Neon", Role::Duelist),
        ] {
            c.add_agent(id, name, role);
        }
        c.add_map("/Game/Maps/Ascent/Ascent", "Ascent");
        c
    }
}

fn game(stage: Stage, matchmaking: bool) -> Signal {
    Signal::Game { stage, matchmaking, queue: "competitive".into() }
}

async fn script(mode: String, tx: mpsc::Sender<Signal>) {
    let steps: Vec<(Signal, u64)> = match mode.as_str() {
        "closed" => vec![(Signal::ClientGone, 0)],
        "menu" => vec![(game(Stage::Menus, false), 0)],
        "queue" => vec![(game(Stage::Menus, true), 0)],
        "lobby" | "refused" => vec![(game(Stage::Pregame, false), 0)],
        "match" | "stress" => {
            vec![(game(Stage::Pregame, false), 1), (game(Stage::Ingame, false), 1), (Signal::Score(7, 5), 0)]
        }
        "post" => vec![(game(Stage::Ingame, false), 2), (game(Stage::Menus, false), 0)],
        // Deathmatch has no agent select: straight from the menus into the match, then out.
        "deathmatch" => vec![
            (Signal::Game { stage: Stage::Ingame, matchmaking: false, queue: "deathmatch".into() }, 12),
            (Signal::Game { stage: Stage::Menus, matchmaking: false, queue: "deathmatch".into() }, 0),
        ],
        "dm" | "dm12" => vec![(Signal::Game { stage: Stage::Ingame, matchmaking: false, queue: "deathmatch".into() }, 0)],
        _ => vec![
            (Signal::ClientGone, 2),
            (Signal::ClientOnly, 2),
            (game(Stage::Menus, false), 3),
            (game(Stage::Menus, true), 3),
            (game(Stage::Pregame, false), 9),
            (game(Stage::Ingame, false), 8),
            (game(Stage::Menus, false), 6),
            (game(Stage::Menus, true), 0),
        ],
    };
    for (signal, hold) in steps {
        if tx.send(signal).await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_secs(hold)).await;
    }
    // Keep the channel open: the app stays in the last state.
    std::future::pending::<()>().await;
}

impl DataSource for Fixture {
    fn watch(&mut self) -> mpsc::Receiver<Signal> {
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(script(self.mode.clone(), tx));
        rx
    }

    async fn roster(&mut self, phase: Phase) -> Result<Roster, SourceError> {
        match phase {
            Phase::AgentSelect => {
                self.pregame_calls += 1;
                if self.mode == "refused" && self.pregame_calls > 1 {
                    return Err(SourceError::Http(403));
                }
                // The first looks show a lobby still picking; later ones show everyone locked in.
                Ok(Self::pregame_cast(self.pregame_calls > 2, self.show))
            }
            Phase::InGame if self.mode == "dm12" => Ok(Self::deathmatch_cast(true, self.show)),
            Phase::InGame if self.mode == "deathmatch" || self.mode == "dm" => Ok(Self::deathmatch_cast(false, self.show)),
            Phase::InGame => Ok(Self::coregame_cast(self.show)),
            _ => Err(SourceError::NotReady),
        }
    }

    async fn names(&mut self, players: &[PlayerId]) -> Result<Vec<Name>, SourceError> {
        self.asked.extend_from_slice(players);
        let stress = self.mode == "stress";
        const EXTRA: [(&str, &str, &str); 2] = [
            ("00000000-0000-4000-8000-00000000000b", "mossback", "4040"),
            ("00000000-0000-4000-8000-00000000000c", "tenfold", "x10"),
        ];
        let show = self.show;
        Ok(NAMES
            .iter()
            .chain(EXTRA.iter())
            .enumerate()
            .filter_map(|(index, (id, name, tag))| {
                let id = Uuid::parse(id)?;
                // The longest a name and tag can be, in the widest letters.
                let (name, tag) = if stress {
                    ("WWWWWWWWMMMMMMMM", "WWWWW")
                } else if show {
                    (SHOW[index].0, SHOW[index].1)
                } else {
                    (*name, *tag)
                };
                players.contains(&id).then(|| Name { id, name: name.to_string(), tag: tag.to_string() })
            })
            .collect())
    }

    async fn catalog(&mut self) -> Result<Catalog, SourceError> {
        Ok(Self::catalog_data())
    }

    fn sleep(&mut self) {
        self.pregame_calls = 0;
    }

    fn diagnostics(&self) -> String {
        if self.show {
            // As the live line reads in each state, for pictures.
            let state = match self.mode.as_str() {
                "menu" | "closed" | "tour" => "presence MENUS / DEFAULT (nested)",
                "queue" => "presence MENUS / MATCHMAKING (nested)",
                "lobby" => "presence PREGAME / DEFAULT (nested) · pre-game 5 players, 1 hidden",
                "dm12" => "presence INGAME / DEFAULT (nested) · core-game 12 players, 2 hidden",
                "deathmatch" | "dm" => "presence INGAME / DEFAULT (nested) · core-game 10 players, 2 hidden",
                "post" => "presence MENUS / DEFAULT (nested) · core-game 5 + 5 players, 2 hidden",
                _ => "presence INGAME / DEFAULT (nested) · core-game 5 + 5 players, 2 hidden",
            };
            return format!("event socket connected · 14 events · {state}");
        }
        // About as long as the live line gets, so the footer is checked with real lengths.
        "fixture data · event socket connected · 14 events · presence INGAME / DEFAULT (nested) · \
         core-game 5 + 5 players, 2 hidden"
            .to_string()
    }
}

// ---- made-up stats ---------------------------------------------------------------------------

fn clock() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn player(n: u8) -> Uuid {
    Uuid::parse(&format!("00000000-0000-4000-8000-0000000000{n:02x}")).expect("fixture id")
}

/// Which fixture player an id is: 1 to 10, in the order of `NAMES`.
fn number(id: PlayerId) -> Option<u8> {
    (1..=10u8).find(|n| player(*n) == id)
}

const AGENTS: [(&str, &str); 5] = [
    ("8e253930-4c05-31dd-1b6c-968525494517", "Omen"),
    ("add6443a-41bd-e414-f6ad-e58d267f4e95", "Jett"),
    ("320b2a48-4d9b-a075-30f1-1f93a9b638fa", "Sova"),
    ("f94c3b30-42be-e959-889c-5aa313dba261", "Raze"),
    ("569fdd95-4d10-43ab-ca70-79becc718b46", "Sage"),
];

/// Tier, RR, peak, ranked matches ever, then recent form per match: kills, deaths, headshots
/// per 100 shots, damage per round, wins out of 10. Player 9 is a new account playing far above
/// its level; player 4 is a new account playing normally, which must not be flagged.
const FORM: [(u8, i32, u8, u32, u32, u32, u32, u32, u32); 10] = [
    (22, 61, 24, 640, 19, 16, 26, 152, 6),
    (21, 34, 22, 410, 15, 15, 19, 131, 5),
    (25, 10, 25, 900, 22, 14, 30, 170, 6),
    (20, 80, 21, 250, 14, 15, 17, 128, 5),
    (18, 55, 20, 150, 16, 14, 22, 140, 6),
    (21, 12, 23, 520, 20, 15, 29, 166, 7),
    (22, 80, 22, 300, 21, 14, 24, 171, 4),
    (26, 40, 26, 990, 24, 12, 33, 190, 8),
    (19, 12, 19, 22, 28, 13, 38, 204, 8),
    (20, 40, 20, 180, 13, 16, 15, 119, 3),
];

/// Stats that answer from the table above. Records every id it was asked about, so tests can
/// prove a hidden player was never looked up.
pub struct FixtureStats {
    pub asked: Rc<RefCell<Vec<PlayerId>>>,
    /// The showcase cast is on.
    show: bool,
}

impl FixtureStats {
    #[cfg(test)]
    pub fn new() -> Self {
        Self { asked: Rc::default(), show: false }
    }

    /// For a fixture mode, which may carry the `show:` prefix.
    pub fn for_mode(mode: &str) -> Self {
        Self { asked: Rc::default(), show: showcase(mode).0 }
    }

    fn games(n: u8, show: bool) -> Vec<Game> {
        let (tier, _, _, _, kills, deaths, head, adr, wins) = FORM[usize::from(n - 1)];
        let now = clock();
        (0..10u32)
            .map(|i| {
                // Small, fixed variation so the lines are not all identical.
                let swing = (i * 7 + u32::from(n) * 3) % 5;
                let rounds = 18 + swing;
                Game {
                    match_id: Uuid::parse(&format!("000000{n:02x}-0000-4000-8000-0000000001{i:02x}")).expect("fixture id"),
                    started_at: now - i64::from(i + 1) * 40_000,
                    map: ["Ascent", "Bind", "Haven", "Ascent", "Lotus"][((i + u32::from(n)) % 5) as usize].to_string(),
                    agent: if show {
                        // Mostly their own agent, with two others mixed in.
                        SHOW[((u32::from(n) - 1 + (i / 4) * 3) % 12) as usize].3.to_string()
                    } else {
                        AGENTS[((u32::from(n) + i / 4) % 5) as usize].1.to_string()
                    },
                    // Spread the wins through the ten matches instead of bunching them.
                    won: u8::from((i * 7 + u32::from(n)) % 10 < wins),
                    rounds,
                    kills: kills + swing - 2,
                    deaths: deaths + 2 - swing.min(2),
                    assists: 4 + swing,
                    score: 200 * rounds,
                    damage: adr * rounds,
                    head,
                    body: 92 - head,
                    leg: 8,
                    tier,
                }
            })
            .collect()
    }

    /// The fixture match as the stats API would describe it once it is over, in that API's own
    /// JSON shape, so it goes through the real reading code. All ten players are in it by name,
    /// including the two who were hidden live.
    fn finished_match_json(show: bool) -> String {
        let players: Vec<String> = (1..=10u8)
            .map(|n| {
                let (tier, _, _, _, kills, deaths, head, adr, _) = FORM[usize::from(n - 1)];
                let cast = SHOW[usize::from(n - 1)];
                let (agent_id, agent) = if show { (cast.2, cast.3) } else { AGENTS[usize::from((n - 1) % 5)] };
                let (name, tag) = if show { (cast.0, cast.1) } else { (NAMES[usize::from(n - 1)].1, NAMES[usize::from(n - 1)].2) };
                format!(
                    r#"{{"puuid":"{}","name":"{name}","tag":"{tag}","team_id":"{}","party_id":"p{n}",
                        "agent":{{"id":"{agent_id}","name":"{agent}"}},"tier":{{"id":{tier},"name":""}},
                        "stats":{{"score":{},"kills":{kills},"deaths":{deaths},"assists":{},"headshots":{head},
                                  "bodyshots":{},"legshots":8,"damage":{{"dealt":{},"received":2900}}}}}}"#,
                    player(n),
                    if n <= 5 { "Blue" } else { "Red" },
                    // Combat score follows kills and damage, so the scoreboard has an order.
                    kills * 150 + adr * 13,
                    // Mine stays at 5; the others spread from 5 to 11.
                    5 + (n - 1) * 5 % 7,
                    92 - head,
                    adr * 22,
                )
            })
            .collect();
        // Blue (my team) wins 13-9, attacking first.
        let rounds: Vec<String> = (0..22u32)
            .map(|i| {
                let blue_wins = matches!(i, 0..=3 | 6 | 8 | 10 | 12..=14 | 17 | 19 | 21);
                let blue_attacks = i < 12;
                let role = if blue_wins == blue_attacks { "Attacker" } else { "Defender" };
                format!(
                    r#"{{"id":{i},"winning_team":"{}","winning_team_role":"{role}","result":"Elimination"}}"#,
                    if blue_wins { "Blue" } else { "Red" }
                )
            })
            .collect();
        let kill = |round: u32, ms: u32, killer: u8, victim: u8| {
            let team = |n: u8| if n <= 5 { "Blue" } else { "Red" };
            format!(
                r#"{{"round":{round},"time_in_round_in_ms":{ms},"killer":{{"puuid":"{}","team":"{}"}},
                    "victim":{{"puuid":"{}","team":"{}"}}}}"#,
                player(killer),
                team(killer),
                player(victim),
                team(victim)
            )
        };
        let kills = [
            // Round 0: I open it. Round 1: I am the first to fall.
            kill(0, 8_000, 1, 6),
            kill(0, 21_000, 2, 7),
            kill(1, 6_000, 9, 1),
            // Round 2: my team falls around me and I win it alone.
            kill(2, 9_000, 6, 2),
            kill(2, 14_000, 7, 3),
            kill(2, 19_000, 9, 4),
            kill(2, 23_000, 9, 5),
            kill(2, 31_000, 1, 6),
            kill(2, 36_000, 1, 7),
            // Round 4: the same, but lost.
            kill(4, 7_000, 6, 2),
            kill(4, 12_000, 7, 3),
            kill(4, 18_000, 9, 4),
            kill(4, 22_000, 9, 5),
            kill(4, 40_000, 9, 1),
        ];
        format!(
            r#"{{"metadata":{{"match_id":"00000000-0000-4000-8000-0000000000a1","map":{{"id":"m","name":"Ascent"}},
                 "started_at":"2026-01-01T00:00:00Z","is_completed":true,"queue":{{"id":"competitive","name":"Competitive"}}}},
               "players":[{}],
               "teams":[{{"team_id":"Blue","rounds":{{"won":13,"lost":9}},"won":true}},
                        {{"team_id":"Red","rounds":{{"won":9,"lost":13}},"won":false}}],
               "rounds":[{}],"kills":[{}]}}"#,
            players.join(","),
            rounds.join(","),
            kills.join(",")
        )
    }
}

impl StatsSource for FixtureStats {
    async fn rank(&mut self, id: PlayerId) -> Result<Rank, SourceError> {
        tokio::time::sleep(Duration::from_millis(120)).await;
        self.asked.borrow_mut().push(id);
        let n = number(id).ok_or(SourceError::Http(404))?;
        let (tier, rr, peak, games, ..) = FORM[usize::from(n - 1)];
        // My rank answer names the fixture match and carries the RR it gave.
        let last_match = (n == 1).then(|| Uuid::parse("00000000-0000-4000-8000-0000000000a1")).flatten();
        Ok(Rank { tier, rr, peak, last_change: 21, last_match, games, fetched_at: 0 })
    }

    async fn history(&mut self, id: PlayerId) -> Result<Vec<Game>, SourceError> {
        tokio::time::sleep(Duration::from_millis(120)).await;
        self.asked.borrow_mut().push(id);
        Ok(Self::games(number(id).ok_or(SourceError::Http(404))?, self.show))
    }

    async fn last_match(&mut self, me: PlayerId, hidden: &[(Side, Uuid)]) -> Result<Option<FullMatch>, SourceError> {
        tokio::time::sleep(Duration::from_millis(120)).await;
        let now = clock();
        let mut m = super::henrik::parse_full_match(&Self::finished_match_json(self.show), me, hidden);
        if let Some(m) = m.as_mut() {
            m.started_at = now;
            m.mine.started_at = now;
            for p in &mut m.players {
                p.game.started_at = now;
            }
        }
        Ok(m)
    }

    fn budget(&self) -> Budget {
        Budget { remaining: 30, limit: 30, reset_in: Duration::ZERO }
    }

    fn sleep(&mut self) {}

    fn settle(&self) -> Duration {
        Duration::from_secs(2)
    }
}

/// Fills an empty database with a few earlier made-up matches, so "seen before", notes and the
/// session line have something to show when running on fixture data.
pub fn seed(store: &Store) {
    let now = clock();
    let me = Fixture::me();
    store.set_me(me);
    let agent = |n: u8| AGENTS[usize::from((n - 1) % 5)].1.to_string();
    // (hours ago, result, RR, who else was there and on which side)
    let past: [(i64, u8, i32, &[(u8, Side)]); 6] = [
        (100, 1, 19, &[(2, Side::Ally), (7, Side::Enemy)]),
        (99, 0, -17, &[(2, Side::Ally), (7, Side::Enemy)]),
        (98, 1, 22, &[(2, Side::Ally)]),
        (3, 1, 18, &[(6, Side::Ally)]),
        (2, 0, -16, &[(10, Side::Enemy)]),
        (1, 1, 21, &[]),
    ];
    for (i, (hours, result, rr, others)) in past.iter().enumerate() {
        let match_id = Uuid::parse(&format!("00000000-0000-4000-8000-0000000002{i:02x}")).expect("fixture id");
        let at = now - hours * 3600;
        let others: Vec<(PlayerId, Side, String, u8)> =
            others.iter().map(|(n, side)| (player(*n), *side, agent(*n), 0)).collect();
        store.begin_game(match_id, at, "Ascent", "competitive", "Omen", &[], &others);
        let mine = Game {
            match_id,
            started_at: at,
            map: "Ascent".to_string(),
            agent: "Omen".to_string(),
            won: *result,
            rounds: 22,
            kills: 17 + i as u32,
            deaths: 15,
            assists: 6,
            score: 4800,
            damage: 150 * 22,
            head: 24,
            body: 68,
            leg: 8,
            tier: 22,
        };
        let detail = MyDetail {
            first_kills: 2,
            first_deaths: 1,
            clutch_tries: 1,
            clutch_wins: u32::from(*result == 1 && i % 2 == 0),
            atk_rounds: 11,
            atk_won: 6,
            def_rounds: 11,
            def_won: 5,
        };
        let (won, lost) = if *result == 1 { (13, 9) } else { (9, 13) };
        let finished = FullMatch {
            match_id,
            started_at: at,
            map: "Ascent".to_string(),
            queue: "competitive".to_string(),
            rounds_won: won,
            rounds_lost: lost,
            result: *result,
            mine: mine.clone(),
            detail,
            players: vec![crate::model::MatchPlayer {
                id: me,
                name: "Rique".to_string(),
                tag: "EUW".to_string(),
                ally: true,
                was_hidden: false,
                game: mine,
            }],
        };
        store.finish_game(&finished, me, Some(match_id), true, true);
        store.apply_rr(match_id, *rr);
    }
    store.set_note(player(6), "plays operator mid, pushes early", now);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pregame_fixture_applies_the_hidden_rule() {
        let r = Fixture::pregame(false);
        assert_eq!(r.slots.len(), 5);
        assert!(r.slots.iter().all(|s| s.side == Side::Ally));
        assert!(r.slots[0].is_me && r.slots[0].id.is_some());
        assert!(r.slots[1].id.is_some(), "party member stays visible");
        assert!(r.slots[2].id.is_none(), "hidden stranger");
        assert!(!r.slots[3].locked && r.slots[3].agent.is_none(), "still picking");
        assert_eq!(r.slots[4].level, None);
        assert_eq!(r.hidden, 1);
        assert!(Fixture::pregame(true).slots.iter().all(|s| s.locked && s.agent.is_some()));
    }

    #[test]
    fn the_showcase_cast_changes_names_and_agents_but_not_who_is_hidden() {
        assert_eq!(showcase("show:match"), (true, "match"));
        assert_eq!(showcase("match"), (false, "match"));
        let plain = Fixture::coregame();
        let shown = Fixture::coregame_cast(true);
        assert_eq!(shown.slots.len(), plain.slots.len());
        assert_eq!(shown.hidden, plain.hidden, "the same two players are hidden");
        for (a, b) in shown.slots.iter().zip(plain.slots.iter()) {
            assert_eq!((a.id, a.side, a.is_me), (b.id, b.side, b.is_me));
        }
        // Ten players, each on a different agent, all of them known to the fixture's catalog.
        let catalog = Fixture::catalog_data();
        let mut agents: Vec<&str> = shown.slots.iter().filter_map(|s| catalog.agent(&s.agent?)).collect();
        agents.sort_unstable();
        agents.dedup();
        assert_eq!(agents.len(), 10);
        // In agent select a player who has not picked still has not picked.
        assert!(Fixture::pregame_cast(false, true).slots[3].agent.is_none());
    }

    #[test]
    fn coregame_fixture_has_both_teams() {
        let r = Fixture::coregame();
        assert_eq!(r.slots.iter().filter(|s| s.side == Side::Ally).count(), 5);
        assert_eq!(r.slots.iter().filter(|s| s.side == Side::Enemy).count(), 5);
        assert_eq!(r.hidden, 2);
        assert_eq!(r.slots[5].party, r.slots[6].party);
        assert_ne!(r.slots[5].party, 0);
    }
}
