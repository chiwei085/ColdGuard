//! Node readings and operations. Exceptions show only while they last.

use crate::{
    Live,
    link::Link,
    scene::{scan, temperature_color},
};
use bevy::{
    picking::hover::Hovered,
    prelude::*,
    text::FontSize,
    ui_widgets::{Activate, Button},
};
use sim_core::{Action, Control, Frame, NodeId, Report, Sensor};

pub const PANEL: Color = Color::srgba(0.03, 0.045, 0.06, 0.88);
pub const MUTED: Color = Color::srgb(0.6, 0.68, 0.75);
pub const WARNING: Color = Color::srgb(1.0, 0.72, 0.3);
pub const IDLE: Color = Color::srgb(0.13, 0.18, 0.24);
const HOVER: Color = Color::srgb(0.2, 0.29, 0.38);
const ACTIVE: Color = Color::srgb(0.1, 0.42, 0.45);
const FAULT: Color = Color::srgb(0.6, 0.16, 0.16);

const NODES: [(NodeId, &str); 3] = [
    (NodeId::Fridge, "FRIDGE"),
    (NodeId::Carrier, "CARRIER"),
    (NodeId::Inventory, "INVENTORY"),
];

pub fn plugin(app: &mut App) {
    app.init_resource::<Hint>()
        .init_resource::<Sent>()
        .add_systems(Startup, build)
        .add_systems(
            Update,
            (
                cards,
                clock,
                speed,
                buttons,
                keys.run_if(not(crate::stock::typing)),
                caption,
                outcome,
                tooltip,
            )
                .chain(),
        );
}

#[derive(Component)]
struct Reading(NodeId);

#[derive(Component)]
struct Details(NodeId);

/// What the pointer is on.
#[derive(Resource, Default)]
pub struct Hint(pub Option<&'static str>);

/// Time of the last command. The unit has not answered it yet.
#[derive(Resource, Default)]
pub struct Sent(pub Option<u64>);

/// Hidden when the view cannot be operated.
#[derive(Component)]
pub struct Controls;

#[derive(Component)]
struct CaptionText;

#[derive(Component)]
struct ChapterText;

#[derive(Component)]
struct Outcome;

#[derive(Component)]
struct Tooltip;

/// Help for an operable model node.
pub fn hint(name: &str) -> Option<&'static str> {
    Some(match name {
        "fridge_door" => "Fridge door  [click]",
        "carrier_lid" | "carrier_body" => "Box lid  [click]",
        "reader" | "badge" => "Show staff badge  [click]",
        "lamp_inventory_screen" => "Stock and records  [click]",
        _ => return None,
    })
}

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
    let thermal = f.world.fridge.thermal.as_ref();
    match op {
        Op::Door(id) => (f.world.node(id).door_open(), false),
        Op::Mains => (!fridge.mains, true),
        Op::Relay => (fridge.relay_on, false),
        Op::CoolingFault => (thermal.is_some_and(|t| t.cooling_efficiency == 0.0), true),
        Op::SensorFault => (
            thermal.is_some_and(|t| t.sensor == Sensor::Disconnected),
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
                // Firmware cannot run that fast.
                x if x < 60.0 && f.fleet.is_none() => 60.0,
                _ => 1.0,
            },
        }),
        Op::Reset => link.send(&Control::Reset),
    }
}

pub fn label(text: &str, size: f32, color: Color) -> impl Bundle + use<> {
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

    let center = |node: Node| Node {
        position_type: PositionType::Absolute,
        width: percent(100),
        justify_content: JustifyContent::Center,
        ..node
    };
    for (marker, node) in [
        (
            true,
            center(Node {
                bottom: px(24),
                ..default()
            }),
        ),
        (
            false,
            center(Node {
                bottom: px(80),
                ..default()
            }),
        ),
    ] {
        commands
            .spawn((node, bevy::picking::Pickable::IGNORE))
            .with_children(|p| {
                let mut line = p.spawn((
                    Node {
                        padding: UiRect::axes(px(16), px(9)),
                        border_radius: BorderRadius::all(px(10)),
                        display: Display::None,
                        max_width: px(930),
                        ..default()
                    },
                    BackgroundColor(PANEL),
                    label("", if marker { 18.0 } else { 16.0 }, Color::WHITE),
                ));
                if marker {
                    line.insert(CaptionText);
                } else {
                    line.insert(Outcome);
                }
            });
    }
    commands
        .spawn((
            center(Node {
                top: px(16),
                ..default()
            }),
            bevy::picking::Pickable::IGNORE,
        ))
        .with_children(|p| {
            p.spawn((
                ChapterText,
                Node {
                    padding: UiRect::axes(px(14), px(6)),
                    border_radius: BorderRadius::all(px(8)),
                    display: Display::None,
                    ..default()
                },
                BackgroundColor(PANEL),
                label("", 15.0, MUTED),
            ));
        });
    commands.spawn((
        Tooltip,
        GlobalZIndex(10),
        bevy::picking::Pickable::IGNORE,
        Node {
            position_type: PositionType::Absolute,
            padding: UiRect::axes(px(8), px(4)),
            border_radius: BorderRadius::all(px(5)),
            display: Display::None,
            ..default()
        },
        BackgroundColor(PANEL),
        label("", 13.0, Color::WHITE),
    ));

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
        .spawn((
            Controls,
            bar(Node {
                left: px(16),
                ..default()
            }),
        ))
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
        Controls,
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
            set_text(&mut text, "--".into());
            color.set_if_neq(TextColor(MUTED));
        }
        return;
    };
    for (id, mut text, mut color) in &mut readings {
        let n = f.world.node(id.0);
        let Some(t) = &n.thermal else {
            let (state, shade) = if n.powered {
                ("READY", Color::WHITE)
            } else {
                ("OFF", MUTED)
            };
            set_text(&mut text, state.into());
            color.set_if_neq(TextColor(shade));
            continue;
        };
        let reading = match &f.fleet {
            Some(fleet) => fleet
                .report(id.0)
                .temp_c
                .filter(|_| fleet.report(id.0).online),
            None => t.reading_c().filter(|_| n.powered),
        };
        let value = reading.map_or("NO DATA".into(), |c| format!("{c:.1} C"));
        set_text(&mut text, value);
        color.set_if_neq(TextColor(temperature_color(reading)));
    }
    for (id, mut text, mut layout) in &mut details {
        let n = f.world.node(id.0);
        let list = match &f.fleet {
            Some(fleet) => reported(fleet.report(id.0), n),
            None => exceptions(id.0, n),
        };
        let value = (!list.is_empty()).then(|| list.join("\n"));
        show(&mut text, &mut layout, value);
    }
}

/// What the firmware and the network say.
fn reported(r: &Report, n: &sim_core::Node) -> Vec<String> {
    if !r.online {
        // The last value is never shown as the current one.
        let last = r.temp_c.map(|c| format!("last {c:.1} C"));
        let age = format!("{} s ago", r.age_ms / 1000);
        let heard = last.map(|l| format!("{l}, {age}"));
        return std::iter::once("offline".into()).chain(heard).collect();
    }
    let unknown = !n.mains && r.safe_s.is_none() && r.alarms.iter().any(|a| a == "mains_lost");
    let mut list: Vec<_> = r.alarms.iter().map(|a| a.replace('_', " ")).collect();
    if r.door_s > 0 {
        list.push(format!("open {} s", r.door_s));
    }
    if !n.mains
        && let Some(s) = r.safe_s.filter(|_| !r.alarms.is_empty())
    {
        list.push(format!("safe for {}", minutes(s)));
    }
    if r.transit == 0
        && let Some(left) = r.stable_left_s.filter(|s| *s > 0)
    {
        list.push(format!("stable to depart in {}", minutes(left)));
    }
    if unknown {
        list.push("safe time unknown".into());
    }
    list
}

fn exceptions(id: NodeId, n: &sim_core::Node) -> Vec<String> {
    let mut list = Vec::new();
    if n.door_open() {
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
        let value = match &live.0 {
            None => "Connecting".into(),
            Some(f) => {
                let s = f.world.time_ms / 1000;
                format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
            }
        };
        set_text(&mut text, value);
    }
}

fn speed(live: Res<Live>, mut labels: Query<&mut Text, With<SpeedLabel>>) {
    let Some(f) = &live.0 else { return };
    for mut text in &mut labels {
        set_text(&mut text, format!("{}x", f.speed));
    }
}

fn buttons(live: Res<Live>, mut query: Query<(&Op, &Hovered, &mut BackgroundColor)>) {
    for (op, hovered, mut color) in &mut query {
        let shade = match live.0.as_ref().map(|f| state(*op, f)) {
            Some((true, true)) => FAULT,
            Some((true, false)) => ACTIVE,
            _ if hovered.0 => HOVER,
            _ => IDLE,
        };
        color.set_if_neq(BackgroundColor(shade));
    }
}

pub fn minutes(s: u64) -> String {
    format!("{}:{:02}", s / 60, s % 60)
}

/// A write lays the text out again, so equal values are skipped.
pub fn set_text(text: &mut Mut<Text>, value: String) {
    if text.0 != value {
        text.0 = value;
    }
}

fn show(text: &mut Mut<Text>, node: &mut Mut<Node>, value: Option<String>) {
    let display = if value.is_some() {
        Display::Flex
    } else {
        Display::None
    };
    if node.display != display {
        node.display = display;
    }
    if let Some(value) = value {
        set_text(text, value);
    }
}

fn caption(
    live: Res<Live>,
    line: Single<(&mut Text, &mut Node), (With<CaptionText>, Without<ChapterText>)>,
    chapter: Single<(&mut Text, &mut Node), With<ChapterText>>,
) {
    let (mut text, mut node) = line.into_inner();
    let caption = live.0.as_ref().and_then(|f| f.caption.as_ref());
    show(&mut text, &mut node, caption.map(|c| c.text.clone()));
    let (mut text, mut node) = chapter.into_inner();
    let act = caption.map(|c| c.chapter.clone()).filter(|c| !c.is_empty());
    show(&mut text, &mut node, act);
}

/// Shown for this long.
const OUTCOME_MS: u64 = 5000;
const ANSWER_MS: u64 = 8000;

fn outcome(
    live: Res<Live>,
    mut sent: ResMut<Sent>,
    line: Single<(&mut Text, &mut Node, &mut TextColor), With<Outcome>>,
) {
    let (mut text, mut node, mut color) = line.into_inner();
    let Some(f) = &live.0 else { return };
    let now = f.world.time_ms;
    let latest = f.fleet.as_ref().and_then(|fleet| {
        let mut list = fleet.events.iter().rev();
        list.find(|e| matches!(e.kind.as_str(), "op" | "scan"))
    });
    if sent.0.is_some_and(|t| {
        now < t || now > t + ANSWER_MS || latest.is_some_and(|e| e.kind == "op" && e.time_ms > t)
    }) {
        sent.0 = None;
    }
    let recent = latest.filter(|e| now.saturating_sub(e.time_ms) < OUTCOME_MS);
    let shown = match (sent.0, recent) {
        (Some(_), _) => Some(("Processing".to_string(), false)),
        (None, Some(e)) => crate::stock::describe(e),
        _ => None,
    };
    color.set_if_neq(TextColor(match &shown {
        Some((_, true)) => WARNING,
        _ => Color::WHITE,
    }));
    show(&mut text, &mut node, shown.map(|(t, _)| t));
}

fn tooltip(
    hint: Res<Hint>,
    window: Single<&Window>,
    tip: Single<(&mut Text, &mut Node), With<Tooltip>>,
) {
    let (mut text, mut node) = tip.into_inner();
    if hint.0.is_some()
        && let Some(p) = window.cursor_position()
    {
        node.left = px(p.x + 16.0);
        node.top = px(p.y + 12.0);
    }
    show(&mut text, &mut node, hint.0.map(String::from));
}
