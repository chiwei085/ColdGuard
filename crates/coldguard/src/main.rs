mod accept;
mod board;
mod cosim;
mod devices;
mod doctor;
mod hub;
mod lab;
mod live;
mod manifest;
mod proxy;
mod setup;
mod smoke;
mod toolchain;

use clap::{Parser, Subcommand};
use sim_core::{Scenario, Simulation};
use std::{
    error::Error,
    ffi::OsString,
    fs,
    io::{self, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::ExitCode,
};
use toolchain::Toolchain;

#[derive(Parser)]
#[command(about = "ColdGuard development and simulation tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the environment read-only (exit: 0 pass, 1 fail, 2 error).
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Install pinned tools into .tools, then run doctor.
    Setup,
    /// Run a command with the project environment.
    Exec {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
    /// Run a scenario headless. Print one JSON snapshot per line.
    Sim { scenario: PathBuf },
    /// Accept the firmware in closed loop. All cases by default.
    Accept { cases: Vec<String> },
    /// Print stored records, one JSON object per line.
    Records {
        /// Only the latest session.
        #[arg(long)]
        latest: bool,
    },
    /// Run a scenario in real time with the 3D viewer.
    Demo {
        #[arg(default_value = "scenarios/demo.toml")]
        scenario: PathBuf,
        /// Serve without opening the viewer.
        #[arg(long)]
        headless: bool,
        /// Run physics only.
        #[arg(long)]
        no_firmware: bool,
        /// Save the view after 8 seconds and exit.
        #[arg(long)]
        screenshot: Option<PathBuf>,
        /// Simulated seconds of the pictures instead.
        #[arg(long, value_delimiter = ',', requires = "screenshot")]
        at: Vec<u64>,
        /// Initial speed.
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
    },
}

fn load_scenario(path: &Path) -> Result<Scenario, Box<dyn Error>> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Scenario::parse(&text)?)
}

/// Rebuild the node firmware when its sources changed.
fn prepare(t: &Toolchain) -> Result<(), Box<dyn Error>> {
    if smoke::stale(&t.node_source(), &t.node_image()).unwrap_or(true) {
        setup::node(t)?;
    }
    Ok(())
}

fn demo(
    t: &Toolchain,
    path: &Path,
    headless: bool,
    firmware: bool,
    shot: Option<&Path>,
    at: &[u64],
    speed: f64,
) -> Result<(), Box<dyn Error>> {
    let scenario = load_scenario(path)?;
    if firmware {
        prepare(t)?;
    }
    let units = firmware.then_some(t);
    if headless {
        return live::serve(scenario, units, speed, || false);
    }
    // Build first so the server never waits for the compiler.
    let cargo = t.cargo_bin("cargo").ok_or("cargo not found")?;
    let status = std::process::Command::new(cargo)
        .args(["build", "--package", "viewer"])
        .current_dir(&t.root)
        .status()?;
    if !status.success() {
        return Err("viewer build failed".into());
    }
    let mut viewer = std::process::Command::new(t.root.join("target/debug/viewer"));
    if let Some(path) = shot {
        viewer.arg("--screenshot").arg(std::path::absolute(path)?);
        viewer.args(at.iter().map(u64::to_string));
    }
    let mut viewer = viewer.spawn()?;
    let result = live::serve(scenario, units, speed, || {
        !matches!(viewer.try_wait(), Ok(None))
    });
    let _ = viewer.kill();
    let _ = viewer.wait();
    result
}

fn records(t: &Toolchain) -> PathBuf {
    t.tools().join("records.sqlite3")
}

fn sim(path: &Path) -> Result<(), Box<dyn Error>> {
    let scenario = load_scenario(path)?;
    let mut out = io::stdout().lock();
    Simulation::new(scenario)?.run(|world| {
        serde_json::to_writer(&mut out, world)?;
        writeln!(out)?;
        Ok::<(), Box<dyn Error>>(())
    })
}

fn find_root() -> Result<PathBuf, Box<dyn Error>> {
    let cwd = std::env::current_dir()?;
    cwd.ancestors()
        .find(|d| d.join("toolchain.toml").is_file())
        .map(PathBuf::from)
        .ok_or_else(|| "toolchain.toml not found. Run from within the project directory".into())
}

fn check(t: &Toolchain, json: bool) -> Result<bool, Box<dyn Error>> {
    let checks = doctor::run(t);
    if json {
        println!("{}", serde_json::to_string_pretty(&checks)?);
    } else {
        doctor::print(&checks);
    }
    Ok(checks.iter().all(|c| c.status != doctor::Status::Fail))
}

fn run() -> Result<bool, Box<dyn Error>> {
    let command = Cli::parse().command;
    let root = find_root()?;
    let m = manifest::load(&root)?;
    let t = Toolchain { root, m };
    match command {
        Command::Doctor { json } => check(&t, json),
        Command::Setup => {
            setup::run(&t)?;
            check(&t, false)
        }
        Command::Demo {
            scenario,
            headless,
            no_firmware,
            screenshot,
            at,
            speed,
        } => demo(
            &t,
            &scenario,
            headless,
            !no_firmware,
            screenshot.as_deref(),
            &at,
            speed,
        )
        .map(|()| true),
        Command::Records { latest } => {
            hub::export(&records(&t), latest, &mut io::stdout().lock())?;
            Ok(true)
        }
        Command::Accept { cases } => {
            prepare(&t)?;
            Ok(accept::run(&t, &cases)?)
        }
        Command::Sim { scenario } => sim(&scenario).map(|()| true),
        Command::Exec { command } => {
            // exec returns only on failure.
            let e = t.idf_command(&command[0])?.args(&command[1..]).exec();
            Err(format!("{}: {e}", command[0].to_string_lossy()).into())
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(2)
        }
    }
}
