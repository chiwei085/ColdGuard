//! 3D clinic. Shows server state and sends operations.

mod batches;
mod display;
mod hud;
mod link;
mod scene;
mod stock;

use bevy::{
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
};
use link::Link;
use sim_core::Frame;

/// Copy of the latest server state.
#[derive(Resource, Default)]
pub struct Live(pub Option<Frame>);

#[derive(Resource)]
struct Shots {
    path: String,
    at_ms: Vec<u64>,
}

fn main() {
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins
            .set(AssetPlugin {
                file_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets").into(),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "ColdGuard".into(),
                    resolution: (1600, 900).into(),
                    ..default()
                }),
                ..default()
            }),
        MeshPickingPlugin,
    ))
    .insert_resource(Link::start())
    .init_resource::<Live>()
    .add_systems(PreUpdate, receive)
    .add_plugins((
        scene::plugin,
        hud::plugin,
        stock::plugin,
        display::plugin,
        batches::plugin,
    ));

    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--screenshot")
        && let Some(path) = args.next()
    {
        let at_ms = args.filter_map(|a| a.parse().ok()).map(|s: u64| s * 1000);
        let mut at_ms: Vec<_> = at_ms.collect();
        at_ms.sort_unstable();
        app.insert_resource(Shots { path, at_ms })
            .add_systems(Update, screenshot);
    }
    app.run();
}

fn receive(link: Res<Link>, mut live: ResMut<Live>) {
    live.0 = link.frame();
}

/// Takes one picture at each simulated time, or one after 8 seconds.
fn screenshot(
    mut commands: Commands,
    shots: Res<Shots>,
    live: Res<Live>,
    time: Res<Time>,
    mut taken: Local<usize>,
    mut last: Local<f32>,
    mut exit: MessageWriter<AppExit>,
) {
    let count = shots.at_ms.len().max(1);
    let now = time.elapsed_secs();
    if *taken == count {
        // Leave time to write the file.
        if now > *last + 2.0 {
            exit.write(AppExit::Success);
        }
        return;
    }
    let world_ms = live.0.as_ref().map_or(0, |f| f.world.time_ms);
    let (due, path) = match shots.at_ms.get(*taken) {
        Some(ms) => (
            world_ms >= *ms,
            shots
                .path
                .replace(".png", &format!("-{:04}.png", ms / 1000)),
        ),
        None => (now > 8.0, shots.path.clone()),
    };
    if due {
        *taken += 1;
        *last = now;
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path));
    }
}
