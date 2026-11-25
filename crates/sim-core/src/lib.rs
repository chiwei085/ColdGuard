//! Deterministic physical world. Independent of rendering and firmware.

mod protocol;
mod scenario;
mod simulation;
mod world;

pub use protocol::{Control, Frame, LIVE_PORT};
pub use scenario::{Event, Scenario};
pub use simulation::Simulation;
pub use world::{Action, Config, Node, NodeId, STEP_MS, Sensor, Thermal, ThermalConfig, World};
