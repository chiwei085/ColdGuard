//! Video of a scripted replay. The units run the scenario once at 1x and
//! every view state is saved. The viewer then renders that timeline offline,
//! so camera, captions and encoding can change without running it again.

use crate::{lab::Lab, toolchain::Toolchain};
use sim_core::{Frame, Simulation, VIDEO_FPS, VIDEO_SIZE};
use std::{
    error::Error,
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Matches the video frame rate so 1x sections never repeat a state.
const SAMPLE_MS: f64 = 1000.0 / VIDEO_FPS as f64;

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
        render(t, &timeline, scenario, &part)
            .map_err(|e| format!("render {}: {e}", scenario.display()))?;
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

/// Encodes the raw frames of the viewer to H.264, then checks the finished file.
fn render(
    t: &Toolchain,
    timeline: &Path,
    scenario: &Path,
    out: &Path,
) -> Result<(), Box<dyn Error>> {
    let part = out.with_extension("part.mp4");
    let mut ffmpeg = Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
        ])
        .args(["-s", &format!("{}x{}", VIDEO_SIZE.0, VIDEO_SIZE.1)])
        .args(["-framerate", &VIDEO_FPS.to_string(), "-i", "-"])
        .args([
            "-vf",
            "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p",
        ])
        .args(["-c:v", "libx264", "-preset", "slow", "-crf", "17"])
        .args(["-colorspace", "bt709", "-color_primaries", "bt709"])
        .args(["-color_trc", "bt709", "-color_range", "tv"])
        .args(["-movflags", "+faststart"])
        .arg(&part)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    // A no-op build after the first part.
    let mut viewer = crate::viewer(t)?;
    let (timeline, scenario) = (viewer.path(timeline)?, viewer.path(scenario)?);
    let mut viewer = viewer
        .command
        .arg("--render")
        .args([timeline, scenario])
        .stdout(Stdio::piped())
        .spawn()?;
    let mut frames = viewer.stdout.take().ok_or("viewer output closed")?;
    let mut encoder = ffmpeg.stdin.take().ok_or("ffmpeg input closed")?;
    let copied = io::copy(&mut frames, &mut encoder);
    drop(encoder);
    let (viewer, ffmpeg) = (viewer.wait()?, ffmpeg.wait()?);
    if !viewer.success() {
        return Err(format!("viewer {viewer}").into());
    }
    if !ffmpeg.success() {
        return Err(format!("ffmpeg {ffmpeg}").into());
    }
    let bytes = copied?;
    let frame = u64::from(VIDEO_SIZE.0 * VIDEO_SIZE.1 * 4);
    if bytes == 0 || bytes % frame != 0 {
        return Err(format!("viewer wrote {bytes} bytes, not whole frames").into());
    }
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-count_packets", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_packets", "-of", "csv=p=0"])
        .arg(&part)
        .output()?;
    let decoded = String::from_utf8_lossy(&probe.stdout).trim().to_string();
    if decoded != (bytes / frame).to_string() {
        return Err(format!("wrote {} frames, decoded {decoded}", bytes / frame).into());
    }
    fs::rename(&part, out)?;
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
