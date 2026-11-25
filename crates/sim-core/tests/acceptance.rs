//! B1 and B2 acceptance.

use sim_core::{Action, NodeId, Scenario, Sensor, Simulation, Thermal, World};

fn load(name: &str) -> Scenario {
    let path = format!("{}/../../scenarios/{name}.toml", env!("CARGO_MANIFEST_DIR"));
    Scenario::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn record(sim: &mut Simulation) -> Vec<World> {
    let mut out = vec![];
    sim.run::<String>(|w| {
        out.push(w.clone());
        Ok(())
    })
    .unwrap();
    out
}

fn play(name: &str) -> Vec<World> {
    record(&mut Simulation::new(load(name)).unwrap())
}

fn fridge(w: &World) -> &Thermal {
    w.fridge.thermal.as_ref().unwrap()
}

fn at(run: &[World], ms: u64) -> &World {
    run.iter().find(|w| w.time_ms == ms).unwrap()
}

#[test]
fn b1_cooling() {
    let run = play("cooling");
    let (start, off, end) = (&run[0], at(&run, 600_000), run.last().unwrap());
    assert!(fridge(off).air_c < fridge(start).air_c - 1.0);
    // The probe lags behind the air.
    assert!(fridge(off).probe_c > fridge(off).air_c);
    assert!(fridge(end).air_c > fridge(off).air_c);
    // Matches the analytic solution of the closed fridge.
    let (gain, tau) = (25.0 - 150.0 / 1.5, 40_000.0 / 1.5);
    let want = gain + (5.0 - gain) * f64::exp(-600.0 / tau);
    assert!((fridge(off).air_c - want).abs() < 0.001);
}

#[test]
fn b1_door() {
    let run = play("door");
    let air = |ms| fridge(at(&run, ms)).air_c;
    let closed = air(60_000) - air(0);
    let open = air(120_000) - air(60_000);
    let after = air(480_000) - air(420_000);
    assert!(open > 5.0 * closed && open > 5.0 * after);
}

#[test]
fn b1_outage() {
    let run = play("outage");
    let (cut, end) = (at(&run, 120_000), run.last().unwrap());
    assert!(fridge(cut).cooling_w > 0.0);
    assert!(end.fridge.relay_on && fridge(end).cooling_w == 0.0);
    assert!(fridge(end).air_c > fridge(cut).air_c);
    assert!(end.fridge.powered && end.fridge.battery_level < cut.fridge.battery_level);
}

/// Defaults must let the planned control and alarm thresholds work.
#[test]
fn b1_planned_thresholds_are_reachable() {
    let text = "duration_ms = 14_400_000\nsnapshot_ms = 1000";
    let mut sim = Simulation::new(Scenario::parse(text).unwrap()).unwrap();
    let mut on_since: Option<(u64, f64)> = None;
    let mut switches = 0;
    let mut fastest = u64::MAX;
    let mut last_switch = 0;
    while sim.step().unwrap() {
        let (now, t) = (sim.world().time_ms, fridge(sim.world()).probe_c);
        assert!((2.0..=8.0).contains(&t));
        if now % 1000 != 0 {
            continue;
        }
        let on = sim.world().fridge.relay_on;
        let want = t >= 5.5 || (on && t > 4.5);
        if want != on {
            let node = NodeId::Fridge;
            sim.apply(&Action::Relay { node, on: want }).unwrap();
            on_since = want.then_some((now, t));
            if switches > 0 {
                fastest = fastest.min(now - last_switch);
            }
            last_switch = now;
            switches += 1;
        }
        if let Some((since, from)) = on_since
            && now - since == 300_000
        {
            assert!(from - t >= 0.2, "healthy cooling would raise an alarm");
        }
    }
    assert!(switches >= 4);
    assert!(fastest >= 120_000, "relay switches every {fastest} ms");
}

#[test]
fn b2_replay_is_identical() {
    for name in ["cooling", "door", "outage", "faults"] {
        let first = play(name);
        for _ in 0..3 {
            assert!(play(name) == first, "{name}");
        }
    }
}

#[test]
fn b2_pause_and_reset() {
    let expected = play("faults");
    let mut sim = Simulation::new(load("faults")).unwrap();
    for _ in 0..100_000 {
        sim.step().unwrap();
    }
    sim.paused = true;
    let frozen = sim.world().clone();
    for _ in 0..1000 {
        assert!(!sim.step().unwrap());
    }
    assert_eq!(*sim.world(), frozen);
    sim.paused = false;
    while sim.step().unwrap() {}
    assert_eq!(sim.world(), expected.last().unwrap());
    sim.reset().unwrap();
    assert!(record(&mut sim) == expected);
}

#[test]
fn b2_faults_take_effect() {
    let run = play("faults");
    let end = run.last().unwrap();
    assert_eq!(fridge(at(&run, 40_000)).cooling_w, 0.0);
    assert_eq!(fridge(end).reading_c(), None);
    let carrier = end.carrier.thermal.as_ref().unwrap();
    assert_eq!(carrier.sensor, Sensor::Biased(1.5));
    assert_eq!(carrier.reading_c(), Some(carrier.probe_c + 1.5));
    assert!(!end.carrier.powered && !end.inventory.network_up);
    assert_eq!(end.ambient_c, 35.0);
    assert_eq!(end.inventory.last_scan.as_deref(), Some("04A1B2C3"));
    assert_eq!(end.inventory.scan_count, 1);
}

#[test]
fn b2_invalid_scenarios_are_rejected() {
    let head = "duration_ms = 1000\nsnapshot_ms = 100\n";
    for bad in [
        "[[event]]\nat_ms = 1000\naction = \"ambient\"\ncelsius = 1.0",
        "[[event]]\nat_ms = 0\naction = \"ambient\"\ncelsius = nan",
        "[[event]]\nat_ms = 0\naction = \"unknown\"",
        "[[event]]\nat_ms = 0\naction = \"ambient\"\ncelsius = 1.0\ntypo = 1",
        "[[event]]\nat_ms = 0\naction = \"door\"\nnode = \"inventory\"\nopen = true",
        "[[event]]\nat_ms = 0\naction = \"scan\"\nnode = \"fridge\"\nuid = \"xyz\"",
        "[[event]]\nat_ms = 5\naction = \"ambient\"\ncelsius = 1.0\n[[event]]\nat_ms = 1\naction = \"ambient\"\ncelsius = 1.0",
        "[config]\nbattery_capacity_wh = 0.0",
        "typo = 1",
    ] {
        assert!(Scenario::parse(&format!("{head}{bad}")).is_err(), "{bad}");
    }
    assert!(Scenario::parse("duration_ms = 1000\nsnapshot_ms = 0").is_err());
}
