//! The worker's main loop: waits for game-state signals, loads the roster when there is one,
//! and sends the UI a complete view each time something changes. Does nothing between signals.

use crate::model::{Name, Phase, PlayerId, Roster, Row, Side, Signal, View};
use crate::source::assets::Catalog;
use crate::source::{DataSource, Retry, SourceError};
use crate::state::{self, Effect, RECAP_SECONDS};
use crate::stats::{Command, Insight, LobbyInfo, LobbySlot};
use std::collections::{HashMap, HashSet};
use std::task::Poll;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{timeout_at, Instant};

/// Agent select is re-read at this pace until every agent on my team is locked.
const PICK_REFRESH: Duration = Duration::from_secs(5);
/// A lobby or match that does not exist yet is looked for at this pace, this many times.
const RETRY: Duration = Duration::from_secs(2);
const MAX_RETRIES: u32 = 30;
/// A server asking for room gets it: 15 s, 30 s, 60 s, then no more attempts.
const BACKOFF: Duration = Duration::from_secs(15);
const MAX_BACKOFFS: u32 = 3;
/// A name lookup that got no answer at all is tried again this many times. One that was
/// answered, with names or with a refusal, is final for that lobby.
const MAX_NAME_RETRIES: u32 = 2;
/// Agent and map names are fetched at most this many times per lobby while they keep failing.
const MAX_CATALOG_TRIES: u32 = 2;

struct Lobby {
    roster: Roster,
    /// Read from the running match, not from agent select.
    in_game: bool,
    names: HashMap<PlayerId, Name>,
    /// Players whose name lookup is finished, whatever it gave. They are not asked about again.
    asked: HashSet<PlayerId>,
    name_retries: u32,
    names_problem: Option<SourceError>,
}

struct Engine<S, P> {
    source: S,
    push: P,
    phase: Phase,
    queue: String,
    lobby: Option<Lobby>,
    catalog: Option<Catalog>,
    catalog_tries: u32,
    problem: Option<SourceError>,
    deadline: Option<Instant>,
    retries: u32,
    /// The stats worker, when there is one: where to send it work, and what it last reported.
    commands: Option<mpsc::UnboundedSender<Command>>,
    insights: Option<mpsc::UnboundedReceiver<Insight>>,
    insight: Insight,
    /// Rounds won by my team and the other in the match in progress.
    score: Option<(u32, u32)>,
}

/// The two ends that connect the engine to the stats worker.
pub struct StatsLink {
    pub commands: mpsc::UnboundedSender<Command>,
    pub insights: mpsc::UnboundedReceiver<Insight>,
}

/// What woke the engine up.
enum Wake {
    Signal(Signal),
    Insight(Insight),
    Deadline,
    Closed,
}

/// The player list alone, without stats.
#[cfg(test)]
pub async fn run<S: DataSource, P: FnMut(View)>(source: S, push: P) {
    run_with(source, None, push).await;
}

pub async fn run_with<S: DataSource, P: FnMut(View)>(source: S, stats: Option<StatsLink>, push: P) {
    let (commands, insights) = match stats {
        Some(link) => (Some(link.commands), Some(link.insights)),
        None => (None, None),
    };
    let mut engine = Engine {
        source,
        push,
        phase: Phase::Closed,
        queue: String::new(),
        lobby: None,
        catalog: None,
        catalog_tries: 0,
        problem: None,
        deadline: None,
        retries: 0,
        commands,
        insights,
        insight: Insight::default(),
        score: None,
    };
    engine.run().await;
}

impl<S: DataSource, P: FnMut(View)> Engine<S, P> {
    async fn run(&mut self) {
        let mut signals = self.source.watch();
        self.show();
        loop {
            match self.wake(&mut signals).await {
                Wake::Signal(signal) => self.on_signal(signal),
                Wake::Insight(insight) => self.insight = insight,
                Wake::Deadline => self.on_deadline().await,
                Wake::Closed => return,
            }
            self.show();
        }
    }

    /// Waits for whichever comes first: a change in the game, news from the stats worker, or
    /// the next deadline. The game is looked at first, so stats can never delay a state change.
    async fn wake(&mut self, signals: &mut mpsc::Receiver<Signal>) -> Wake {
        let deadline = self.deadline;
        let insights = &mut self.insights;
        let first = std::future::poll_fn(|cx| {
            match signals.poll_recv(cx) {
                Poll::Ready(Some(signal)) => return Poll::Ready(Wake::Signal(signal)),
                Poll::Ready(None) => return Poll::Ready(Wake::Closed),
                Poll::Pending => {}
            }
            if let Some(rx) = insights.as_mut() {
                match rx.poll_recv(cx) {
                    Poll::Ready(Some(insight)) => return Poll::Ready(Wake::Insight(insight)),
                    // The worker is gone: carry on with the player list alone.
                    Poll::Ready(None) => *insights = None,
                    Poll::Pending => {}
                }
            }
            Poll::Pending
        });
        match deadline {
            Some(at) => timeout_at(at, first).await.unwrap_or(Wake::Deadline),
            None => first.await,
        }
    }

    fn tell(&self, command: Command) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(command);
        }
    }

    /// The current roster as the stats worker needs it. Hidden slots go over without an id,
    /// exactly as they are held here.
    fn lobby_info(&self) -> Option<LobbyInfo> {
        let lobby = self.lobby.as_ref()?;
        let empty = Catalog::default();
        let catalog = self.catalog.as_ref().unwrap_or(&empty);
        Some(LobbyInfo {
            match_id: lobby.roster.match_id,
            in_game: lobby.in_game,
            map: catalog.map(&lobby.roster.map_url),
            queue: lobby.roster.queue.to_ascii_lowercase(),
            slots: lobby
                .roster
                .slots
                .iter()
                .map(|s| LobbySlot {
                    id: s.id,
                    is_me: s.is_me,
                    side: s.side,
                    agent: s.agent,
                    agent_name: s.agent.and_then(|a| catalog.agent(&a)).unwrap_or("").to_string(),
                    level: s.level,
                    party: s.party,
                })
                .collect(),
        })
    }

    fn on_signal(&mut self, signal: Signal) {
        if let Signal::Score(ours, theirs) = signal {
            self.score = Some((ours, theirs));
            return;
        }
        if let Signal::Game { queue, .. } = &signal {
            self.queue = queue.clone();
        }
        let before = self.phase;
        let (next, effect) = state::step(self.phase, &signal);
        self.phase = next;
        if next != before {
            // A score belongs to one match.
            self.score = None;
            match (next, effect) {
                (_, Effect::Sleep) => self.tell(Command::Sleep),
                (_, Effect::Recap) => self.tell(Command::GameOver),
                (Phase::Menu | Phase::Queue, Effect::DropRoster) => {
                    self.tell(Command::Leave);
                    self.tell(Command::Idle);
                }
                (Phase::Menu | Phase::Queue, _) => self.tell(Command::Idle),
                _ => {}
            }
        }
        match effect {
            Effect::None => {}
            Effect::LoadRoster => {
                self.retries = 0;
                self.problem = None;
                self.deadline = Some(Instant::now());
            }
            Effect::DropRoster => {
                self.lobby = None;
                self.problem = None;
                self.deadline = None;
                self.catalog_tries = 0;
            }
            Effect::Recap => {
                self.problem = None;
                self.deadline = Some(Instant::now() + Duration::from_secs(RECAP_SECONDS));
            }
            Effect::Sleep => {
                self.lobby = None;
                self.problem = None;
                self.deadline = None;
                self.queue.clear();
                self.catalog_tries = 0;
                self.source.sleep();
            }
        }
    }

    async fn on_deadline(&mut self) {
        self.deadline = None;
        match self.phase {
            Phase::AgentSelect | Phase::InGame => self.load().await,
            Phase::PostGame => {
                let (next, _) = state::recap_expired(self.phase);
                self.phase = next;
                self.lobby = None;
                self.catalog_tries = 0;
                self.tell(Command::Leave);
                self.tell(Command::Idle);
            }
            _ => {}
        }
    }

    async fn load(&mut self) {
        let phase = self.phase;
        match self.source.roster(phase).await {
            Ok(mut roster) => {
                // Me first, then everyone who can be shown, then hidden players. Each side keeps
                // its own list; the order within each group stays the game's.
                roster.slots.sort_by_key(|s| (s.side == Side::Enemy, !s.is_me, s.id.is_none()));
                self.problem = None;
                self.retries = 0;
                let picking = phase == Phase::AgentSelect
                    && roster.slots.iter().any(|s| s.side == Side::Ally && !s.locked);
                let (mut names, mut asked, name_retries, names_problem) = match self.lobby.take() {
                    Some(old) => (old.names, old.asked, old.name_retries, old.names_problem),
                    None => (HashMap::new(), HashSet::new(), 0, None),
                };
                names.retain(|id, _| roster.slots.iter().any(|s| s.id == Some(*id)));
                asked.retain(|id| roster.slots.iter().any(|s| s.id == Some(*id)));
                // Only ids of visible players exist in the roster, so only those can be asked for.
                let unknown: Vec<PlayerId> = roster
                    .slots
                    .iter()
                    .filter_map(|s| s.id)
                    .filter(|id| !names.contains_key(id) && !asked.contains(id))
                    .collect();
                let in_game = phase == Phase::InGame;
                self.lobby = Some(Lobby { roster, in_game, names, asked, name_retries, names_problem });

                // Tier 1 goes on screen before any further request.
                self.show();

                let mut names_again = false;
                let mut named: Vec<Name> = Vec::new();
                if !unknown.is_empty() {
                    let result = self.source.names(&unknown).await;
                    if let Some(lobby) = self.lobby.as_mut() {
                        match result {
                            Ok(found) => {
                                lobby.names_problem = None;
                                lobby.asked.extend(unknown.iter().copied());
                                named = found.clone();
                                for name in found {
                                    lobby.names.insert(name.id, name);
                                }
                            }
                            Err(e) => {
                                lobby.names_problem = Some(e);
                                if e.retry() == Retry::Soon && lobby.name_retries < MAX_NAME_RETRIES {
                                    lobby.name_retries += 1;
                                    names_again = true;
                                } else {
                                    // Refused, told to slow down, or out of tries: this is final.
                                    lobby.asked.extend(unknown.iter().copied());
                                }
                            }
                        }
                    }
                    self.show();
                }
                if self.catalog.is_none() && self.catalog_tries < MAX_CATALOG_TRIES {
                    self.catalog_tries += 1;
                    self.catalog = self.source.catalog().await.ok();
                }
                // Stats start once the map and agents have names, so what gets stored is readable.
                if let Some(info) = self.lobby_info() {
                    self.tell(Command::Lobby(info));
                }
                // After the lobby, so the worker knows these players belong to it.
                if !named.is_empty() {
                    self.tell(Command::Names(named));
                }
                if picking || names_again {
                    self.deadline = Some(Instant::now() + PICK_REFRESH);
                }
            }
            Err(e) => {
                // The last good roster stays on screen whatever happens here.
                self.problem = Some(e);
                self.retries += 1;
                self.deadline = match e.retry() {
                    Retry::Soon if self.retries < MAX_RETRIES => Some(Instant::now() + RETRY),
                    Retry::BackOff if self.retries <= MAX_BACKOFFS => {
                        Some(Instant::now() + BACKOFF * 2u32.pow(self.retries - 1))
                    }
                    _ => None,
                };
            }
        }
    }

    fn show(&mut self) {
        let view = self.view();
        (self.push)(view);
    }

    fn view(&self) -> View {
        let mut view = View::empty(self.phase);
        view.queue = self.queue.clone();
        view.panel = self.insight.panel.clone();
        view.status = match self.phase {
            Phase::Closed => "Waiting for VALORANT",
            Phase::Menu => "In menus",
            Phase::Queue => "In queue",
            Phase::AgentSelect => "Agent select",
            Phase::InGame => "In game",
            Phase::PostGame => "Post game",
        }
        .to_string();
        let names_problem = self.lobby.as_ref().and_then(|l| l.names_problem);
        view.detail = match (self.problem, names_problem) {
            (Some(SourceError::NotReady), _) => "Loading players…".to_string(),
            (Some(e), _) => format!("Players unavailable: {e}. Showing what is known."),
            (None, Some(e)) => format!("Names unavailable: {e}."),
            (None, None) => self.source.diagnostics(),
        };
        let Some(lobby) = &self.lobby else {
            view.queue = title_case(&view.queue);
            // The last match's scoreboard stays up in the menus and the queue until the next lobby.
            if let (Phase::Menu | Phase::Queue | Phase::PostGame, Some((allies, enemies))) = (self.phase, &self.insight.board) {
                view.board = true;
                view.allies = allies.clone();
                view.enemies = enemies.clone();
            }
            return view;
        };
        let empty = Catalog::default();
        let catalog = self.catalog.as_ref().unwrap_or(&empty);
        if !lobby.roster.map_url.is_empty() {
            view.map = catalog.map(&lobby.roster.map_url);
        }
        if view.queue.is_empty() {
            view.queue = lobby.roster.queue.clone();
        }
        view.queue = title_case(&view.queue);
        view.one_list = lobby.in_game && lobby.roster.slots.iter().all(|s| s.side == Side::Ally);
        if let (Phase::InGame, false, Some((ours, theirs))) = (self.phase, view.one_list, self.score) {
            view.score = format!("{ours} - {theirs}");
        }
        // Once the finished match is in, the recap shows its scoreboard instead of the lobby.
        if let (Phase::PostGame, Some((allies, enemies))) = (self.phase, &self.insight.board) {
            view.board = true;
            view.allies = allies.clone();
            view.enemies = enemies.clone();
            return view;
        }
        for slot in &lobby.roster.slots {
            let name = slot.id.and_then(|id| lobby.names.get(&id)).filter(|n| !n.name.is_empty());
            let row = Row {
                name: name.map(|n| n.name.clone()).unwrap_or_default(),
                tag: name.map(|n| n.tag.clone()).unwrap_or_default(),
                hidden: slot.id.is_none(),
                is_me: slot.is_me,
                unnamed: name.is_none() && slot.id.is_some_and(|id| lobby.asked.contains(&id)),
                agent: slot.agent.and_then(|a| catalog.agent(&a)).unwrap_or("").to_string(),
                role: slot.agent.map(|a| catalog.role(&a)).unwrap_or_default().label(),
                picked: slot.agent.is_some(),
                locked: slot.locked,
                level: slot.level,
                party: slot.party,
                // Cards are keyed by id, and a hidden slot has none: it can only get an empty one.
                card: slot.id.and_then(|id| self.insight.cards.get(&id)).cloned().unwrap_or_default(),
            };
            match slot.side {
                Side::Ally => view.allies.push(row),
                Side::Enemy => view.enemies.push(row),
            }
        }
        view
    }
}

/// Queue ids arrive lower-case ("competitive"); show them as words.
fn title_case(id: &str) -> String {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Stage, Uuid};
    use crate::source::fixture::Fixture;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A fixture whose signals are fed by the test.
    struct Scripted {
        inner: Fixture,
        rx: Option<mpsc::Receiver<Signal>>,
        asked: Rc<RefCell<Vec<PlayerId>>>,
        slept: Rc<RefCell<u32>>,
        /// Roster reads so far, and the error to give from the second read on.
        reads: Rc<RefCell<u32>>,
        fail_after_first: Option<SourceError>,
        /// Name lookups so far, and the error every one of them gets.
        name_calls: Rc<RefCell<u32>>,
        names_error: Option<SourceError>,
        /// Catalog fetches so far, and whether they fail.
        catalog_calls: Rc<RefCell<u32>>,
        catalog_fails: bool,
    }

    impl Scripted {
        fn new(mode: &str, rx: mpsc::Receiver<Signal>) -> Self {
            Self {
                inner: Fixture::new(mode),
                rx: Some(rx),
                asked: Rc::default(),
                slept: Rc::default(),
                reads: Rc::default(),
                fail_after_first: None,
                name_calls: Rc::default(),
                names_error: None,
                catalog_calls: Rc::default(),
                catalog_fails: false,
            }
        }
    }

    impl DataSource for Scripted {
        fn watch(&mut self) -> mpsc::Receiver<Signal> {
            self.rx.take().expect("watch called once")
        }
        async fn roster(&mut self, phase: Phase) -> Result<Roster, SourceError> {
            *self.reads.borrow_mut() += 1;
            match self.fail_after_first {
                Some(e) if *self.reads.borrow() > 1 => Err(e),
                _ => self.inner.roster(phase).await,
            }
        }
        async fn names(&mut self, players: &[PlayerId]) -> Result<Vec<Name>, SourceError> {
            *self.name_calls.borrow_mut() += 1;
            self.asked.borrow_mut().extend_from_slice(players);
            match self.names_error {
                Some(e) => Err(e),
                None => self.inner.names(players).await,
            }
        }
        async fn catalog(&mut self) -> Result<Catalog, SourceError> {
            *self.catalog_calls.borrow_mut() += 1;
            if self.catalog_fails {
                return Err(SourceError::Network);
            }
            self.inner.catalog().await
        }
        fn sleep(&mut self) {
            *self.slept.borrow_mut() += 1;
        }
        fn diagnostics(&self) -> String {
            String::new()
        }
    }

    fn game(stage: Stage, matchmaking: bool) -> Signal {
        Signal::Game { stage, matchmaking, queue: "competitive".into() }
    }

    #[test]
    fn a_whole_match_end_to_end() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap();
        let (tx, rx) = mpsc::channel(8);
        let views: Rc<RefCell<Vec<View>>> = Rc::new(RefCell::new(Vec::new()));
        let source = Scripted::new("tour", rx);
        let (asked, slept, catalog_calls) = (source.asked.clone(), source.slept.clone(), source.catalog_calls.clone());
        let sink = views.clone();

        rt.block_on(async move {
            let feed = async move {
                for (signal, hold) in [
                    (Signal::ClientOnly, 1),
                    (game(Stage::Menus, false), 1),
                    (game(Stage::Menus, true), 1),
                    (game(Stage::Pregame, false), 12),
                    (game(Stage::Ingame, false), 5),
                    (game(Stage::Menus, false), RECAP_SECONDS + 5),
                    (Signal::ClientGone, 1),
                ] {
                    tx.send(signal).await.unwrap();
                    tokio::time::sleep(Duration::from_secs(hold)).await;
                }
            };
            // The engine ends when the feed is dropped.
            let engine = run(source, move |v| sink.borrow_mut().push(v));
            let mut engine = std::pin::pin!(engine);
            let mut feed = std::pin::pin!(feed);
            std::future::poll_fn(|cx| {
                let _ = engine.as_mut().poll(cx);
                feed.as_mut().poll(cx)
            })
            .await;
        });

        let views = views.borrow();
        let phases: Vec<Phase> = views.iter().map(|v| v.phase).collect();
        let mut order = phases.clone();
        order.dedup();
        assert_eq!(
            order,
            [Phase::Closed, Phase::Menu, Phase::Queue, Phase::AgentSelect, Phase::InGame, Phase::PostGame, Phase::Menu, Phase::Closed]
        );

        // Agent select: my team only, names filled in, hidden player never named.
        let lobby = views.iter().filter(|v| v.phase == Phase::AgentSelect).last().unwrap();
        assert_eq!((lobby.allies.len(), lobby.enemies.len()), (5, 0));
        assert_eq!(lobby.map, "Ascent");
        assert_eq!(lobby.allies[0].name, "Rique");
        assert!(lobby.allies[0].is_me);
        assert_eq!(lobby.allies[1].name, "nightowl");
        assert!(lobby.allies[4].hidden && lobby.allies[4].name.is_empty() && lobby.allies[4].level.is_none());
        assert_eq!(lobby.allies[4].agent, "Sova", "a hidden player still shows the agent");
        let hidden_order: Vec<bool> = lobby.allies.iter().map(|r| r.hidden).collect();
        assert_eq!(hidden_order, [false, false, false, false, true], "players that can be shown come first");
        assert!(lobby.allies.iter().all(|r| r.locked), "the last look has everyone locked in");

        // In game: both teams.
        let game_view = views.iter().filter(|v| v.phase == Phase::InGame).last().unwrap();
        assert_eq!((game_view.allies.len(), game_view.enemies.len()), (5, 5));
        assert_eq!(game_view.enemies[0].name, "wholesome jett");
        assert!(game_view.enemies[4].hidden && game_view.enemies[4].name.is_empty());
        assert!(game_view.enemies[..4].iter().all(|r| !r.hidden));
        assert_eq!(game_view.enemies[0].party, game_view.enemies[1].party);

        // The recap keeps the match on screen; the menu afterwards does not.
        assert_eq!(views.iter().filter(|v| v.phase == Phase::PostGame).last().unwrap().enemies.len(), 5);
        assert!(views.iter().rev().find(|v| v.phase == Phase::Menu).unwrap().allies.is_empty());

        // No name was ever requested for a hidden player, and each visible one was asked once.
        let hidden = [
            Uuid::parse("00000000-0000-4000-8000-000000000003").unwrap(),
            Uuid::parse("00000000-0000-4000-8000-000000000008").unwrap(),
        ];
        let asked = asked.borrow();
        assert!(asked.iter().all(|id| !hidden.contains(id)));
        assert_eq!(asked.len(), 8);
        let mut unique = asked.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 8);

        // Nowhere in any view does a hidden player's name appear.
        for v in views.iter() {
            for r in v.allies.iter().chain(v.enemies.iter()) {
                assert!(!r.name.contains("must stay hidden"));
            }
        }
        assert_eq!(*slept.borrow(), 1, "going away puts the source to sleep once");
        assert_eq!(*catalog_calls.borrow(), 1, "agent and map names are fetched once");

        // Row states: agents carry a role, a chosen agent counts as picked, nobody is left unnamed.
        assert_eq!((lobby.allies[0].agent.as_str(), lobby.allies[0].role), ("Omen", "Controller"));
        assert!(game_view.allies.iter().chain(game_view.enemies.iter()).all(|r| r.picked && !r.unnamed));
    }

    struct Outcome {
        reads: u32,
        name_calls: u32,
        catalog_calls: u32,
        views: Vec<View>,
    }

    /// Holds one game state for two minutes with a source set up by `prepare`.
    fn hold(mode: &str, stage: Stage, prepare: impl FnOnce(&mut Scripted)) -> Outcome {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap();
        let (tx, rx) = mpsc::channel(8);
        let views: Rc<RefCell<Vec<View>>> = Rc::new(RefCell::new(Vec::new()));
        let mut source = Scripted::new(mode, rx);
        prepare(&mut source);
        let (reads, name_calls, catalog_calls) =
            (source.reads.clone(), source.name_calls.clone(), source.catalog_calls.clone());
        let sink = views.clone();
        rt.block_on(async move {
            let feed = async move {
                tx.send(game(stage, false)).await.unwrap();
                tokio::time::sleep(Duration::from_secs(120)).await;
            };
            let engine = run(source, move |v| sink.borrow_mut().push(v));
            let mut engine = std::pin::pin!(engine);
            let mut feed = std::pin::pin!(feed);
            std::future::poll_fn(|cx| {
                let _ = engine.as_mut().poll(cx);
                feed.as_mut().poll(cx)
            })
            .await;
        });
        let views = views.borrow().clone();
        Outcome { reads: *reads.borrow(), name_calls: *name_calls.borrow(), catalog_calls: *catalog_calls.borrow(), views }
    }

    /// Agent select with a source that fails from the second roster read on.
    fn agent_select_with_failure(error: SourceError) -> (u32, Vec<View>) {
        let out = hold("lobby", Stage::Pregame, |s| s.fail_after_first = Some(error));
        (out.reads, out.views)
    }

    #[test]
    fn a_refusal_is_not_retried_and_the_roster_stays() {
        for error in [SourceError::Auth, SourceError::Http(403), SourceError::Shape("lobby")] {
            let (reads, views) = agent_select_with_failure(error);
            assert_eq!(reads, 2, "one good read, one refused read, then nothing for {error:?}");
            let last = views.last().unwrap();
            assert_eq!(last.allies.len(), 5, "the last good roster is still shown");
            assert_eq!(last.allies[0].name, "Rique");
            assert!(last.detail.contains("Players unavailable"));
        }
    }

    #[test]
    fn a_rate_limit_backs_off_and_then_stops() {
        let (reads, views) = agent_select_with_failure(SourceError::Http(429));
        // Good read at 0 s, refused at 5 s, then retries at 20 s, 50 s and 110 s. No more after that.
        assert_eq!(reads, 5);
        assert_eq!(views.last().unwrap().allies.len(), 5);
    }

    #[test]
    fn a_missing_lobby_is_looked_for_again() {
        let (reads, _) = agent_select_with_failure(SourceError::NotReady);
        // One good read, then every 2 s up to the cap.
        assert_eq!(reads, 1 + MAX_RETRIES);
    }

    #[test]
    fn while_the_match_roster_is_loading_the_lobby_still_reads_as_two_teams() {
        // Agent select, then the match starts but its roster is not readable yet.
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap();
        let (tx, rx) = mpsc::channel(8);
        let views: Rc<RefCell<Vec<View>>> = Rc::new(RefCell::new(Vec::new()));
        let mut source = Scripted::new("match", rx);
        source.fail_after_first = Some(SourceError::NotReady);
        let sink = views.clone();
        rt.block_on(async move {
            let feed = async move {
                tx.send(game(Stage::Pregame, false)).await.unwrap();
                tokio::time::sleep(Duration::from_secs(1)).await;
                tx.send(game(Stage::Ingame, false)).await.unwrap();
                tokio::time::sleep(Duration::from_secs(10)).await;
            };
            let engine = run(source, move |v| sink.borrow_mut().push(v));
            let mut engine = std::pin::pin!(engine);
            let mut feed = std::pin::pin!(feed);
            std::future::poll_fn(|cx| {
                let _ = engine.as_mut().poll(cx);
                feed.as_mut().poll(cx)
            })
            .await;
        });
        let views = views.borrow();
        let in_game: Vec<&View> = views.iter().filter(|v| v.phase == Phase::InGame).collect();
        assert!(!in_game.is_empty());
        for v in in_game {
            assert_eq!((v.allies.len(), v.enemies.len()), (5, 0), "my team from agent select stays up");
            assert!(!v.one_list, "five allies and no enemies yet is not a match without teams");
        }
    }

    #[test]
    fn a_match_roster_with_one_side_is_one_list() {
        let out = hold("deathmatch", Stage::Ingame, |_| {});
        let last = out.views.last().unwrap();
        assert_eq!((last.allies.len(), last.enemies.len()), (10, 0));
        assert!(last.one_list);
        let teams = hold("match", Stage::Ingame, |_| {});
        assert!(!teams.views.last().unwrap().one_list);
        let lobby = hold("lobby", Stage::Pregame, |_| {});
        assert!(!lobby.views.last().unwrap().one_list, "agent select never is");
    }

    #[test]
    fn a_refused_name_lookup_is_asked_once() {
        for error in [SourceError::Http(403), SourceError::Auth, SourceError::Http(429), SourceError::Shape("names")] {
            // The fixture lobby is read three times (0 s, 5 s, 10 s) before everyone is locked in.
            let out = hold("lobby", Stage::Pregame, |s| s.names_error = Some(error));
            assert_eq!(out.reads, 3, "{error:?}");
            assert_eq!(out.name_calls, 1, "one lookup, not one per refresh, for {error:?}");
            let last = out.views.last().unwrap();
            assert_eq!(last.allies.len(), 5, "the roster is still shown");
            assert!(last.detail.contains("Names unavailable"));
            // Visible players are marked as unnamed instead of loading forever; hidden ones are not.
            assert!(last.allies[0].unnamed && last.allies[0].name.is_empty());
            assert!(last.allies[4].hidden && !last.allies[4].unnamed);
        }
    }

    #[test]
    fn a_name_lookup_with_no_answer_is_tried_again_a_few_times() {
        // In game there is no refresh, so the retries are the only further reads.
        let out = hold("match", Stage::Ingame, |s| s.names_error = Some(SourceError::Network));
        assert_eq!(out.name_calls, 1 + MAX_NAME_RETRIES);
        assert_eq!(out.reads, 1 + MAX_NAME_RETRIES);
        let last = out.views.last().unwrap();
        assert_eq!((last.allies.len(), last.enemies.len()), (5, 5));
        assert!(last.enemies[0].unnamed);
        let loading_first = out.views.iter().any(|v| v.enemies.first().is_some_and(|r| !r.unnamed && r.name.is_empty()));
        assert!(loading_first, "rows read as loading until the lookup is given up");
    }

    #[test]
    fn a_failing_catalog_is_fetched_a_limited_number_of_times() {
        let out = hold("lobby", Stage::Pregame, |s| s.catalog_fails = true);
        assert_eq!(out.reads, 3);
        assert_eq!(out.catalog_calls, MAX_CATALOG_TRIES);
        let last = out.views.last().unwrap();
        // A chosen agent without a known name is still "picked", not "picking".
        assert!(last.allies[0].picked && last.allies[0].agent.is_empty());
        assert_eq!(last.allies[0].name, "Rique");
    }
}
