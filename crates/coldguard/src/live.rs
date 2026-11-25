//! Real-time simulation server for viewers.

use sim_core::{Control, Frame, LIVE_PORT, STEP_MS, Scenario, Simulation};
use std::{
    error::Error,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::mpsc::{Sender, channel},
    thread::{sleep, spawn},
    time::{Duration, Instant},
};

const FRAME: Duration = Duration::from_millis(33);
const MAX_SPEED: f64 = 600.0;
const MAX_LINE: u64 = 4096;

/// Serve until stop returns true.
pub fn serve(scenario: Scenario, mut stop: impl FnMut() -> bool) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, LIVE_PORT))
        .map_err(|e| format!("port {LIVE_PORT}: {e}"))?;
    listener.set_nonblocking(true)?;
    let mut sim = Simulation::new(scenario)?;
    let mut speed = 1.0;
    let mut viewers: Vec<TcpStream> = vec![];
    let (tx, controls) = channel();
    let mut last = Instant::now();
    let mut owed_ms = 0.0;
    while !stop() {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    // A stalled viewer is dropped instead of stopping the world.
                    stream.set_write_timeout(Some(FRAME))?;
                    read_controls(stream.try_clone()?, tx.clone());
                    viewers.push(stream);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        for control in controls.try_iter() {
            match control {
                Control::Act { input } => {
                    if let Err(e) = sim.apply(&input) {
                        eprintln!("Rejected input: {e}");
                    }
                }
                Control::Pause { paused } => sim.paused = paused,
                Control::Speed { factor } if factor > 0.0 => speed = factor.min(MAX_SPEED),
                Control::Speed { .. } => {}
                Control::Reset => sim.reset()?,
            }
        }
        let now = Instant::now();
        // Skip host stalls instead of replaying them.
        let elapsed = (now - last).min(FRAME * 4);
        last = now;
        owed_ms = if sim.paused {
            0.0
        } else {
            owed_ms + elapsed.as_secs_f64() * 1000.0 * speed
        };
        while owed_ms >= STEP_MS as f64 {
            sim.step()?;
            owed_ms -= STEP_MS as f64;
        }
        let mut line = serde_json::to_vec(&Frame {
            paused: sim.paused,
            finished: sim.finished(),
            speed,
            world: sim.world().clone(),
        })?;
        line.push(b'\n');
        viewers.retain_mut(|v| v.write_all(&line).is_ok());
        sleep(FRAME.saturating_sub(now.elapsed()));
    }
    Ok(())
}

fn read_controls(stream: TcpStream, tx: Sender<Control>) {
    spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match (&mut reader).take(MAX_LINE).read_line(&mut line) {
                Ok(1..) if line.ends_with('\n') => {}
                _ => return,
            }
            match serde_json::from_str(&line) {
                Ok(control) => {
                    if tx.send(control).is_err() {
                        return;
                    }
                }
                Err(e) => eprintln!("Rejected control: {e}"),
            }
        }
    });
}
