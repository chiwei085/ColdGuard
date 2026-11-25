//! Stock table on the terminal screen and the custody panel it opens.

use crate::{
    Live,
    hud::{IDLE, MUTED, PANEL, WARNING, label},
    link::Link,
    scene::{badge, scan},
};
use bevy::{
    camera::RenderTarget,
    prelude::*,
    render::render_resource::TextureFormat,
    ui_widgets::{Activate, Button},
};
use sim_core::{Action, Batch, Fleet, NodeId};

const SCREEN: UVec2 = UVec2::new(640, 360);
const GOOD: Color = Color::srgb(0.3, 0.85, 0.5);
const BAD: Color = Color::srgb(1.0, 0.35, 0.3);
const SHOWN_EVENTS: usize = 3;

pub fn plugin(app: &mut App) {
    app.init_resource::<PanelOpen>()
        .add_systems(Startup, build)
        .add_systems(Update, (toggle, refresh));
}

#[derive(Resource, Default)]
pub struct PanelOpen(pub bool);

/// Texture shown on the terminal.
#[derive(Resource)]
pub struct ScreenImage(pub Handle<Image>);

#[derive(Component)]
struct ScreenRoot;

#[derive(Component)]
struct PanelRoot;

fn build(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let image = images.add(Image::new_target_texture(
        SCREEN.x,
        SCREEN.y,
        TextureFormat::Bgra8UnormSrgb,
        None,
    ));
    let camera = commands
        .spawn((
            Camera2d,
            Camera {
                order: -1,
                clear_color: Color::srgb(0.03, 0.05, 0.07).into(),
                ..default()
            },
            RenderTarget::Image(image.clone().into()),
        ))
        .id();
    commands.insert_resource(ScreenImage(image));
    commands.spawn((
        ScreenRoot,
        UiTargetCamera(camera),
        column(Node {
            width: percent(100),
            height: percent(100),
            padding: UiRect::all(px(22)),
            row_gap: px(6),
            ..default()
        }),
    ));
    commands.spawn((
        PanelRoot,
        column(Node {
            position_type: PositionType::Absolute,
            right: px(16),
            top: px(16),
            width: px(430),
            padding: UiRect::all(px(14)),
            row_gap: px(8),
            border_radius: BorderRadius::all(px(10)),
            display: Display::None,
            ..default()
        }),
        BackgroundColor(PANEL),
    ));
}

fn column(node: Node) -> Node {
    Node {
        flex_direction: FlexDirection::Column,
        ..node
    }
}

fn toggle(input: Res<ButtonInput<KeyCode>>, mut open: ResMut<PanelOpen>) {
    if input.just_pressed(KeyCode::Tab) {
        open.0 = !open.0;
    }
}

/// Only what the two views show, so other telemetry does not rebuild them.
#[derive(Default, PartialEq)]
struct Shown {
    connected: bool,
    open: bool,
    batches: Vec<Batch>,
    person: String,
    transit: u64,
    receiver: bool,
    events: Vec<(String, bool)>,
}

impl Shown {
    fn new(fleet: Option<&Fleet>, open: bool) -> Self {
        let Some(f) = fleet else {
            return Self { open, ..default() };
        };
        let mut events: Vec<_> = f
            .events
            .iter()
            .rev()
            .filter_map(|e| describe(&e.kind, &e.body, e.node))
            .take(SHOWN_EVENTS)
            .collect();
        events.reverse();
        Self {
            connected: f.inventory.online,
            open,
            batches: f.batches.clone(),
            person: f.inventory.person.clone(),
            transit: f.carrier.transit,
            receiver: f.carrier.receiver,
            events,
        }
    }
}

/// One line for an operator, and whether it is bad news.
fn describe(kind: &str, body: &str, node: NodeId) -> Option<(String, bool)> {
    let b: serde_json::Value = serde_json::from_str(body).ok()?;
    let text = |key: &str| b[key].as_str().unwrap_or_default().to_string();
    let name = node.name();
    Some(match kind {
        "op" if b["ok"] == 1 => (format!("{} {} done", text("cmd"), text("batch")), false),
        "op" => (
            format!("{} {} refused: {}", text("cmd"), text("batch"), text("why")),
            true,
        ),
        "alarm" if b["on"] == 1 => (format!("{name}: {}", text("alarm").replace('_', " ")), true),
        "peer" if b["online"] == 0 => (format!("{} lost contact", text("peer")), true),
        "peer" => (format!("{} connected", text("peer")), false),
        _ => return None,
    })
}

fn refresh(
    mut commands: Commands,
    live: Res<Live>,
    open: Res<PanelOpen>,
    screen: Single<Entity, With<ScreenRoot>>,
    panel: Single<(Entity, &mut Node), With<PanelRoot>>,
    mut last: Local<Option<Shown>>,
) {
    let fleet = live.0.as_ref().and_then(|f| f.fleet.as_ref());
    let shown = Shown::new(fleet, open.0);
    if last.as_ref() == Some(&shown) {
        return;
    }
    let (panel, mut layout) = panel.into_inner();
    layout.display = if shown.open {
        Display::Flex
    } else {
        Display::None
    };
    commands
        .entity(*screen)
        .despawn_children()
        .with_children(|p| draw_screen(p, &shown));
    commands
        .entity(panel)
        .despawn_children()
        .with_children(|p| draw_panel(p, &shown));
    *last = Some(shown);
}

fn verdict(b: &Batch) -> (&'static str, Color) {
    match b.status.as_str() {
        "ok" => ("OK", GOOD),
        "dnu" => ("DO NOT USE", BAD),
        _ => ("DISCARDED", MUTED),
    }
}

fn row() -> Node {
    Node {
        justify_content: JustifyContent::SpaceBetween,
        align_items: AlignItems::Center,
        column_gap: px(8),
        ..default()
    }
}

fn draw_screen(p: &mut ChildSpawnerCommands, s: &Shown) {
    p.spawn((
        row(),
        children![
            label("VACCINE STOCK", 34.0, Color::WHITE),
            if s.connected {
                label("ONLINE", 26.0, GOOD)
            } else {
                label("OFFLINE", 26.0, BAD)
            },
        ],
    ));
    for b in &s.batches {
        let (text, color) = verdict(b);
        p.spawn((
            row(),
            children![
                label(&b.name, 28.0, Color::WHITE),
                label(&format!("{} {}", b.place, b.phase), 20.0, MUTED),
                label(text, 28.0, color),
            ],
        ));
    }
    for (text, bad) in s.events.iter().rev().take(1) {
        p.spawn(label(text, 24.0, if *bad { WARNING } else { MUTED }));
    }
}

fn draw_panel(p: &mut ChildSpawnerCommands, s: &Shown) {
    let badge_text = if s.person.is_empty() {
        "Scan a badge first".into()
    } else {
        format!("Badge {}", s.person)
    };
    p.spawn(row()).with_children(|p| {
        p.spawn(label(&badge_text, 14.0, MUTED));
        button(p, "Scan", vec![scan()]);
        if let Some(name) = next_name(&s.batches) {
            button(p, "Check in", vec![operate("checkin", &name, "")]);
        }
    });
    for b in &s.batches {
        let (text, color) = verdict(b);
        p.spawn(row()).with_children(|p| {
            p.spawn(label(&b.name, 15.0, Color::WHITE));
            p.spawn(label(text, 13.0, color));
            for (text, cmd, note) in choices(b) {
                button(p, text, vec![operate(cmd, &b.name, note)]);
            }
        });
    }
    let carrier = match s.transit {
        1 => "Carrier on the way",
        2 => "Carrier arrived",
        _ => "Carrier",
    };
    p.spawn(row()).with_children(|p| {
        p.spawn(label(carrier, 14.0, MUTED));
        // The person at the box presents the badge there.
        let commands: &[(&str, &str, &str)] = match (s.transit, s.receiver) {
            (1, _) => &[("Arrive", "arrive", "handed over")],
            (_, false) => &[("Receiver ready", "receiver", "receiver confirmed")],
            (_, true) => &[("Depart", "depart", "")],
        };
        for (text, cmd, note) in commands {
            let node = NodeId::Carrier;
            button(p, text, vec![badge(node), command(node, cmd, "", note)]);
        }
    });
    for (text, bad) in &s.events {
        p.spawn(label(text, 13.0, if *bad { WARNING } else { MUTED }));
    }
}

/// Offers only what may apply. The firmware decides.
fn choices(b: &Batch) -> &'static [(&'static str, &'static str, &'static str)] {
    match (b.status.as_str(), b.phase.as_str()) {
        ("dnu", _) => &[
            ("Release", "release", "maker confirmed stable"),
            ("Discard", "discard", "maker rejected"),
        ],
        ("ok", "pending") => &[("Accept", "accept", "transport log checked")],
        ("ok", "stored") if b.place == "fridge" => {
            &[("Check out", "checkout", ""), ("Load", "load", "")]
        }
        ("ok", "stored") => &[("Check out", "checkout", "")],
        _ => &[],
    }
}

/// First free demo name.
fn next_name(batches: &[Batch]) -> Option<String> {
    (1..=8)
        .map(|i| format!("VAX-{i:02}"))
        .find(|n| !batches.iter().any(|b| b.name == *n && b.phase != "out"))
}

fn operate(cmd: &str, batch: &str, note: &str) -> Action {
    command(NodeId::Inventory, cmd, batch, note)
}

fn command(node: NodeId, cmd: &str, batch: &str, note: &str) -> Action {
    Action::Operate {
        node,
        cmd: cmd.into(),
        batch: batch.into(),
        to: "fridge".into(),
        note: note.into(),
    }
}

fn button(p: &mut ChildSpawnerCommands, text: &str, actions: Vec<Action>) {
    p.spawn((
        Button,
        Node {
            padding: UiRect::axes(px(10), px(5)),
            border_radius: BorderRadius::all(px(6)),
            ..default()
        },
        BackgroundColor(IDLE),
        children![label(text, 13.0, Color::WHITE)],
    ))
    .observe(move |_: On<Activate>, link: Res<Link>| {
        for action in &actions {
            link.act(action.clone());
        }
    });
}
