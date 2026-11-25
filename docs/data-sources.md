# Data sources

Status: Phase 0 research, 2026-11-23. Nothing here is implemented yet.

Reference studied: `mdevio/VALORANT-rank-yoinker` at commit `3bc8960` (release 3.0.2, 2026-10-31, ISC),
cloned to `reference/`. It was the most active fork on the day: 61 commits ahead of upstream, 51 of
them since late May 2026. Upstream `zayKenyon/VALORANT-rank-yoinker` last moved on 2026-05-21.

## 1. Read this first: policy

Sightline's first priority is "safe". The sources below were read on 2026-11-23. Where a page does
not address something, this document says "not stated" rather than guessing.

### What Riot's pages say

**VALORANT Developer API Policy** (support-developer.riotgames.com, "updated 1 year ago")

- "If your product serves players, you must register it with us regardless of whether or not your
  product uses official documented APIs."
- Listed under *unapproved use cases*:
  - "Scouting, which is seeing an opponents stats before a match starts."
  - "Apps that are not public and are designed for personal use only."
  - In-game apps and overlays with real-time data that would improve performance immediately.
- "Personal Key Applications are currently not supported."
- Game integrity: products cannot create alternatives to the official ranking system, and
  "MMR or ELO calculators" are named as prohibited. Products cannot de-anonymize players who cannot
  reasonably be identified from visible information.
- Player data needs opt-in through Riot Sign On, which needs a production key. Data of players who
  have not opted in "should not be made available to other players".

**Game Memory Access Removed for Third Party Apps** (support.riotgames.com, 2026-09-29)

- From 2026-10-06 Riot blocks game memory access for unknown third-party apps, all titles.
- The goal is the official API "as the only allowed mechanism for retrieving game data".
- Known, approved tools have a grace period until April 2027.
- Bans: "This change does not result in bans by itself". It blocks tools; it does not punish players.
- The local Riot Client API, the lockfile, the client websocket and log files: not mentioned.

**Riot's Stance on third-party applications** (support.riotgames.com, dated 2025-02-10)

- Apps found to be violating policy can be taken down; account suspensions "can vary in length".
- "Exposing information that's intentionally obfuscated" is given as an example of an unfair
  advantage.

**Enforcement history for tools of this kind:** not stated in any source read. Not guessed here.

### What HenrikDev's pages say

- Terms (README v4.5.0): get the user's consent before using their data; not allowed are big
  analytics projects, public store checkers and "TOS breaking stuff", with showing hidden names
  given as the example. Credit the project when you use it.
- Rate limit (docs, since v4.0.0): each call costs 1, and every request HenrikDev has to make to
  Riot in the background to answer it costs 1 more. Cached or stored data costs nothing extra.
- This key, measured from the live response headers on 2026-11-23: **30 requests per minute**, one
  window (`per1min`). That is the Basic tier. Free tier cache TTL is 300 s.
- Paid tiers exist (130, 200, 300 requests per minute at USD 10.99, 15.99, 25.99 a month).
- Limits are per key. Headers: `RateLimit-Policy`, `RateLimit`, `X-RateLimit-Limit`,
  `X-RateLimit-Remaining`, `X-RateLimit-Reset` (seconds), `Retry-After` on 429,
  `X-Cache-Status`, `X-Cache-TTL`.

### What this means for Sightline

1. **No data path makes this tool approved.** Showing other players' stats in the lobby is the
   "scouting" case by Riot's own wording, and a private personal-use app is also on the unapproved
   list. The official API is not an option for a personal app at all.
2. **Sightline never touches game memory**, so the October 2026 block does not apply to how it
   works. The notice's stated direction (official API only) is still broader than memory.
3. **The live roster cannot come from HenrikDev.** Knowing who is in the lobby right now needs the
   local Riot Client plus two Riot endpoints called with the running client's own session. Every
   option that shows a live lobby includes that. The options differ in how much *more* they ask of
   that session and of third parties.
4. **Three planned features sit close to named prohibitions**, separate from the data path:
   - the win estimate (rank gap meter) resembles an MMR/ELO calculator,
   - the smurf flag analyses individual players,
   - enemy stats in agent select and in-game is scouting.
   Section 5 lists them as open decisions.
5. **Hidden players stay hidden.** This is a hard rule in the spec and matches both Riot's and
   HenrikDev's wording. See section 4.

No option is known to be safe for the account. The options differ in how much they ask for.

## 2. Every endpoint the fork uses

Classes: **L** local client, **R** Riot remote using the client's session, **T** third party.

### L: local files and the local Riot Client

| Source | Returns | Limits | Risk |
| --- | --- | --- | --- |
| File `%LOCALAPPDATA%\Riot Games\Riot Client\Config\lockfile` | `name:pid:port:password:protocol` for the local API. Exists only while the Riot Client runs. | none | Low. Plain file read. Not addressed by policy. |
| File `%LOCALAPPDATA%\VALORANT\Saved\Logs\ShooterGame.log` | Region and shard (from logged `pd.` / `glz-` host names) and the client version string. | none | Low. Plain file read. |
| `GET https://127.0.0.1:{port}/entitlements/v1/token` (Basic `riot:{password}`) | `accessToken`, entitlement `token`, `subject` (my PUUID). | not stated | **These are my Riot tokens.** Only needed for class R. Held in memory, never stored, never logged. The fork writes the Basic header to its log file (`requestsV.py:239`); Sightline must not. |
| `GET https://127.0.0.1:{port}/chat/v4/presences` | Presence list. Each entry has a base64 `private` blob with `sessionLoopState` (MENUS / PREGAME / INGAME), `partyState` (the fork tests for values such as `CUSTOM_GAME_SETUP`; a matchmaking value is expected but its exact name is to verify live), `queueId`, party id and size, account level (`main.py:456-476`). Riot has shipped both a nested and a flat layout of this blob; the fork handles both. | not stated | Low to medium. Local, read-only, undocumented. |
| `wss://127.0.0.1:{port}` subscribe `OnJsonApiEvent_chat_v4_presences` | Pushes presence changes. This is the event source for the state machine. | n/a | Same as above. |
| `wss://…` subscribe `OnJsonApiEvent_chat_v6_messages` | In-game chat messages. | n/a | **Not used by Sightline.** |

The local API uses a self-signed certificate. The fork disables certificate checks on every
request, including the ones to Riot's servers. Sightline relaxes verification for `127.0.0.1` only.

### R: Riot servers, called with the client's session

All of these send `Authorization: Bearer {accessToken}`, `X-Riot-Entitlements-JWT`, and three
headers that identify the caller as the game client (`X-Riot-ClientPlatform`,
`X-Riot-ClientVersion`, a `ShooterGame/…` user agent). That last part is its own risk: the requests
present themselves as the game.

| Endpoint | Returns | Fork uses it for | Risk |
| --- | --- | --- | --- |
| `GET glz …/pregame/v1/players/{puuid}` | Current pregame match id, or not found. | Agent select detection. | Medium. Undocumented, own session. |
| `GET glz …/pregame/v1/matches/{id}` | `AllyTeam.Players[]` (`Subject`, `CharacterID`, lock state, `PlayerIdentity` with `AccountLevel`, `Incognito`, `HideAccountLevel`), map, queue, server. The fork reads **only the ally team** here (`main.py:807`). | Agent select roster. | Medium. |
| `GET glz …/core-game/v1/players/{puuid}` | Current live match id. | In-game detection. | Medium. |
| `GET glz …/core-game/v1/matches/{id}` | `Players[]` for **both teams** (`Subject`, `TeamID`, `CharacterID`, `PlayerIdentity`), `MapID`, mode. | In-game roster. | Medium. |
| `GET glz …/{pregame,core-game}/v1/matches/{id}/loadouts` | Equipped skins. | Skin table. | **Not used by Sightline.** |
| `PUT pd …/name-service/v2/players` body `[puuid, …]` | `GameName`, `TagLine` per PUUID. Answers for hidden players too. | Names. | High if misused: it will resolve a hidden player. Sightline never sends an Incognito PUUID. |
| `GET pd …/mmr/v1/players/{puuid}` | Per-season competitive tier, RR, leaderboard rank, wins by tier (peak rank), games, wins. | Rank, RR, peak, win rate. | High. Other players' data through my session. This is the scouting lookup. |
| `GET pd …/mmr/v1/players/{puuid}/competitiveupdates?startIndex&endIndex&queue=competitive` | Recent competitive matches: match id, map, start time, RR change, AFK penalty. | Last match, RR change. | High, same reason. |
| `GET pd …/match-details/v1/matches/{matchId}` | Full match: players, per-round damage and hit locations, kills, teams, result. | KD, HS%, result for encounter history. | High, same reason. |
| `GET shared …/content-service/v3/content` | Seasons and acts (current act id). | Season lookup. | Medium. |
| `auth.riotgames.com` login flow, `account-xp`, `contracts` | The fork's account switcher signs in with stored credentials. | Multi-account. | **Never used by Sightline.** No credentials, ever. |

Rate limits for class R: not published. The fork sleeps and retries on 429.

Enemy roster timing: the fork's code gets enemies only from core-game, which exists once the match
has loaded. If that holds, **agent select shows my team only**; the enemy column fills when the
match starts. To verify in a live match (step 1 checklist).

### T: third parties

| Source | Returns | Limits | Risk |
| --- | --- | --- | --- |
| `valorant-api.com/v1/{agents,maps,competitivetiers,version,…}` | Static names and images. No key. | not checked | Low. No player data. Used once per game version, then cached. |
| `api.github.com` releases, `raw.githubusercontent.com` status file | The fork's self-update and status notice. | n/a | **Not used by Sightline.** No update checks, no telemetry. |
| HenrikDev `api.henrikdev.xyz` | **Not used by the fork at all.** It does everything through the client session. | 30/min on this key | See below. |

HenrikDev endpoints Sightline would use (affinity is `na`, `eu`, `ap`, `kr`, `br` or `latam`):

| Endpoint | Returns | Cost |
| --- | --- | --- |
| `GET /valorant/v3/by-puuid/mmr/{affinity}/pc/{puuid}` | Current rank, RR, peak, seasonal history. | 1, plus 1 per background Riot request. |
| `GET /valorant/v4/by-puuid/matches/{affinity}/pc/{puuid}?mode=competitive&size=5` | Up to 10 full matches per call. | 1 + 1 for the history lookup + 1 per match HenrikDev does not already hold. Worst case 7 for size 5. |
| `GET /valorant/v1/by-puuid/stored-matches/{affinity}/{puuid}?mode=competitive&size=N` | Summaries of matches HenrikDev already stored. May be incomplete or stale. | 1. |
| `GET /valorant/v4/match/{affinity}/{matchId}` | One full match. | 1, plus 1 if not cached. Path taken from the docs index, not opened. |
| `GET /valorant/v2/by-puuid/account/{puuid}` | Name, tag, level. | 1, plus 1 if not cached. |

What leaves the PC with HenrikDev: the PUUIDs of the players in my lobby, sent to a third party.

## 3. Rate arithmetic for one cold lobby

Ten players, five matches each, nothing cached anywhere. Budget: 30 units per minute.

| Step | Calls | Units, best case | Units, worst case |
| --- | --- | --- | --- |
| Roster, agents, parties, levels (local + glz) | 2 to 4 | 0 | 0 |
| Ranks, 10 × mmr v3 | 10 | 10 | 20 |
| History, 10 × matches v4 size 5 | 10 | 10 | 70 |
| **Total** | | **20** | **90** |

- **Measured 2026-11-24: every call cost 2 units**, for rank, stored matches and a full match,
  with the cache reporting a hit or not. So a cold lobby of ten is 40 units for ranks and
  histories through the stored-matches path below, which is what was built.
- The worst case for ranks assumes one background Riot request per rank call. The docs give the
  rule (1 per background request) but not the count per endpoint, so this is an assumption to
  check against the `X-RateLimit-Remaining` header in step 2.
- Ranks fit inside one minute's budget either way, and the ten calls can run in parallel, so the
  2 s target for ranks is realistic when the minute's budget is unspent.
- History takes from under a minute (HenrikDev already holds the matches) to about three minutes
  (it holds none). Cards never wait for it.
- Cheaper history path: `stored-matches` first (1 unit per player, 10 for the lobby), then fetch
  only the match ids missing from the local database. Every match is stored once and never fetched
  again, so repeat opponents and long sessions cost almost nothing.
- Hidden players are skipped entirely, which lowers every number above.
- The same lobby through the client session (the fork's way) is about 70 requests to Riot with no
  published limit.

## 4. Hidden players

- The roster's `PlayerIdentity.Incognito` flag is checked before anything else is done with a PUUID.
- An Incognito player gets a card with the agent, a "Hidden" badge, and nothing else: no name
  request, no rank or history request, no local database lookup, no "seen before", no stored
  encounter. Their PUUID is dropped when the roster is parsed; later code never holds it.
- `HideAccountLevel` hides the level the same way.

> **Changed 2026-11-24 (decisions 37 and 38).** The post-game rule below was the Phase 0
> proposal. Rique decided that once a match is over its scoreboard shows everyone by name, as
> the game does, and that those players are kept from that one match. The live rule above is
> unchanged: a hidden slot has no id, and nothing is looked up about a player who was hidden.

**Post-game is where this could leak, so it gets its own rule.** A finished match fetched from any
API lists all ten players by name, including the ones who were hidden live.

- During the game, each hidden slot is remembered as team + agent only (agents are unique within a
  team). No PUUID.
- When my finished match is stored, any player whose team + agent matches a hidden slot is dropped
  while parsing: not written to `players`, `match_players` or `encounters`, and not shown in the
  recap. Match-level facts (score, map, rounds) are kept.
- Matches fetched for *another* player's history store only that player's row. The other nine are
  not written. Whether they were hidden in a game Sightline never saw cannot be known, so they are
  never stored by name. This also cuts the database to a tenth of the rows.
- If the app is restarted mid-game, hidden slots are in the `game_hidden_slots` table (team, agent),
  so the rule still holds after a restart.

**Who the rule does not apply to**

- Me. My own card is never "Hidden", whatever my own privacy setting is; otherwise my own stats
  would break.
- My party members: open decision. The game itself shows me their names even when they hide from
  everyone else, so treating them as visible reveals nothing new. Proposed: visible. The strict
  alternative is to follow the flag for them too.

## 5. Data-path options

All options use `valorant-api.com` for static images. All except D read the lockfile and the local
presence feed.

| | A. HenrikDev for stats | B. Client session for everything | C. Roster only, stats for me | D. No client access |
| --- | --- | --- | --- | --- |
| Live roster | local + glz: 3 requests on entering agent select, 1 every 5 s while agents are being picked, 3 when the match loads | same | same | none |
| Names | one name-service call per lobby (non-hidden only) | same | same | from finished matches only |
| Ranks and history of others | HenrikDev | pd with my session, about 70 requests per cold lobby | not fetched | not fetched live |
| My own stats | HenrikDev | pd | HenrikDev | HenrikDev |
| Uses my Riot session for | token + roster + one name call | everything | token + roster + one name call | nothing |
| Third party sees | lobby PUUIDs | nothing | my PUUID | my PUUID |
| Speed, cold lobby | ranks about 2 s, history 1 to 3 min | fastest, bounded by latency | instant | after the match |
| Features lost | none | none | others' ranks and stats, smurf flag, rank gap, enemy map stats | everything live; keeps session tracker, map stats, recap, seen-before built from finished matches |
| Scouting by Riot's wording | yes | yes | no stats shown, roster only | no |
| Exposure | medium | highest | low | lowest |

**Recommendation: build A, with C as a switch.** Because all access sits behind the `DataSource`
trait, C is A with "look up other players" turned off, so one build covers both and the choice can
be changed in Settings without a rebuild. A keeps the heaviest and most sensitive lookups off the
Riot session and inside a key that can be revoked. B is the fork's approach and is not recommended:
it is the fastest, but it spends the account's own session on roughly 70 undocumented requests per
lobby while presenting itself as the game client.

Open decisions:

1. Data path: A, B, C or D. And for A, whether "look up other players" defaults on or off.
2. Win estimate: keep it, show the rank gap bar without a percentage, or drop it.
3. Smurf flag: keep, or drop.
4. Names: one name-service call per lobby through the client session (cheap, non-hidden players
   only), or HenrikDev account lookups (10 to 20 units per lobby, slows ranks).
5. If agent select really is my team only, is that acceptable for the agent-select view?
6. Party members with hidden names: visible (proposed) or hidden.
