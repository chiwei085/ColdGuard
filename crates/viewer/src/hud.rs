//! Status cards, history chart and operations.

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
use std::collections::VecDeque;

const PANEL: Color = Color::srgba(0.04, 0.06, 0.09, 0.86);
const MUTED: Color = Color::srgb(0.6, 0.68, 0.75);
const IDLE: Color = Color::srgb(0.13, 0.18, 0.24);
const HOVER: Color = Color::srgb(0.2, 0.29, 0.38);
const ACTIVE: Color = Color::srgb(0.1, 0.42, 0.45);
const FAULT: Color = Color::srgb(0.6, 0.16, 0.16);

const NODES: [(NodeId, &str); 3] = [
    (NodeId::Fridge, "FRIDGE"),
    (NodeId::Carrier, "CARRIER"),
    (NodeId::Inventory, "INVENTORY"),
];

const BARS: usize = 150;
/// Simulated time between chart samples.
const SAMPLE_MS: u64 = 10_000;
const CHART_MIN: f64 = -2.0;
const CHART_MAX: f64 = 14.0;
const CHART_HEIGHT: f32 = 110.0;

pub fn plugin(app: &mut App) {
    app.init_resource::<History>()
        .add_systems(Startup, build)
        .add_systems(Update, (cards, clock, buttons, record, chart, keys).chain());
}

#[derive(Resource, Default)]
struct History {
    last_ms: u64,
    samples: VecDeque<Option<f64>>,
}

#[derive(Component)]
struct Reading(NodeId);

#[derive(Component)]
struct Details(NodeId);

#[derive(Component)]
struct Clock;

#[derive(Component)]
struct Bar(usize);

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
    Speed(f64),
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
        Op::Speed(x) => (f.speed == x, false),
        Op::Scan | Op::Reset => (false, false),
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
        Op::Speed(factor) => link.send(&Control::Speed { factor }),
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
            width: px(270),
            flex_direction: FlexDirection::Column,
            row_gap: px(6),
            ..default()
        }))
        .with_children(|p| {
            p.spawn(label("COLDGUARD", 24.0, Color::WHITE));
            p.spawn((Clock, label("Connecting", 15.0, MUTED)));
            for (id, name) in NODES {
                p.spawn((
                    Node {
                        margin: UiRect::top(px(10)),
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    children![
                        label(name, 16.0, MUTED),
                        (Reading(id), label("--", 26.0, Color::WHITE)),
                    ],
                ));
                p.spawn((Details(id), label("", 14.0, MUTED)));
            }
        });

    commands
        .spawn(panel(Node {
            right: px(16),
            top: px(16),
            width: px(BARS as f32 * 2.0 + 28.0),
            flex_direction: FlexDirection::Column,
            row_gap: px(8),
            ..default()
        }))
        .with_children(|p| {
            p.spawn(label("Fridge probe, last 25 simulated min", 14.0, MUTED));
            p.spawn(Node {
                height: px(CHART_HEIGHT),
                ..default()
            })
            .with_children(|c| {
                // Safe band from 2 to 8 C.
                let y = |t: f64| ((t - CHART_MIN) / (CHART_MAX - CHART_MIN)) as f32 * CHART_HEIGHT;
                c.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        bottom: px(y(2.0)),
                        height: px(y(8.0) - y(2.0)),
                        width: px(BARS as f32 * 2.0),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.1, 0.7, 0.4, 0.14)),
                ));
                for i in 0..BARS {
                    c.spawn((
                        Bar(i),
                        Node {
                            position_type: PositionType::Absolute,
                            left: px(i as f32 * 2.0),
                            width: px(2),
                            height: px(3),
                            ..default()
                        },
                        BackgroundColor(Color::NONE),
                    ));
                }
            });
            p.spawn(label("-2 to 14 C, band 2 to 8 C", 12.0, MUTED));
        });

    commands
        .spawn(panel(Node {
            left: px(16),
            right: px(16),
            bottom: px(16),
            flex_wrap: FlexWrap::Wrap,
            column_gap: px(8),
            row_gap: px(8),
            justify_content: JustifyContent::Center,
            ..default()
        }))
        .with_children(|p| {
            for (op, text) in [
                (Op::Door(NodeId::Fridge), "Door [1]"),
                (Op::Door(NodeId::Carrier), "Lid [2]"),
                (Op::Scan, "Scan [3]"),
                (Op::Relay, "Relay [4]"),
                (Op::Mains, "Mains [5]"),
                (Op::CoolingFault, "Cooling [6]"),
                (Op::SensorFault, "Sensor [7]"),
                (Op::Network, "Network [8]"),
                (Op::Pause, "Pause [Space]"),
                (Op::Speed(1.0), "1x"),
                (Op::Speed(10.0), "10x"),
                (Op::Speed(60.0), "60x"),
                (Op::Reset, "Reset [R]"),
            ] {
                p.spawn((
                    op,
                    Button,
                    Hovered::default(),
                    Node {
                        padding: UiRect::axes(px(12), px(8)),
                        border_radius: BorderRadius::all(px(6)),
                        ..default()
                    },
                    BackgroundColor(IDLE),
                    children![label(text, 14.0, Color::WHITE)],
                ))
                .observe(
                    move |_: On<Activate>, live: Res<Live>, link: Res<Link>| {
                        if let Some(f) = &live.0 {
                            perform(op, f, &link);
                        }
                    },
                );
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
    mut details: Query<(&Details, &mut Text), Without<Reading>>,
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
        text.0 = reading.map_or("NO DATA".into(), |c| format!("{c:.2} C"));
        color.0 = temperature_color(reading);
    }
    for (id, mut text) in &mut details {
        let n = node(f, id.0);
        let power = if n.mains { "mains" } else { "battery" };
        let network = if n.network_up { "online" } else { "offline" };
        let mut s = format!("{power} {:.0}%  {network}", n.battery_level * 100.0);
        if let Some(t) = &n.thermal {
            let door = if t.door_open { "open" } else { "closed" };
            s += &format!(
                "\ndoor {door}  air {:.2} C  cooling {:.0} W",
                t.air_c, t.cooling_w
            );
        } else {
            let badge = n.last_scan.as_deref().unwrap_or("none");
            s += &format!("\nlast badge {badge} ({})", n.scan_count);
        }
        text.0 = s;
    }
}

fn clock(live: Res<Live>, mut clocks: Query<&mut Text, With<Clock>>) {
    for mut text in &mut clocks {
        text.0 = match &live.0 {
            None => "Connecting".into(),
            Some(f) => {
                let s = f.world.time_ms / 1000;
                let mode = if f.finished {
                    "finished"
                } else if f.paused {
                    "paused"
                } else {
                    "running"
                };
                format!(
                    "{:02}:{:02}:{:02}  {}x  {mode}",
                    s / 3600,
                    s / 60 % 60,
                    s % 60,
                    f.speed
                )
            }
        };
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

fn record(live: Res<Live>, mut history: ResMut<History>) {
    let Some(f) = &live.0 else { return };
    let now = f.world.time_ms;
    if now < history.last_ms {
        *history = History::default();
    }
    if history.samples.is_empty() || now >= history.last_ms + SAMPLE_MS {
        history.last_ms = now;
        let n = &f.world.fridge;
        let reading = n.thermal.as_ref().and_then(|t| t.reading_c());
        history.samples.push_back(reading.filter(|_| n.powered));
        if history.samples.len() > BARS {
            history.samples.pop_front();
        }
    }
}

fn chart(history: Res<History>, mut bars: Query<(&Bar, &mut Node, &mut BackgroundColor)>) {
    if !history.is_changed() {
        return;
    }
    for (bar, mut node, mut color) in &mut bars {
        match history.samples.get(bar.0).copied().flatten() {
            Some(t) => {
                let ratio = ((t - CHART_MIN) / (CHART_MAX - CHART_MIN)).clamp(0.0, 1.0);
                node.bottom = px(ratio as f32 * (CHART_HEIGHT - 3.0));
                color.0 = temperature_color(Some(t));
            }
            None => color.0 = Color::NONE,
        }
    }
}
