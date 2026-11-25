//! 3D clinic. Shows server state and sends operations.

mod hud;
mod link;
mod scene;

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
struct ScreenshotPath(String);

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
    .add_plugins((scene::plugin, hud::plugin));

    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--screenshot")
        && let Some(path) = args.next()
    {
        app.insert_resource(ScreenshotPath(path))
            .add_systems(Update, screenshot);
    }
    app.run();
}

fn receive(link: Res<Link>, mut live: ResMut<Live>) {
    live.0 = link.frame();
}

fn screenshot(
    mut commands: Commands,
    path: Res<ScreenshotPath>,
    time: Res<Time>,
    mut taken: Local<bool>,
    mut exit: MessageWriter<AppExit>,
) {
    let now = time.elapsed_secs();
    if now > 8.0 && !*taken {
        *taken = true;
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path.0.clone()));
    }
    // Leave time to write the file.
    if now > 10.0 {
        exit.write(AppExit::Success);
    }
}
