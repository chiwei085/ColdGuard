use serde::{Deserialize, Serialize};

/// Matches the 1 ms coordinator quantum.
pub const STEP_MS: u64 = 1;
const STEP_S: f64 = STEP_MS as f64 / 1000.0;
/// Commands pile up only without firmware.
const MAX_OUTBOX: usize = 64;
/// From the tap of a tag to the press of a button. The tag frame arrives first.
const HANDLING_MS: u64 = 500;
pub const MAX_BATCHES: u8 = 6;
const STEP_H: f64 = STEP_S / 3600.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeId {
    Fridge,
    Carrier,
    Inventory,
}

impl NodeId {
    pub const ALL: [Self; 3] = [Self::Fridge, Self::Carrier, Self::Inventory];

    pub fn name(self) -> &'static str {
        match self {
            Self::Fridge => "fridge",
            Self::Carrier => "carrier",
            Self::Inventory => "inventory",
        }
    }
}

/// Engineering defaults. Calibrate here, not in the equations.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ambient_c: f64,
    pub fridge: ThermalConfig,
    pub carrier: ThermalConfig,
    pub battery_capacity_wh: f64,
    pub battery_level: f64,
    pub battery_load_w: f64,
    pub charge_w: f64,
    /// Batches delivered to the clinic.
    pub batches: u8,
    /// Carrying time to the receiver.
    pub travel_s: f64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThermalConfig {
    pub initial_c: f64,
    pub heat_capacity_j_per_k: f64,
    pub closed_w_per_k: f64,
    pub open_w_per_k: f64,
    /// Zero means passive cooling only.
    pub cooling_w: f64,
    /// Glycol probe time constant.
    pub probe_tau_s: f64,
}

impl Default for ThermalConfig {
    fn default() -> Self {
        Config::default().fridge
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ambient_c: 25.0,
            fridge: ThermalConfig {
                initial_c: 5.0,
                heat_capacity_j_per_k: 40_000.0,
                closed_w_per_k: 1.5,
                open_w_per_k: 15.0,
                cooling_w: 150.0,
                probe_tau_s: 60.0,
            },
            carrier: ThermalConfig {
                initial_c: 5.0,
                heat_capacity_j_per_k: 10_000.0,
                closed_w_per_k: 0.1,
                open_w_per_k: 2.0,
                cooling_w: 0.0,
                probe_tau_s: 60.0,
            },
            battery_capacity_wh: 10.0,
            battery_level: 1.0,
            battery_load_w: 1.0,
            charge_w: 5.0,
            batches: 3,
            travel_s: 60.0,
        }
    }
}

impl Config {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let positive = |name: &str, v: f64| {
            (v.is_finite() && v > 0.0)
                .then_some(())
                .ok_or(format!("{name} must be positive"))
        };
        let not_negative = |name: &str, v: f64| {
            (v.is_finite() && v >= 0.0)
                .then_some(())
                .ok_or(format!("{name} must not be negative"))
        };
        for t in [&self.fridge, &self.carrier] {
            temperature(t.initial_c)?;
            positive("heat_capacity_j_per_k", t.heat_capacity_j_per_k)?;
            positive("probe_tau_s", t.probe_tau_s)?;
            not_negative("closed_w_per_k", t.closed_w_per_k)?;
            not_negative("open_w_per_k", t.open_w_per_k)?;
            not_negative("cooling_w", t.cooling_w)?;
            // Explicit integration diverges with large steps.
            let leak = t.closed_w_per_k.max(t.open_w_per_k);
            if STEP_S > t.probe_tau_s || STEP_S * leak > t.heat_capacity_j_per_k {
                return Err("thermal time constants must exceed the step".into());
            }
        }
        temperature(self.ambient_c)?;
        positive("battery_capacity_wh", self.battery_capacity_wh)?;
        not_negative("battery_load_w", self.battery_load_w)?;
        not_negative("charge_w", self.charge_w)?;
        positive("travel_s", self.travel_s)?;
        if self.batches > MAX_BATCHES {
            return Err(format!("batches must not exceed {MAX_BATCHES}"));
        }
        ratio("battery_level", self.battery_level)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sensor {
    Ok,
    /// The virtual device answers NACK.
    Disconnected,
    /// Constant reading error in kelvin.
    Biased(f64),
}

/// Physical inputs and faults. None can touch firmware state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Relay {
        node: NodeId,
        on: bool,
    },
    Door {
        node: NodeId,
        open: bool,
    },
    Mains {
        node: NodeId,
        on: bool,
    },
    Ambient {
        celsius: f64,
    },
    /// Fraction of the rated cooling power.
    Cooling {
        node: NodeId,
        efficiency: f64,
    },
    Sensor {
        node: NodeId,
        state: Sensor,
    },
    Battery {
        node: NodeId,
        level: f64,
    },
    Network {
        node: NodeId,
        up: bool,
    },
    Scan {
        node: NodeId,
        uid: String,
        /// Corrupts the frame on the wire.
        #[serde(default)]
        garbled: bool,
    },
    /// Staff carry the box to the receiver or back.
    Carry {
        away: bool,
    },
    /// Operator command for the firmware. No physical effect.
    Operate {
        node: NodeId,
        cmd: String,
        #[serde(default)]
        batch: String,
        #[serde(default)]
        to: String,
        #[serde(default)]
        note: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Thermal {
    pub air_c: f64,
    pub probe_c: f64,
    pub door_open: bool,
    pub cooling_efficiency: f64,
    /// Actual heat removal.
    pub cooling_w: f64,
    pub sensor: Sensor,
    #[serde(skip)]
    config: ThermalConfig,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub thermal: Option<Thermal>,
    pub relay_on: bool,
    pub mains: bool,
    /// State of charge from 0 to 1.
    pub battery_level: f64,
    pub powered: bool,
    pub network_up: bool,
    /// Last RFID tag presented.
    pub last_scan: Option<String>,
    /// Distinguishes repeated scans of one tag.
    pub scan_count: u64,
    pub scan_garbled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct World {
    pub time_ms: u64,
    pub ambient_c: f64,
    pub fridge: Node,
    pub carrier: Node,
    pub inventory: Node,
    /// Batches delivered to the clinic.
    pub delivered: u8,
    /// Way of the box to the receiver, from 0 to 1.
    pub travel: f64,
    pub away: bool,
    /// Operator commands and when they are entered.
    #[serde(skip)]
    outbox: Vec<(u64, Action)>,
    #[serde(skip)]
    config: Config,
}

impl World {
    pub fn new(config: Config) -> Result<Self, String> {
        config.validate()?;
        let node = |thermal: Option<&ThermalConfig>, mains| Node {
            thermal: thermal.map(|c| Thermal {
                air_c: c.initial_c,
                probe_c: c.initial_c,
                door_open: false,
                cooling_efficiency: 1.0,
                cooling_w: 0.0,
                sensor: Sensor::Ok,
                config: c.clone(),
            }),
            relay_on: false,
            mains,
            battery_level: config.battery_level,
            powered: mains || config.battery_level > 0.0,
            network_up: true,
            last_scan: None,
            scan_count: 0,
            scan_garbled: false,
        };
        Ok(Self {
            time_ms: 0,
            ambient_c: config.ambient_c,
            fridge: node(Some(&config.fridge), true),
            carrier: node(Some(&config.carrier), false),
            inventory: node(None, true),
            delivered: config.batches,
            travel: 0.0,
            away: false,
            outbox: vec![],
            config,
        })
    }

    pub fn node(&self, id: NodeId) -> &Node {
        match id {
            NodeId::Fridge => &self.fridge,
            NodeId::Carrier => &self.carrier,
            NodeId::Inventory => &self.inventory,
        }
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        match id {
            NodeId::Fridge => &mut self.fridge,
            NodeId::Carrier => &mut self.carrier,
            NodeId::Inventory => &mut self.inventory,
        }
    }

    /// Reject invalid input without changing state.
    pub fn apply(&mut self, action: &Action) -> Result<(), String> {
        match action {
            Action::Relay { node, on } => self.node_mut(*node).relay_on = *on,
            Action::Door { node, open } => self.thermal_mut(*node)?.door_open = *open,
            Action::Mains { node, on } => self.node_mut(*node).mains = *on,
            Action::Ambient { celsius } => {
                temperature(*celsius)?;
                self.ambient_c = *celsius;
            }
            Action::Cooling { node, efficiency } => {
                ratio("efficiency", *efficiency)?;
                self.thermal_mut(*node)?.cooling_efficiency = *efficiency;
            }
            Action::Sensor { node, state } => {
                if let Sensor::Biased(k) = state {
                    temperature(*k)?;
                }
                self.thermal_mut(*node)?.sensor = *state;
            }
            Action::Battery { node, level } => {
                ratio("level", *level)?;
                self.node_mut(*node).battery_level = *level;
            }
            Action::Network { node, up } => self.node_mut(*node).network_up = *up,
            Action::Scan { node, uid, garbled } => {
                if uid.len() != 8 || !uid.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("uid must be 8 hex digits".into());
                }
                let node = self.node_mut(*node);
                node.last_scan = Some(uid.to_ascii_uppercase());
                node.scan_count += 1;
                node.scan_garbled = *garbled;
            }
            Action::Carry { away } => self.away = *away,
            Action::Operate { .. } => {
                if self.outbox.len() >= MAX_OUTBOX {
                    return Err("too many pending commands".into());
                }
                self.outbox
                    .push((self.time_ms + HANDLING_MS, action.clone()));
            }
        }
        Ok(())
    }

    /// Remove the commands that are entered by now.
    pub fn take_commands(&mut self) -> Vec<Action> {
        let due = self.outbox.partition_point(|(t, _)| *t <= self.time_ms);
        self.outbox.drain(..due).map(|(_, a)| a).collect()
    }

    /// Advance by one fixed step.
    pub fn step(&mut self) {
        let (ambient, config) = (self.ambient_c, &self.config);
        for node in [&mut self.fridge, &mut self.carrier, &mut self.inventory] {
            node.step(ambient, config);
        }
        let way = STEP_S / config.travel_s * if self.away { 1.0 } else { -1.0 };
        self.travel = (self.travel + way).clamp(0.0, 1.0);
        self.time_ms += STEP_MS;
    }

    fn thermal_mut(&mut self, id: NodeId) -> Result<&mut Thermal, String> {
        self.node_mut(id)
            .thermal
            .as_mut()
            .ok_or(format!("{id:?} has no storage compartment"))
    }
}

impl Node {
    fn step(&mut self, ambient: f64, config: &Config) {
        let battery_w = if self.mains {
            config.charge_w
        } else {
            -config.battery_load_w
        };
        let delta = battery_w * STEP_H / config.battery_capacity_wh;
        self.battery_level = (self.battery_level + delta).clamp(0.0, 1.0);
        self.powered = self.mains || self.battery_level > 0.0;
        if let Some(t) = &mut self.thermal {
            t.step(ambient, self.relay_on && self.mains);
        }
    }
}

impl Thermal {
    /// Sensor reading, or none when disconnected.
    pub fn reading_c(&self) -> Option<f64> {
        match self.sensor {
            Sensor::Ok => Some(self.probe_c),
            Sensor::Disconnected => None,
            Sensor::Biased(k) => Some(self.probe_c + k),
        }
    }

    fn step(&mut self, ambient: f64, cooling: bool) {
        let c = &self.config;
        let leak = if self.door_open {
            c.open_w_per_k
        } else {
            c.closed_w_per_k
        };
        self.cooling_w = if cooling {
            c.cooling_w * self.cooling_efficiency
        } else {
            0.0
        };
        let heat_w = leak * (ambient - self.air_c) - self.cooling_w;
        self.air_c += heat_w * STEP_S / c.heat_capacity_j_per_k;
        self.probe_c += (self.air_c - self.probe_c) * STEP_S / c.probe_tau_s;
    }
}

fn temperature(v: f64) -> Result<(), String> {
    (-100.0..=100.0)
        .contains(&v)
        .then_some(())
        .ok_or(format!("temperature {v} is out of range"))
}

fn ratio(name: &str, v: f64) -> Result<(), String> {
    (0.0..=1.0)
        .contains(&v)
        .then_some(())
        .ok_or(format!("{name} must be between 0 and 1"))
}
