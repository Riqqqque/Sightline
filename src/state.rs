//! The game-state machine. Pure: a phase and a signal in, a phase and what to do out.

use crate::model::{Phase, Signal, Stage};

/// What the engine should do after a transition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Nothing beyond showing the new phase.
    None,
    /// Entered a lobby or match: fetch who is in it.
    LoadRoster,
    /// Left a lobby or match without finishing it (dodge, or back to menus): drop it.
    DropRoster,
    /// A match just ended: keep it on screen as a recap.
    Recap,
    /// The game or client went away: drop everything and stop working.
    Sleep,
}

/// How long the recap stays before the idle view returns.
pub const RECAP_SECONDS: u64 = 180;

pub fn step(phase: Phase, signal: &Signal) -> (Phase, Effect) {
    use Phase::*;
    let next = match signal {
        Signal::Score(..) => return (phase, Effect::None),
        Signal::ClientGone | Signal::ClientOnly => Closed,
        Signal::Game { stage: Stage::Pregame, .. } => AgentSelect,
        Signal::Game { stage: Stage::Ingame, .. } => InGame,
        Signal::Game { stage: Stage::Menus, matchmaking: true, .. } => Queue,
        Signal::Game { stage: Stage::Menus, matchmaking: false, .. } => match phase {
            InGame | PostGame => PostGame,
            _ => Menu,
        },
    };
    let effect = match (phase, next) {
        (a, b) if a == b => Effect::None,
        (_, Closed) => Effect::Sleep,
        (_, AgentSelect) | (_, InGame) => Effect::LoadRoster,
        (InGame, PostGame) => Effect::Recap,
        (AgentSelect | InGame | PostGame, Menu | Queue) => Effect::DropRoster,
        _ => Effect::None,
    };
    (next, effect)
}

/// The recap timed out.
pub fn recap_expired(phase: Phase) -> (Phase, Effect) {
    match phase {
        Phase::PostGame => (Phase::Menu, Effect::DropRoster),
        other => (other, Effect::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Phase::*;

    fn game(stage: Stage, matchmaking: bool) -> Signal {
        Signal::Game { stage, matchmaking, queue: String::new() }
    }

    #[test]
    fn a_normal_match() {
        let mut p = Closed;
        let script = [
            (Signal::ClientOnly, Closed, Effect::None),
            (game(Stage::Menus, false), Menu, Effect::None),
            (game(Stage::Menus, true), Queue, Effect::None),
            (game(Stage::Pregame, false), AgentSelect, Effect::LoadRoster),
            (game(Stage::Pregame, false), AgentSelect, Effect::None),
            (game(Stage::Ingame, false), InGame, Effect::LoadRoster),
            (game(Stage::Menus, false), PostGame, Effect::Recap),
            (game(Stage::Menus, false), PostGame, Effect::None),
            (game(Stage::Menus, true), Queue, Effect::DropRoster),
        ];
        for (signal, want, effect) in script {
            let (next, e) = step(p, &signal);
            assert_eq!((next, e), (want, effect), "from {p:?} on {signal:?}");
            p = next;
        }
    }

    #[test]
    fn dodge_returns_to_queue_or_menu() {
        assert_eq!(step(AgentSelect, &game(Stage::Menus, true)), (Queue, Effect::DropRoster));
        assert_eq!(step(AgentSelect, &game(Stage::Menus, false)), (Menu, Effect::DropRoster));
    }

    #[test]
    fn closing_from_anywhere_sleeps() {
        for p in [Menu, Queue, AgentSelect, InGame, PostGame] {
            assert_eq!(step(p, &Signal::ClientGone), (Closed, Effect::Sleep));
            assert_eq!(step(p, &Signal::ClientOnly), (Closed, Effect::Sleep));
        }
        assert_eq!(step(Closed, &Signal::ClientGone), (Closed, Effect::None));
    }

    #[test]
    fn started_mid_match() {
        assert_eq!(step(Closed, &game(Stage::Ingame, false)), (InGame, Effect::LoadRoster));
        assert_eq!(step(Closed, &game(Stage::Pregame, false)), (AgentSelect, Effect::LoadRoster));
    }

    #[test]
    fn recap_expires_to_menu() {
        assert_eq!(recap_expired(PostGame), (Menu, Effect::DropRoster));
        assert_eq!(recap_expired(InGame), (InGame, Effect::None));
    }
}
