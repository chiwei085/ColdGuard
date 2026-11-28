//! Deterministic physical world. Independent of rendering and firmware.

mod protocol;
mod scenario;
mod simulation;
mod world;

pub use protocol::{
    Batch, Control, Entry, Fleet, Frame, LIVE_PORT, Report, STAFF_BADGE, VIDEO_FPS, VIDEO_SIZE,
    batch_tag,
};
pub use scenario::{Caption, Check, Event, Scenario};
pub use simulation::Simulation;
pub use world::{
    Action, Config, MAX_BATCHES, Node, NodeId, STEP_MS, Sensor, Thermal, ThermalConfig, World,
};
