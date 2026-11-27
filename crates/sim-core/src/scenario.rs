use crate::{
    protocol::Fleet,
    world::{Action, Config, NodeId, World},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub duration_ms: u64,
    pub snapshot_ms: u64,
    #[serde(default)]
    pub config: Config,
    #[serde(default, rename = "event")]
    pub events: Vec<Event>,
    #[serde(default, rename = "caption")]
    pub captions: Vec<Caption>,
    #[serde(default, rename = "check")]
    pub checks: Vec<Check>,
}

/// Narration of a replay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Caption {
    pub at_ms: u64,
    pub text: String,
    /// Camera view.
    #[serde(default)]
    pub view: String,
    /// Act shown in the corner. Carries over to later captions.
    #[serde(default)]
    pub chapter: String,
    /// Playback speed in a recorded video until the next caption.
    #[serde(default = "normal_speed")]
    pub video_speed: f64,
    /// Leave this caption's span out of a recorded video.
    #[serde(default)]
    pub video_cut: bool,
}

fn normal_speed() -> f64 {
    1.0
}

/// What the firmware must report within the time limit. Words are separated
/// by spaces. Every given field must hold at once.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Check {
    pub at_ms: u64,
    pub within_ms: Option<u64>,
    /// Node and alarm.
    pub alarm: Option<String>,
    pub no_alarm: Option<String>,
    /// Node.
    pub online: Option<NodeId>,
    pub offline: Option<NodeId>,
    /// Name, place, phase and status.
    pub batch: Option<String>,
    /// Command and batch, if any, of the latest operation.
    pub done: Option<String>,
    /// Likewise, followed by the reason.
    pub refused: Option<String>,
    pub transit: Option<u64>,
    pub relay: Option<bool>,
}

const WITHIN_MS: u64 = 5000;

impl Check {
    pub fn deadline(&self) -> u64 {
        self.at_ms + self.within_ms.unwrap_or(WITHIN_MS)
    }

    pub fn holds(&self, f: &Fleet) -> bool {
        let alarm = |text: &String| {
            NodeId::ALL.iter().any(|id| {
                let alarms = &f.report(*id).alarms;
                alarms.iter().any(|a| *text == format!("{} {a}", id.name()))
            })
        };
        let last = || -> Option<serde_json::Value> {
            let e = f.events.iter().rev().find(|e| e.kind == "op")?;
            serde_json::from_str(&e.body).ok()
        };
        let op = |ok: u64, text: &String| {
            last().is_some_and(|o| {
                let parts = ["cmd", "batch", "why"].map(|k| o[k].as_str().unwrap_or_default());
                let words: Vec<_> = parts.into_iter().filter(|w| !w.is_empty()).collect();
                o["ok"] == ok && words.join(" ") == *text
            })
        };
        self.alarm.as_ref().is_none_or(alarm)
            && self.no_alarm.as_ref().is_none_or(|a| !alarm(a))
            && self.online.is_none_or(|id| f.report(id).online)
            && self.offline.is_none_or(|id| !f.report(id).online)
            && self.batch.as_ref().is_none_or(|want| {
                f.batches
                    .iter()
                    .any(|b| *want == format!("{} {} {} {}", b.name, b.place, b.phase, b.status))
            })
            && self.done.as_ref().is_none_or(|t| op(1, t))
            && self.refused.as_ref().is_none_or(|t| op(0, t))
            && self.transit.is_none_or(|t| f.carrier.transit == t)
            && self.relay.is_none_or(|r| f.fridge.relay == r)
    }
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
    /// The caption showing at a simulated time.
    pub fn caption_at(&self, time_ms: f64) -> Option<&Caption> {
        self.captions.iter().rfind(|c| c.at_ms as f64 <= time_ms)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut s: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        s.validate()?;
        let mut chapter = String::new();
        for c in &mut s.captions {
            if c.video_speed.is_nan() || c.video_speed < 1.0 {
                return Err(format!(
                    "caption at {} ms: video_speed must be at least 1",
                    c.at_ms
                ));
            }
            if c.chapter.is_empty() {
                c.chapter.clone_from(&chapter);
            } else {
                chapter.clone_from(&c.chapter);
            }
        }
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
        if !self.captions.is_sorted_by_key(|c| c.at_ms) {
            return Err("captions must be in time order".into());
        }
        if let Some(c) = self
            .checks
            .iter()
            .find(|c| c.deadline() >= self.duration_ms)
        {
            return Err(format!("check at {} ms ends after the scenario", c.at_ms));
        }
        Ok(())
    }
}
