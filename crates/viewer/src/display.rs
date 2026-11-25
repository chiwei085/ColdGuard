//! Panels on the devices. They show local signals, with or without a network.

use crate::{
    Live,
    hud::{MUTED, WARNING, label},
    scene::{node, temperature_color},
    stock::target,
};
use bevy::prelude::*;
use sim_core::NodeId;

const SIZES: [(NodeId, UVec2); 2] = [
    (NodeId::Fridge, UVec2::new(672, 120)),
    (NodeId::Carrier, UVec2::new(440, 116)),
];

pub fn plugin(app: &mut App) {
    app.add_systems(Startup, build).add_systems(Update, refresh);
}

#[derive(Resource)]
pub struct Displays(Vec<(NodeId, Handle<Image>)>);

impl Displays {
    pub fn image(&self, id: NodeId) -> Option<Handle<Image>> {
        let found = self.0.iter().find(|(n, _)| *n == id)?;
        Some(found.1.clone())
    }
}

#[derive(Component)]
struct Temperature(NodeId);

#[derive(Component)]
struct Status(NodeId);

fn build(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let mut list = vec![];
    for (id, size) in SIZES {
        let (image, camera) = target(&mut commands, &mut images, size, -2 - id as isize);
        commands.spawn((
            UiTargetCamera(camera),
            Node {
                width: percent(100),
                height: percent(100),
                padding: UiRect::axes(px(18), px(6)),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                ..default()
            },
            children![
                (Temperature(id), label("", 64.0, MUTED)),
                (Status(id), label("", 26.0, Color::WHITE)),
            ],
        ));
        list.push((id, image));
    }
    commands.insert_resource(Displays(list));
}

fn refresh(
    live: Res<Live>,
    mut readings: Query<(&Temperature, &mut Text, &mut TextColor), Without<Status>>,
    mut states: Query<(&Status, &mut Text, &mut TextColor), Without<Temperature>>,
) {
    let Some(f) = &live.0 else { return };
    for (id, mut text, mut color) in &mut readings {
        let n = node(f, id.0);
        let reading = n.thermal.as_ref().and_then(|t| t.reading_c());
        text.0 = match reading {
            _ if !n.powered => String::new(),
            Some(c) => format!("{c:.1} C"),
            None => "SENSOR".into(),
        };
        color.0 = temperature_color(reading);
    }
    for (id, mut text, mut color) in &mut states {
        let n = node(f, id.0);
        if !n.powered {
            text.0.clear();
            continue;
        }
        let report = f.fleet.as_ref().map(|fleet| fleet.report(id.0));
        let battery = format!("BATTERY {:.0}%", n.battery_level * 100.0);
        let power = if n.mains {
            let cooling = if n.relay_on { "COOLING" } else { "IDLE" };
            format!("MAINS  {cooling}")
        } else {
            battery
        };
        let online = report.map_or(n.network_up, |r| r.online);
        let link = if online { "ONLINE" } else { "NO NETWORK" };
        let alarm = report.is_some_and(|r| r.buzzer);
        let alarm = if alarm { "  ALARM" } else { "" };
        text.0 = format!("{power}\n{link}{alarm}");
        color.0 = if alarm.is_empty() && online {
            Color::WHITE
        } else {
            WARNING
        };
    }
}
