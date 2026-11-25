use crate::{
    scenario::Caption,
    world::{Action, NodeId, World},
};
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
    pub caption: Option<Caption>,
    /// Missing without firmware.
    pub fleet: Option<Fleet>,
}

/// What the firmware reported.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Fleet {
    pub fridge: Report,
    pub carrier: Report,
    pub inventory: Report,
    pub batches: Vec<Batch>,
    /// Newest last.
    pub events: Vec<Entry>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub online: bool,
    /// Time since the last telemetry.
    pub age_ms: u64,
    /// Local buzzer pin. Works without a network.
    pub buzzer: bool,
    pub boot_id: u32,
    pub temp_c: Option<f64>,
    pub battery: f64,
    pub relay: bool,
    pub alarms: Vec<String>,
    /// Estimated safe storage time. None is unknown.
    pub safe_s: Option<u64>,
    /// 0 idle, 1 on the way, 2 arrived.
    pub transit: u64,
    pub receiver: bool,
    /// Badge that may operate now.
    pub person: String,
    /// Records not yet confirmed by the host.
    pub backlog: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    pub name: String,
    /// fridge, carrier, quarantine, receiver or discard.
    pub place: String,
    /// stored, transit, pending or out.
    pub phase: String,
    /// ok, dnu or discarded.
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub time_ms: u64,
    pub node: NodeId,
    pub kind: String,
    pub body: String,
}

/// RFID tag on a batch. Names end in a two digit number.
pub fn batch_tag(name: &str) -> String {
    let digits = name.get(name.len().saturating_sub(2)..);
    format!("B10000{}", digits.unwrap_or("00"))
}

impl Fleet {
    pub fn report(&self, id: NodeId) -> &Report {
        match id {
            NodeId::Fridge => &self.fridge,
            NodeId::Carrier => &self.carrier,
            NodeId::Inventory => &self.inventory,
        }
    }

    pub fn report_mut(&mut self, id: NodeId) -> &mut Report {
        match id {
            NodeId::Fridge => &mut self.fridge,
            NodeId::Carrier => &mut self.carrier,
            NodeId::Inventory => &mut self.inventory,
        }
    }
}
