mod cosim;
mod doctor;
mod manifest;
mod setup;
mod smoke;
mod toolchain;

use clap::{Parser, Subcommand};
use std::{
    error::Error, ffi::OsString, os::unix::process::CommandExt, path::PathBuf, process::ExitCode,
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
