//! Video of a scripted replay. The units run the scenario once at 1x and
//! every view state is saved. The viewer then renders that timeline offline,
//! so camera, captions and encoding can change without running it again.

use crate::{lab::Lab, toolchain::Toolchain};
use sim_core::{Frame, Simulation};
use std::{
    error::Error,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::Command,
};

/// Matches the video frame rate so 1x sections never repeat a state.
const SAMPLE_MS: f64 = 1000.0 / 60.0;

/// Render each scenario, then join them in order.
pub fn run(
    t: &Toolchain,
    out: &Path,
    scenarios: &[PathBuf],
    reuse: bool,
) -> Result<(), Box<dyn Error>> {
    let dir = t.tools().join("film");
    fs::create_dir_all(&dir)?;
    let mut parts = vec![];
    for scenario in scenarios {
        let name = scenario.file_stem().unwrap_or_default().to_string_lossy();
        let timeline = dir.join(format!("{name}.jsonl"));
        if !reuse {
            capture(t, scenario, &timeline)?;
        } else if !timeline.is_file() {
            return Err(format!(
                "{} not found. Run without --reuse first",
                timeline.display()
            )
            .into());
        }
        let part = dir.join(format!("{name}.mp4"));
        // A no-op build after the first part.
        let status = crate::viewer(t)?
            .arg("--render")
            .args([&timeline, &std::path::absolute(scenario)?, &part])
            .status()?;
        if !status.success() {
            return Err(format!("render {}: {status}", scenario.display()).into());
        }
        parts.push(part);
    }
    if let [only] = &parts[..] {
        fs::copy(only, out)?;
        return Ok(());
    }
    // Parts share one encoding, so they join without encoding again.
    let list = dir.join("parts.txt");
    let lines: Vec<_> = parts
        .iter()
        .map(|p| format!("file '{}'", p.display()))
        .collect();
    fs::write(&list, lines.join("\n"))?;
    let joined = out.with_extension("part.mp4");
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
        ])
        .arg(&list)
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(&joined)
        .status()?;
    if !status.success() {
        return Err(format!("join {status}").into());
    }
    fs::rename(joined, out)?;
    Ok(())
}

/// One JSON frame per line. Events are left empty while unchanged. Only a
/// run whose checks hold is kept.
fn capture(t: &Toolchain, scenario: &Path, path: &Path) -> Result<(), Box<dyn Error>> {
    let loaded = crate::load_scenario(scenario)?;
    crate::prepare(t)?;
    let part = path.with_extension("part");
    let mut out = BufWriter::new(File::create(&part)?);
    let (mut last_events, mut next_ms) = (vec![], 0.0);
    let total_s = loaded.duration_ms / 1000;
    let mut sample = |sim: &Simulation, lab: &Lab| -> Result<(), Box<dyn Error>> {
        let now = sim.world().time_ms;
        if now.is_multiple_of(30_000) {
            eprintln!("Simulating {} / {total_s} s", now / 1000);
        }
        if (now as f64) < next_ms {
            return Ok(());
        }
        next_ms += SAMPLE_MS;
        let mut fleet = lab.fleet();
        if fleet.events == last_events {
            fleet.events.clear();
        } else {
            last_events.clone_from(&fleet.events);
        }
        let frame = Frame {
            paused: false,
            finished: false,
            speed: 1.0,
            world: sim.world().clone(),
            caption: None,
            fleet: Some(fleet),
        };
        serde_json::to_writer(&mut out, &frame)?;
        out.write_all(b"\n")?;
        Ok(())
    };
    crate::accept::run_checked(t, loaded, |sim, lab| {
        sample(sim, lab).map_err(|e| e.to_string())
    })
    .map_err(|e| format!("{}: {e}", scenario.display()))?;
    out.into_inner()?.sync_all()?;
    fs::rename(part, path)?;
    Ok(())
}
