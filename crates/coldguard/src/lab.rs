//! Three firmware units, their network and the world in lockstep.

use crate::{
    board::Board,
    hub::Hub,
    proxy::Proxy,
    smoke::{KillOnDrop, free_port, run_broker},
    toolchain::Toolchain,
};
use sim_core::{Action, Fleet, NodeId, Simulation};
use std::path::Path;

type Result<T = ()> = std::result::Result<T, String>;

pub struct Lab {
    // Units stop before the network does.
    pub boards: Vec<Board>,
    pub hub: Hub,
    proxies: Vec<Proxy>,
    _broker: KillOnDrop,
}

/// Run one quantum on the units and pass their outputs to the world.
pub fn exchange(boards: &mut [Board], sim: &mut Simulation) -> Result {
    let world = sim.world();
    for board in &mut *boards {
        board.begin(world.node(board.id))?;
    }
    for board in &mut *boards {
        board.finish(world.node(board.id), world.time_ms)?;
    }
    for board in boards {
        sim.apply(&Action::Relay {
            node: board.id,
            on: board.relay(),
        })?;
    }
    Ok(())
}

impl Lab {
    pub fn start(t: &Toolchain, port: Option<u16>, db: &Path) -> Result<Self> {
        let program = t.which("mosquitto").ok_or("mosquitto not found")?;
        let port = port.map_or_else(free_port, Ok)?;
        let broker = run_broker(t.plain(program), port)?;
        let hub = Hub::start(port, db)?;
        let mut lab = Self {
            boards: vec![],
            hub,
            proxies: vec![],
            _broker: broker,
        };
        for id in NodeId::ALL {
            let proxy = Proxy::start(port)?;
            lab.boards.push(Board::start(t, id, proxy.port)?);
            lab.proxies.push(proxy);
        }
        Ok(lab)
    }

    /// Run one quantum on every unit, then one world step.
    /// Return false while paused or finished.
    pub fn step(&mut self, sim: &mut Simulation) -> Result<bool> {
        if sim.paused || sim.finished() {
            return Ok(false);
        }
        let now = sim.world().time_ms;
        for (board, proxy) in self.boards.iter().zip(&self.proxies) {
            proxy.set(sim.world().node(board.id).network_up);
        }
        exchange(&mut self.boards, sim)?;
        self.hub.tick(now);
        let commands = sim.world_mut().take_commands();
        for command in commands {
            self.hub.command(&command)?;
        }
        // Staff move the box once its departure is confirmed.
        match self.hub.transit() {
            0 => sim.apply(&Action::Carry { away: false })?,
            1 => sim.apply(&Action::Carry { away: true })?,
            _ => {}
        }
        sim.step()
    }

    pub fn fleet(&self) -> Fleet {
        let mut fleet = self.hub.fleet();
        for board in &self.boards {
            fleet.report_mut(board.id).buzzer = board.buzzer();
        }
        fleet
    }
}
