//! The window's side of Prostir's local API (LOCAL-API.md at the repository's root).
//! The state, the requests and the answers are `prostir-api`'s, shared with the
//! service, so the two cannot drift apart; here is only what the window adds: the
//! client trait, what can go wrong talking to the service, a state with its
//! revision, and the changes the service tells about.

pub use prostir_api::{
    Answer, ApiError, BUS_NAME, Battery, CALL_SECONDS, Codec, CodecChoice, Counting, DeskRect,
    Display, DisplayKind, DisplayState, ERROR_PREFIX, Encoding, Headset, INTERFACE, Limits,
    MicState, Mode, OBJECT_PATH, OutputEncoding, PairResult, Pairing, Pc, Phase, Problem, Request,
    Role, STATE_VERSION, Session, Sound, State, ThemeMode, Using, Via, Volume, WindowLook,
};

/// A state and its number: a newer state has a larger revision (counted from 1 each
/// time the service starts).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub revision: u32,
    pub state: Box<State>,
}

impl Snapshot {
    /// The state from `GetState` or `StateChanged`.
    pub fn parse(revision: u32, json: &str) -> Result<Self, ClientError> {
        let state = State::from_json(json).map_err(|e| ClientError::Broken(e.to_string()))?;
        if state.v > STATE_VERSION {
            log::warn!(
                "the service's state is version {}, this window reads {STATE_VERSION}",
                state.v
            );
        }
        Ok(Self {
            revision,
            state: Box::new(state),
        })
    }
}

/// The answer of `Call`; a failure carried in it (a transport without errors of its
/// own) is a refusal.
pub fn parse_answer(json: &str) -> Result<Answer, ClientError> {
    let answer: Answer =
        serde_json::from_str(json).map_err(|e| ClientError::Broken(e.to_string()))?;
    match answer.failure() {
        Some(f) => Err(ClientError::Refused {
            code: f.error,
            message: f.text,
        }),
        None => Ok(answer),
    }
}

/// A request's name on the wire (`add_monitor`): what the window remembers a
/// request by when the service does not do it.
pub fn kind(request: &Request) -> String {
    serde_json::to_value(request)
        .ok()
        .and_then(|v| v.get("request")?.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientError {
    /// No session bus to talk on.
    NoBus(String),
    /// The service is not on the bus.
    NotRunning,
    /// Something owns the name but has not this interface or method: a Prostir older
    /// than the window.
    TooOld,
    /// The service said no: why, and its text in the PC's language.
    Refused { code: ApiError, message: String },
    /// No answer in time.
    TimedOut,
    /// An answer that does not read, or the connection broke.
    Broken(String),
}

impl ClientError {
    /// The service does not do this (yet): a control to disable quietly, not a
    /// failure to report.
    pub fn unsupported(&self) -> bool {
        matches!(
            self,
            ClientError::Refused {
                code: ApiError::Unsupported,
                ..
            }
        )
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::NoBus(e) => write!(f, "no session bus: {e}"),
            ClientError::NotRunning => write!(f, "Prostir is not running"),
            ClientError::TooOld => write!(f, "this Prostir is older than the window"),
            ClientError::Refused { code, message } => write!(f, "{}: {message}", code.name()),
            ClientError::TimedOut => write!(f, "no answer in time"),
            ClientError::Broken(e) => write!(f, "{e}"),
        }
    }
}

/// A change the service tells about without being asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// A new state (`StateChanged`).
    State(Snapshot),
    /// The service appeared on the bus.
    Appeared,
    /// The service left the bus.
    Vanished,
}

/// Where a client puts changes.
pub type Sink = Box<dyn Fn(Change) + Send + Sync>;

/// A way to talk to the service: D-Bus on Linux, a mock in tests and pictures, later a
/// named pipe on Windows. Calls block; the window makes them off its own thread.
/// Calls and changes come from different threads, so a slow call never holds up a
/// change: the client is shared.
pub trait Client: Send + Sync + 'static {
    /// The whole state.
    fn get_state(&self) -> Result<Snapshot, ClientError>;
    /// One request and its answer.
    fn call(&self, request: &Request) -> Result<Answer, ClientError>;
    /// Starts telling `sink` about changes, for as long as the client lives.
    fn watch(&self, sink: Sink) -> Result<(), ClientError>;
    /// Whether the service is there at all (its name has an owner); none: cannot
    /// tell. A service that is there but did not answer is asked again.
    fn present(&self) -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_reads_leniently() {
        let s = Snapshot::parse(
            7,
            r#"{"v":1,"future":{"x":1},"headsets":[{"name":"Quest 3","session":{"via":"bluetooth","worn":true}}],
                "displays":[{"id":3,"kind":"hologram","state":"failed","failure":{"code":"Melted","text":"No."}}],
                "window":{"theme":"high-contrast"},
                "pairing":{"seconds_left":100,"last":{"cancelled":{"by":"headset"}}}}"#,
        )
        .unwrap();
        assert_eq!(s.revision, 7);
        let (h, session) = s.state.connected().unwrap();
        assert_eq!(h.name, "Quest 3");
        assert_eq!(session.via, Via::Other);
        let d = &s.state.displays[0];
        assert_eq!(
            (d.kind, d.state),
            (DisplayKind::Other, DisplayState::Failed)
        );
        assert_eq!(d.failure.as_ref().unwrap().code, ApiError::Other);
        assert_eq!(s.state.window.theme, ThemeMode::Other);
        assert_eq!(s.state.pairing.unwrap().last, Some(PairResult::Other));
        assert!(matches!(
            Snapshot::parse(1, "[1"),
            Err(ClientError::Broken(_))
        ));
    }

    #[test]
    fn answers_and_their_failures() {
        let a = parse_answer(r#"{"id":103,"revision":8}"#).unwrap();
        assert_eq!((a.id, a.revision), (Some(103), 8));
        let refused = parse_answer(r#"{"error":"Unsupported","text":"Not yet","revision":7}"#);
        assert!(refused.as_ref().unwrap_err().unsupported(), "{refused:?}");
        assert!(matches!(parse_answer("{"), Err(ClientError::Broken(_))));
        assert_eq!(
            kind(&Request::AddMonitor {
                w: 1,
                h: 1,
                scale: 100
            }),
            "add_monitor"
        );
        assert_eq!(kind(&Request::PairNewHeadset), "pair_new_headset");
    }

    #[test]
    fn a_full_state_goes_there_and_back() {
        let s = crate::mock::Mock::connected().state();
        assert_eq!(*Snapshot::parse(1, &s.to_json()).unwrap().state, s);
    }
}
