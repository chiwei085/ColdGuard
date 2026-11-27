//! Offline rendering of a saved timeline to video. One update is one video
//! frame at a fixed time step, so motion and capture never depend on how fast
//! the host renders.

use crate::Live;
use bevy::{
    camera::RenderTarget,
    prelude::*,
    render::{
        render_resource::{TextureFormat, TextureUsages},
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    time::TimeUpdateStrategy,
};
use sim_core::{Entry, Frame, Scenario};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::{JoinHandle, spawn},
    time::Duration,
};

const FPS: u32 = 60;
const SIZE: (u32, u32) = (1920, 1080);
/// Frames drawn at time zero to settle the camera and the TAA history.
const WARMUP: u32 = 90;

#[derive(Resource)]
struct Film {
    /// Events are kept apart because they rarely change.
    frames: Vec<Frame>,
    events: Vec<(usize, Vec<Entry>)>,
    scenario: Scenario,
    target: Handle<Image>,
    sim_ms: f64,
    end_ms: f64,
    warmup: u32,
    taken: u64,
    /// Shared with pending captures so closing it ends the encoder.
    tx: Arc<Mutex<Option<SyncSender<(u64, Image)>>>>,
    written: Arc<AtomicU64>,
    writer: Option<JoinHandle<Result<(), String>>>,
}

/// Arguments are the timeline, the scenario for captions and the output file.
pub fn start(app: &mut App, args: &[String]) -> Result<(), String> {
    let [timeline, scenario, out] = args else {
        return Err("usage: viewer --render TIMELINE SCENARIO OUT".into());
    };
    let (frames, events) = load(Path::new(timeline))?;
    let text = fs::read_to_string(scenario).map_err(|e| format!("{scenario}: {e}"))?;
    let scenario = Scenario::parse(&text)?;
    let end_ms = frames.last().map_or(0, |f| f.world.time_ms) as f64;

    let mut image = Image::new_target_texture(SIZE.0, SIZE.1, TextureFormat::Rgba8UnormSrgb, None);
    image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = app.world_mut().resource_mut::<Assets<Image>>().add(image);

    // Bounded so a slow encoder slows rendering instead of filling memory.
    let (tx, rx) = sync_channel(16);
    let written = Arc::new(AtomicU64::new(0));
    let (out, count) = (PathBuf::from(out), written.clone());
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
        1.0 / f64::from(FPS),
    )))
    .insert_resource(Film {
        frames,
        events,
        scenario,
        target,
        sim_ms: 0.0,
        end_ms,
        warmup: WARMUP,
        taken: 0,
        tx: Arc::new(Mutex::new(Some(tx))),
        written,
        writer: Some(spawn(move || encode(&out, &rx, &count))),
    })
    .add_systems(Update, (aim, hide_controls))
    .add_systems(PreUpdate, play.after(crate::receive));
    Ok(())
}

fn load(path: &Path) -> Result<(Vec<Frame>, Vec<(usize, Vec<Entry>)>), String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (mut frames, mut events) = (vec![], vec![(0, vec![])]);
    for (i, line) in text.lines().enumerate() {
        let mut frame: Frame =
            serde_json::from_str(line).map_err(|e| format!("line {}: {e}", i + 1))?;
        if let Some(fleet) = &mut frame.fleet
            && !fleet.events.is_empty()
        {
            events.push((i, std::mem::take(&mut fleet.events)));
        }
        frames.push(frame);
    }
    if frames.is_empty() {
        return Err(format!("{} is empty", path.display()));
    }
    Ok((frames, events))
}

impl Film {
    /// The saved state at a simulated time, with captions from the scenario.
    fn frame(&self, sim_ms: f64) -> Frame {
        let i = self
            .frames
            .partition_point(|f| f.world.time_ms as f64 <= sim_ms)
            .saturating_sub(1);
        let mut frame = self.frames[i].clone();
        let changed = self.events.partition_point(|(at, _)| *at <= i) - 1;
        if let Some(fleet) = &mut frame.fleet {
            fleet.events.clone_from(&self.events[changed].1);
        }
        frame.caption = self.scenario.caption_at(sim_ms).cloned();
        frame
    }

    /// Simulated time after some video time. Speed changes inside the span
    /// are honoured and cut spans take no video time.
    fn advance(&self, mut sim_ms: f64, mut video_ms: f64) -> f64 {
        loop {
            let caption = self.scenario.caption_at(sim_ms);
            let next = self
                .scenario
                .captions
                .iter()
                .find(|c| c.at_ms as f64 > sim_ms)
                .map_or(f64::INFINITY, |c| c.at_ms as f64);
            if caption.is_some_and(|c| c.video_cut) {
                // A cut at the end ends the video.
                if next.is_infinite() {
                    return next;
                }
                sim_ms = next;
                continue;
            }
            let speed = caption.map_or(1.0, |c| c.video_speed);
            let room = (next - sim_ms) / speed;
            if room >= video_ms {
                return sim_ms + video_ms * speed;
            }
            sim_ms = next;
            video_ms -= room;
        }
    }
}

/// Point the scene camera at the offscreen image.
fn aim(mut commands: Commands, film: Res<Film>, cameras: Query<Entity, Added<Camera3d>>) {
    for camera in &cameras {
        commands
            .entity(camera)
            .insert(RenderTarget::Image(film.target.clone().into()));
    }
}

/// Buttons cannot be pressed in a video.
fn hide_controls(controls: Query<&mut Node, Added<crate::hud::Controls>>) {
    for mut node in controls {
        node.display = Display::None;
    }
}

fn play(
    mut commands: Commands,
    mut film: ResMut<Film>,
    mut live: ResMut<Live>,
    ready: Option<Res<crate::scene::SceneReady>>,
    mut exit: MessageWriter<AppExit>,
) {
    if film.warmup > 0 {
        film.sim_ms = film.advance(0.0, 0.0);
        live.0 = Some(film.frame(film.sim_ms));
        if ready.is_some() {
            film.warmup -= 1;
        }
        return;
    }
    if film.sim_ms <= film.end_ms {
        live.0 = Some(film.frame(film.sim_ms));
        let (index, tx) = (film.taken, film.tx.clone());
        commands
            .spawn(Screenshot::image(film.target.clone()))
            .observe(move |shot: On<ScreenshotCaptured>| {
                if let Some(tx) = &*tx.lock().unwrap() {
                    let _ = tx.send((index, shot.image.clone()));
                }
            });
        film.taken += 1;
        film.sim_ms = film.advance(film.sim_ms, 1000.0 / f64::from(FPS));
        if film.taken % (u64::from(FPS) * 10) == 0 {
            info!(
                "Rendered {} s of video, {:.0} / {:.0} s simulated",
                film.taken / u64::from(FPS),
                film.sim_ms / 1000.0,
                film.end_ms / 1000.0
            );
        }
        return;
    }
    // Every capture must reach the encoder before it closes.
    if film.written.load(Ordering::Relaxed) < film.taken {
        return;
    }
    film.tx.lock().unwrap().take();
    let result = film.writer.take().map(JoinHandle::join);
    match result {
        Some(Ok(Ok(()))) => {
            info!("Video saved, {} frames", film.taken);
            exit.write(AppExit::Success);
        }
        Some(Ok(Err(e))) => {
            error!("Video failed: {e}");
            exit.write(AppExit::error());
        }
        _ => {
            exit.write(AppExit::error());
        }
    }
}

/// Writes frames in order to ffmpeg, then checks the finished file.
fn encode(out: &Path, shots: &Receiver<(u64, Image)>, written: &AtomicU64) -> Result<(), String> {
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
        .args(["-s", &format!("{}x{}", SIZE.0, SIZE.1)])
        .args(["-framerate", &FPS.to_string(), "-i", "-"])
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
    let mut stdin = ffmpeg.stdin.take().ok_or("ffmpeg input closed")?;
    let mut early = BTreeMap::new();
    let mut next = 0;
    for (index, image) in shots {
        early.insert(index, image);
        while let Some(image) = early.remove(&next) {
            let data = image.data.as_deref().ok_or("empty frame")?;
            if data.len() != (SIZE.0 * SIZE.1 * 4) as usize {
                return Err(format!("frame {next} has {} bytes", data.len()));
            }
            stdin.write_all(data).map_err(|e| format!("ffmpeg: {e}"))?;
            next += 1;
            written.store(next, Ordering::Relaxed);
        }
    }
    drop(stdin);
    let status = ffmpeg.wait().map_err(|e| format!("ffmpeg: {e}"))?;
    if !status.success() {
        return Err(format!("ffmpeg {status}"));
    }
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-count_packets", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_packets", "-of", "csv=p=0"])
        .arg(&part)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    let decoded = String::from_utf8_lossy(&probe.stdout).trim().to_string();
    if decoded != next.to_string() {
        return Err(format!("wrote {next} frames, decoded {decoded}"));
    }
    fs::rename(&part, out).map_err(|e| format!("{}: {e}", out.display()))
}
