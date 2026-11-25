> Steps 2 to 8 were built on 2026-11-24. The current test count, numbers and the one live
> checklist to follow are in `steps-2-8.md`. This page is kept as the record of step 1.

# Step 1: state machine and player list

Status: built and tested 2026-11-23; reviewed again and restyled 2026-11-24. Everything that can
run without the game has run. The parts that talk to the live client have not, and need the
checklist at the end.

## What exists

- `src/state.rs`: the state machine as a pure function. Closed, Menu, Queue, Agent Select,
  In Game, Post Game.
- `src/source/`: the `DataSource` trait and two implementations.
  - `local.rs`: lockfile, local API, local event socket, the two lobby endpoints, the name lookup.
  - `fixture.rs`: made-up data that goes through the same parsing, for tests and `--fixture`.
  - `wire.rs`: the shapes of the client's answers and the hidden-player rule, no I/O.
  - `assets.rs`: agent and map names.
- `src/engine.rs`: waits for signals, loads the roster, sends complete views to the UI.
- `ui/tokens.slint`, `ui/app.slint`: design tokens and the player list (stat cards are step 3).
- `tools/measure.ps1`, `tools/screenshot.ps1`, `tools/make-icons.ps1`.
- `assets/icon/options/`: four app icon options and `sheet.png`, which compares them.

## Behaviour

- **Game closed:** the only work is one check for the lockfile every 5 seconds. No sockets, no
  HTTP clients, no TLS state. Measured: 0 open connections, 0% CPU.
- **Client running:** one local socket, idle until the client pushes a presence change. No polling.
- **Agent select:** roster on entry, then every 5 seconds until all five agents are locked, then
  nothing.
- **In game:** roster once (looked for every 2 seconds until the match exists), then nothing.
- **Post game:** the last roster stays on screen for 3 minutes or until the next queue.
- **Requests per match to Riot:** the lobby is found once (1 request) and each read after that is
  1 request, plus 1 name lookup when new visible players appear. Agent select: 3 on entry, then 1
  every 5 seconds while anyone is still picking, so about 5 to 20 in total. In game: 3. Parties
  come from the local presence feed at no extra cost.
- **When Riot says no:** a refusal (401, 403, other 4xx) or an unreadable answer is not retried;
  the last roster stays and the footer says why. A 429 or 5xx is retried after 15, 30 and 60
  seconds, then not again. Only "not there yet" is looked for every 2 seconds, at most 30 times.
- **Names:** each player is asked about once per lobby. An answer of any kind is final, including
  a refusal or a 429; the row then says "Name unavailable" and the footer says why. Only a lookup
  that got no answer at all is repeated, twice at most, 5 seconds apart.
- **Agent and map names:** fetched once and kept. While the fetch keeps failing it is tried
  twice per lobby, not on every refresh. Without them a chosen agent shows as "Agent".
- **Worst case in game:** 3 requests to Riot normally. If the name lookup gets no answer, each of
  its two repeats also re-reads the match, so 7 at most.
- **Request headers:** only what the endpoints require, with a plain user agent
  (`desktop-companion/0.1`). No game user-agent string.
- **Hidden players:** their id is dropped inside the roster parser and exists nowhere after it.
  No name is requested for them. They get no level and no party bracket. I am never hidden;
  members of my own party are shown. A missing identity flag counts as hidden.
- **Failures:** every source call returns an error value. The view keeps the last roster and the
  footer says what is wrong. Nothing panics on bad data.
- **Secrets:** the lockfile password and the session tokens are held in memory only, in types
  without debug output, and wiped when the game goes away. The app has no logging at all.

## Commands and results

```powershell
# from the repository folder
cargo test
cargo test -- --ignored real_ --nocapture
cargo build --release
powershell -NoProfile -ExecutionPolicy Bypass -File tools\measure.ps1 | Format-Table -AutoSize
```

- `cargo test`: 32 passed, 0 failed. Covers the state machine, both presence layouts, null
  fields in presence data, the hidden rule, roster shapes, the lockfile and region parsers, and
  the engine on a paused clock: a whole match end to end (which also proves no name is ever
  requested for a hidden player), a refusal that is not retried while the roster stays on screen,
  a rate limit that backs off and stops, a missing lobby that is looked for again, a refused name
  lookup that is sent once and not on every refresh, an unanswered one that is repeated twice,
  and a failing agent-name fetch that stops after two tries.
- `cargo test -- --ignored real_`: 3 passed. These touch the real machine and network: the game
  log gives a region, a game-server region and a client version; the lockfile test reports that
  the client is not running; and the agent and map names are fetched for real over the remote
  client (Schannel TLS, full verification).
- `cargo build --release`: clean, no warnings. `target\release\sightline.exe` is 8.4 MB.

### Memory and CPU

Second monitor (3840x2160, 150%), window 2112 px tall, each state held for 13 seconds.
"Before" is the Phase 0 renderer test at the same width, idle. Measured again on 2026-11-24 after
the restyle and with the icon built in; neither moved the numbers by more than 0.4 MB.

| Width | State | Private Bytes | Working Set | CPU | Threads | Open connections |
| --- | --- | --- | --- | --- | --- | --- |
| Narrow 720 | before (renderer test) | 10.0 MB | 25.4 MB | 0% | | |
| Narrow 720 | game closed (live) | 8.9 MB | 24.6 MB | 0% | 7 | 0 |
| Narrow 720 | menu (fixture) | 9.0 MB | 24.7 MB | 0% | 7 | 0 |
| Narrow 720 | lobby (fixture) | 9.5 MB | 25.4 MB | 0% | 7 | 0 |
| Narrow 720 | in match (fixture) | 9.8 MB | 25.6 MB | 0% | 7 | 0 |
| Wide 1920 | before (renderer test) | 19.9 MB | 35.3 MB | 0% | | |
| Wide 1920 | game closed (live) | 18.6 MB | 34.3 MB | 0% | 7 | 0 |
| Wide 1920 | menu (fixture) | 18.7 MB | 34.3 MB | 0% | 7 | 0 |
| Wide 1920 | lobby (fixture) | 19.1 MB | 34.9 MB | 0% | 7 | 0 |
| Wide 1920 | in match (fixture) | 19.1 MB | 35.0 MB | 0% | 7 | 0 |

1920 px is the widest the window goes (the 1280 logical px cap at 150%). Fixture states do no
network I/O, so the live in-match figure will be higher by whatever the HTTP and TLS state costs;
that is measured in the checklist below.

## Look

Restyled 2026-11-24. Still only data the app already has; no stat columns are mocked up.

- Header: map name (or "Sightline") with the queue under it, and the state as a pill with a dot.
- Each team has a coloured dot (teal mine, coral theirs) and a rule. Rows are two lines: name,
  tag and a "YOU" mark on the first; agent, role and "not locked" on the second. The square on
  the left carries the agent's first letter and is where the agent picture goes in step 3.
- Agent roles come from the same request as the agent names. No extra request.
- Hidden rows show the agent line and a "Hidden" badge, nothing else.
- Agent select: the enemy column is one panel ("Not shown yet") as tall as five rows, so my team
  does not move when the match loads. While a roster is loading, rows are grey placeholders.
- Idle states are one centred message. Nothing animates, so idle CPU stays at 0%.
- The footer wraps instead of cutting off, and stacks below 760 px, so the connection line can
  always be read in full.
- Font: Segoe UI Variable, which ships with Windows 11. Checked by building once with plain
  Segoe UI and comparing the screenshots: the text pixels differ, so the family is really loaded.
- When the window is too short for both teams, the list scrolls and the footer stays put.
- Checked by screenshot: in match and agent select at 1280, 720 and 420 logical px; closed and
  post game at 1280; queue at 720; menu at 420. Also the longest possible names (16 wide letters
  plus a 5-letter tag) at 420 px, where the name is cut with "â€¦" and the tag, the "YOU" mark and
  the level stay whole; a window too short for ten rows; and a refused request, whose message
  shows in full in the footer. `--fixture stress` and `--fixture refused` reproduce the last three.
- The software renderer has no shadows, paths or rounded clipping; the design uses none.

## App icon

Option B ("Two rosters"), chosen by Rique on 2026-11-24 from the four in
`assets\icon\options\sheet.png`. All of them are drawn by `tools\make-icons.ps1`; nothing was
downloaded. `tools\make-icons.ps1 -Pick <letter>` switches to another one.

- `assets\icon\sightline.ico` (16 to 256 px) is built into the exe by `build.rs`. Checked by
  pulling the icon back out of `sightline.exe` at 256, 48, 32 and 16 px.
- `assets\icon\sightline.png` (64 px) is the window icon. Checked on the title bar and on the
  taskbar of the 4K monitor, where it is sharp.
- The exe also carries the file description "Sightline", which is the name Task Manager shows.
- Cost: the exe went from 8.1 to 8.4 MB. Memory did not move (see the table above).

## Not verified yet

These need the running game. Nothing below has been exercised against a real client.

- The local socket connects and delivers presence events.
- Whether that socket also pushes lobby changes during agent select. If it does, the 5-second
  refresh can become event-driven; until that is known it is a timer.
- The exact `partyState` value while in queue.
- What happens to my presence when the game closes while the Riot Client stays open.
- The lobby endpoints accept requests that do not carry the game's user-agent string.
- Whether agent select returns the enemy team.
- That a real name lookup works.

## Live checklist

No ranked game is needed. A custom game on your own covers everything except the queue check.

The footer (bottom left) says how far the connection got, so if a step fails, its text tells
which link broke: "no Riot Client", "Riot Client found, connecting", "event socket refused",
"event socket connected, not signed in yet", "event socket connected Â· N events Â· presence â€¦".
"Stats key missing" in the bottom right is expected in step 1 if the app was started from a
terminal opened before the variable was set; the key is not used until step 2.

1. Start Sightline first, with the game closed:
   `target\release\sightline.exe`.
   Expect: pill says "Waiting for VALORANT", footer says "no Riot Client".
2. Start VALORANT and wait for the main menu.
   Expect: pill changes to "In menus" within a few seconds. Note the footer text.
3. Queue for anything (Unrated or Deathmatch is fine), wait 5 seconds, cancel.
   Expect: "In queue", then back to "In menus". Note the footer text while queued: it shows the
   real `partyState` value.
4. Start a custom game alone and reach agent select.
   Expect: "Agent select", your name, level and agent under MY TEAM; a "Not shown yet" panel
   under ENEMY TEAM. Note the footer text.
5. Lock in and load into the map.
   Expect: "In game", the map name in the header, your row still there.
6. While in the map, run this in a terminal and note the two numbers:
   `Get-Process sightline | Select-Object @{n='PrivateMB';e={[math]::Round($_.PrivateMemorySize64/1MB,1)}}, @{n='WorkingSetMB';e={[math]::Round($_.WorkingSet64/1MB,1)}}`
7. Leave the custom game.
   Expect: "Post game" with the roster kept, then "In menus" after 3 minutes or when you queue.
8. Close VALORANT.
   Expect: "Waiting for VALORANT" within about 10 seconds.

If any step shows "Players unavailable: â€¦" or "Names unavailable: â€¦" in the footer, that text
says which request failed. In a narrow window the key status sits under the footer text instead
of to its right.
The one real match still worth watching later: whether enemies appear during agent select or only
once the match loads.
