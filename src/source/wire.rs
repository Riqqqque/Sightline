//! Shapes of the game client's answers, and the pure functions that turn them into `model` types.
//! No I/O here, so all of it is unit tested and shared by the real client and the fixture.

use crate::model::{Roster, Side, Signal, Slot, Stage, Uuid};
use base64::Engine;
use serde::Deserialize;
use std::collections::HashMap;

// ---------- presence ----------

#[derive(Deserialize)]
pub struct PresenceList {
    #[serde(default)]
    pub presences: Vec<Presence>,
}

#[derive(Deserialize)]
pub struct Presence {
    // All optional: one null field must not make the whole frame unreadable.
    #[serde(default)]
    pub puuid: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub private: Option<String>,
}

impl Presence {
    pub fn id(&self) -> Option<Uuid> {
        Uuid::parse(self.puuid.as_deref()?)
    }

    pub fn is_valorant(&self) -> bool {
        self.product.as_deref() == Some("valorant")
    }
}

/// One websocket event frame: `[8, "OnJsonApiEvent_chat_v4_presences", { ... }]`.
#[derive(Deserialize)]
pub struct Frame(pub serde::de::IgnoredAny, pub serde::de::IgnoredAny, pub FrameBody);

#[derive(Deserialize)]
pub struct FrameBody {
    #[serde(default)]
    pub data: Option<PresenceList>,
    #[serde(default, rename = "eventType")]
    pub event_type: String,
}

/// The base64 `private` blob inside a presence. Riot has shipped a nested and a flat layout.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Private {
    session_loop_state: Option<String>,
    party_state: Option<String>,
    party_id: Option<String>,
    party_size: Option<u32>,
    account_level: Option<u32>,
    queue_id: Option<String>,
    provisioning_flow: Option<String>,
    party_owner_match_score_ally_team: Option<u32>,
    party_owner_match_score_enemy_team: Option<u32>,
    match_presence_data: Option<Nested>,
    party_presence_data: Option<Nested>,
    player_presence_data: Option<Nested>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Nested {
    session_loop_state: Option<String>,
    party_state: Option<String>,
    party_id: Option<String>,
    party_size: Option<u32>,
    account_level: Option<u32>,
    queue_id: Option<String>,
    provisioning_flow: Option<String>,
    party_owner_match_score_ally_team: Option<u32>,
    party_owner_match_score_enemy_team: Option<u32>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PresenceInfo {
    pub stage: Option<Stage>,
    pub loop_state: String,
    pub party_state: String,
    pub party_id: String,
    pub party_size: u32,
    pub level: Option<u32>,
    pub queue: String,
    pub nested: bool,
    /// Rounds won by my team and by the other, while a match is on.
    pub score: Option<(u32, u32)>,
}

impl PresenceInfo {
    pub fn matchmaking(&self) -> bool {
        // The exact value is confirmed in a live queue; anything that mentions matchmaking counts.
        self.party_state.to_ascii_uppercase().contains("MATCHMAKING")
    }

    pub fn signal(&self) -> Option<Signal> {
        let stage = self.stage?;
        Some(Signal::Game { stage, matchmaking: self.matchmaking(), queue: self.queue.clone() })
    }
}

pub fn parse_private(blob: &str) -> Option<PresenceInfo> {
    let blob = blob.trim();
    if blob.is_empty() {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(blob).ok()?;
    let p: Private = serde_json::from_slice(&bytes).ok()?;
    let nested = p.match_presence_data.is_some() || p.party_presence_data.is_some();
    let m = p.match_presence_data.unwrap_or_default();
    let party = p.party_presence_data.unwrap_or_default();
    let player = p.player_presence_data.unwrap_or_default();
    let loop_state = m.session_loop_state.or(p.session_loop_state).unwrap_or_default();
    // The shooting range reports itself as "in game". It is not a match: no lobby, no result.
    let flow = m.provisioning_flow.or(p.provisioning_flow).unwrap_or_default();
    let range = flow.eq_ignore_ascii_case("ShootingRange");
    let stage = match loop_state.as_str() {
        "MENUS" => Some(Stage::Menus),
        "PREGAME" => Some(Stage::Pregame),
        "INGAME" if range => Some(Stage::Menus),
        "INGAME" => Some(Stage::Ingame),
        _ => None,
    };
    let ours = party.party_owner_match_score_ally_team.or(p.party_owner_match_score_ally_team);
    let theirs = party.party_owner_match_score_enemy_team.or(p.party_owner_match_score_enemy_team);
    let score = match (stage, ours, theirs) {
        (Some(Stage::Ingame), Some(a), Some(b)) => Some((a, b)),
        _ => None,
    };
    Some(PresenceInfo {
        stage,
        loop_state,
        party_state: party.party_state.or(p.party_state).unwrap_or_default(),
        party_id: party.party_id.or(p.party_id).unwrap_or_default(),
        party_size: party.party_size.or(p.party_size).unwrap_or(0),
        level: player.account_level.or(p.account_level).or(party.account_level).or(m.account_level),
        queue: m.queue_id.or(party.queue_id).or(p.queue_id).or(player.queue_id).unwrap_or_default(),
        nested,
        score,
    })
}

/// Party id per player, for the players the local presence feed knows about.
pub fn party_ids(list: &PresenceList) -> HashMap<Uuid, String> {
    let mut out = HashMap::new();
    for p in &list.presences {
        if !p.is_valorant() {
            continue;
        }
        let (Some(id), Some(blob)) = (p.id(), p.private.as_deref()) else { continue };
        if let Some(info) = parse_private(blob) {
            if !info.party_id.is_empty() && info.party_size > 1 {
                out.insert(id, info.party_id);
            }
        }
    }
    out
}

/// My own presence, if VALORANT is running.
pub fn my_presence(list: &PresenceList, me: Uuid) -> Option<PresenceInfo> {
    list.presences
        .iter()
        .filter(|p| p.is_valorant() && p.id() == Some(me))
        .find_map(|p| parse_private(p.private.as_deref()?))
}

// ---------- local session ----------

#[derive(Deserialize)]
pub struct ChatSession {
    #[serde(default)]
    pub puuid: String,
}

#[derive(Deserialize)]
pub struct Entitlements {
    #[serde(default, rename = "accessToken")]
    pub access_token: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub subject: String,
}

// ---------- lobby and match ----------

#[derive(Deserialize)]
pub struct PlayerRef {
    #[serde(rename = "MatchID")]
    pub match_id: String,
}

#[derive(Deserialize, Default)]
pub struct RawIdentity {
    #[serde(rename = "AccountLevel")]
    pub account_level: Option<u32>,
    #[serde(rename = "Incognito")]
    pub incognito: Option<bool>,
    #[serde(rename = "HideAccountLevel")]
    pub hide_account_level: Option<bool>,
}

#[derive(Deserialize)]
pub struct RawPlayer {
    #[serde(rename = "Subject")]
    pub subject: String,
    #[serde(default, rename = "TeamID")]
    pub team_id: Option<String>,
    #[serde(default, rename = "CharacterID")]
    pub character_id: Option<String>,
    #[serde(default, rename = "CharacterSelectionState")]
    pub selection_state: Option<String>,
    #[serde(default, rename = "PlayerIdentity")]
    pub identity: Option<RawIdentity>,
}

#[derive(Deserialize, Default)]
pub struct RawTeam {
    #[serde(default, rename = "Players")]
    pub players: Vec<RawPlayer>,
}

#[derive(Deserialize)]
pub struct PregameMatch {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(default, rename = "MapID")]
    pub map_id: Option<String>,
    #[serde(default, rename = "QueueID")]
    pub queue_id: Option<String>,
    #[serde(default, rename = "AllyTeam")]
    pub ally_team: Option<RawTeam>,
    #[serde(default, rename = "EnemyTeam")]
    pub enemy_team: Option<RawTeam>,
}

#[derive(Deserialize)]
pub struct CoreMatch {
    #[serde(rename = "MatchID")]
    pub match_id: String,
    #[serde(default, rename = "MapID")]
    pub map_id: Option<String>,
    #[serde(default, rename = "MatchmakingData")]
    pub matchmaking: Option<Matchmaking>,
    #[serde(default, rename = "Players")]
    pub players: Vec<RawPlayer>,
}

#[derive(Deserialize, Default)]
pub struct Matchmaking {
    #[serde(default, rename = "QueueID")]
    pub queue_id: Option<String>,
}

#[derive(Deserialize)]
pub struct RawName {
    #[serde(rename = "Subject")]
    pub subject: String,
    #[serde(default, rename = "GameName")]
    pub game_name: String,
    #[serde(default, rename = "TagLine")]
    pub tag_line: String,
}

/// Builds the roster and applies the hidden rule. This is the only place a hidden player's id
/// exists, and it does not leave this function.
///
/// - I am never hidden.
/// - Members of my own party are shown: the game shows them to me anyway.
/// - A missing identity flag counts as hidden.
/// - Hidden players get no level and no party bracket.
pub fn build_roster(
    match_id: &str,
    map_url: &str,
    queue: &str,
    players: Vec<(Side, RawPlayer)>,
    me: Uuid,
    parties: &HashMap<Uuid, String>,
) -> Option<Roster> {
    let match_id = Uuid::parse(match_id)?;
    let my_party = parties.get(&me);
    let mut slots = Vec::with_capacity(players.len());
    let mut slot_party: Vec<Option<&String>> = Vec::with_capacity(players.len());
    let (mut hidden, mut flag_missing) = (0u8, 0u8);

    for (side, raw) in players {
        let Some(id) = Uuid::parse(&raw.subject) else { continue };
        let identity = raw.identity.unwrap_or_default();
        let is_me = id == me;
        let party = parties.get(&id);
        let in_my_party = !is_me && my_party.is_some() && party == my_party;
        if identity.incognito.is_none() && !is_me {
            flag_missing += 1;
        }
        let visible = is_me || in_my_party || identity.incognito == Some(false);
        if !visible {
            hidden += 1;
        }
        let show_level = visible && (is_me || identity.hide_account_level == Some(false));
        slots.push(Slot {
            id: visible.then_some(id),
            is_me,
            side,
            agent: raw.character_id.as_deref().and_then(Uuid::parse),
            locked: raw.selection_state.as_deref().is_none_or(|s| s == "locked"),
            level: if show_level { identity.account_level } else { None },
            party: 0,
        });
        slot_party.push(if visible { party } else { None });
    }

    // Number the parties that have at least two visible members in this match.
    let mut next = 1u8;
    let mut seen: Vec<(&String, u8)> = Vec::new();
    for i in 0..slots.len() {
        let Some(p) = slot_party[i] else { continue };
        if slot_party.iter().filter(|q| **q == Some(p)).count() < 2 {
            continue;
        }
        let index = match seen.iter().find(|(q, _)| *q == p) {
            Some((_, n)) => *n,
            None => {
                seen.push((p, next));
                next += 1;
                next - 1
            }
        };
        slots[i].party = index;
    }

    Some(Roster {
        match_id,
        map_url: map_url.to_string(),
        queue: queue.to_string(),
        slots,
        hidden,
        flag_missing,
    })
}

pub fn pregame_roster(m: PregameMatch, me: Uuid, parties: &HashMap<Uuid, String>) -> Option<Roster> {
    let mut players = Vec::new();
    for p in m.ally_team.unwrap_or_default().players {
        players.push((Side::Ally, p));
    }
    for p in m.enemy_team.unwrap_or_default().players {
        players.push((Side::Enemy, p));
    }
    build_roster(
        &m.id,
        m.map_id.as_deref().unwrap_or(""),
        m.queue_id.as_deref().unwrap_or(""),
        players,
        me,
        parties,
    )
}

pub fn core_roster(m: CoreMatch, me: Uuid, parties: &HashMap<Uuid, String>) -> Option<Roster> {
    let my_team = m
        .players
        .iter()
        .find(|p| Uuid::parse(&p.subject) == Some(me))
        .and_then(|p| p.team_id.clone());
    let players = m
        .players
        .into_iter()
        .map(|p| {
            let side = if my_team.is_some() && p.team_id == my_team { Side::Ally } else { Side::Enemy };
            (side, p)
        })
        .collect();
    let queue = m.matchmaking.unwrap_or_default().queue_id.unwrap_or_default();
    build_roster(&m.match_id, m.map_id.as_deref().unwrap_or(""), &queue, players, me, parties)
}

// ---------- region ----------

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Region {
    /// e.g. `eu`
    pub shard: String,
    /// e.g. `eu-1`
    pub glz: String,
    /// The client version string the game logged.
    pub version: String,
}

fn host_part(s: &str) -> Option<&str> {
    (!s.is_empty() && s.len() <= 16 && s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'))
        .then_some(s)
}

/// Reads the shard, the game-server region and the client version out of the game's own log.
/// Only short lowercase tokens are accepted, since they end up in host names.
pub fn parse_region<I: Iterator<Item = String>>(lines: I) -> Option<Region> {
    let (mut shard, mut glz, mut version) = (None::<String>, None::<(String, String)>, None::<String>);
    for line in lines {
        if version.is_none() {
            if let Some((_, v)) = line.split_once("CI server version: ") {
                let v = v.trim();
                if !v.is_empty() && v.len() <= 64 && v.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.') {
                    version = Some(v.to_string());
                }
            }
        }
        if shard.is_none() {
            if let Some((before, _)) = line.split_once(".a.pvp.net/account-xp/v1/") {
                if let Some(s) = before.rsplit('.').next().and_then(host_part) {
                    shard = Some(s.to_string());
                }
            }
        }
        if glz.is_none() {
            if let Some((_, after)) = line.split_once("https://glz-") {
                let mut parts = after.split('.');
                if let (Some(a), Some(b)) = (parts.next().and_then(host_part), parts.next().and_then(host_part)) {
                    glz = Some((a.to_string(), b.to_string()));
                }
            }
        }
        if shard.is_some() && glz.is_some() && version.is_some() {
            break;
        }
    }
    let (glz, glz_shard) = glz?;
    Some(Region { shard: shard.unwrap_or(glz_shard), glz, version: version? })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(json: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(json)
    }

    #[test]
    fn presence_flat_layout() {
        let p = parse_private(&b64(
            r#"{"isValid":true,"sessionLoopState":"MENUS","partyState":"MATCHMAKING","partyId":"p1","partySize":2,"accountLevel":212,"queueId":"competitive"}"#,
        ))
        .unwrap();
        assert_eq!(p.stage, Some(Stage::Menus));
        assert!(p.matchmaking());
        assert_eq!((p.party_id.as_str(), p.party_size, p.level), ("p1", 2, Some(212)));
        assert_eq!(p.queue, "competitive");
        assert!(!p.nested);
        assert_eq!(
            p.signal(),
            Some(Signal::Game { stage: Stage::Menus, matchmaking: true, queue: "competitive".into() })
        );
    }

    #[test]
    fn presence_nested_layout() {
        let p = parse_private(&b64(
            r#"{"isValid":true,"matchPresenceData":{"sessionLoopState":"INGAME","queueId":"competitive"},"partyPresenceData":{"partyState":"DEFAULT","partyId":"p9","partySize":1},"playerPresenceData":{"accountLevel":40}}"#,
        ))
        .unwrap();
        assert_eq!(p.stage, Some(Stage::Ingame));
        assert!(!p.matchmaking());
        assert_eq!((p.party_id.as_str(), p.party_size, p.level), ("p9", 1, Some(40)));
        assert!(p.nested);
    }

    #[test]
    fn presence_garbage_is_ignored() {
        assert!(parse_private("").is_none());
        assert!(parse_private("not base64 !!").is_none());
        assert!(parse_private(&b64("[1,2,3]")).is_none());
        let unknown = parse_private(&b64(r#"{"somethingNew":1}"#)).unwrap();
        assert_eq!(unknown.stage, None);
        assert_eq!(unknown.signal(), None);
    }

    fn id(n: u8) -> String {
        format!("00000000-0000-0000-0000-0000000000{n:02x}")
    }

    fn player(n: u8, team: &str, incognito: Option<bool>, hide_level: bool) -> RawPlayer {
        RawPlayer {
            subject: id(n),
            team_id: Some(team.to_string()),
            character_id: Some("add6443a-41bd-e414-f6ad-e58d267f4e95".to_string()),
            selection_state: Some("locked".to_string()),
            identity: Some(RawIdentity {
                account_level: Some(100 + n as u32),
                incognito,
                hide_account_level: Some(hide_level),
            }),
        }
    }

    #[test]
    fn hidden_rule() {
        let me = Uuid::parse(&id(1)).unwrap();
        let mut parties = HashMap::new();
        // me + player 2 are a party; players 4 and 5 are a party; players 6 and 7 are a party.
        for (n, party) in [(1, "mine"), (2, "mine"), (4, "duo"), (5, "duo"), (6, "half"), (7, "half")] {
            parties.insert(Uuid::parse(&id(n)).unwrap(), party.to_string());
        }
        let players = vec![
            (Side::Ally, player(1, "Blue", Some(true), true)),   // me, with my own name hidden
            (Side::Ally, player(2, "Blue", Some(true), false)),  // my party member, hidden to others
            (Side::Ally, player(3, "Blue", Some(true), false)),  // hidden stranger
            (Side::Enemy, player(4, "Red", Some(false), false)), // visible duo
            (Side::Enemy, player(5, "Red", Some(false), true)),  // visible duo, level hidden
            (Side::Enemy, player(6, "Red", Some(false), false)), // duo with a hidden player
            (Side::Enemy, player(7, "Red", Some(true), false)),  // the hidden half of that duo
            (Side::Enemy, player(8, "Red", None, false)),        // flag missing
        ];
        let r = build_roster(&id(99), "/Game/Maps/Ascent/Ascent", "competitive", players, me, &parties).unwrap();
        let s = &r.slots;

        assert!(s[0].is_me && s[0].id == Some(me) && s[0].level == Some(101));
        assert!(s[1].id.is_some(), "my party member is shown");
        assert_eq!(s[0].party, s[1].party);
        assert_ne!(s[0].party, 0);

        assert!(s[2].id.is_none() && s[2].level.is_none() && s[2].party == 0, "hidden stranger");
        assert!(s[2].agent.is_some(), "a hidden player still shows the agent");

        assert!(s[3].id.is_some() && s[4].id.is_some());
        assert_eq!(s[3].party, s[4].party);
        assert_ne!(s[3].party, s[0].party);
        assert_eq!(s[4].level, None, "level hidden on request");

        assert!(s[5].id.is_some() && s[5].party == 0, "no bracket when the partner is hidden");
        assert!(s[6].id.is_none() && s[6].party == 0);
        assert!(s[7].id.is_none(), "missing flag counts as hidden");

        assert_eq!((r.hidden, r.flag_missing), (3, 1));
    }

    #[test]
    fn core_match_sides_follow_my_team() {
        let me = Uuid::parse(&id(3)).unwrap();
        let m = CoreMatch {
            match_id: id(50),
            map_id: Some("/Game/Maps/Bonsai/Bonsai".into()),
            matchmaking: None,
            players: vec![
                player(1, "Red", Some(false), false),
                player(3, "Red", Some(false), false),
                player(4, "Blue", Some(false), false),
            ],
        };
        let r = core_roster(m, me, &HashMap::new()).unwrap();
        let sides: Vec<Side> = r.slots.iter().map(|s| s.side).collect();
        assert_eq!(sides, [Side::Ally, Side::Ally, Side::Enemy]);
    }

    #[test]
    fn pregame_json_shape() {
        let json = r#"{"ID":"00000000-0000-0000-0000-000000000063","MapID":"/Game/Maps/Ascent/Ascent","QueueID":"competitive",
            "AllyTeam":{"TeamID":"Blue","Players":[
              {"Subject":"00000000-0000-0000-0000-000000000001","CharacterID":"","CharacterSelectionState":"",
               "PlayerIdentity":{"Subject":"00000000-0000-0000-0000-000000000001","AccountLevel":77,"Incognito":false,"HideAccountLevel":false}}]},
            "EnemyTeam":null,"SomethingNew":[1,2]}"#;
        let m: PregameMatch = serde_json::from_str(json).unwrap();
        let r = pregame_roster(m, Uuid::parse(&id(1)).unwrap(), &HashMap::new()).unwrap();
        assert_eq!(r.slots.len(), 1);
        assert!(!r.slots[0].locked && r.slots[0].agent.is_none());
        assert_eq!(r.queue, "competitive");
    }

    #[test]
    fn websocket_frame_shape() {
        let json = r#"[8,"OnJsonApiEvent_chat_v4_presences",{"data":{"presences":[{"puuid":"x","product":"valorant","private":"e30="}]},"eventType":"Update","uri":"/chat/v4/presences"}]"#;
        let f: Frame = serde_json::from_str(json).unwrap();
        assert_eq!(f.2.event_type, "Update");
        assert_eq!(f.2.data.unwrap().presences.len(), 1);
    }

    #[test]
    fn presence_with_null_fields_still_parses() {
        let json = r#"{"presences":[{"puuid":null,"product":null,"private":null},{"puuid":"00000000-0000-0000-0000-000000000001","product":"valorant","private":"e30="}]}"#;
        let list: PresenceList = serde_json::from_str(json).unwrap();
        assert_eq!(list.presences.len(), 2);
        assert!(list.presences[0].id().is_none() && !list.presences[0].is_valorant());
        assert!(list.presences[1].is_valorant());
    }

    #[test]
    fn region_from_log() {
        let log = [
            "[2026.11.23-10.00.00:000][  0]LogShooter: Display: CI server version: release-11.09-22-3456789",
            "LogPlatformCommon: Platform HTTP Query End. QueryName: [AccountXP_GetPlayer], URL [GET https://pd.eu.a.pvp.net/account-xp/v1/players/abc], TraceID: x",
            "LogPlatformCommon: URL [GET https://glz-eu-1.eu.a.pvp.net/parties/v1/players/abc]",
        ];
        let r = parse_region(log.iter().map(|s| s.to_string())).unwrap();
        assert_eq!(
            r,
            Region { shard: "eu".into(), glz: "eu-1".into(), version: "release-11.09-22-3456789".into() }
        );
    }

    #[test]
    fn region_rejects_odd_hosts() {
        let log = [
            "CI server version: release-11.09-22-3456789",
            "URL [GET https://glz-EVIL.example/x.eu.a.pvp.net/]",
        ];
        assert!(parse_region(log.iter().map(|s| s.to_string())).is_none());
    }
}
