//! Deterministic physical world. Independent of rendering and firmware.

mod protocol;
mod scenario;
mod simulation;
mod world;

pub use protocol::{Batch, Control, Entry, Fleet, Frame, LIVE_PORT, Report};
pub use scenario::{Event, Scenario};
pub use simulation::Simulation;
pub use world::{Action, Config, Node, NodeId, STEP_MS, Sensor, Thermal, ThermalConfig, World};
