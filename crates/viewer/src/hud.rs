//! Node readings and operations. Exceptions show only while they last.

use crate::{
    Live,
    link::Link,
    scene::{node, scan, temperature_color},
};
use bevy::{
    picking::hover::Hovered,
    prelude::*,
    text::FontSize,
    ui_widgets::{Activate, Button},
};
use sim_core::{Action, Control, Frame, NodeId, Sensor};

const PANEL: Color = Color::srgba(0.03, 0.045, 0.06, 0.8);
const MUTED: Color = Color::srgb(0.6, 0.68, 0.75);
const WARNING: Color = Color::srgb(1.0, 0.72, 0.3);
const IDLE: Color = Color::srgb(0.13, 0.18, 0.24);
const HOVER: Color = Color::srgb(0.2, 0.29, 0.38);
const ACTIVE: Color = Color::srgb(0.1, 0.42, 0.45);
const FAULT: Color = Color::srgb(0.6, 0.16, 0.16);

const NODES: [(NodeId, &str); 3] = [
    (NodeId::Fridge, "FRIDGE"),
    (NodeId::Carrier, "CARRIER"),
    (NodeId::Inventory, "INVENTORY"),
];

pub fn plugin(app: &mut App) {
    app.add_systems(Startup, build)
        .add_systems(Update, (cards, clock, speed, buttons, keys).chain());
}

#[derive(Component)]
struct Reading(NodeId);

#[derive(Component)]
struct Details(NodeId);

#[derive(Component)]
struct Clock;

#[derive(Component)]
struct SpeedLabel;

#[derive(Component, Clone, Copy)]
enum Op {
    Door(NodeId),
    Mains,
    Relay,
    CoolingFault,
    SensorFault,
    Network,
    Scan,
    Pause,
    /// Steps through 1x, 10x and 60x.
    Speed,
    Reset,
}

/// Whether an operation is in effect, and whether it is a fault.
fn state(op: Op, f: &Frame) -> (bool, bool) {
    let fridge = &f.world.fridge;
    let thermal = |id| node(f, id).thermal.as_ref();
    match op {
        Op::Door(id) => (thermal(id).is_some_and(|t| t.door_open), false),
        Op::Mains => (!fridge.mains, true),
        Op::Relay => (fridge.relay_on, false),
        Op::CoolingFault => (
            thermal(NodeId::Fridge).is_some_and(|t| t.cooling_efficiency == 0.0),
            true,
        ),
        Op::SensorFault => (
            thermal(NodeId::Fridge).is_some_and(|t| t.sensor == Sensor::Disconnected),
            true,
        ),
        Op::Network => (!f.world.inventory.network_up, true),
        Op::Pause => (f.paused, false),
        Op::Scan | Op::Speed | Op::Reset => (false, false),
    }
}

fn perform(op: Op, f: &Frame, link: &Link) {
    let on = state(op, f).0;
    let fridge = NodeId::Fridge;
    match op {
        Op::Door(id) => link.act(Action::Door {
            node: id,
            open: !on,
        }),
        Op::Mains => link.act(Action::Mains { node: fridge, on }),
        Op::Relay => link.act(Action::Relay {
            node: fridge,
            on: !on,
        }),
        Op::CoolingFault => link.act(Action::Cooling {
            node: fridge,
            efficiency: if on { 1.0 } else { 0.0 },
        }),
        Op::SensorFault => link.act(Action::Sensor {
            node: fridge,
            state: if on { Sensor::Ok } else { Sensor::Disconnected },
        }),
        Op::Network => link.act(Action::Network {
            node: NodeId::Inventory,
            up: on,
        }),
        Op::Scan => link.act(scan()),
        Op::Pause => link.send(&Control::Pause { paused: !on }),
        Op::Speed => link.send(&Control::Speed {
            factor: match f.speed {
                x if x < 10.0 => 10.0,
                x if x < 60.0 => 60.0,
                _ => 1.0,
            },
        }),
        Op::Reset => link.send(&Control::Reset),
    }
}

fn label(text: &str, size: f32, color: Color) -> impl Bundle + use<> {
    (
        Text::new(text),
        TextFont {
            font_size: FontSize::Px(size),
            ..default()
        },
        TextColor(color),
    )
}

fn build(mut commands: Commands) {
    let panel = |node: Node| {
        (
            Node {
                position_type: PositionType::Absolute,
                padding: UiRect::all(px(14)),
                border_radius: BorderRadius::all(px(10)),
                ..node
            },
            BackgroundColor(PANEL),
        )
    };

    commands
        .spawn(panel(Node {
            left: px(16),
            top: px(16),
            width: px(190),
            flex_direction: FlexDirection::Column,
            row_gap: px(6),
            ..default()
        }))
        .with_children(|p| {
            for (id, name) in NODES {
                p.spawn((
                    Node {
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    children![
                        label(name, 14.0, MUTED),
                        (Reading(id), label("--", 18.0, Color::WHITE)),
                    ],
                ));
                p.spawn((
                    Details(id),
                    Node {
                        display: Display::None,
                        ..default()
                    },
                    label("", 13.0, WARNING),
                ));
            }
        });

    // Doors, lid and reader are operated in the scene.
    let faults = [
        (Op::Mains, "Mains"),
        (Op::CoolingFault, "Cooling"),
        (Op::SensorFault, "Sensor"),
        (Op::Network, "Network"),
    ];
    let time = [
        (Op::Pause, "Pause"),
        (Op::Speed, "1x"),
        (Op::Reset, "Reset"),
    ];
    let bar = |node: Node| {
        panel(Node {
            bottom: px(16),
            padding: UiRect::all(px(6)),
            column_gap: px(6),
            align_items: AlignItems::Center,
            ..node
        })
    };
    commands
        .spawn(bar(Node {
            left: px(16),
            ..default()
        }))
        .with_children(|p| {
            for (op, text) in faults {
                button(p, op, text);
            }
        });
    commands
        .spawn(bar(Node {
            right: px(16),
            ..default()
        }))
        .with_children(|p| {
            p.spawn((
                Node {
                    margin: UiRect::horizontal(px(8)),
                    ..default()
                },
                Clock,
                label("", 14.0, MUTED),
            ));
            for (op, text) in time {
                button(p, op, text);
            }
        });
}

fn button(parent: &mut ChildSpawnerCommands, op: Op, text: &str) {
    let mut entity = parent.spawn((
        op,
        Button,
        Hovered::default(),
        Node {
            padding: UiRect::axes(px(10), px(6)),
            border_radius: BorderRadius::all(px(6)),
            ..default()
        },
        BackgroundColor(IDLE),
    ));
    entity.with_children(|p| {
        let mut text = p.spawn(label(text, 13.0, Color::WHITE));
        if matches!(op, Op::Speed) {
            text.insert(SpeedLabel);
        }
    });
    entity.observe(move |_: On<Activate>, live: Res<Live>, link: Res<Link>| {
        if let Some(f) = &live.0 {
            perform(op, f, &link);
        }
    });
}

fn keys(input: Res<ButtonInput<KeyCode>>, live: Res<Live>, link: Res<Link>) {
    let Some(f) = &live.0 else { return };
    for (key, op) in [
        (KeyCode::Digit1, Op::Door(NodeId::Fridge)),
        (KeyCode::Digit2, Op::Door(NodeId::Carrier)),
        (KeyCode::Digit3, Op::Scan),
        (KeyCode::Digit4, Op::Relay),
        (KeyCode::Digit5, Op::Mains),
        (KeyCode::Digit6, Op::CoolingFault),
        (KeyCode::Digit7, Op::SensorFault),
        (KeyCode::Digit8, Op::Network),
        (KeyCode::Space, Op::Pause),
        (KeyCode::KeyR, Op::Reset),
    ] {
        if input.just_pressed(key) {
            perform(op, f, &link);
        }
    }
}

fn cards(
    live: Res<Live>,
    mut readings: Query<(&Reading, &mut Text, &mut TextColor), Without<Details>>,
    mut details: Query<(&Details, &mut Text, &mut Node), Without<Reading>>,
) {
    let Some(f) = &live.0 else {
        for (_, mut text, mut color) in &mut readings {
            text.0 = "--".into();
            color.0 = MUTED;
        }
        return;
    };
    for (id, mut text, mut color) in &mut readings {
        let n = node(f, id.0);
        let Some(t) = &n.thermal else {
            text.0 = if n.powered { "READY" } else { "OFF" }.into();
            color.0 = if n.powered { Color::WHITE } else { MUTED };
            continue;
        };
        let reading = t.reading_c().filter(|_| n.powered);
        text.0 = reading.map_or("NO DATA".into(), |c| format!("{c:.1} C"));
        color.0 = temperature_color(reading);
    }
    for (id, mut text, mut layout) in &mut details {
        text.0 = exceptions(id.0, node(f, id.0)).join("  ");
        layout.display = if text.0.is_empty() {
            Display::None
        } else {
            Display::Flex
        };
    }
}

fn exceptions(id: NodeId, n: &sim_core::Node) -> Vec<String> {
    let mut list = Vec::new();
    if n.thermal.as_ref().is_some_and(|t| t.door_open) {
        list.push("open".into());
    }
    // The carrier always runs on its battery.
    let carrier = id == NodeId::Carrier;
    if !n.mains && (!carrier || n.battery_level < 0.2) {
        list.push(format!("battery {:.0}%", n.battery_level * 100.0));
    }
    if !n.network_up {
        list.push("offline".into());
    }
    list
}

fn clock(live: Res<Live>, mut clocks: Query<&mut Text, With<Clock>>) {
    for mut text in &mut clocks {
        text.0 = match &live.0 {
            None => "Connecting".into(),
            Some(f) => {
                let s = f.world.time_ms / 1000;
                format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
            }
        };
    }
}

fn speed(live: Res<Live>, mut labels: Query<&mut Text, With<SpeedLabel>>) {
    let Some(f) = &live.0 else { return };
    for mut text in &mut labels {
        text.0 = format!("{}x", f.speed);
    }
}

fn buttons(live: Res<Live>, mut query: Query<(&Op, &Hovered, &mut BackgroundColor)>) {
    for (op, hovered, mut color) in &mut query {
        color.0 = match live.0.as_ref().map(|f| state(*op, f)) {
            Some((true, true)) => FAULT,
            Some((true, false)) => ACTIVE,
            _ if hovered.0 => HOVER,
            _ => IDLE,
        };
    }
}
