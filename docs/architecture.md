# Architecture

Status: Phase 0 proposal, 2026-11-23. Written for data path A in `data-sources.md` (HenrikDev for
stats, local client for the live roster) with path C as a switch. Paths B and D change the
`source/` module only.

## 1. Shape

Two threads, one process, no helpers.

```
 UI thread (Slint event loop)                Worker thread (tokio current_thread)
 ┌───────────────────────────┐   commands    ┌──────────────────────────────────────┐
 │ views per state           │ ────────────▶ │ state machine                        │
 │ view models (plain data)  │               │   ▲ events                           │
 │ design tokens             │ ◀──────────── │ DataSource (trait)                   │
 └───────────────────────────┘   snapshots   │   ├ LocalClient   lockfile, wss, glz │
                                             │   ├ Henrik        stats              │
                                             │   └ StaticAssets  names, images      │
                                             │ request queue (rate limit, priority) │
                                             │ store (SQLite, running totals)       │
                                             └──────────────────────────────────────┘
```

- The UI thread only renders. It never does I/O and never touches SQLite.
- The worker owns everything else. It sends the UI small, complete snapshots (`LobbyView`,
  `SessionView`, `MapView`, `Status`), built from running totals, never from raw history.
- UI to worker: a `tokio::sync::mpsc` channel of commands (settings changed, note edited, window
  minimised or restored). Worker to UI: `slint::invoke_from_event_loop`.
- If a source fails, the worker keeps serving the last snapshot from the store and sets a status
  line ("Riot client not reachable", "Stats paused, retrying"). Nothing panics across the boundary:
  every source call returns `Result`, and `panic="abort"` is the last resort, not a code path.

## 2. The DataSource trait

One trait, so a Riot patch breaks one file.

```rust
pub trait DataSource {
    /// Game state changes, pushed. Ends when the client goes away.
    async fn events(&mut self) -> Result<GameEvent, SourceError>;
    /// Who is in my lobby or match right now. Incognito players arrive as Hidden, without a PUUID.
    async fn roster(&mut self, phase: Phase) -> Result<Roster, SourceError>;
    /// Names for non-hidden players only.
    async fn names(&mut self, players: &[PlayerId]) -> Result<Vec<Name>, SourceError>;
    async fn rank(&mut self, player: PlayerId) -> Result<RankSnapshot, SourceError>;
    async fn match_ids(&mut self, player: PlayerId, count: u8) -> Result<Vec<MatchId>, SourceError>;
    async fn match_detail(&mut self, id: MatchId) -> Result<MatchRecord, SourceError>;
}
```

- `Roster` is where hidden players are filtered. A hidden player has no `PlayerId` in the type, so
  no later code can look one up by mistake.
- Source-specific JSON shapes never leave `source/`. Everything above works on `MatchRecord`,
  `RankSnapshot` and the other plain structs in `model.rs`.
- Implementations are composed: roster and events from `LocalClient`, stats from `Henrik`. A
  `Fixture` implementation replays recorded JSON for tests and for running the UI without the game.

## 3. Game-state machine

```
            lockfile appears            presence: MENUS + queueing
  Closed ───────────────────▶ Menu ─────────────────────────────▶ Queue
    ▲                          ▲  ▲                                 │
    │ lockfile gone            │  │ presence: MENUS                 │ presence: PREGAME
    │ (from any state)         │  └────────────── Post Game ◀──┐    ▼
    │                          │   timeout or next queue       │  Agent Select
    └──────────────────────────┘                               │    │ presence: INGAME
                                                presence: MENUS│    ▼
                                                               └── In Game
```

| State | How it is detected | Work done |
| --- | --- | --- |
| Closed | No lockfile. | One timer checking for the lockfile every 5 s. No sockets, no HTTP, caches dropped, working set trimmed. |
| Menu | Lockfile present, presence says MENUS. | Websocket open and idle. Slow backfill of my own history with spare rate limit. |
| Queue | Presence `partyState` shows matchmaking (exact value to verify live; the fork reads `partyState` and `queueId` at `main.py:456-476`). | Refresh my own rank and totals. Warm the HTTP connection. |
| Agent Select | Presence says PREGAME. | Roster once, then on change. Tier 1 cards immediately, then ranks, then history. |
| In Game | Presence says INGAME. | Roster once (adds the enemy team), map context. No polling during the match. |
| Post Game | INGAME to MENUS. | Fetch the finished match (retry until available), drop hidden slots while parsing, store it, update totals, encounters, prediction result, session. |

- Events come from the local websocket (`OnJsonApiEvent_chat_v4_presences`). The poll rate setting
  applies only as a fallback when the websocket is unavailable: 5 s in Menu, 2 s in Queue.
- The presence blob has shipped in a nested and a flat layout. The parser accepts both and reports
  an unknown layout as a status, not a crash.
- Tokens are fetched from the local client when a class-R call needs them, held in one struct in
  memory with no `Debug` output, refreshed on an auth error, and zeroed on Closed.

## 4. Request queue

One queue in front of the shared HTTP client.

- **Budget.** A counter mirrored from the `X-RateLimit-Remaining` and `X-RateLimit-Reset` headers of
  every response. A request is sent only if the budget covers its worst-case cost (1 + expected
  background Riot requests). The app's own ceiling sits two units under the server's.
- **Priority.** 0 ranks for the current lobby, 1 match ids for the current lobby, 2 missing match
  details for the current lobby, 3 my own post-game match, 4 idle backfill. Backfill only spends
  budget that would otherwise expire.
- **Dedupe.** Keyed by (endpoint, id). A second request for the same thing joins the first.
  A match already in the store is never requested.
- **429.** Honour `Retry-After`, put the request back, show nothing. The status line says
  "loading" for as long as anything is queued.
- **Cancel.** Leaving a lobby drops its queued priority 0 to 2 requests.
- Two clients share this queue: the main client (remote hosts, full certificate verification) and a
  localhost-only client for the Riot Client's self-signed certificate. This is the one deviation
  from "one shared HTTP client"; the alternative would be to weaken verification for every host.

## 5. Loading tiers and memory

| Tier | Content | Source | Target |
| --- | --- | --- | --- |
| 1 | Agent, name or Hidden, level, party brackets, seen-before | roster + store | under 300 ms |
| 2 | Current and peak rank, RR | store if younger than 10 min, else queue priority 0 | under 2 s cold |
| 3 | W/L dots, KD, HS%, ADR, main agents, role tags | running totals, then queue priority 1 to 2 | background |

- Every card has its final size from the first frame. Tier 2 and 3 slots show skeletons of the same
  dimensions, so nothing shifts.
- In memory: the current lobby's ten card view models, the session summary, one map summary, the
  decoded icons on screen. Everything else is read from SQLite on demand and dropped.
- Match JSON is parsed straight from the response buffer into a `MatchRecord`, written, and freed.
- Images: rank icons are embedded at display size at build time. Agent and map images are fetched
  on first use, downscaled once to display size for the current scale factor, and stored in the
  `images` table. Only the ones on screen are decoded.
- Minimised or game closed: drop view models and decoded images, close idle connections,
  `PRAGMA shrink_memory`, trim the working set.

## 6. File layout

```
Sightline/
  Cargo.toml  Cargo.lock  rust-toolchain.toml  build.rs
  NOTICE  .gitignore
  assets/
    fonts/            one bundled font + its licence
    ranks/            rank icons at display size
  docs/
    data-sources.md  renderer-check.md  dependencies.md  architecture.md
    smurf-score.md    (step 4)
  ui/
    tokens.slint      colors, spacing scale, type scale, radii: the only place they are defined
    app.slint         window, state switch, density
    views/            idle.slint  lobby.slint  ingame.slint  postgame.slint  settings.slint  about.slint
    parts/            card.slint  rank.slint  party.slint  skeleton.slint  gapbar.slint  mapheader.slint
  src/
    main.rs           start-up, threads, wiring
    model.rs          plain data: PlayerId, MatchRecord, RankSnapshot, Roster, view models
    state.rs          the state machine
    queue.rs          rate-limit-aware request queue
    source/
      mod.rs          DataSource trait, SourceError
      local.rs        lockfile, log, local API, websocket, glz roster
      henrik.rs       HenrikDev endpoints
      assets.rs       static names and images
      fixture.rs      recorded data for tests and offline UI work
    store/
      mod.rs          open, pragmas, migrations
      schema.sql
      matches.rs      insert match + update totals in one transaction
      players.rs      encounters, notes, tags
      session.rs      session tracker, prediction log
    logic/
      smurf.rs  rankgap.rs  roles.rs  mapctx.rs
    ui_bridge.rs      snapshots to Slint models
    win.rs            working-set trim
  spikes/renderer-test/   Phase 0 measurement app (not part of the build)
  reference/              upstream clone, git-ignored
```

Data folder: `%LOCALAPPDATA%\Sightline\sightline.db` (plus `-wal`, `-shm`). The API key is read from
the `HENRIKDEV_API_KEY` user variable at start and is never written anywhere.

## 7. SQLite schema

> As built on 2026-11-24 the layout is simpler than this proposal: see the top of `src/store.rs`
> and decision 28 in `decisions.md`. The hidden-player rules below are unchanged.

Opened with: `journal_mode=WAL`, `synchronous=NORMAL`, `cache_size=-4096` (4 MB),
`foreign_keys=ON`, `wal_autocheckpoint=256`. All statements prepared once and cached.
Ids are 16-byte blobs; times are Unix seconds; `agent` and `map` are small integer ids from the
lookup tables.

```sql
CREATE TABLE meta     (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;

CREATE TABLE agents (id INTEGER PRIMARY KEY, uuid BLOB NOT NULL UNIQUE, name TEXT NOT NULL, role INTEGER);
CREATE TABLE maps   (id INTEGER PRIMARY KEY, uuid BLOB NOT NULL UNIQUE, url TEXT NOT NULL UNIQUE, name TEXT NOT NULL);

-- Every non-hidden player ever seen or fetched.
CREATE TABLE players (
  id              INTEGER PRIMARY KEY,
  puuid           BLOB NOT NULL UNIQUE,
  name            TEXT,
  tag             TEXT,
  level           INTEGER,
  tier            INTEGER,
  rr              INTEGER,
  peak_tier       INTEGER,
  rank_fetched_at INTEGER,
  first_seen      INTEGER NOT NULL,
  last_seen       INTEGER NOT NULL,
  note            TEXT
);
CREATE TABLE player_tags (
  player_id INTEGER NOT NULL REFERENCES players(id),
  tag       TEXT NOT NULL,
  PRIMARY KEY (player_id, tag)
) WITHOUT ROWID;

-- Rank over time, for the "fast RR gains" smurf signal.
CREATE TABLE rank_history (
  player_id INTEGER NOT NULL REFERENCES players(id),
  at        INTEGER NOT NULL,
  tier      INTEGER NOT NULL,
  rr        INTEGER NOT NULL,
  PRIMARY KEY (player_id, at)
) WITHOUT ROWID;

-- Finished matches. Immutable once stored; never refetched.
CREATE TABLE matches (
  id          INTEGER PRIMARY KEY,
  match_id    BLOB NOT NULL UNIQUE,
  map         INTEGER NOT NULL REFERENCES maps(id),
  queue       TEXT NOT NULL,
  season      BLOB,
  started_at  INTEGER NOT NULL,
  duration_s  INTEGER NOT NULL,
  rounds_a    INTEGER NOT NULL,
  rounds_b    INTEGER NOT NULL,
  winner      INTEGER NOT NULL          -- 0 team A, 1 team B, 2 draw
);
CREATE INDEX matches_started ON matches(started_at);

-- One row per player per match: the full per-player record, including round-derived facts
-- computed once at ingest (first bloods, clutches, side splits).
CREATE TABLE match_players (
  match_id        INTEGER NOT NULL REFERENCES matches(id),
  player_id       INTEGER NOT NULL REFERENCES players(id),
  team            INTEGER NOT NULL,
  party           INTEGER NOT NULL,     -- party index within the match
  agent           INTEGER NOT NULL REFERENCES agents(id),
  tier            INTEGER,
  won             INTEGER NOT NULL,
  rounds          INTEGER NOT NULL,
  kills           INTEGER NOT NULL,
  deaths          INTEGER NOT NULL,
  assists         INTEGER NOT NULL,
  score           INTEGER NOT NULL,
  damage          INTEGER NOT NULL,
  headshots       INTEGER NOT NULL,
  bodyshots       INTEGER NOT NULL,
  legshots        INTEGER NOT NULL,
  first_kills     INTEGER NOT NULL,
  first_deaths    INTEGER NOT NULL,
  clutch_tries    INTEGER NOT NULL,
  clutch_wins     INTEGER NOT NULL,
  atk_rounds      INTEGER NOT NULL,
  atk_won         INTEGER NOT NULL,
  def_rounds      INTEGER NOT NULL,
  def_won         INTEGER NOT NULL,
  PRIMARY KEY (match_id, player_id)
) WITHOUT ROWID;
CREATE INDEX match_players_player ON match_players(player_id, match_id);

-- Running totals, updated in the same transaction that inserts a match.
CREATE TABLE player_totals (
  player_id    INTEGER PRIMARY KEY REFERENCES players(id),
  matches      INTEGER NOT NULL, wins INTEGER NOT NULL, rounds INTEGER NOT NULL,
  kills        INTEGER NOT NULL, deaths INTEGER NOT NULL, assists INTEGER NOT NULL,
  damage       INTEGER NOT NULL,
  headshots    INTEGER NOT NULL, bodyshots INTEGER NOT NULL, legshots INTEGER NOT NULL,
  first_kills  INTEGER NOT NULL, first_deaths INTEGER NOT NULL,
  clutch_tries INTEGER NOT NULL, clutch_wins INTEGER NOT NULL,
  last5        INTEGER NOT NULL,        -- bit per match, newest in bit 0; see notes
  last5_count  INTEGER NOT NULL,
  last_match   INTEGER NOT NULL
);
CREATE TABLE player_agent_totals (
  player_id INTEGER NOT NULL REFERENCES players(id),
  agent     INTEGER NOT NULL REFERENCES agents(id),
  matches INTEGER NOT NULL, wins INTEGER NOT NULL, rounds INTEGER NOT NULL,
  kills INTEGER NOT NULL, deaths INTEGER NOT NULL, damage INTEGER NOT NULL,
  PRIMARY KEY (player_id, agent)
) WITHOUT ROWID;
CREATE TABLE player_map_totals (
  player_id INTEGER NOT NULL REFERENCES players(id),
  map       INTEGER NOT NULL REFERENCES maps(id),
  matches INTEGER NOT NULL, wins INTEGER NOT NULL, rounds INTEGER NOT NULL,
  kills INTEGER NOT NULL, deaths INTEGER NOT NULL, damage INTEGER NOT NULL,
  atk_rounds INTEGER NOT NULL, atk_won INTEGER NOT NULL,
  def_rounds INTEGER NOT NULL, def_won INTEGER NOT NULL,
  PRIMARY KEY (player_id, map)
) WITHOUT ROWID;
-- Mine only: best and worst agents per map.
CREATE TABLE my_map_agent_totals (
  map   INTEGER NOT NULL REFERENCES maps(id),
  agent INTEGER NOT NULL REFERENCES agents(id),
  matches INTEGER NOT NULL, wins INTEGER NOT NULL, rounds INTEGER NOT NULL,
  kills INTEGER NOT NULL, deaths INTEGER NOT NULL, damage INTEGER NOT NULL,
  PRIMARY KEY (map, agent)
) WITHOUT ROWID;

-- My own live games: session tracker, prediction log, post-game recap.
CREATE TABLE my_games (
  id            INTEGER PRIMARY KEY,
  match_id      BLOB NOT NULL UNIQUE,
  seen_at       INTEGER NOT NULL,
  map           INTEGER REFERENCES maps(id),
  queue         TEXT,
  agent         INTEGER REFERENCES agents(id),
  ally_avg_tier REAL,
  enemy_avg_tier REAL,
  ranked_known  INTEGER,                -- how many of the 10 ranks the estimate used
  win_estimate  REAL,                   -- logged at match start; NULL if the feature is off
  result        INTEGER,                -- NULL until finished: 1 win, 0 loss, 2 draw
  rr_change     INTEGER,
  kills INTEGER, deaths INTEGER, damage INTEGER, rounds INTEGER,
  headshots INTEGER, shots INTEGER
);
CREATE INDEX my_games_seen ON my_games(seen_at);

-- "Seen before": one row per non-hidden player per game of mine.
CREATE TABLE encounters (
  player_id INTEGER NOT NULL REFERENCES players(id),
  game_id   INTEGER NOT NULL REFERENCES my_games(id),
  relation  INTEGER NOT NULL,           -- 0 with me, 1 against me
  agent     INTEGER REFERENCES agents(id),
  party     INTEGER NOT NULL,           -- party index within the game, 0 = solo
  PRIMARY KEY (player_id, game_id)
) WITHOUT ROWID;
CREATE INDEX encounters_game ON encounters(game_id);

-- Hidden players in a game of mine: team and agent only, never a PUUID. Used to drop them when
-- the finished match is stored, including after a restart.
CREATE TABLE game_hidden_slots (
  game_id INTEGER NOT NULL REFERENCES my_games(id),
  team    INTEGER NOT NULL,
  agent   INTEGER NOT NULL REFERENCES agents(id),
  PRIMARY KEY (game_id, team, agent)
) WITHOUT ROWID;

-- Agent and map images, already at display size.
CREATE TABLE images (
  kind    INTEGER NOT NULL,             -- 0 agent, 1 map header
  ref_id  INTEGER NOT NULL,
  px      INTEGER NOT NULL,             -- pixel height it was scaled for
  version TEXT NOT NULL,                -- game version it came from
  data    BLOB NOT NULL,
  PRIMARY KEY (kind, ref_id, px)
) WITHOUT ROWID;
```

Notes on the schema:

- "Indexes on PUUID and match ID" are the two `UNIQUE` constraints, plus the composite keys.
- **Raw match JSON is not kept.** A full match is 100 to 300 KB of JSON; a single cold lobby would
  add over 10 MB. `match_players` holds every per-player number the features use, and the
  round-derived ones are computed once at ingest. If you want raw payloads kept for future
  features, that is a setting and a `match_raw` table, with the disk cost that implies.
- Repeat duos come from `encounters`: two players sharing a `party` in two or more games.
- **Hidden players never reach `players`, `match_players`, `encounters` or any lookup.** Live, they
  have no id in the roster type. Post-game, rows matching a `game_hidden_slots` entry are dropped
  while parsing. Full rules are in `data-sources.md` section 4.
- **`match_players` rows per match:** for my own games, every non-hidden player. For matches
  fetched as part of someone else's history, only the row of the player who was looked up.
  "Never refetched" is therefore per (match, player). Totals are only updated when a row is
  actually inserted, so inserts stay idempotent.
- `last5` is recomputed from the five newest rows by `started_at` (one indexed read) each time a
  row is inserted, so idle backfill of older matches cannot scramble it.
- "With 3x (2-1)" is one indexed query on `encounters` joined to `my_games.result`.

## 8. Wireframes

Half-width layout shown (about 1280 logical px). Density modes: **narrow** (under 760 px) stacks the
two teams in one column and hides the stat columns; **half** shows everything below; **full** adds
main agents and role tags inline instead of on hover. Measured memory per width is in
`renderer-check.md`.

### Closed

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ SIGHTLINE                                              ○ Waiting for VALORANT │
├──────────────────────────────────────────────────────────────────────────────┤
│  LAST SESSION            3W 2L    +38 RR    KD 1.14    HS 24%                │
│  ●●○●○                                                                       │
│                                                                              │
│  Nothing is running. Sightline wakes up when the game starts.                │
│                                                        [Settings]  [About]   │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Menu (idle)

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ SIGHTLINE        [rank] Ascendant 2 · 61 RR                  ● In menus      │
├──────────────────────────────────────────────────────────────────────────────┤
│  TODAY      4W 2L     +52 RR     streak W3     KD 1.21 ▲     HS 26% ▲        │
│  ●●○●●●                                                                      │
├──────────────────────────────────────────────────────────────────────────────┤
│  MY MAPS (last 50)                                                           │
│  Ascent    58%  ████████░░░░  KD 1.18  ADR 152   best Omen · worst Jett      │
│  Bind      44%  █████░░░░░░░  KD 0.97  ADR 139   best Viper · worst Raze     │
│  Haven     51%  ██████░░░░░░  KD 1.05  ADR 144   best Omen · worst Sova      │
│  …                                                                           │
│                                            history 212 matches · backfilling │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Queue

Same as Menu, with the status `◌ In queue 0:42 · Competitive` and a thin progress line under the
header while ranks and totals are refreshed.

### Agent select

My team only, if the live check confirms enemies are not available yet. The enemy column shows five
empty slots so the layout does not change when the match starts.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ [map image] ASCENT · Competitive                           ◌ Agent select    │
│ You here: 58% · KD 1.18 · ADR 152     best Omen 64% · worst Jett 41%         │
├───────────────────────────────────────┬──────────────────────────────────────┤
│ MY TEAM                               │ ENEMY TEAM                           │
│ ▌[ag] Rique #EUW         [rk] 61 RR   │ ┌──────────────────────────────────┐ │
│ ▌     Lv 212 ●●○●●   1.21  26%  152   │ │                                  │ │
│ ▌[ag] nightowl #0001     [rk] 34 RR   │ └──────────────────────────────────┘ │
│ ▌     Lv 88  ●○○●●   0.98  19%  131   │ ┌──────────────────────────────────┐ │
│  [ag] Hidden  ⓗ                       │ │   shown when the match loads     │ │
│                                       │ └──────────────────────────────────┘ │
│  [ag] kz #kzkz           ▒▒▒ ▒▒▒      │ ┌──────────────────────────────────┐ │
│       Lv 24  ▒▒▒▒▒   ▒▒▒  ▒▒▒  ▒▒▒    │ │                                  │ │
│       With 3x (2-1) · 4d ago · Jett   │ └──────────────────────────────────┘ │
│  [ag] abc #123  ⚠ Possible smurf      │ …                                    │
│       Lv 31  ●●●●●   1.84  38%  201   │                                      │
└───────────────────────────────────────┴──────────────────────────────────────┘
```

`▌` party bracket (one color per party) · `▒` skeleton, same size as the value it replaces ·
`ⓗ` hidden badge · `⚠` smurf badge with the signals in its tooltip.

### In game

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ [map image] ASCENT · Competitive                              ● In game      │
│ You here: 58% · KD 1.18 · ADR 152    Attack 54% · Defense 49%                │
│ Rank gap  ██████████████▌░░░░░░░░░░  Asc 1 avg vs Dia 3 avg · est. 56%       │
├───────────────────────────────────────┬──────────────────────────────────────┤
│ MY TEAM                               │ ENEMY TEAM                           │
│ ▌[ag] Rique #EUW         [rk] 61 RR   │ ▐[ag] wholesome jett #meow [rk] 12 RR│
│ ▌     Lv 212 ●●○●●   1.21  26%  152   │ ▐     Lv 140 ●●●○●   1.33  29%  166  │
│ ▌[ag] nightowl #0001     [rk] 34 RR   │ ▐[ag] one tap only #tap   [rk] 80 RR │
│ ▌     Lv 88  ●○○●●   0.98  19%  131   │ ▐     Lv 67  ○●●●●   1.52  41%  171  │
│  [ag] Hidden  ⓗ                       │       Against 2x (0-2) · duo again   │
│  …                                    │  [ag] Hidden  ⓗ                      │
│                                       │  …                                   │
├───────────────────────────────────────┴──────────────────────────────────────┤
│ Enemy on Ascent: Jett main 61% (14) · Omen main 48% (9)        loading 3 of 9│
└──────────────────────────────────────────────────────────────────────────────┘
```

Hover on a card: peak rank and act, main agents with win rates, first-kill and first-death rate,
clutches, role tag, note and tags (editable).

### Post game

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ASCENT · WIN 13-9 · +21 RR                                    ● Post game    │
├──────────────────────────────────────────────────────────────────────────────┤
│  You    22 / 14 / 6    KD 1.57    HS 31%    ADR 171    FK 4 · FD 2 · 1v2 won │
│  Estimate was 56% → win                                                      │
├──────────────────────────────────────────────────────────────────────────────┤
│  TODAY      5W 2L     +73 RR     streak W4     KD 1.26 ▲     HS 27% ▲        │
│  ●●○●●●●                                                                     │
├──────────────────────────────────────────────────────────────────────────────┤
│  Players from this match (hidden ones are not listed)   [add note]  [tag]    │
│  one tap only #tap    Against 3x (1-2)                                       │
│  nightowl #0001       With 4x (3-1)                                          │
└──────────────────────────────────────────────────────────────────────────────┘
```

A break nudge appears here after the configured losing streak, as one dismissible line.

### Settings

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ SETTINGS                                                             [Back]  │
├──────────────────────────────────────────────────────────────────────────────┤
│  API key          ● Found           (set HENRIKDEV_API_KEY to change)        │
│                   ○ Missing: stats are off, roster still works               │
│  Look up other players        [on | off]                                     │
│  Smurf flag                   [on | off]      Win estimate   [on | bar | off]│
│  Seen before                  [on | off]      Break nudge    [off | 2 | 3]   │
│  Matches per player           [ 5 ]           Fallback poll  [ 5 s ]         │
│  Session reset after          [ 3 h ]         Density        [auto|n|h|f]    │
│  Status                       Riot client ● · Stats ● 27/30 · Store ● 41 MB  │
└──────────────────────────────────────────────────────────────────────────────┘
```

The key itself is never displayed, only found or missing.

### About

Version, author (Rique), the ISC notice for the reference project, the font licence, and Slint's
`AboutSlint` widget.

## 9. Open points

1. Data path and the feature decisions listed at the end of `data-sources.md`.
2. Maximum window width (see `renderer-check.md`): cap at about 1280 logical px, or allow the full
   4K width at about 45 MB.
3. Raw match JSON: not kept (proposed), or kept at a disk cost. Keeping it would also keep the
   names of players who were hidden live, so "not kept" is the option that fits the hidden rule.
4. Font: Inter is the suggestion; it needs a download.
5. The 40 MB target: measured as Private Bytes (proposed) or Working Set.
