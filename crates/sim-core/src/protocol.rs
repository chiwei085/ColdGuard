use crate::world::{Action, World};
use serde::{Deserialize, Serialize};

/// Loopback port for viewers.
pub const LIVE_PORT: u16 = 7878;

/// Viewer input, one JSON object per line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "control", rename_all = "snake_case")]
pub enum Control {
    Act {
        input: Action,
    },
    Pause {
        paused: bool,
    },
    /// Simulated seconds per real second.
    Speed {
        factor: f64,
    },
    Reset,
}

/// Viewer output, one JSON object per line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub paused: bool,
    pub finished: bool,
    pub speed: f64,
    pub world: World,
}
