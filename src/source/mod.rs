//! Everything that talks to the game client or to the network lives under this module.
//! The rest of the app sees only the `DataSource` trait and the plain types in `model`.

pub mod assets;
pub mod fixture;
pub mod henrik;
pub mod local;
pub mod wire;

use crate::model::{FullMatch, Game, Name, Phase, PlayerId, Rank, Roster, Side, Signal, Uuid};
use std::fmt;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceError {
    /// The Riot Client is not running or not answering.
    ClientUnavailable,
    /// The client is up but the thing asked for does not exist yet (no lobby, tokens not ready).
    NotReady,
    /// The session was rejected even after a refresh.
    Auth,
    /// The region could not be worked out from the game's log.
    Region,
    /// A server answered with this status.
    Http(u16),
    /// No answer.
    Network,
    /// The answer did not have the expected shape. The name says which answer.
    Shape(&'static str),
}

impl SourceError {
    /// What to do after a failed roster read.
    pub fn retry(&self) -> Retry {
        match self {
            // Nothing was refused: the lobby or the client simply is not there yet.
            Self::NotReady | Self::Network | Self::ClientUnavailable | Self::Region => Retry::Soon,
            // The server is asking for room, or is having trouble.
            Self::Http(429) | Self::Http(500..=599) => Retry::BackOff,
            // Refused or unreadable. Asking again would only repeat it.
            Self::Auth | Self::Http(_) | Self::Shape(_) => Retry::Never,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Retry {
    Soon,
    BackOff,
    Never,
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClientUnavailable => f.write_str("Riot Client not reachable"),
            Self::NotReady => f.write_str("not ready yet"),
            Self::Auth => f.write_str("session not accepted"),
            Self::Region => f.write_str("region not found in the game log"),
            Self::Http(403) => f.write_str("request refused (403)"),
            Self::Http(429) => f.write_str("asked to slow down (429)"),
            Self::Http(code) => write!(f, "server answered {code}"),
            Self::Network => f.write_str("no answer"),
            Self::Shape(what) => write!(f, "unexpected {what} format"),
        }
    }
}

/// The one seam between Sightline and the outside world.
///
/// Implementations: `local::LocalClient` (the real game client) and `fixture::Fixture`
/// (recorded data, for tests and for working on the UI without the game).
pub trait DataSource {
    /// Starts watching the game client. Signals arrive only when something changes.
    fn watch(&mut self) -> mpsc::Receiver<Signal>;

    /// Who is in my lobby or match right now. Hidden players come back without an id.
    async fn roster(&mut self, phase: Phase) -> Result<Roster, SourceError>;

    /// Names for players we are allowed to know.
    async fn names(&mut self, players: &[PlayerId]) -> Result<Vec<Name>, SourceError>;

    /// Display names for agents and maps. No player data involved.
    async fn catalog(&mut self) -> Result<assets::Catalog, SourceError>;

    /// Drop anything held for the current session (tokens, connections).
    fn sleep(&mut self);

    /// One line for the status area: what the source last saw, without identities.
    fn diagnostics(&self) -> String;
}

/// How much of the stats allowance is left in the current window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Budget {
    pub remaining: u32,
    pub limit: u32,
    pub reset_in: Duration,
}

/// Where rank and match history come from. Kept apart from `DataSource` on purpose: this side
/// only ever sees ids of players who are not hidden, because nothing else can be passed to it.
///
/// Implementations: `henrik::Henrik` and `fixture::FixtureStats`.
pub trait StatsSource {
    async fn rank(&mut self, player: PlayerId) -> Result<Rank, SourceError>;

    /// Recent competitive matches, one line per match for this player only.
    async fn history(&mut self, player: PlayerId) -> Result<Vec<Game>, SourceError>;

    /// One page (1, 2, ...) of everything the service has stored of a player's ranked matches,
    /// a hundred at a time. Used for my own history only.
    async fn full_history(&mut self, _player: PlayerId, _page: u32) -> Result<Vec<Game>, SourceError> {
        Ok(Vec::new())
    }

    /// My most recent finished match, with all of its players. `hidden` lists the slots that
    /// were hidden live (side and agent); the players in them come back marked as such.
    async fn last_match(&mut self, me: PlayerId, hidden: &[(Side, Uuid)]) -> Result<Option<FullMatch>, SourceError>;

    fn budget(&self) -> Budget;

    /// Drop connections while nothing is going on.
    fn sleep(&mut self);

    /// How long after a match ends its result is first asked for.
    fn settle(&self) -> Duration {
        Duration::from_secs(45)
    }
}
