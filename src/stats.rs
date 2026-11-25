//! The stats worker: decides which lookups are worth making, makes them one at a time inside
//! the rate limit, keeps the results in the local database, and tells the engine what to show.
//!
//! It runs next to the engine on the same thread and never blocks it: the engine keeps reacting
//! to the game while a lookup is waiting for an answer or for the rate limit.
//!
//! The hidden-player rule holds here by construction: every lookup, note and database row is
//! keyed by a player id, and a hidden slot has none.

use crate::logic;
use crate::model::{Card, MapRow, Name, Panel, PlayerId, Row, Side, Uuid};
use crate::source::{SourceError, StatsSource};
use crate::store::{MyGame, Seen, Store, Tally};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::{timeout_at, Instant};

/// A stored rank younger than this is not fetched again.
const RANK_FRESH: i64 = 10 * 60;
/// Match history changes only when a match ends, so it is asked for less often.
const HISTORY_FRESH: i64 = 30 * 60;
/// What one call is assumed to cost, and what is always left unspent. Measured on 2026-11-24:
/// every call took two units, cached or not.
const COST: u32 = 2;
const RESERVE: u32 = 2;
/// A finished match takes a moment to appear. The stats service keeps an answer for five
/// minutes, so asking again sooner would only return the same one: a second and third look
/// come after that, and then no more.
const POST_RETRY: Duration = Duration::from_secs(310);
const POST_TRIES: u32 = 3;
/// A gap this long between two matches starts a new session.
const SESSION_GAP: i64 = 3 * 3600;
/// How many recent matches the form line covers.
const FORM_MATCHES: u32 = 10;

pub enum Command {
    /// The roster as it stands. Sent on every roster read.
    Lobby(LobbyInfo),
    /// Names that were looked up for visible players.
    Names(Vec<Name>),
    /// Left the lobby or the recap: drop its queued lookups.
    Leave,
    /// The match just ended: fetch its result.
    GameOver,
    /// In the menus or in queue: keep my own numbers fresh.
    Idle,
    /// The game went away.
    Sleep,
    Note { side: Side, index: usize, text: String },
    Setting { key: String, value: String },
    /// Download and install the update that was found.
    Update,
}

#[derive(Clone, PartialEq, Eq)]
pub struct LobbyInfo {
    pub match_id: Uuid,
    pub in_game: bool,
    pub map: String,
    pub queue: String,
    pub slots: Vec<LobbySlot>,
}

impl LobbyInfo {
    /// A match with a result worth keeping. Deathmatch has no teams and no result: its players
    /// are shown while it runs, and nothing about it is stored as a match of mine.
    fn kept(&self) -> bool {
        self.in_game && self.queue != "deathmatch"
    }
}

/// One slot as the worker needs it. `id` is `None` for a hidden player, as everywhere else.
#[derive(Clone, PartialEq, Eq)]
pub struct LobbySlot {
    pub id: Option<PlayerId>,
    pub is_me: bool,
    pub side: Side,
    pub agent: Option<Uuid>,
    pub agent_name: String,
    pub level: Option<u32>,
    pub party: u8,
}

/// What the engine merges into the view.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Insight {
    pub cards: HashMap<PlayerId, Card>,
    pub panel: Panel,
    /// The finished match's scoreboard, my team then the other, once its result is in.
    pub board: Option<(Vec<Row>, Vec<Row>)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Kind {
    Rank,
    History,
    /// Everything the service has stored of my own ranked matches. Runs last, when idle.
    Backfill,
}

struct Job {
    kind: Kind,
    id: PlayerId,
    tries: u8,
    /// Which page of the full history, for `Backfill`.
    page: u32,
}

/// A finished match that still has to be fetched.
struct Post {
    live: Uuid,
    seen_at: i64,
    at: Instant,
    tries: u32,
}

#[derive(Clone, PartialEq, Eq)]
struct Settings {
    lookup: bool,
    smurf: bool,
    /// "on", "bar" or "off".
    estimate: String,
    seen: bool,
    /// Losses in a row before the break line shows; 0 is off.
    nudge: u32,
    /// After a match, name the players who hid their name while it was live.
    after: bool,
    /// Ask GitHub once a day whether there is a newer release.
    updates: bool,
}

impl Settings {
    fn load(store: &Store) -> Self {
        let get = |key: &str, default: &str| store.setting(key).unwrap_or_else(|| default.to_string());
        Self {
            lookup: get("lookup", "on") != "off",
            smurf: get("smurf", "on") != "off",
            estimate: match get("estimate", "on").as_str() {
                "bar" => "bar",
                "off" => "off",
                _ => "on",
            }
            .to_string(),
            seen: get("seen", "on") != "off",
            nudge: get("nudge", "3").parse().unwrap_or(0),
            // Off unless chosen: see `Store::adopt`, which keeps it on for an existing install.
            after: get("after", "off") == "on",
            updates: get("updates", "on") != "off",
        }
    }

    fn pairs(&self) -> Vec<(String, String)> {
        let flag = |on: bool| if on { "on" } else { "off" }.to_string();
        vec![
            ("lookup".to_string(), flag(self.lookup)),
            ("smurf".to_string(), flag(self.smurf)),
            ("estimate".to_string(), self.estimate.clone()),
            ("seen".to_string(), flag(self.seen)),
            ("nudge".to_string(), if self.nudge == 0 { "off".to_string() } else { self.nudge.to_string() }),
            ("after".to_string(), flag(self.after)),
            ("updates".to_string(), flag(self.updates)),
        ]
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

enum Step {
    Job,
    Post,
    /// Ask whether there is a newer release.
    Check,
    /// Download and install the release that was found.
    Install,
    Wait(Instant),
    Idle,
}

/// The updater's state. Absent when running on made-up data.
struct Updater {
    http: Option<reqwest::Client>,
    checked: Option<Instant>,
    release: Option<crate::update::Release>,
    status: String,
    install: bool,
}

struct Worker<'a, S> {
    source: Option<S>,
    store: &'a Store,
    settings: Settings,
    lobby: Option<LobbyInfo>,
    me: Option<PlayerId>,
    jobs: VecDeque<Job>,
    /// When a lookup was last finished, with or without a result. Not repeated for a while.
    tried: HashMap<(Kind, PlayerId), i64>,
    post: Option<Post>,
    /// The recap is on screen: a match ended and no new lobby has started.
    recap: bool,
    /// Set when the stats side cannot be used at all. Says why.
    blocked: Option<&'static str>,
    hold: Option<Instant>,
    updater: Option<Updater>,
}

/// Runs until the command channel closes. `source` is `None` when there is no stats key: the
/// database-only features (seen before, notes) still work then. No update checks: this is the
/// entry for made-up data and tests.
pub async fn run<S: StatsSource>(
    source: Option<S>,
    store: &Store,
    commands: mpsc::UnboundedReceiver<Command>,
    emit: impl FnMut(Insight),
) {
    run_inner(source, store, commands, emit, false).await;
}

/// The real app: as `run`, and it also looks for a newer release once a day.
pub async fn run_live<S: StatsSource>(
    source: Option<S>,
    store: &Store,
    commands: mpsc::UnboundedReceiver<Command>,
    emit: impl FnMut(Insight),
) {
    run_inner(source, store, commands, emit, true).await;
}

async fn run_inner<S: StatsSource>(
    source: Option<S>,
    store: &Store,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut emit: impl FnMut(Insight),
    updates: bool,
) {
    let settings = Settings::load(store);
    let me = store.me();
    let mut worker = Worker {
        source,
        store,
        settings,
        lobby: None,
        me,
        jobs: VecDeque::new(),
        tried: HashMap::new(),
        post: None,
        recap: false,
        blocked: None,
        hold: None,
        updater: updates.then(|| Updater { http: None, checked: None, release: None, status: String::new(), install: false }),
    };
    let mut last: Option<Insight> = None;
    loop {
        while let Ok(command) = commands.try_recv() {
            worker.on_command(command);
        }
        let insight = worker.insight();
        if last.as_ref() != Some(&insight) {
            last = Some(insight.clone());
            emit(insight);
        }
        let waited = match worker.next_step() {
            Step::Job => {
                worker.run_job().await;
                continue;
            }
            Step::Post => {
                worker.run_post().await;
                continue;
            }
            Step::Check => {
                worker.check_update().await;
                continue;
            }
            Step::Install => {
                worker.install_update().await;
                continue;
            }
            Step::Wait(until) => match timeout_at(until, commands.recv()).await {
                Ok(command) => command,
                Err(_) => continue,
            },
            Step::Idle => commands.recv().await,
        };
        match waited {
            Some(command) => worker.on_command(command),
            None => return,
        }
    }
}

impl<S: StatsSource> Worker<'_, S> {
    fn usable(&self) -> bool {
        self.source.is_some() && self.blocked.is_none()
    }

    fn on_command(&mut self, command: Command) {
        let now = now();
        match command {
            Command::Lobby(info) => {
                if let Some(me) = info.slots.iter().find(|s| s.is_me).and_then(|s| s.id) {
                    if self.me != Some(me) {
                        self.me = Some(me);
                        self.store.set_me(me);
                    }
                }
                if self.lobby.as_ref().map(|l| l.match_id) != Some(info.match_id) {
                    self.jobs.clear();
                }
                self.recap = false;
                for id in info.slots.iter().filter_map(|s| s.id) {
                    self.store.touch(id, now);
                }
                if info.kept() {
                    self.begin(&info, now);
                }
                self.lobby = Some(info);
                self.plan();
            }
            Command::Names(names) => {
                // Only names of players in the current lobby, and those all have ids.
                let Some(lobby) = &self.lobby else { return };
                for name in names.iter().filter(|n| lobby.slots.iter().any(|s| s.id == Some(n.id))) {
                    self.store.set_name(name.id, &name.name, &name.tag, now);
                }
            }
            Command::Leave => {
                // The last match's recap and scoreboard are not dropped here: they stay until
                // the next lobby, so a result that arrives late is still shown.
                self.lobby = None;
                self.jobs.clear();
                self.plan();
            }
            Command::GameOver => {
                let live = self.lobby.as_ref().filter(|l| l.kept()).map(|l| l.match_id);
                self.recap = live.is_some();
                self.jobs.clear();
                if let (Some(live), Some(source), None, Some(_)) = (live, &self.source, self.blocked, self.me) {
                    let seen_at = self.store.open_game(now - 6 * 3600).filter(|(id, _)| *id == live).map_or(now, |(_, at)| at);
                    self.post = Some(Post { live, seen_at, at: Instant::now() + source.settle(), tries: 0 });
                }
            }
            Command::Idle => {
                // A match that was being watched when the app last closed still needs its result.
                if self.post.is_none() && self.usable() && self.me.is_some() {
                    if let Some((live, seen_at)) = self.store.open_game(now - 12 * 3600) {
                        self.post = Some(Post { live, seen_at, at: Instant::now(), tries: POST_TRIES - 1 });
                    }
                }
                self.plan();
                // Once a day, while nothing else is going on: my whole ranked history.
                if let (true, Some(me)) = (self.usable(), self.me) {
                    let due = self.store.setting("history_full_at").and_then(|v| v.parse::<i64>().ok()).is_none_or(|at| now - at > 86_400);
                    let queued = self.jobs.iter().any(|j| j.kind == Kind::Backfill);
                    let tried = self.tried.get(&(Kind::Backfill, me)).is_some_and(|at| now - at < RANK_FRESH);
                    if due && !queued && !tried {
                        self.jobs.push_back(Job { kind: Kind::Backfill, id: me, tries: 0, page: 1 });
                    }
                }
            }
            Command::Sleep => {
                self.lobby = None;
                self.recap = false;
                self.jobs.clear();
                if let Some(source) = self.source.as_mut() {
                    source.sleep();
                }
                self.store.rest();
            }
            Command::Note { side, index, text } => {
                let Some(lobby) = &self.lobby else { return };
                // The row's place on screen is mapped back to a player here. A hidden row has no
                // id, so a note typed against it has nowhere to go.
                if let Some(id) = lobby.slots.iter().filter(|s| s.side == side).nth(index).and_then(|s| s.id) {
                    let text: String = text.chars().filter(|c| !c.is_control()).take(200).collect();
                    self.store.set_note(id, &text, now);
                }
            }
            Command::Update => {
                if let Some(updater) = self.updater.as_mut().filter(|u| u.release.is_some()) {
                    updater.install = true;
                    updater.status = "Downloading the update…".to_string();
                }
            }
            Command::Setting { key, value } => {
                if matches!(key.as_str(), "lookup" | "smurf" | "estimate" | "seen" | "nudge" | "after" | "updates") && value.len() <= 8 {
                    self.store.set_setting(&key, &value);
                    self.settings = Settings::load(self.store);
                    if !self.settings.lookup {
                        let me = self.me;
                        self.jobs.retain(|j| Some(j.id) == me);
                    }
                    self.plan();
                }
            }
        }
    }

    /// Asks GitHub whether there is a newer release. At most once a day, and never during a match.
    async fn check_update(&mut self) {
        let Some(updater) = self.updater.as_mut() else { return };
        updater.checked = Some(Instant::now());
        if updater.http.is_none() {
            updater.http = crate::source::local::remote_http();
        }
        let Some(http) = updater.http.clone() else { return };
        match crate::update::check(&http, crate::update::current()).await {
            Ok(Some(release)) => {
                updater.status = format!("Version {} is available. You have {}.", release.version, crate::update::current());
                updater.release = Some(release);
            }
            Ok(None) => {
                updater.release = None;
                updater.status = format!("Up to date: version {}.", crate::update::current());
            }
            Err(why) => updater.status = format!("Could not check for updates: {why}."),
        }
        // The connection is not kept open for a once-a-day question.
        updater.http = None;
    }

    /// Downloads the release that was found, checks it, swaps it in and restarts. On any
    /// failure nothing on disk has changed and the reason is shown.
    async fn install_update(&mut self) {
        let Some(updater) = self.updater.as_mut() else { return };
        updater.install = false;
        let (Some(release), Some(http)) = (updater.release.clone(), crate::source::local::remote_http()) else { return };
        match crate::update::install(&http, &release).await {
            Ok(()) => crate::update::restart(),
            Err(why) => updater.status = format!("Update not installed: {why}."),
        }
    }

    /// Records a match of mine as it starts: who was visible, and which slots were hidden.
    fn begin(&mut self, info: &LobbyInfo, now: i64) {
        let hidden: Vec<(Side, Uuid)> =
            info.slots.iter().filter(|s| s.id.is_none()).filter_map(|s| Some((s.side, s.agent?))).collect();
        let others: Vec<(PlayerId, Side, String, u8)> = if self.settings.seen {
            info.slots.iter().filter(|s| !s.is_me).filter_map(|s| Some((s.id?, s.side, s.agent_name.clone(), s.party))).collect()
        } else {
            Vec::new()
        };
        let mine = info.slots.iter().find(|s| s.is_me).map(|s| s.agent_name.clone()).unwrap_or_default();
        self.store.begin_game(info.match_id, now, &info.map, &info.queue, &mine, &hidden, &others);
    }

    /// Queues the lookups worth making right now: me first, then my team, then the other team;
    /// every rank before any history.
    fn plan(&mut self) {
        if !self.usable() {
            return;
        }
        let now = now();
        let mut ids: Vec<PlayerId> = Vec::new();
        match &self.lobby {
            Some(lobby) => {
                for pass in 0..3 {
                    for slot in &lobby.slots {
                        let Some(id) = slot.id else { continue };
                        let turn = match pass {
                            0 => slot.is_me,
                            1 => !slot.is_me && slot.side == Side::Ally,
                            _ => slot.side == Side::Enemy,
                        };
                        if turn && (slot.is_me || self.settings.lookup) {
                            ids.push(id);
                        }
                    }
                }
            }
            None => ids.extend(self.me),
        }
        for kind in [Kind::Rank, Kind::History] {
            for id in &ids {
                if self.jobs.iter().any(|j| j.kind == kind && j.id == *id) {
                    continue;
                }
                if self.tried.get(&(kind, *id)).is_some_and(|at| now - at < RANK_FRESH) {
                    continue;
                }
                let fresh = match kind {
                    Kind::Rank => self.store.rank(*id).is_some_and(|r| now - r.fetched_at < RANK_FRESH),
                    Kind::History => self.store.history_at(*id).is_some_and(|at| now - at < HISTORY_FRESH),
                    // Never planned per lobby; it is queued once a day from the idle screen.
                    Kind::Backfill => true,
                };
                if !fresh {
                    self.jobs.push_back(Job { kind, id: *id, tries: 0, page: 0 });
                }
            }
        }
        self.jobs.make_contiguous().sort_by_key(|j| j.kind);
    }

    fn next_step(&self) -> Step {
        let now = Instant::now();
        if let Some(updater) = &self.updater {
            if updater.install {
                return Step::Install;
            }
            // Only while nothing else is going on, and not while a match is on screen.
            let quiet = self.lobby.is_none() && self.jobs.is_empty() && self.post.is_none();
            let due = updater.checked.is_none_or(|at| now.duration_since(at) > Duration::from_secs(86_400));
            if self.settings.updates && quiet && due {
                return Step::Check;
            }
        }
        if let Some(hold) = self.hold.filter(|at| *at > now) {
            return Step::Wait(hold);
        }
        let Some(source) = self.source.as_ref().filter(|_| self.blocked.is_none()) else { return Step::Idle };
        let budget = source.budget();
        let affordable = budget.remaining >= COST + RESERVE;
        let refill = now + budget.reset_in.max(Duration::from_secs(1));
        let post_at = self.post.as_ref().map(|p| p.at);
        match post_at {
            Some(at) if at <= now => {
                if affordable {
                    Step::Post
                } else {
                    Step::Wait(refill)
                }
            }
            _ if !self.jobs.is_empty() => {
                if affordable {
                    Step::Job
                } else {
                    Step::Wait(post_at.map_or(refill, |at| at.min(refill)))
                }
            }
            Some(at) => Step::Wait(at),
            None => Step::Idle,
        }
    }

    async fn run_job(&mut self) {
        let (Some(source), Some(job)) = (self.source.as_mut(), self.jobs.pop_front()) else { return };
        let now = now();
        let outcome = match job.kind {
            Kind::Rank => source.rank(job.id).await.map(|rank| {
                self.store.save_rank(job.id, &rank, now);
                if let (true, Some(last)) = (Some(job.id) == self.me, rank.last_match) {
                    self.store.apply_rr(last, rank.last_change);
                }
            }),
            Kind::History => source.history(job.id).await.map(|games| {
                self.store.save_games(job.id, &games);
                self.store.mark_history(job.id, now);
            }),
            Kind::Backfill => source.full_history(job.id, job.page).await.map(|games| {
                let added = self.store.save_games(job.id, &games);
                // Newest first: go on while a page still brings matches that were not known.
                // Stopping on "nothing new" also ends it if the service ever repeats a page.
                if added > 0 && job.page < 20 {
                    self.jobs.push_back(Job { kind: Kind::Backfill, id: job.id, tries: 0, page: job.page + 1 });
                } else {
                    self.store.set_setting("history_full_at", &now.to_string());
                }
            }),
        };
        match outcome {
            Ok(()) => {
                self.tried.insert((job.kind, job.id), now);
            }
            Err(error) => self.failed(job, error, now),
        }
    }

    fn failed(&mut self, mut job: Job, error: SourceError, now: i64) {
        match error {
            SourceError::Auth => {
                // The key was refused. Asking again with the same key would only repeat it.
                self.blocked = Some("Stats key not accepted");
                self.jobs.clear();
                self.post = None;
            }
            SourceError::Region => {
                self.blocked = Some("Stats paused: region not found in the game log");
                self.jobs.clear();
            }
            // Told to slow down: the allowance now reads as empty, so this waits for the refill.
            SourceError::Http(429) if job.tries < 3 => {
                job.tries += 1;
                self.jobs.push_front(job);
            }
            SourceError::Network | SourceError::Http(500..=599) if job.tries < 2 => {
                job.tries += 1;
                self.jobs.push_back(job);
                self.hold = Some(Instant::now() + Duration::from_secs(5));
            }
            // No data for this player, an unreadable answer, or out of tries: final for now.
            _ => {
                self.tried.insert((job.kind, job.id), now);
            }
        }
    }

    async fn run_post(&mut self) {
        let (Some(source), Some(me), Some(mut post)) = (self.source.as_mut(), self.me, self.post.take()) else {
            return;
        };
        let hidden = self.store.hidden_slots(post.live);
        let result = source.last_match(me, &hidden).await;
        post.tries += 1;
        match result {
            // The newest finished match is the one that was watched: same id, or started when
            // Sightline saw it start.
            Ok(Some(m)) if m.match_id == post.live || (m.started_at - post.seen_at).abs() <= 420 => {
                self.store.finish_game(&m, me, Some(post.live), self.settings.seen, self.settings.after);
                // The RR change comes with my rank, so that is read again right away.
                self.tried.remove(&(Kind::Rank, me));
                self.jobs.retain(|j| !(j.kind == Kind::Rank && j.id == me));
                self.jobs.push_front(Job { kind: Kind::Rank, id: me, tries: 0, page: 0 });
            }
            Err(SourceError::Auth) => {
                self.blocked = Some("Stats key not accepted");
                self.jobs.clear();
            }
            _ if post.tries < POST_TRIES => {
                post.at = Instant::now() + POST_RETRY;
                self.post = Some(post);
            }
            // Never showed up. The match stays in "seen before", without a result.
            _ => self.store.close_unknown(post.live),
        }
    }

    // ---- what to show ----

    fn insight(&self) -> Insight {
        let now = now();
        let mut cards = HashMap::new();
        let (mut ally_ratings, mut enemy_ratings) = (Vec::new(), Vec::new());
        if let Some(lobby) = &self.lobby {
            for slot in &lobby.slots {
                // A hidden player gets no card: no id means nothing to look up and nothing to key by.
                let Some(id) = slot.id else { continue };
                let on = self.usable() && (slot.is_me || self.settings.lookup);
                let mut card = Card { stats_on: on, ..Card::default() };
                if on {
                    let pending = |kind: Kind| self.jobs.iter().any(|j| j.kind == kind && j.id == id);
                    let rank = self.store.rank(id);
                    card.rank_done = rank.is_some() || !pending(Kind::Rank);
                    if let Some(rank) = &rank {
                        card.rank = logic::tier_name(rank.tier);
                        card.rank_tint = logic::tier_tint(rank.tier);
                        card.tier = rank.tier;
                        if rank.tier >= 3 {
                            card.rr = format!("{} RR", rank.rr);
                        }
                        if rank.peak > rank.tier {
                            card.peak = format!("peak {}", logic::tier_name(rank.peak));
                        }
                        if let Some(rating) = logic::rating(rank) {
                            match slot.side {
                                Side::Ally => ally_ratings.push(rating),
                                Side::Enemy => enemy_ratings.push(rating),
                            }
                        }
                    }
                    let form = self.store.summary(id, FORM_MATCHES);
                    card.stats_done = form.games > 0 || !pending(Kind::History);
                    if form.games > 0 {
                        card.dots = form.last5.clone();
                        card.stats = logic::stat_line(&form);
                        card.mains = form
                            .agents
                            .iter()
                            .take(3)
                            .map(|(agent, games, wins)| format!("{agent} {}% ({games})", logic::percent(*wins, *games)))
                            .collect::<Vec<_>>()
                            .join("  ·  ");
                        if let Some(map) = self.store.maps(id, 50).into_iter().find(|m| m.name == lobby.map) {
                            card.on_map = format!("{} {}% ({})", map.name, logic::percent(map.wins, map.games), map.games);
                        }
                        if self.settings.smurf && !slot.is_me {
                            let signals = logic::smurf_signals(slot.level, rank.as_ref(), &form);
                            card.smurf = !signals.is_empty();
                            card.signals = signals.join("  ·  ");
                        }
                    }
                }
                if self.settings.seen && !slot.is_me {
                    card.seen = seen_line(&self.store.seen(id, Some(lobby.match_id)), now);
                }
                card.note = self.store.note(id);
                cards.insert(id, card);
            }
        }

        let mut panel = Panel { gap_pct: -1, settings: self.settings.pairs(), ..Panel::default() };
        if let Some(lobby) = self.lobby.as_ref().filter(|l| l.kept() && self.settings.estimate != "off") {
            if let Some(gap) = logic::rank_gap(&ally_ratings, &enemy_ratings) {
                let shown = self.settings.estimate == "on";
                panel.gap_pct = gap.percent;
                panel.gap_label = format!(
                    "{} avg  vs  {} avg{}",
                    logic::rating_name(gap.ally_avg),
                    logic::rating_name(gap.enemy_avg),
                    if shown { format!("  ·  est. {}%", gap.percent) } else { String::new() }
                );
                self.store.set_estimate(lobby.match_id, gap.ally_avg, gap.enemy_avg, shown.then_some(gap.percent));
            }
        }
        if let Some(me) = self.me {
            if let Some(rank) = self.store.rank(me).filter(|_| self.usable()) {
                panel.my_rank = match rank.tier {
                    3.. => format!("{}  ·  {} RR", logic::tier_name(rank.tier), rank.rr),
                    _ => logic::tier_name(rank.tier),
                };
            }
            let maps = self.store.maps(me, 5000);
            if let Some(lobby) = &self.lobby {
                if let Some(mine) = maps.iter().find(|m| m.name == lobby.map) {
                    panel.map_line = self.map_line(me, mine);
                    panel.map_win = format!("{}%", logic::percent(mine.wins, mine.games));
                }
            }
            panel.maps = maps
                .iter()
                .take(7)
                .map(|m| MapRow {
                    name: m.name.clone(),
                    pct: logic::percent(m.wins, m.games) as u8,
                    line: format!(
                        "{}%  ·  KD {}  ·  ADR {}  ·  {} {}",
                        logic::percent(m.wins, m.games),
                        logic::kd(m.kills, m.deaths),
                        logic::adr(m.damage, m.rounds),
                        m.games,
                        if m.games == 1 { "match" } else { "matches" }
                    ),
                })
                .collect();
        }
        let games = self.store.my_games(40);
        self.session(&games, now, &mut panel);
        if self.recap {
            self.recap_lines(games.first(), now, &mut panel);
        }
        panel.stats_status = match (&self.source, self.blocked) {
            (None, _) => "Stats key missing".to_string(),
            (_, Some(why)) => why.to_string(),
            (Some(source), None) => {
                let budget = source.budget();
                let waiting = self.jobs.len() + usize::from(self.post.is_some());
                if waiting > 0 {
                    format!("Stats loading, {waiting} left  ·  {}/{}", budget.remaining, budget.limit)
                } else {
                    format!("Stats ready  ·  {}/{}", budget.remaining, budget.limit)
                }
            }
        };
        if let Some(updater) = &self.updater {
            panel.update = updater.release.as_ref().map(|r| r.version.clone()).unwrap_or_default();
            panel.update_status = if !self.settings.updates && updater.release.is_none() {
                format!("Update checks are off. You have version {}.", crate::update::current())
            } else {
                updater.status.clone()
            };
        }
        let (players, matches) = self.store.counts();
        panel.store_status = format!(
            "{players} players  ·  {matches} matches  ·  {} KB on disk",
            self.store.size_bytes().div_ceil(1024)
        );
        let board = if self.recap && self.post.is_none() { self.board(now) } else { None };
        Insight { cards, panel, board }
    }

    /// The finished match as a scoreboard: everyone in it with that match's numbers, as the
    /// game's own end screen shows them, my team and the other each sorted by score.
    fn board(&self, now: i64) -> Option<(Vec<Row>, Vec<Row>)> {
        let lines = self.store.latest_board(now - 3 * 3600);
        if lines.is_empty() {
            return None;
        }
        let row = |l: &crate::store::BoardLine| Row {
            name: l.name.clone(),
            tag: l.tag.clone(),
            // Kept nameless (the "names after the match" setting is off): numbers and agent only.
            hidden: l.was_hidden && l.name.is_empty(),
            is_me: l.me,
            agent: l.agent.clone(),
            picked: true,
            locked: true,
            card: Card {
                stats_on: true,
                rank_done: true,
                stats_done: true,
                rank: if l.tier >= 3 { logic::tier_name(l.tier) } else { String::new() },
                rank_tint: logic::tier_tint(l.tier),
                tier: l.tier,
                // Where a live row shows RR, this one shows kills / deaths / assists.
                rr: format!("{} / {} / {}", l.kills, l.deaths, l.assists),
                stats: format!(
                    "KD {}  ·  HS {}%  ·  ADR {}",
                    logic::kd(l.kills, l.deaths),
                    logic::percent(l.head, l.shots),
                    logic::adr(l.damage, l.rounds)
                ),
                seen: format!("ACS {}{}", logic::adr(l.score, l.rounds), if l.was_hidden { "  ·  was hidden" } else { "" }),
                ..Card::default()
            },
            ..Row::default()
        };
        let side = |ally: bool| lines.iter().filter(|l| l.ally == ally).map(row).collect::<Vec<Row>>();
        Some((side(true), side(false)))
    }

    /// "58% over 12 · KD 1.18 · ADR 152 · best Omen 64% · worst Jett 41% · Attack 54% · Defense 49%"
    fn map_line(&self, me: PlayerId, mine: &crate::store::MapStat) -> String {
        let mut line = format!(
            "You on {}:  {}% over {}  ·  KD {}  ·  ADR {}",
            mine.name,
            logic::percent(mine.wins, mine.games),
            mine.games,
            logic::kd(mine.kills, mine.deaths),
            logic::adr(mine.damage, mine.rounds)
        );
        // Best and worst agent need at least two matches each to mean anything.
        let agents: Vec<(String, u32)> = self
            .store
            .agents_on_map(me, &mine.name)
            .into_iter()
            .filter(|(_, games, _)| *games >= 2)
            .map(|(agent, games, wins)| (agent, logic::percent(wins, games)))
            .collect();
        let best = agents.iter().max_by_key(|(_, pct)| *pct);
        let worst = agents.iter().min_by_key(|(_, pct)| *pct);
        if let (Some(best), Some(worst)) = (best, worst) {
            line.push_str(&format!("  ·  best {} {}%", best.0, best.1));
            if worst.0 != best.0 {
                line.push_str(&format!("  ·  worst {} {}%", worst.0, worst.1));
            }
        }
        let (atk, atk_won, def, def_won) = self.store.sides(&mine.name);
        if atk + def > 0 {
            line.push_str(&format!(
                "  ·  Attack {}%  ·  Defense {}%",
                logic::percent(atk_won, atk),
                logic::percent(def_won, def)
            ));
        }
        line
    }

    /// The run of matches without a long break, newest first.
    fn session(&self, games: &[MyGame], now: i64, panel: &mut Panel) {
        let Some(newest) = games.first() else { return };
        let mut run: Vec<&MyGame> = vec![newest];
        for pair in games.windows(2) {
            if pair[0].seen_at - pair[1].seen_at >= SESSION_GAP {
                break;
            }
            run.push(&pair[1]);
        }
        let current = now - newest.seen_at < SESSION_GAP;
        panel.session_title = if current { "THIS SESSION" } else { "LAST SESSION" }.to_string();
        let count = |result: u8| run.iter().filter(|g| g.result == Some(result)).count();
        let (wins, losses, draws) = (count(1), count(0), count(2));
        let sum = |f: fn(&MyGame) -> u32| run.iter().map(|g| f(g)).sum::<u32>();
        let mut parts = vec![format!("{wins}W {losses}L{}", if draws > 0 { format!(" {draws}D") } else { String::new() })];
        if run.iter().any(|g| g.rr_change.is_some()) {
            parts.push(format!("{:+} RR", run.iter().filter_map(|g| g.rr_change).sum::<i32>()));
        }
        let streak = run.iter().take_while(|g| g.result == newest.result).count();
        let streak_letter = match newest.result {
            Some(1) => "W",
            Some(0) => "L",
            _ => "",
        };
        if streak >= 2 && !streak_letter.is_empty() {
            parts.push(format!("{streak_letter}{streak} streak"));
        }
        parts.push(format!("KD {}", logic::kd(sum(|g| g.kills), sum(|g| g.deaths))));
        parts.push(format!("HS {}%", logic::percent(sum(|g| g.head), sum(|g| g.shots))));
        panel.session_line = parts.join("  ·  ");
        panel.session_extra = format!(
            "ADR {}  ·  First kills {}  ·  First deaths {}  ·  Clutches {} of {}",
            logic::adr(sum(|g| g.damage), sum(|g| g.rounds)),
            sum(|g| g.first_kills),
            sum(|g| g.first_deaths),
            sum(|g| g.clutch_wins),
            sum(|g| g.clutch_tries)
        );
        panel.session_dots = run.iter().take(14).map(|g| g.result.unwrap_or(2)).collect();
        if current && self.settings.nudge > 0 && newest.result == Some(0) && streak as u32 >= self.settings.nudge {
            panel.nudge = format!("{streak} losses in a row. A short break might help.");
        }
    }

    fn recap_lines(&self, newest: Option<&MyGame>, now: i64, panel: &mut Panel) {
        // The finished match is the newest one, stored a moment ago. Anything older is a
        // previous match, and the result of this one is still on its way.
        let fetched = newest.filter(|g| self.post.is_none() && now - g.seen_at < 3 * 3600);
        let Some(g) = fetched else {
            panel.recap_title = "Match over".to_string();
            panel.recap_line = match (self.usable(), self.post.is_some()) {
                (true, true) => "Fetching the result…",
                (true, false) => "The result is not available.",
                (false, _) => "Results need the stats key.",
            }
            .to_string();
            return;
        };
        let (word, code) = match g.result {
            Some(1) => ("WIN", 1),
            Some(0) => ("LOSS", 2),
            _ => ("DRAW", 3),
        };
        panel.recap_result = code;
        // Modes without rounds (team deathmatch) have a winner but no round score.
        let score = if g.rounds_won + g.rounds_lost > 0 { format!(" {}-{}", g.rounds_won, g.rounds_lost) } else { String::new() };
        panel.recap_title =
            format!("{}  ·  {word}{score}{}", g.map, g.rr_change.map_or(String::new(), |rr| format!("  ·  {rr:+} RR")));
        panel.recap_line = format!(
            "{} / {} / {}  ·  KD {}  ·  HS {}%  ·  ADR {}",
            g.kills,
            g.deaths,
            g.assists,
            logic::kd(g.kills, g.deaths),
            logic::percent(g.head, g.shots),
            logic::adr(g.damage, g.rounds)
        );
        panel.recap_extra = format!(
            "First kills {}  ·  First deaths {}  ·  Clutches {} of {}{}",
            g.first_kills,
            g.first_deaths,
            g.clutch_wins,
            g.clutch_tries,
            g.estimate.map_or(String::new(), |e| format!("  ·  Estimate was {e}%"))
        );
    }
}

/// "With 3x (2-1) · Against 1x (0-1) · 4d ago"
fn seen_line(seen: &Seen, now: i64) -> String {
    let part = |label: &str, t: &Tally| (t.games > 0).then(|| format!("{label} {}x ({}-{})", t.games, t.wins, t.losses));
    let mut parts: Vec<String> = [part("With", &seen.with), part("Against", &seen.against)].into_iter().flatten().collect();
    if parts.is_empty() {
        return String::new();
    }
    if seen.last_at > 0 {
        parts.push(logic::ago(now - seen.last_at));
    }
    parts.join("  ·  ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{self, StatsLink};
    use crate::model::{FullMatch, Game, Phase, Rank, View};
    use crate::source::fixture::{self, Fixture, FixtureStats};
    use crate::source::Budget;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn player(n: u8) -> Uuid {
        Uuid::parse(&format!("00000000-0000-4000-8000-0000000000{n:02x}")).unwrap()
    }

    /// Runs the whole app (engine, stats worker, made-up game) for `seconds` of paused time.
    /// `early` are commands sent before anything else; `late` are sent after `late_at` seconds.
    fn play<S: StatsSource>(
        mode: &str,
        seconds: u64,
        store: &Store,
        source: Option<S>,
        early: Vec<Command>,
        late_at: u64,
        late: Vec<Command>,
    ) -> Vec<View> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap();
        let views: Rc<RefCell<Vec<View>>> = Rc::default();
        let sink = views.clone();
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (insight_tx, insight_rx) = mpsc::unbounded_channel();
        for command in early {
            command_tx.send(command).ok();
        }
        let link = StatsLink { commands: command_tx.clone(), insights: insight_rx };
        rt.block_on(async {
            let app = crate::together(engine::run_with(Fixture::new(mode), Some(link), move |v| sink.borrow_mut().push(v)), async {
                run(source, store, command_rx, move |i| {
                    insight_tx.send(i).ok();
                })
                .await
            });
            let script = async {
                tokio::time::sleep(Duration::from_secs(late_at)).await;
                for command in late {
                    command_tx.send(command).ok();
                }
                tokio::time::sleep(Duration::from_secs(seconds.saturating_sub(late_at))).await;
            };
            // The made-up game never ends on its own; the script's clock ends the run.
            crate::together(script, app).await;
        });
        let out = views.borrow().clone();
        out
    }

    fn seeded() -> Store {
        let store = Store::open(None).unwrap();
        fixture::seed(&store);
        store
    }

    #[test]
    fn by_default_a_player_who_was_hidden_stays_nameless_after_the_match() {
        let store = seeded();
        let views = play("post", 12, &store, Some(FixtureStats::new()), Vec::new(), 0, Vec::new());
        let recap = views.iter().filter(|v| v.phase == Phase::PostGame).last().unwrap();
        assert!(recap.board);
        let rows: Vec<&Row> = recap.allies.iter().chain(recap.enemies.iter()).collect();
        assert_eq!(rows.len(), 10, "everyone's numbers are on the scoreboard");
        assert_eq!(rows.iter().filter(|r| r.hidden).count(), 2);
        assert!(rows.iter().filter(|r| r.hidden).all(|r| r.name.is_empty() && !r.card.stats.is_empty()));
        assert!(rows.iter().all(|r| !r.name.contains("must stay hidden")));
        let (text, _) = store.dump();
        assert!(!text.contains("must stay hidden"), "nothing about them was kept");
        assert_eq!(store.summary(player(8), 50).games, 0);
    }

    #[test]
    fn a_whole_match_with_stats_never_touches_a_hidden_player() {
        let store = seeded();
        // The owner's choice for this install: name everyone once the match is over.
        store.set_setting("after", "on");
        let source = FixtureStats::new();
        let asked = source.asked.clone();
        let views = play("tour", 40, &store, Some(source), Vec::new(), 0, Vec::new());

        // Nothing was ever asked about the two hidden players, and both were in the match.
        let hidden = [player(3), player(8)];
        let asked = asked.borrow();
        assert!(asked.iter().all(|id| !hidden.contains(id)), "a hidden player was looked up");
        for n in [1u8, 2, 4, 5, 6, 7, 9, 10] {
            assert!(asked.contains(&player(n)), "player {n} was never looked up");
        }

        // In game: every visible player has a rank and a form line; hidden rows have nothing.
        let game = views.iter().filter(|v| v.phase == Phase::InGame).last().unwrap();
        for row in game.allies.iter().chain(game.enemies.iter()) {
            if row.hidden {
                assert_eq!(row.card, Card::default(), "a hidden row carries no stats, seen-before or note");
            } else {
                assert!(row.card.stats_on && !row.card.rank.is_empty() && !row.card.stats.is_empty(), "{}", row.name);
                assert_eq!(row.card.dots.len(), 5);
            }
        }
        assert_eq!(game.allies[0].card.rank, "Asc 2");
        assert_eq!(game.allies[0].card.rr, "61 RR");
        assert_eq!(game.allies[0].card.peak, "peak Imm 1");

        // The smurf flag: the new account far above its level, and nobody else. Never me.
        let flagged: Vec<&str> =
            game.allies.iter().chain(game.enemies.iter()).filter(|r| r.card.smurf).map(|r| r.name.as_str()).collect();
        assert_eq!(flagged, ["a very long name"]);
        assert!(game.enemies[2].card.signals.contains("level 19"));
        // Visible players first, hidden last, on both sides.
        assert!(game.allies[4].hidden && game.enemies[4].hidden);

        // Seen before, from the earlier matches: with nightowl three times, against one tap twice.
        assert!(game.allies[1].card.seen.starts_with("With 3x (2-1)"), "{}", game.allies[1].card.seen);
        assert!(game.enemies[1].card.seen.starts_with("Against 2x (1-1)"), "{}", game.enemies[1].card.seen);
        assert_eq!(game.enemies[0].card.note, "plays operator mid, pushes early");
        assert!(game.allies[0].card.seen.is_empty(), "I have not 'seen myself before'");

        // Map context and the rank gap.
        assert!(game.panel.map_line.starts_with("You on Ascent:"), "{}", game.panel.map_line);
        assert!(game.panel.map_line.contains("Attack") && game.panel.map_line.contains("Defense"));
        assert!((5..=95).contains(&game.panel.gap_pct));
        assert!(game.panel.gap_label.contains("est."));

        // Agent select: my team only, and no rank gap before the other team is known.
        let lobby = views.iter().filter(|v| v.phase == Phase::AgentSelect).last().unwrap();
        assert_eq!(lobby.panel.gap_pct, -1);
        assert_eq!(lobby.allies[0].card.rank, "Asc 2");

        // Post game: the result, the RR, my numbers and the estimate.
        let recap = views.iter().filter(|v| v.phase == Phase::PostGame).last().unwrap();
        assert_eq!(recap.panel.recap_result, 1);
        assert!(recap.panel.recap_title.contains("WIN 13-9"), "{}", recap.panel.recap_title);
        assert!(recap.panel.recap_title.contains("+21 RR"), "{}", recap.panel.recap_title);
        assert!(recap.panel.recap_extra.contains("First kills 1"), "{}", recap.panel.recap_extra);
        assert!(recap.panel.recap_extra.contains("First deaths 1"));
        assert!(recap.panel.recap_extra.contains("Clutches 1 of 2"));
        assert!(recap.panel.recap_extra.contains("Estimate was"));
        assert!(recap.panel.session_line.starts_with("3W 1L"), "{}", recap.panel.session_line);

        // While the match was live, no view ever named a hidden player.
        for v in views.iter().filter(|v| matches!(v.phase, Phase::AgentSelect | Phase::InGame)) {
            assert!(v.allies.iter().chain(v.enemies.iter()).all(|r| !r.name.contains("must stay hidden")));
        }
        // After it ended, the scoreboard lists all ten with that match's numbers, by name, as the
        // game's own end screen does, and the rows cannot be selected.
        assert!(recap.board);
        assert_eq!((recap.allies.len(), recap.enemies.len()), (5, 5));
        assert!(recap.allies.iter().chain(recap.enemies.iter()).all(|r| !r.hidden && !r.name.is_empty() && !r.card.stats.is_empty()));
        let was = recap.enemies.iter().find(|r| r.name == "must stay hidden too").expect("named after the match");
        assert!(was.card.seen.contains("was hidden") && was.card.rr.contains(" / "));
        assert!(recap.allies.iter().any(|r| r.is_me && r.name == "Rique"));

        // The database: visible players as before, and now the two who were hidden, from this one
        // match only. They were never looked up (checked at the top), and they count as met.
        let (text, _) = store.dump();
        assert!(text.contains("nightowl") && text.contains("one tap only"), "visible names are stored");
        assert_eq!(store.summary(player(6), 50).games, 11, "ten looked up, plus this match");
        assert_eq!(store.summary(player(3), 50).games, 1, "only this match: nothing was looked up");
        assert_eq!(store.summary(player(8), 50).games, 1);
        assert_eq!(store.seen(player(8), None).against.games, 1);
        // Hidden slots are remembered as side and agent only.
        assert_eq!(store.hidden_slots(Uuid::parse("00000000-0000-4000-8000-0000000000a1").unwrap()).len(), 2);
    }

    #[test]
    fn with_lookups_off_only_my_own_stats_are_fetched() {
        let store = seeded();
        store.set_setting("lookup", "off");
        let source = FixtureStats::new();
        let asked = source.asked.clone();
        let views = play("match", 20, &store, Some(source), Vec::new(), 0, Vec::new());
        assert!(!asked.borrow().is_empty());
        assert!(asked.borrow().iter().all(|id| *id == player(1)), "someone else's id left this PC");
        let game = views.last().unwrap();
        assert_eq!(game.allies[0].card.rank, "Asc 2");
        assert!(!game.allies[1].card.stats_on && game.allies[1].card.rank.is_empty());
        assert_eq!(game.panel.gap_pct, -1, "no gap without the other ranks");
        // Seen-before and notes come from this PC, so they still show.
        assert!(game.allies[1].card.seen.starts_with("With 3x"));
    }

    #[test]
    fn without_a_key_the_roster_notes_and_seen_before_still_work() {
        let store = seeded();
        let views = play("match", 20, &store, None::<FixtureStats>, Vec::new(), 0, Vec::new());
        let game = views.last().unwrap();
        assert_eq!((game.allies.len(), game.enemies.len()), (5, 5));
        assert_eq!(game.panel.stats_status, "Stats key missing");
        assert!(game.allies.iter().chain(game.enemies.iter()).all(|r| !r.card.stats_on && r.card.rank.is_empty()));
        assert!(game.allies[1].card.seen.starts_with("With 3x"));
        assert_eq!(game.enemies[0].card.note, "plays operator mid, pushes early");
    }

    #[test]
    fn a_note_goes_to_the_visible_player_and_nowhere_for_a_hidden_row() {
        let store = seeded();
        let late = vec![
            Command::Note { side: Side::Ally, index: 1, text: "calm\u{7} shot caller".into() },
            // The last row on my team and the last on theirs are the hidden players.
            Command::Note { side: Side::Ally, index: 4, text: "typed against a hidden row".into() },
            Command::Note { side: Side::Enemy, index: 4, text: "typed against a hidden row".into() },
            Command::Note { side: Side::Enemy, index: 9, text: "no such row".into() },
        ];
        let views = play("match", 20, &store, Some(FixtureStats::new()), Vec::new(), 10, late);
        assert_eq!(store.note(player(2)), "calm shot caller", "control characters are dropped");
        assert_eq!(views.last().unwrap().allies[1].card.note, "calm shot caller");
        let (text, _) = store.dump();
        assert!(!text.contains("hidden row") && !text.contains("no such row"));
    }

    #[test]
    fn settings_are_stored_and_take_effect() {
        let store = seeded();
        let early = vec![
            Command::Setting { key: "estimate".into(), value: "bar".into() },
            Command::Setting { key: "smurf".into(), value: "off".into() },
            Command::Setting { key: "seen".into(), value: "off".into() },
            Command::Setting { key: "not a setting".into(), value: "x".into() },
        ];
        let views = play("match", 20, &store, Some(FixtureStats::new()), early, 0, Vec::new());
        let game = views.last().unwrap();
        assert!(game.panel.gap_pct >= 0 && !game.panel.gap_label.contains("est."), "{}", game.panel.gap_label);
        assert!(game.enemies.iter().all(|r| !r.card.smurf));
        assert!(game.allies.iter().all(|r| r.card.seen.is_empty()));
        assert_eq!(store.setting("estimate").as_deref(), Some("bar"));
        assert_eq!(store.setting("not a setting"), None);
        // With seen-before off, nobody from this match was written down as met.
        assert_eq!(store.seen(player(9), None), Seen::default());
    }

    #[test]
    fn the_last_match_stays_up_in_the_menus_until_the_next_lobby() {
        let store = seeded();
        // "post": a match, then the menus. The recap time (three minutes) runs out during this.
        let views = play("post", 260, &store, Some(FixtureStats::new()), Vec::new(), 0, Vec::new());
        let last = views.last().unwrap();
        assert_eq!(last.phase, Phase::Menu, "the post-game time is over");
        assert!(last.board, "the scoreboard is still there");
        assert_eq!((last.allies.len(), last.enemies.len()), (5, 5));
        assert!(last.panel.recap_title.contains("WIN 13-9"), "{}", last.panel.recap_title);
    }

    #[test]
    fn a_deathmatch_shows_everyone_and_keeps_nothing_as_a_match() {
        let store = seeded();
        let before = store.my_games(50).len();
        let source = FixtureStats::new();
        let asked = source.asked.clone();
        let views = play("deathmatch", 30, &store, Some(source), Vec::new(), 0, Vec::new());
        let game = views.iter().filter(|v| v.phase == Phase::InGame).last().unwrap();
        // No teams: all ten are in one list, me first, the two hidden players last.
        assert_eq!((game.allies.len(), game.enemies.len()), (10, 0));
        assert!(game.allies[0].is_me);
        assert!(game.allies[..8].iter().all(|r| !r.hidden) && game.allies[8].hidden && game.allies[9].hidden);
        assert!(game.allies[..8].iter().all(|r| !r.card.rank.is_empty() && !r.card.stats.is_empty()));
        assert_eq!(game.queue, "Deathmatch");
        assert_eq!(game.panel.gap_pct, -1, "no teams, so no rank gap");
        // Hidden players are still never looked up.
        assert!(asked.borrow().iter().all(|id| *id != player(3) && *id != player(8)));
        // Nothing was written down as a match of mine, and nobody as met.
        assert_eq!(store.my_games(50).len(), before);
        assert_eq!(store.open_game(0), None);
        assert_eq!(store.seen(player(9), None), Seen::default());
        // After it ends there is no result to wait for and no recap of some other match.
        let after = views.iter().filter(|v| v.phase == Phase::PostGame).last().unwrap();
        assert!(after.panel.recap_title.is_empty(), "{}", after.panel.recap_title);
        assert_eq!(after.allies.len(), 10, "the list stays up for the recap time");
    }

    /// A source with a small allowance that refills after a minute.
    struct Tight {
        inner: FixtureStats,
        left: u32,
        refill: Instant,
        calls: Rc<RefCell<Vec<Instant>>>,
    }

    impl Tight {
        fn spend(&mut self) {
            self.calls.borrow_mut().push(Instant::now());
            self.left = self.left.saturating_sub(COST);
        }
    }

    impl StatsSource for Tight {
        async fn rank(&mut self, id: PlayerId) -> Result<Rank, SourceError> {
            self.spend();
            self.inner.rank(id).await
        }
        async fn history(&mut self, id: PlayerId) -> Result<Vec<Game>, SourceError> {
            self.spend();
            self.inner.history(id).await
        }
        async fn last_match(&mut self, me: PlayerId, hidden: &[(Side, Uuid)]) -> Result<Option<FullMatch>, SourceError> {
            self.spend();
            self.inner.last_match(me, hidden).await
        }
        fn budget(&self) -> Budget {
            let now = Instant::now();
            if now >= self.refill {
                Budget { remaining: 30, limit: 30, reset_in: Duration::ZERO }
            } else {
                Budget { remaining: self.left, limit: 30, reset_in: self.refill - now }
            }
        }
        fn sleep(&mut self) {}
    }

    #[test]
    fn lookups_stop_at_the_allowance_and_resume_after_the_refill() {
        let store = seeded();
        let calls: Rc<RefCell<Vec<Instant>>> = Rc::default();
        let rt_start = Rc::new(RefCell::new(None::<Instant>));
        let (calls_in, start_in) = (calls.clone(), rt_start.clone());
        // The source is built inside the run so its clock is the paused one.
        struct Lazy<F: FnMut() -> Tight>(Option<Tight>, F);
        impl<F: FnMut() -> Tight> Lazy<F> {
            fn get(&mut self) -> &mut Tight {
                if self.0.is_none() {
                    self.0 = Some((self.1)());
                }
                self.0.as_mut().unwrap()
            }
        }
        impl<F: FnMut() -> Tight> StatsSource for Lazy<F> {
            async fn rank(&mut self, id: PlayerId) -> Result<Rank, SourceError> {
                self.get().rank(id).await
            }
            async fn history(&mut self, id: PlayerId) -> Result<Vec<Game>, SourceError> {
                self.get().history(id).await
            }
            async fn last_match(&mut self, me: PlayerId, hidden: &[(Side, Uuid)]) -> Result<Option<FullMatch>, SourceError> {
                self.get().last_match(me, hidden).await
            }
            fn budget(&self) -> Budget {
                match &self.0 {
                    Some(tight) => tight.budget(),
                    None => Budget { remaining: 10, limit: 30, reset_in: Duration::from_secs(60) },
                }
            }
            fn sleep(&mut self) {}
        }
        let source = Lazy(None, move || {
            let now = Instant::now();
            *start_in.borrow_mut() = Some(now);
            Tight { inner: FixtureStats::new(), left: 10, refill: now + Duration::from_secs(60), calls: calls_in.clone() }
        });
        play("match", 120, &store, Some(source), Vec::new(), 0, Vec::new());
        let start = rt_start.borrow().unwrap();
        let calls = calls.borrow();
        // Ten units, two per call, two always kept back: four calls, then a wait for the refill.
        let early = calls.iter().filter(|at| **at - start < Duration::from_secs(55)).count();
        assert_eq!(early, 4, "calls before the refill");
        assert_eq!(calls.len(), 16, "eight visible players, a rank and a history each, by the end");
    }
}
