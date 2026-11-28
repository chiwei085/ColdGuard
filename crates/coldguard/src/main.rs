mod accept;
mod assets;
mod board;
mod cosim;
mod devices;
mod doctor;
mod film;
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
    /// Rebuild the clinic model with Blender 4.0.
    Assets,
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
    /// Render scripted replays to one mp4 file. Each runs once at 1x first.
    Video {
        out: PathBuf,
        #[arg(required = true)]
        scenarios: Vec<PathBuf>,
        /// Render the last runs again. Captions come from the current files.
        #[arg(long)]
        reuse: bool,
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
    shots: Option<(PathBuf, Vec<u64>)>,
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
    let mut viewer = viewer(t)?;
    if let Some((path, at)) = shots {
        let path = viewer.path(&path)?;
        viewer.command.arg("--screenshot").arg(path);
        viewer.command.args(at.iter().map(u64::to_string));
    }
    let mut viewer = viewer.command.spawn()?;
    let result = live::serve(scenario, units, speed, || {
        !matches!(viewer.try_wait(), Ok(None))
    });
    let _ = viewer.kill();
    let status = viewer.wait()?;
    result?;
    // The server stops only after the viewer exits.
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("viewer {status}").into())
}

/// The viewer built for this host. Under WSL it is a Windows program, so
/// rendering uses the Windows GPU driver.
struct Viewer {
    command: std::process::Command,
    windows: bool,
}

impl Viewer {
    /// A path the viewer can open.
    fn path(&self, path: &Path) -> Result<OsString, Box<dyn Error>> {
        let path = std::path::absolute(path)?;
        if !self.windows {
            return Ok(path.into());
        }
        let out = std::process::Command::new("wslpath")
            .arg("-w")
            .arg(&path)
            .output()?;
        if !out.status.success() {
            return Err(format!("wslpath {}: {}", path.display(), out.status).into());
        }
        Ok(String::from_utf8(out.stdout)?.trim_end().into())
    }
}

/// Build the viewer for this host.
fn viewer(t: &Toolchain) -> Result<Viewer, Box<dyn Error>> {
    let windows = toolchain::windows_viewer();
    let cargo = t.cargo_bin("cargo").ok_or("cargo not found")?;
    let mut build = std::process::Command::new(cargo);
    build.args(["build", "--package", "viewer"]);
    if windows {
        build.args(["--target", toolchain::WINDOWS_TARGET]);
    }
    if !build.current_dir(&t.root).status()?.success() {
        return Err("viewer build failed".into());
    }
    let bin = if windows {
        let target = t.root.join("target").join(toolchain::WINDOWS_TARGET);
        target.join("debug/viewer.exe")
    } else {
        t.root.join("target/debug/viewer")
    };
    let mut command = std::process::Command::new(bin);
    if windows {
        // The /p flag converts the path when it crosses to Windows.
        let mut share = std::env::var_os("WSLENV").unwrap_or_default();
        if !share.is_empty() {
            share.push(":");
        }
        share.push("COLDGUARD_ASSETS/p");
        command
            .env("COLDGUARD_ASSETS", t.root.join("crates/viewer/assets"))
            .env("WSLENV", share);
    }
    Ok(Viewer { command, windows })
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
        Command::Assets => assets::run(&t).map(|()| true),
        Command::Demo {
            scenario,
            headless,
            no_firmware,
            screenshot,
            at,
            speed,
        } => {
            let shots = screenshot.map(|path| (path, at));
            demo(&t, &scenario, headless, !no_firmware, shots, speed).map(|()| true)
        }
        Command::Records { latest } => {
            hub::export(&records(&t), latest, &mut io::stdout().lock())?;
            Ok(true)
        }
        Command::Accept { cases } => {
            prepare(&t)?;
            Ok(accept::run(&t, &cases)?)
        }
        Command::Video {
            out,
            scenarios,
            reuse,
        } => film::run(&t, &out, &scenarios, reuse).map(|()| true),
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
