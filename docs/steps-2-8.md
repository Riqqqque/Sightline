# Steps 2 to 8: stats, cards, notes, map, session

Status: built and tested on made-up data on 2026-11-24, in one pass, because Rique asked for the
rest "now". The stats client was run against the real API for my own account. The app was run
against the real game client **in the menus only**. No agent select, match or post-game has been
seen live yet. The checklist at the end is what closes that gap.

## What was built

| Step | What it does | Where |
| --- | --- | --- |
| 2 Data layer | Stats client for HenrikDev (rank, recent matches, my finished match). Local SQLite database. A worker that queues lookups inside the rate limit. The key is read from the user variable, also when the process environment does not carry it. | `src/source/henrik.rs`, `src/store.rs`, `src/stats.rs`, `src/win.rs` |
| 3 Cards | Each row shows rank, RR, peak, the last five results, KD, HS% and ADR over the last ten ranked matches. Placeholders of the same size while loading. Click a row for details. | `ui/app.slint`, `src/ui_bridge.rs` |
| 4 Smurf flag and rank gap | A quiet flag for a new account with unusually strong results (`docs/smurf-score.md`). A bar comparing both teams' average rank, with an estimate. | `src/logic.rs` |
| 5 Seen before and notes | "With 3x (2-1) · 4d ago" per player, and a note per player, typed in the details panel. | `src/store.rs`, `src/stats.rs` |
| 6 Map context | "You on Ascent: 70% over 10 · KD · ADR · best and worst agent · attack and defense". The details panel shows the selected player's record on the map. | `src/stats.rs` |
| 7 Session and clutch | Session line (W/L, RR, streak, KD, HS%), first kills, first deaths, clutches. A recap after each match. A break reminder after losses in a row. | `src/stats.rs`, `src/source/henrik.rs` |
| 8 Polish | Settings screen with the missing-key message, the switches below, status and about. Memory trimmed when the game closes. | `ui/app.slint`, `src/win.rs` |

Settings (stored in the database, changed on the Settings screen): look up other players
(on/off), smurf flag (on/off), win estimate (on/bar/off), seen before (on/off), break reminder
(off/2/3).

## How stats are fetched

- One lookup at a time, me first, then my team, then the other team; every rank before any
  history.
- **Measured cost: every call took 2 units of the 30 per minute**, cached or not (rank, stored
  matches and full match alike). The worker assumes 2 and always leaves 2 unspent, so it makes at
  most 14 calls a minute.
- A full lobby with eight visible strangers needs 20 calls. Ranks come first and fit in the first
  minute; the last few histories wait for the next minute. Rows show a placeholder meanwhile.
- A rank younger than 10 minutes and a history younger than 30 minutes are not fetched again. A
  lookup that failed is not repeated for 10 minutes. A refused key stops all lookups for the run.
- Recent matches come from the "stored matches" endpoint: one line per match for the player who
  was looked up, nobody else's.
- After a match of mine, the full match is fetched once, 45 seconds after it ends. The service
  keeps an answer for five minutes, so a second and third look come 310 seconds apart, then none.
- Nothing is fetched while the game is closed. In the menus only my own rank and history are
  refreshed, and only when they are stale.

## Hidden players

> The post-game part of this section was changed on 2026-11-24: see "After the match, full
> history, round score" below and decisions 37 and 38. What follows still holds while a match
> is live.

The rule from step 1 carries through every new part, and tests prove each point:

- A hidden slot has no id. Lookups, notes, seen-before and database rows are all keyed by id, so
  there is nothing to key a hidden player by.
- The finished match names all ten players. Slots that were hidden live are remembered as side
  and agent only, and the players in them are dropped while the answer is read.
- For a match Sightline did not watch, only my own line is stored.
- A hidden row cannot be clicked, so no note can be typed against it.
- With "look up other players" off, only my own id is ever sent.

One choice to confirm: **a party member who hides their name from others.** The game shows them
to me, so step 1 shows them by name (decision 7). Since step 2 their rank and history are looked
up like any other visible player's. The spec's wording is "Incognito: no stat lookups", read
strictly that would exclude them. It is decision 29, and it is Rique's to overrule.

## Commands and results

- `cargo test`: **64 passed**, 4 ignored (they need this PC's game log or the network). It was 59
  before the deathmatch changes of the same evening.
- `cargo test -- --ignored real_stats --nocapture`: passed against the real API for my own
  account. Rank read, ten stored matches read, last full match read (10 players, sides add up,
  clutch wins within tries). Allowance went 30 â†’ 28 â†’ 24 over three calls.
- `cargo build --release`: clean, no warnings. `target\release\sightline.exe` is 10.2 MB (was
  8.4 MB; the difference is SQLite).
- `tools\screenshot.ps1` at 1920, 1140 and 630 px: match, agent select, menu, post game,
  settings, missing key, longest names, details panel. The tool now captures the window itself,
  not the screen.
- `tools\measure.ps1`, 4K monitor at 150%, window 2112 px tall, made-up data with a filled
  database:

| Width | State | Private Bytes | Working Set | CPU | Threads | Connections |
| --- | --- | --- | --- | --- | --- | --- |
| 720 | menus, real game client | 12.1 MB | 31.7 MB | 0% | 9 | 2 to this PC, 0 remote |
| 720 | menu | 9.7 MB | 26.5 MB | 0% | 7 | 0 |
| 720 | agent select | 10.4 MB | 27.4 MB | 0.04% | 7 | 0 |
| 720 | in match | 10.7 MB | 27.7 MB | 0% | 7 | 0 |
| 720 | post game | 10.9 MB | 28.0 MB | 0% | 7 | 0 |
| 1920 | menus, real game client | 22.1 MB | 41.4 MB | 0% | 9 | 2 to this PC, 0 remote |
| 1920 | menu | 19.4 MB | 36.1 MB | 0% | 7 | 0 |
| 1920 | agent select | 20.0 MB | 37.0 MB | 0.06% | 7 | 0 |
| 1920 | in match | 20.4 MB | 37.3 MB | 0.04% | 7 | 0 |
| 1920 | post game | 20.7 MB | 37.8 MB | 0% | 7 | 0 |

CPU is the share of the whole processor over eight seconds. The non-zero rows are the stat
answers arriving and the agent-select refresh inside the measuring window; nothing runs between
events. The database and the stats code add about 1 MB to the step 1 numbers. The 40 MB target
(Private Bytes) holds at every width.

The two live rows: the system listed four entries for the process, two connected to the Riot
Client on this PC and two not connected to anything. No connection to another machine was open.

Not measured with this build: the "game closed" state (the game was running during the
measurement), and a real match.

## Seen live on 2026-11-24

VALORANT was started while the measurement ran, so the two "live" rows above are the first
contact with the real client. In the menus the app showed: status "In menus", the queue name
(Swiftplay), footer "event socket connected · presence MENUS / DEFAULT (nested)", "Stats ready ·
30/30". That confirms the lockfile, the event socket, the presence layout (nested) and state
detection in the menus. No lobby was open, so no request went to Riot's servers.

## Seen live in a deathmatch, and what changed (2026-11-24, evening)

Rique ran the desktop app during deathmatches. A copy of the app's own window (taken without
touching focus) and a count of what the database held showed:

- **Working live:** in-game detection without agent select, the roster (12 players), names,
  ranks, recent form, and the hidden rule (4 of 12 hidden: pill and agent only). Started in the
  middle of a match, it picked the match up at once.
- **Wrong:** deathmatch gives every player the same team id, so all twelve sat under "MY TEAM"
  in the game's order with hidden rows mixed in, and "ENEMY TEAM" showed loading bars forever.
  The match was also stored as a win of mine and its players as "seen with".
- **Also seen:** a maximised window ignores the width cap. On the 4K monitor that is 3840 px
  wide and 38.7 MB private, almost all of it the frame buffer. Still under 40 MB, barely.

Changed in response, same evening (64 tests pass):

- **Order:** me first, then every player that can be shown, then hidden players. In every mode,
  on each side.
- **Any mode:** a match without teams is one list titled "PLAYERS", spread over both columns so
  twelve fit without scrolling. A match with nobody on the other side shows one list and no
  loading bars. Team modes without rounds (team deathmatch) take the winner from the match's own
  flag and show "WIN" without a round score.
- **Deathmatch is shown, not kept:** players and their stats show while it runs. It is not
  stored as a match of mine, not counted in the session, and its players are not "seen before".
  Rows written by the first build are ignored, not deleted.
- **Names** now reach the database (they were sent to the worker before it knew the lobby).

Seen live at 20:05, on the 19:58 build, in a deathmatch on Sunset (a silent copy of the running
window): "PLAYERS" and "MORE PLAYERS" with six rows each, me first, nine visible players with
rank and form, the three hidden ones last, "Stats ready".

A second build at 20:03 fixed one thing a review found before it could show: while a 5v5 match
is loading, my team from agent select must still read as "MY TEAM" with the other side loading,
not as a match without teams. That build is the one the shortcut now starts. It has passed the
tests and has not been seen on screen.

Other modes (unrated, competitive, swiftplay, spike rush, team deathmatch, custom) use the same
code paths and have not been seen live.

## The Scoreboard look (2026-11-24, late evening)

Rique rejected the first mockups as plain, not VALORANT enough and hard to read, chose the
"Scoreboard" design from the second round (`docs/mockups/sightline-valorant.html`), asked for
wins and losses as W and L letters, and approved downloading the art. The app now looks like
`docs/screens/match-wide.png`, which is drawn by the app itself.

- **Layout:** one bar per player, tinted in the team colour (teal mine, red theirs, gold me),
  agent face, rank badge, name in capitals, W/L letters, KD, HS and ADR as large numbers, seen
  before. Enemy team first. The player in focus sits on the right with their agent's art: the
  one you clicked, otherwise the opponent with the best recent KD.
- **Art:** agent faces, rank badges, agent portraits and map banners from
  media.valorant-api.com. `tools\make-art.ps1 -Download` fetches the originals (38 MB, in
  `assets\art\src`) and shrinks them to display size (2.2 MB), which is what goes into the exe.
  Nothing is fetched at run time. The art is Riot's: `assets\art`, `ui\art.slint`, and the
  pictures in `docs\screens` and `docs\mockups` that show it are all git-ignored.
- **Type and colour:** Bahnschrift (ships with Windows, same DIN shapes as the game's font),
  navy, bone white and red. No fonts were downloaded.
- **Narrower windows:** the seen-before column goes first, then the side panel narrows, then HS
  and ADR, then the W/L letters. Under 900 px the clicked player opens above the footer.
- **Pictures without a window:** `tools\render.ps1` draws the real window into a PNG in memory.
  Nothing appears on screen, so it is safe while a game is running. Every picture in
  `docs/screens` named below was made this way.

Checked this way, with the release exe: match (1280, 960 and 420 wide), deathmatch, agent
select, post game, menu, settings. 65 tests pass. The exe is 12.8 MB (was 10.2; the art).

Not yet done: seeing it in a real window on the monitor (same renderer, so it should match),
and measuring memory with the art in. Both need the app restarted by Rique.

## After the match, full history, round score (2026-11-24, night)

Seen live first: two ranked 5v5 matches were tracked end to end (results, RR, first kills,
clutches), the queue signal reads `MATCHMAKING`, and **five of ten players were hidden** in the
last one. The new look runs in a real window. Maximised on the 4K monitor the app used
**42.6 MB** private, over the 40 MB target. A normal window size has not been measured since
the art went in; before it, 960 x 820 was 15 MB.

Then, asked for by Rique:

- **Post-game scoreboard.** When the result of my match arrives (about a minute after it ends,
  or about six if the first look was too early), the list becomes that match's scoreboard, and
  it stays up through the menus and the queue until the next lobby loads: all ten players by name, K / D / A, KD, HS, ADR and
  ACS, each team sorted by score. Players who were hidden live are named here, marked "was
  hidden", and kept from this one match so "seen before" can work later. Decision 37.
- **Still hidden while live.** Nothing changed there, and nothing is looked up about a player
  who was hidden: the scoreboard comes from the one match answer that was already being fetched
  for my own recap. No new requests.
- **My full history.** Once a day, while idle, everything the stats service has stored of my
  ranked matches is pulled a hundred at a time (226 were on record on 2026-11-24). "My maps" and
  "you on this map" now cover all of it, not the last ten. Not from tracker.gg (decision 40).
- **Round score while in a match**, in the header, from my own presence in the Riot Client.
- **Not possible:** per-player kills and deaths during the match (decision 39).
- **Fixed:** the shooting range was being recorded as a match.

66 tests pass, and the history pull was run against the real service (228 matches in three pages). The tests include: a hidden player is never looked up; no live view names one; after the
match the scoreboard names all ten; a match that was not watched stores only my own line.

Checked by pictures drawn in memory. Not yet seen live: the post-game scoreboard, the round
score in a real match, and the scoreboard staying up through the menus.

## Public release 0.2.0 (2026-11-24, late night)

Rique decided to publish: a public repository, MIT, releases other people can run (decisions 42
to 47).

- **Updater.** Once a day, and never during a match, the app asks GitHub for the newest release.
  Settings then offers "Update to x.y.z"; the click downloads the exe, checks it against the
  SHA-256 published next to it, swaps it in and restarts. Only this repository's release files
  over HTTPS are accepted, and a download that fails the check changes nothing.
- **Checked against the real release:** an install pretending to be 0.0.1 was offered 0.2.0,
  downloaded all 13,513,216 bytes and matched the checksum. The swap is covered by tests in a
  scratch folder. **Not yet done end to end:** the click in a running app, because that needs a
  release newer than the one installed.
- **Nothing personal in the exe.** Builds remap the user profile and the repository folder; the
  first release build was found to carry the Windows account name in 585 file paths and was
  rebuilt before upload.
- **Names after the match** is a setting, off for a new install.
- **A checkout without the art builds**, with letters and text; the art is fetched by
  `tools\make-art.ps1 -Download` and only ever ships inside a released exe.
- 76 tests pass.

## Simplified or left out

Said plainly, so nothing here is mistaken for done:

- **No bundled font.** The art is in (see "The Scoreboard look"); the font is Bahnschrift, which
  Windows already has.
- **Details open on click, not on hover.**
- **Notes only.** Tags and "duo again" detection are not built.
- **Ranks load one after another**, about half a second each, not in parallel. Ten ranks take
  about five seconds, not the two seconds the plan aimed for.
- **Only the last ten ranked matches per player.** No background backfill of older history.
- **Other players' first-kill and clutch numbers are not shown.** The cheap history endpoint does
  not carry them; they exist for my own matches only.
- **Clutches are approximate**: a round counts when I was the last one alive on my team with an
  enemy still up. Revives are not modelled.
- **Database layout is simpler than `docs/architecture.md` section 7**: text names instead of
  lookup tables, and small indexed queries instead of running-total tables. The real layout is at
  the top of `src/store.rs`.
- **My own rank and map list on the idle screen appear after the first lobby** on a new database:
  that is when the app first learns which player is me.
- Not built: the queue timer, trend arrows, and the settings for density, poll rate, matches per
  player and session length. The Slint notice is a text line on the Settings screen, not the
  `AboutSlint` widget.
- Memory is trimmed when the game closes, not when the window is minimised.

## Live checklist

Open Sightline from the desktop shortcut before you queue, on the second monitor. It takes focus
once when it opens. Play one ranked match and note:

1. **Menus.** Status says "In menus"; the footer ends with "Stats ready".
2. **Queue.** Status changes to "In queue" when you search. If it does not, note the footer text.
3. **Agent select.** Your five rows appear. Does the enemy column say "Not shown yet"? Do ranks
   fill in within a few seconds, your own first?
4. **Hidden players.** Anyone with a hidden name must show the "Hidden" pill, the agent, and
   nothing else. This is the one that matters most.
5. **Match start.** The enemy team appears. The rank gap bar shows once three ranks per side are
   known. The footer counts "Stats loading, N left" down to "Stats ready".
6. **Click a player.** The details panel opens; type a note and press Enter.
7. **After the match.** Within about a minute the recap shows the score. The RR change can take
   up to ten more minutes.
8. **Next lobby.** Anyone you just played with shows "With 1x" or "Against 1x".
9. **Task Manager.** Note Sightline's memory during the match.

If anything looks wrong, the footer text is the most useful thing to send back. Nothing in it
identifies a player.
