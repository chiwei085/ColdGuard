mod doctor;
mod manifest;

use clap::{Parser, Subcommand};
use std::{error::Error, path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(about = "ColdGuard 開發與模擬工具")]
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
}

fn find_root() -> Result<PathBuf, Box<dyn Error>> {
    let cwd = std::env::current_dir()?;
    cwd.ancestors()
        .find(|d| d.join("toolchain.toml").is_file())
        .map(PathBuf::from)
        .ok_or_else(|| "找不到 toolchain.toml，請在專案目錄內執行".into())
}

fn run() -> Result<bool, Box<dyn Error>> {
    match Cli::parse().command {
        Command::Doctor { json } => {
            let root = find_root()?;
            let checks = doctor::run(&root, &manifest::load(&root)?);
            if json {
                println!("{}", serde_json::to_string_pretty(&checks)?);
            } else {
                doctor::print(&checks);
            }
            Ok(checks.iter().all(|c| c.status != doctor::Status::Fail))
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("錯誤：{e}");
            ExitCode::from(2)
        }
    }
}
