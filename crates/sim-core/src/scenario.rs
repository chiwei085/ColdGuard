use crate::world::{Action, Config, World};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub duration_ms: u64,
    pub snapshot_ms: u64,
    #[serde(default)]
    pub config: Config,
    #[serde(default, rename = "event")]
    pub events: Vec<Event>,
}

// Serde cannot combine flatten with deny_unknown_fields.
// Action itself rejects unknown fields.
#[derive(Clone, Debug, Deserialize)]
pub struct Event {
    pub at_ms: u64,
    #[serde(flatten)]
    pub action: Action,
}

impl Scenario {
    pub fn parse(text: &str) -> Result<Self, String> {
        let s: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        s.validate()?;
        Ok(s)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.snapshot_ms == 0 {
            return Err("snapshot_ms must be positive".into());
        }
        let mut scratch = World::new(self.config.clone())?;
        let mut last = 0;
        for (i, e) in self.events.iter().enumerate() {
            let fail = |m: &str| Err(format!("event {}: {m}", i + 1));
            if e.at_ms < last {
                return fail("events must be in time order");
            }
            if e.at_ms >= self.duration_ms {
                return fail("at_ms must be before duration_ms");
            }
            if let Err(m) = scratch.apply(&e.action) {
                return fail(&m);
            }
            last = e.at_ms;
        }
        Ok(())
    }
}
