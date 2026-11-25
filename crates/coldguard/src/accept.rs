//! Headless acceptance of the firmware in closed loop.

use crate::{board::Board, lab::Lab, toolchain::Toolchain};
use sim_core::{Action, NodeId, STEP_MS, Scenario, Sensor, Simulation, batch_tag};
use std::{
    thread::sleep,
    time::{Duration, Instant},
};

type Result<T = String> = std::result::Result<T, String>;
type Case = fn(&Toolchain) -> Result;

const CASES: [(&str, Case); 18] = [
    ("S1", pause),
    ("S2", closed_loop),
    ("S3", repeatable),
    ("S4", three_nodes),
    ("B4", messaging),
    ("F1", fridge_alarms),
    ("F2", stock),
    ("F3", power),
    ("F4", transport),
    ("F5", network_loss),
    ("F5R", unit_reset),
    ("F2R", damaged_frame),
    ("F3P", battery_empty),
    ("F5L", log_full),
    ("D2S", |t| replay(t, "story")),
    ("D2E", |t| replay(t, "excursion")),
    ("D2F", |t| replay(t, "failures")),
    ("D2R", |t| replay(t, "refusals")),
];

const FRIDGE: NodeId = NodeId::Fridge;
const CARRIER: NodeId = NodeId::Carrier;
const INVENTORY: NodeId = NodeId::Inventory;
const NURSE: &str = "04A1B2C3";

/// Run the named cases, or all of them.
pub fn run(t: &Toolchain, names: &[String]) -> Result<bool> {
    if let Some(n) = names.iter().find(|n| !CASES.iter().any(|(c, _)| c == n)) {
        return Err(format!("unknown case {n}"));
    }
    let mut passed = true;
    for (name, case) in CASES {
        if !names.is_empty() && !names.iter().any(|n| n == name) {
            continue;
        }
        let begin = Instant::now();
        let (mark, text) = match case(t) {
            Ok(text) => ("PASS", text),
            Err(text) => {
                passed = false;
                ("FAIL", text)
            }
        };
        println!(
            "{mark} {name:4} {:>6.1} s  {text}",
            begin.elapsed().as_secs_f64()
        );
    }
    Ok(passed)
}

fn simulation(initial_c: f64) -> Result<Simulation> {
    let mut scenario = Scenario::parse("duration_ms = 1000000000\nsnapshot_ms = 1000")?;
    scenario.config.fridge.initial_c = initial_c;
    Simulation::new(scenario)
}

/// One offline unit.
struct Solo {
    board: Board,
    sim: Simulation,
}

impl Solo {
    fn start(t: &Toolchain, initial_c: f64) -> Result<Self> {
        Ok(Self {
            board: Board::start(t, FRIDGE, 0)?,
            sim: simulation(initial_c)?,
        })
    }

    fn step(&mut self) -> Result<()> {
        let world = self.sim.world();
        self.board.begin(&world.fridge)?;
        self.board.finish(&world.fridge, world.time_ms)?;
        let on = self.board.relay();
        self.sim.apply(&Action::Relay { node: FRIDGE, on })?;
        self.sim.step().map(|_| ())
    }

    fn run(&mut self, seconds: u64) -> Result<()> {
        (0..seconds * 1000 / STEP_MS).try_for_each(|_| self.step())
    }

    /// Wait for a relay level. Return the world time.
    fn relay(&mut self, on: bool, limit_s: u64) -> Result<f64> {
        for _ in 0..limit_s * 1000 {
            self.step()?;
            if self.board.relay() == on {
                return Ok(self.sim.world().time_ms as f64 / 1000.0);
            }
        }
        Err(format!("relay never became {on}"))
    }
}

fn pause(t: &Toolchain) -> Result {
    let mut s = Solo::start(t, 5.0)?;
    s.run(3)?;
    let before = s.board.stopped_ns;
    sleep(Duration::from_secs(30));
    s.step()?;
    let moved = s.board.stopped_ns - before;
    // The chip may stop within one timer step of the grant.
    if moved.abs_diff(1_000_000) > 1_000 {
        return Err(format!("moved {moved} ns after the pause"));
    }
    Ok(format!(
        "30 s without grants, then {moved} ns for one 1 ms grant"
    ))
}

fn closed_loop(t: &Toolchain) -> Result {
    let mut s = Solo::start(t, 6.0)?;
    let on = s.relay(true, 10)?;
    let off = s.relay(false, 1800)?;
    let cold = s
        .sim
        .world()
        .fridge
        .thermal
        .as_ref()
        .map_or(0.0, |t| t.probe_c);
    let again = s.relay(true, 3600)?;
    let warm = s
        .sim
        .world()
        .fridge
        .thermal
        .as_ref()
        .map_or(0.0, |t| t.probe_c);
    if (cold - 4.5).abs() > 0.1 || (warm - 5.5).abs() > 0.1 {
        return Err(format!("switched at {cold:.2} and {warm:.2} C"));
    }
    Ok(format!(
        "relay on {on:.0} s, off {off:.0} s at {cold:.2} C, on {again:.0} s at {warm:.2} C"
    ))
}

/// Order and content of everything the unit did, with its times.
fn history(board: &Board) -> Result<(Vec<String>, Vec<u64>)> {
    let (mut what, mut when) = (vec![], vec![]);
    for (ms, pin, level) in &board.trace {
        what.push(format!("pin {pin} {level}"));
        when.push(*ms);
    }
    let text = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .trim_end_matches('\0')
            .to_string()
    };
    for slot in board.log()?.chunks(256) {
        if slot.starts_with(&[0x1d, 0xc0]) {
            what.push(format!("{} {}", text(&slot[24..36]), text(&slot[36..252])));
            when.push(
                slot[16..24]
                    .iter()
                    .rev()
                    .fold(0, |n, b| n << 8 | u64::from(*b)),
            );
        }
    }
    Ok((what, when))
}

/// Chip clock phase left over from before the clock was claimed.
const PHASE_MS: u64 = 1;

fn repeatable(t: &Toolchain) -> Result {
    let mut first: Option<(Vec<String>, Vec<u64>)> = None;
    let mut spread = 0;
    for round in 1..=10 {
        let mut s = Solo::start(t, 6.0)?;
        s.run(5)?;
        s.sim.apply(&Action::Door {
            node: FRIDGE,
            open: true,
        })?;
        s.sim.apply(&Action::Scan {
            node: FRIDGE,
            uid: NURSE.into(),
            garbled: false,
        })?;
        s.run(90)?;
        s.sim.apply(&Action::Door {
            node: FRIDGE,
            open: false,
        })?;
        s.sim.apply(&Action::Mains {
            node: FRIDGE,
            on: false,
        })?;
        s.run(60)?;
        let now = history(&s.board)?;
        let Some(first) = &first else {
            first = Some(now);
            continue;
        };
        if first.0 != now.0 {
            return Err(format!("run {round} differs"));
        }
        let gaps = first.1.iter().zip(&now.1).map(|(a, b)| a.abs_diff(*b));
        spread = spread.max(gaps.max().unwrap_or(0));
    }
    let count = first.map_or(0, |f| f.0.len());
    if spread > PHASE_MS || count < 8 {
        return Err(format!("{count} events, times differ by {spread} ms"));
    }
    Ok(format!(
        "10 runs, same {count} events, times within {spread} ms"
    ))
}

/// Three units with a broker.
struct Net {
    lab: Lab,
    sim: Simulation,
    db: std::path::PathBuf,
}

impl Drop for Net {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut path = self.db.clone().into_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Net {
    fn start(t: &Toolchain) -> Result<Self> {
        let mut net = Self::load(t, simulation(5.0)?)?;
        net.until("all units online", 30, |n| {
            NodeId::ALL
                .iter()
                .all(|id| n.lab.fleet().report(*id).online)
        })?;
        Ok(net)
    }

    fn load(t: &Toolchain, sim: Simulation) -> Result<Self> {
        let db = t
            .tools()
            .join(format!("accept-{}.sqlite3", std::process::id()));
        let lab = Lab::start(t, None, &db)?;
        Ok(Self { lab, sim, db })
    }

    fn step(&mut self) -> Result<()> {
        self.lab.step(&mut self.sim).map(|_| ())
    }

    /// Networks run in real time, so limit the speed.
    fn run(&mut self, seconds: u64) -> Result<()> {
        for _ in 0..seconds {
            let begin = Instant::now();
            for _ in 0..1000 {
                self.step()?;
            }
            sleep(Duration::from_millis(80).saturating_sub(begin.elapsed()));
        }
        Ok(())
    }

    fn until(&mut self, what: &str, limit_s: u64, done: impl Fn(&Self) -> bool) -> Result<u64> {
        for s in 0..limit_s {
            if done(self) {
                return Ok(s);
            }
            self.run(1)?;
        }
        if done(self) {
            return Ok(limit_s);
        }
        let recent: Vec<_> = self
            .lab
            .fleet()
            .events
            .iter()
            .rev()
            .take(4)
            .map(|e| e.body.clone())
            .collect();
        Err(format!(
            "{what} not seen within {limit_s} s. Recent: {recent:?}"
        ))
    }

    fn act(&mut self, action: Action) -> Result<()> {
        self.sim.apply(&action)
    }

    fn bias(&mut self, node: NodeId, k: f64) -> Result<()> {
        let state = if k == 0.0 {
            Sensor::Ok
        } else {
            Sensor::Biased(k)
        };
        self.act(Action::Sensor { node, state })
    }

    fn badge(&mut self, node: NodeId) -> Result<()> {
        self.act(Action::Scan {
            node,
            uid: NURSE.into(),
            garbled: false,
        })?;
        self.until("badge", 5, |n| n.lab.fleet().report(node).person == NURSE)
            .map(|_| ())
    }

    /// Send a command with a fresh badge. Return ok or the refusal.
    fn operate(&mut self, node: NodeId, cmd: &str, batch: &str, note: &str) -> Result<String> {
        self.badge(node)?;
        if !batch.is_empty() {
            self.act(Action::Scan {
                node,
                uid: batch_tag(batch),
                garbled: false,
            })?;
        }
        self.command(node, cmd, batch, note)
    }

    fn command(&mut self, node: NodeId, cmd: &str, batch: &str, note: &str) -> Result<String> {
        let before = self.ops().len();
        self.act(Action::Operate {
            node,
            cmd: cmd.into(),
            batch: batch.into(),
            to: "fridge".into(),
            note: note.into(),
        })?;
        self.until(cmd, 10, |n| n.ops().len() > before)?;
        let op = self.ops().pop().unwrap_or_default();
        if op["ok"] == 1 && op["person"] != NURSE {
            return Err(format!("{cmd} without a person: {op}"));
        }
        Ok(if op["ok"] == 1 {
            "ok".into()
        } else {
            op["why"].as_str().unwrap_or_default().into()
        })
    }

    fn expect(
        &mut self,
        node: NodeId,
        cmd: &str,
        batch: &str,
        note: &str,
        want: &str,
    ) -> Result<()> {
        let got = self.operate(node, cmd, batch, note)?;
        if got == want {
            Ok(())
        } else {
            Err(format!("{cmd} {batch}: {got}, expected {want}"))
        }
    }

    fn bodies(&self, node: NodeId, kind: &str) -> Vec<serde_json::Value> {
        let events = self.lab.fleet().events;
        let own = events.iter().filter(|e| e.node == node && e.kind == kind);
        own.filter_map(|e| serde_json::from_str(&e.body).ok())
            .collect()
    }

    fn ops(&self) -> Vec<serde_json::Value> {
        let events = self.lab.fleet().events;
        let ops = events.iter().filter(|e| e.kind == "op");
        ops.filter_map(|e| serde_json::from_str(&e.body).ok())
            .collect()
    }

    fn alarm(&self, node: NodeId, name: &str) -> bool {
        self.lab
            .fleet()
            .report(node)
            .alarms
            .iter()
            .any(|a| a == name)
    }

    fn status(&self, batch: &str) -> (String, String, String) {
        let fleet = self.lab.fleet();
        let found = fleet.batches.iter().find(|b| b.name == batch);
        found.map_or_else(Default::default, |b| {
            (b.place.clone(), b.phase.clone(), b.status.clone())
        })
    }

    fn wait_status(&mut self, batch: &str, place: &str, phase: &str, status: &str) -> Result<()> {
        let want = (place.to_string(), phase.to_string(), status.to_string());
        self.until(&format!("{batch} {want:?}"), 10, |n| {
            n.status(batch) == want
        })
        .map(|_| ())
    }
}

fn three_nodes(t: &Toolchain) -> Result {
    let mut n = Net::start(t)?;
    let (begin, from) = (Instant::now(), n.sim.world().time_ms);
    while begin.elapsed() < Duration::from_secs(600) {
        for _ in 0..1000 {
            n.step()?;
        }
        if !NodeId::ALL
            .iter()
            .all(|id| n.lab.fleet().report(*id).online)
        {
            return Err("a unit went offline".into());
        }
    }
    let ratio = (n.sim.world().time_ms - from) as f64 / 1000.0 / begin.elapsed().as_secs_f64();
    if ratio < 1.0 {
        return Err(format!("speed ratio {ratio:.2}"));
    }
    Ok(format!(
        "10 real minutes without deadlock, speed ratio {ratio:.1}"
    ))
}

fn messaging(t: &Toolchain) -> Result {
    let mut n = Net::start(t)?;
    let counts = |n: &Net| NodeId::ALL.map(|id| n.lab.hub.counts(id));
    let before = counts(&n);
    n.run(60)?;
    for (id, (a, b)) in NodeId::ALL.iter().zip(before.iter().zip(counts(&n))) {
        let (telemetry, beats) = (b.0 - a.0, b.1 - a.1);
        if !(55..=65).contains(&telemetry) || !(11..=13).contains(&beats) {
            return Err(format!(
                "{id:?}: {telemetry} telemetry and {beats} heartbeats in 60 s"
            ));
        }
    }
    n.until("backlog confirmed", 20, |n| {
        NodeId::ALL
            .iter()
            .all(|id| n.lab.fleet().report(*id).backlog == 0)
    })?;
    Ok("telemetry 1 s, heartbeat 5 s, all records confirmed".into())
}

fn fridge_alarms(t: &Toolchain) -> Result {
    let mut n = Net::start(t)?;
    n.run(5)?;
    let quiet = n.lab.fleet().fridge.alarms;
    if !quiet.is_empty() {
        return Err(format!("alarms at rest: {quiet:?}"));
    }
    n.act(Action::Door {
        node: FRIDGE,
        open: true,
    })?;
    let door = n.until("door alarm", 70, |n| n.alarm(FRIDGE, "door_open"))?;
    if door < 58 {
        return Err(format!("door alarm after {door} s"));
    }
    n.act(Action::Door {
        node: FRIDGE,
        open: false,
    })?;
    n.until("door alarm cleared", 5, |n| !n.alarm(FRIDGE, "door_open"))?;

    n.bias(FRIDGE, 6.0)?;
    let high = n.until("high excursion", 5, |n| n.alarm(FRIDGE, "excursion"))?;
    n.bias(FRIDGE, -6.5)?;
    n.until("freeze alarm", 5, |n| n.alarm(FRIDGE, "freeze"))?;
    n.until("cooling stopped", 3, |n| !n.lab.boards[0].relay())?;
    n.bias(FRIDGE, 0.0)?;
    n.until("excursion closed", 5, |n| !n.alarm(FRIDGE, "excursion"))?;

    n.act(Action::Sensor {
        node: FRIDGE,
        state: Sensor::Disconnected,
    })?;
    n.until("sensor alarm", 8, |n| n.alarm(FRIDGE, "sensor"))?;
    n.bias(FRIDGE, 1.0)?;
    n.act(Action::Cooling {
        node: FRIDGE,
        efficiency: 0.0,
    })?;
    n.until("cooling requested", 600, |n| n.lab.boards[0].relay())?;
    let cooling = n.until("cooling alarm", 320, |n| n.alarm(FRIDGE, "cooling"))?;
    if cooling < 280 {
        return Err(format!("cooling alarm after {cooling} s"));
    }
    Ok(format!(
        "door {door} s, excursion {high} s, freeze, sensor, cooling failure {cooling} s"
    ))
}

fn stock(t: &Toolchain) -> Result {
    let mut n = Net::start(t)?;
    n.run(3)?;
    let refused = n.command(INVENTORY, "checkin", "VAX-01", "")?;
    if refused != "badge required" {
        return Err(format!("checkin without badge: {refused}"));
    }
    for batch in ["VAX-01", "VAX-02"] {
        n.expect(INVENTORY, "checkin", batch, "", "ok")?;
    }
    n.bias(FRIDGE, 4.0)?;
    let lock = n.until("batches locked", 5, |n| n.status("VAX-01").2 == "dnu")?;
    n.bias(FRIDGE, 0.0)?;
    n.run(10)?;
    n.wait_status("VAX-02", "fridge", "stored", "dnu")?;
    n.expect(INVENTORY, "checkout", "VAX-01", "", "do not use")?;
    n.expect(INVENTORY, "release", "VAX-01", "", "basis required")?;
    n.expect(INVENTORY, "release", "VAX-01", "maker letter 12", "ok")?;

    let boot = n.lab.fleet().inventory.boot_id;
    n.lab.boards[2].reset()?;
    n.until("inventory rebooted", 20, |n| {
        n.lab.fleet().inventory.boot_id > boot
    })?;
    n.run(8)?;
    n.wait_status("VAX-02", "fridge", "stored", "dnu")?;
    n.expect(INVENTORY, "checkout", "VAX-01", "", "ok")?;
    n.expect(INVENTORY, "checkout", "VAX-02", "", "do not use")?;
    n.act(Action::Sensor {
        node: FRIDGE,
        state: Sensor::Disconnected,
    })?;
    n.run(5)?;
    n.expect(INVENTORY, "checkin", "VAX-01", "", "sensor fault")?;
    Ok(format!(
        "locked in {lock} s, kept after recovery and reset, released with basis"
    ))
}

fn power(t: &Toolchain) -> Result {
    let mut n = Net::start(t)?;
    n.run(5)?;
    n.act(Action::Mains {
        node: FRIDGE,
        on: false,
    })?;
    n.until("mains alarm", 5, |n| n.alarm(FRIDGE, "mains_lost"))?;
    n.until("transport advised while unknown", 5, |n| {
        n.alarm(FRIDGE, "transport")
    })?;
    n.until("safe time known", 400, |n| {
        n.lab.fleet().fridge.safe_s.is_some_and(|s| s > 2100)
    })?;
    n.until("advice withdrawn", 5, |n| !n.alarm(FRIDGE, "transport"))?;
    n.until("transport advised", 3600, |n| n.alarm(FRIDGE, "transport"))?;
    let safe = n.lab.fleet().fridge.safe_s.unwrap_or(0);
    let cooling = n
        .sim
        .world()
        .fridge
        .thermal
        .as_ref()
        .map_or(1.0, |t| t.cooling_w);
    if cooling != 0.0 || !(1980..=2100).contains(&safe) {
        return Err(format!("cooling {cooling} W, safe time {safe} s"));
    }
    n.act(Action::Battery {
        node: FRIDGE,
        level: 0.2,
    })?;
    n.until("battery warning", 5, |n| n.alarm(FRIDGE, "battery_low"))?;
    n.act(Action::Battery {
        node: FRIDGE,
        level: 0.1,
    })?;
    n.until("battery alarm", 5, |n| n.alarm(FRIDGE, "battery_critical"))?;
    Ok(format!(
        "cooling stopped, transport advised at {safe} s left"
    ))
}

fn transport(t: &Toolchain) -> Result {
    let mut n = Net::start(t)?;
    n.run(3)?;
    n.expect(INVENTORY, "checkin", "VAX-01", "", "ok")?;
    n.expect(INVENTORY, "load", "VAX-01", "", "ok")?;
    n.expect(CARRIER, "depart", "", "", "receiver not confirmed")?;
    n.expect(CARRIER, "receiver", "", "clinic B ready", "ok")?;
    n.bias(CARRIER, -4.0)?;
    n.run(2)?;
    n.expect(CARRIER, "depart", "", "", "temperature not stable")?;
    n.bias(CARRIER, 0.0)?;
    n.run(310)?;
    n.act(Action::Battery {
        node: CARRIER,
        level: 0.15,
    })?;
    n.run(2)?;
    n.expect(CARRIER, "depart", "", "", "battery")?;
    n.act(Action::Battery {
        node: CARRIER,
        level: 0.9,
    })?;
    n.run(2)?;
    // The cold reading locked the loaded batch.
    n.wait_status("VAX-01", "carrier", "stored", "dnu")?;
    n.expect(INVENTORY, "release", "VAX-01", "sensor test", "ok")?;
    n.expect(CARRIER, "depart", "", "", "ok")?;
    n.wait_status("VAX-01", "carrier", "transit", "ok")?;
    n.expect(INVENTORY, "checkout", "VAX-01", "", "transit")?;
    n.run(30)?;
    n.expect(CARRIER, "arrive", "", "", "ok")?;
    n.wait_status("VAX-01", "carrier", "pending", "ok")?;
    n.expect(INVENTORY, "checkout", "VAX-01", "", "pending")?;
    n.expect(INVENTORY, "accept", "VAX-01", "log checked", "ok")?;
    n.expect(INVENTORY, "checkout", "VAX-01", "", "ok")?;
    for cmd in ["depart", "arrive"] {
        let complete = n.bodies(CARRIER, "op").iter().any(|o| {
            o["cmd"] == cmd
                && o["ok"] == 1
                && o["person"] == NURSE
                && o["temp_c"].is_f64()
                && o["box"].is_string()
        });
        if !complete {
            return Err(format!("{cmd} has no custody record"));
        }
    }
    Ok("refusals, departure, arrival and acceptance recorded".into())
}

fn network_loss(t: &Toolchain) -> Result {
    let mut n = Net::start(t)?;
    n.run(3)?;
    n.expect(INVENTORY, "checkin", "VAX-01", "", "ok")?;
    n.act(Action::Network {
        node: FRIDGE,
        up: false,
    })?;
    let lost = n.until("offline event", 30, |n| {
        n.bodies(INVENTORY, "peer")
            .last()
            .is_some_and(|p| p["peer"] == "fridge" && p["online"] == 0)
    })?;
    if !(10..=22).contains(&lost) {
        return Err(format!("offline after {lost} s"));
    }
    n.expect(INVENTORY, "checkout", "VAX-01", "", "storage offline")?;
    // Local control keeps recording.
    n.bias(FRIDGE, 4.0)?;
    n.run(5)?;
    n.bias(FRIDGE, 0.0)?;
    n.run(5)?;
    if !n.bodies(FRIDGE, "excursion").is_empty() {
        return Err("event crossed a cut network".into());
    }
    n.act(Action::Network {
        node: FRIDGE,
        up: true,
    })?;
    let back = n.until("missed events", 30, |n| {
        n.bodies(FRIDGE, "excursion").len() == 2
    })?;
    n.until("backlog confirmed", 20, |n| {
        n.lab.fleet().fridge.backlog == 0
    })?;
    n.wait_status("VAX-01", "fridge", "stored", "dnu")?;
    // Losing the recorder itself only delays confirmation.
    n.act(Action::Network {
        node: INVENTORY,
        up: false,
    })?;
    n.bias(FRIDGE, 4.0)?;
    n.run(5)?;
    n.act(Action::Network {
        node: INVENTORY,
        up: true,
    })?;
    n.until("inventory back", 30, |n| {
        n.lab.fleet().inventory.backlog == 0 && n.lab.fleet().inventory.online
    })?;
    Ok(format!(
        "offline after {lost} s, records complete {back} s after recovery"
    ))
}

fn unit_reset(t: &Toolchain) -> Result {
    let mut n = Net::start(t)?;
    n.act(Action::Network {
        node: FRIDGE,
        up: false,
    })?;
    n.run(2)?;
    n.bias(FRIDGE, 4.0)?;
    n.run(4)?;
    let boot = n.lab.fleet().fridge.boot_id;
    n.lab.boards[0].reset()?;
    n.run(5)?;
    if n.sim.world().fridge.relay_on != n.lab.boards[0].relay() {
        return Err("relay state lost".into());
    }
    n.act(Action::Network {
        node: FRIDGE,
        up: true,
    })?;
    n.until("reboot seen", 30, |n| {
        n.lab.fleet().fridge.boot_id == boot + 1
    })?;
    n.until("records from before the reset", 20, |n| {
        !n.bodies(FRIDGE, "excursion").is_empty()
    })?;
    n.until("backlog confirmed", 20, |n| {
        n.lab.fleet().fridge.backlog == 0
    })?;
    Ok("unconfirmed records survive a reset".into())
}

fn damaged_frame(t: &Toolchain) -> Result {
    let mut n = Net::start(t)?;
    n.act(Action::Scan {
        node: INVENTORY,
        uid: NURSE.into(),
        garbled: true,
    })?;
    n.until("damaged frame recorded", 10, |n| {
        n.bodies(INVENTORY, "scan")
            .iter()
            .any(|b| b["error"] == "bad frame")
    })?;
    if !n.lab.fleet().inventory.person.is_empty() {
        return Err("damaged frame accepted".into());
    }
    n.badge(INVENTORY)?;
    Ok("damaged frame dropped and recorded, next frame read".into())
}

fn battery_empty(t: &Toolchain) -> Result {
    let mut n = Net::start(t)?;
    let boot = n.lab.fleet().fridge.boot_id;
    n.act(Action::Mains {
        node: FRIDGE,
        on: false,
    })?;
    n.act(Action::Battery {
        node: FRIDGE,
        level: 0.0,
    })?;
    n.until("unit off", 30, |n| !n.lab.fleet().fridge.online)?;
    if n.sim.world().fridge.powered || n.lab.boards[0].relay() {
        return Err("unit still powered".into());
    }
    n.act(Action::Mains {
        node: FRIDGE,
        on: true,
    })?;
    n.until("unit restarted", 30, |n| {
        let r = n.lab.fleet().fridge;
        r.online && r.boot_id == boot + 1
    })?;
    Ok("off without power, restarted with mains".into())
}

/// The host stops storing records until the node log is full.
fn log_full(t: &Toolchain) -> Result {
    use std::sync::atomic::Ordering;
    let mut n = Net::start(t)?;
    n.run(3)?;
    n.expect(INVENTORY, "checkin", "VAX-01", "", "ok")?;
    n.lab.hub.reject.store(true, Ordering::Relaxed);
    for _ in 0..4600 {
        n.act(Action::Scan {
            node: INVENTORY,
            uid: NURSE.into(),
            garbled: false,
        })?;
        for _ in 0..60 {
            n.step()?;
        }
        if n.alarm(INVENTORY, "log_full") {
            break;
        }
    }
    n.until("log alarm", 5, |n| n.alarm(INVENTORY, "log_full"))?;
    let held = n.lab.fleet().inventory.backlog;
    if held < 4096 {
        return Err(format!("full at {held} records"));
    }
    n.badge(INVENTORY)?;
    n.act(Action::Scan {
        node: INVENTORY,
        uid: batch_tag("VAX-01"),
        garbled: false,
    })?;
    n.act(Action::Operate {
        node: INVENTORY,
        cmd: "checkout".into(),
        batch: "VAX-01".into(),
        to: String::new(),
        note: String::new(),
    })?;
    n.run(5)?;
    n.wait_status("VAX-01", "fridge", "stored", "ok")?;
    n.lab.hub.reject.store(false, Ordering::Relaxed);
    n.until("backlog confirmed", 600, |n| {
        n.lab.fleet().inventory.backlog == 0
    })?;
    n.until("log alarm cleared", 5, |n| !n.alarm(INVENTORY, "log_full"))?;
    n.expect(INVENTORY, "checkout", "VAX-01", "", "ok")?;
    Ok(format!(
        "refused with {held} unconfirmed records, recovered after confirmation"
    ))
}

fn replay(t: &Toolchain, name: &str) -> Result {
    let path = t.root.join(format!("scenarios/{name}.toml"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{name}: {e}"))?;
    let mut n = Net::load(t, Simulation::new(Scenario::parse(&text)?)?)?;
    let mut open = n.sim.scenario().checks.clone();
    let count = open.len();
    while !n.sim.finished() {
        let begin = Instant::now();
        for _ in 0..100 {
            n.step()?;
        }
        let (now, fleet) = (n.sim.world().time_ms, n.lab.fleet());
        open.retain(|c| now < c.at_ms || !c.holds(&fleet));
        if let Some(c) = open.iter().find(|c| c.deadline() <= now) {
            let recent: Vec<_> = fleet.events.iter().rev().take(4).collect();
            return Err(format!("{c:?} failed. Recent: {recent:?}"));
        }
        sleep(Duration::from_millis(10).saturating_sub(begin.elapsed()));
    }
    Ok(format!("{count} checks hold"))
}
