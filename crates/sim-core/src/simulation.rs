use crate::{
    scenario::Scenario,
    world::{Action, World},
};

pub struct Simulation {
    scenario: Scenario,
    world: World,
    next_event: usize,
    pub paused: bool,
}

impl Simulation {
    pub fn new(scenario: Scenario) -> Result<Self, String> {
        scenario.validate()?;
        Ok(Self {
            world: World::new(scenario.config.clone())?,
            scenario,
            next_event: 0,
            paused: false,
        })
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn finished(&self) -> bool {
        self.world.time_ms >= self.scenario.duration_ms
    }

    /// Return to the initial state, including scenario events.
    pub fn reset(&mut self) -> Result<(), String> {
        *self = Self::new(self.scenario.clone())?;
        Ok(())
    }

    /// Apply live input at the current time.
    pub fn apply(&mut self, action: &Action) -> Result<(), String> {
        self.world.apply(action)
    }

    /// Apply due events, then advance one step.
    /// Return false while paused or finished.
    pub fn step(&mut self) -> Result<bool, String> {
        if self.paused || self.finished() {
            return Ok(false);
        }
        while let Some(e) = self.scenario.events.get(self.next_event)
            && e.at_ms <= self.world.time_ms
        {
            self.world.apply(&e.action)?;
            self.next_event += 1;
        }
        self.world.step();
        Ok(true)
    }

    /// Run to the end. Report the initial state and every snapshot interval.
    pub fn run<E: From<String>>(
        &mut self,
        mut snapshot: impl FnMut(&World) -> Result<(), E>,
    ) -> Result<(), E> {
        snapshot(&self.world)?;
        while self.step()? {
            if self.world.time_ms.is_multiple_of(self.scenario.snapshot_ms) || self.finished() {
                snapshot(&self.world)?;
            }
        }
        Ok(())
    }
}
