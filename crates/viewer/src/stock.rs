//! Stock table on the terminal screen and the custody panel it opens.

use crate::{
    Live, batches,
    hud::{IDLE, MUTED, PANEL, Sent, WARNING, label, minutes},
    link::Link,
    scene::{badge, scan},
};
use bevy::{
    camera::RenderTarget,
    input_focus::{InputFocus, tab_navigation::TabIndex},
    prelude::*,
    render::render_resource::TextureFormat,
    text::{EditableText, EditableTextFilter},
    ui_widgets::{Activate, Button, TextInput},
};
use sim_core::{Action, Batch, Entry, Frame, NodeId, batch_tag};

const SCREEN: UVec2 = UVec2::new(640, 360);
const GOOD: Color = Color::srgb(0.3, 0.85, 0.5);
const BAD: Color = Color::srgb(1.0, 0.35, 0.3);
const SHOWN_RECORDS: usize = 12;

pub fn plugin(app: &mut App) {
    app.init_resource::<PanelOpen>()
        .add_systems(Startup, build)
        .init_resource::<Doors>()
        .add_systems(Update, (toggle.run_if(not(typing)), refresh, close_doors));
}

#[derive(Clone, Default, PartialEq)]
pub enum Panel {
    #[default]
    Closed,
    All,
    Batch(String),
}

impl Panel {
    pub fn toggle(&mut self) {
        *self = match self {
            Self::Closed => Self::All,
            _ => Self::Closed,
        };
    }
}

#[derive(Resource, Default)]
pub struct PanelOpen(pub Panel);

/// Doors opened for a move, and when they close again.
#[derive(Resource, Default)]
struct Doors(Vec<(NodeId, u64)>);

const DOOR_MS: u64 = 5000;

/// Texture shown on the terminal.
#[derive(Resource)]
pub struct ScreenImage(pub Handle<Image>);

#[derive(Component)]
struct ScreenRoot;

#[derive(Component)]
struct PanelRoot;

/// The part of the panel rebuilt on each change. The basis field stays.
#[derive(Component)]
struct PanelContent;

/// Basis typed for a verdict or a handover.
#[derive(Component)]
struct Basis;

/// Commands whose note is the typed basis.
const NEEDS_BASIS: [&str; 3] = ["release", "discard", "accept"];

/// Image drawn by its own UI camera.
pub fn target(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    size: UVec2,
    order: isize,
) -> (Handle<Image>, Entity) {
    let image = images.add(Image::new_target_texture(
        size.x,
        size.y,
        TextureFormat::Bgra8UnormSrgb,
        None,
    ));
    let camera = commands
        .spawn((
            Camera2d,
            Camera {
                order,
                clear_color: Color::srgb(0.03, 0.05, 0.07).into(),
                ..default()
            },
            RenderTarget::Image(image.clone().into()),
        ))
        .id();
    (image, camera)
}

fn build(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let (image, camera) = target(&mut commands, &mut images, SCREEN, -1);
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
        children![
            (
                PanelContent,
                column(Node {
                    row_gap: px(8),
                    ..default()
                })
            ),
            (
                row(),
                children![
                    label("Basis", 13.0, MUTED),
                    (
                        Basis,
                        TextInput,
                        TabIndex(0),
                        EditableText {
                            // The firmware refuses a longer note.
                            max_characters: Some(47),
                            ..default()
                        },
                        // Typing stops what the firmware would refuse.
                        EditableTextFilter::new(|c| {
                            (c.is_ascii_graphic() || c == ' ') && c != '"' && c != '\\'
                        }),
                        TextFont {
                            font_size: FontSize::Px(13.0),
                            ..default()
                        },
                        Node {
                            flex_grow: 1.0,
                            padding: UiRect::axes(px(8), px(4)),
                            border_radius: BorderRadius::all(px(6)),
                            ..default()
                        },
                        BackgroundColor(IDLE),
                    ),
                ],
            ),
        ],
    ));
}

/// Run condition for keyboard shortcuts, so typing a basis triggers none.
pub fn typing(focus: Res<InputFocus>, inputs: Query<(), With<TextInput>>) -> bool {
    focus.get().is_some_and(|e| inputs.contains(e))
}

fn column(node: Node) -> Node {
    Node {
        flex_direction: FlexDirection::Column,
        ..node
    }
}

fn toggle(input: Res<ButtonInput<KeyCode>>, mut open: ResMut<PanelOpen>) {
    if input.just_pressed(KeyCode::Tab) {
        open.0.toggle();
    }
}

/// Only what the two views show, so other telemetry does not rebuild them.
#[derive(Default, PartialEq)]
struct Shown {
    connected: bool,
    open: Panel,
    delivered: u8,
    at_receiver: bool,
    batches: Vec<Batch>,
    person: String,
    transit: u64,
    receiver: bool,
    carrier_online: bool,
    stable_left_s: Option<u64>,
    unmet: Vec<String>,
    backlog: u64,
    event: Option<(String, bool)>,
    records: Vec<(String, bool)>,
}

impl Shown {
    fn new(frame: Option<&Frame>, open: Panel) -> Self {
        let Some((frame, f)) = frame.and_then(|f| Some((f, f.fleet.as_ref()?))) else {
            return Self { open, ..default() };
        };
        let last = |count, list: &mut dyn Iterator<Item = (String, bool)>| {
            let mut list: Vec<_> = list.take(count).collect();
            list.reverse();
            list
        };
        let entries = || f.events.iter().rev();
        let mut told = entries()
            .filter(|e| matches!(e.kind.as_str(), "op" | "alarm" | "peer"))
            .filter_map(describe);
        let stamped = |e: &Entry| {
            let (text, bad) = describe(e)?;
            let s = e.time_ms / 1000;
            Some((format!("{:02}:{:02}  {text}", s / 60, s % 60), bad))
        };
        let mut all = entries().filter_map(stamped);
        // The evidence for a verdict.
        let mut evidence = entries()
            .filter(|e| e.kind == "excursion" || about(e, &open))
            .filter_map(stamped);
        let records = match open {
            Panel::Closed => vec![],
            Panel::All => last(SHOWN_RECORDS, &mut all),
            Panel::Batch(_) => last(SHOWN_RECORDS / 2, &mut evidence),
        };
        Self {
            connected: f.inventory.online,
            delivered: frame.world.delivered,
            at_receiver: frame.world.travel >= 1.0,
            batches: f.batches.clone(),
            person: f.inventory.person.clone(),
            transit: f.carrier.transit,
            receiver: f.carrier.receiver,
            carrier_online: f.carrier.online,
            stable_left_s: f.carrier.stable_left_s,
            unmet: f.carrier.unmet.clone(),
            backlog: f.carrier.backlog,
            event: told.next(),
            records,
            open,
        }
    }
}

/// Whether a record names the batch shown.
fn about(e: &Entry, open: &Panel) -> bool {
    matches!(open, Panel::Batch(name) if e.body.contains(&format!("\"{name}\"")))
}

/// One line for an operator, and whether it is bad news.
pub fn describe(e: &Entry) -> Option<(String, bool)> {
    let b: serde_json::Value = serde_json::from_str(&e.body).ok()?;
    let text = |key: &str| b[key].as_str().unwrap_or_default().to_string();
    let name = e.node.name();
    let subject = [text("cmd"), text("batch"), text("box")]
        .into_iter()
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let by = match text("person") {
        p if p.is_empty() => p,
        p => format!(" by {p}"),
    };
    Some(match e.kind.as_str() {
        "op" if b["ok"] == 1 && !text("note").is_empty() => {
            (format!("{subject} done{by}: {}", text("note")), false)
        }
        "op" if b["ok"] == 1 => (format!("{subject} done{by}"), false),
        "op" => (format!("{subject} refused: {}", text("why")), true),
        "alarm" => {
            let on = b["on"] == 1;
            let end = if on { "" } else { " cleared" };
            (
                format!("{name}: {}{end}", text("alarm").replace('_', " ")),
                on,
            )
        }
        "excursion" if b["on"] == 1 => (format!("{name}: out of range at {} C", b["temp_c"]), true),
        "excursion" => (
            format!(
                "{name}: {} to {} C for {} s",
                b["min_c"], b["max_c"], b["secs"]
            ),
            true,
        ),
        "batch" if b["status"] == "dnu" => (format!("{} locked", text("batch")), true),
        "batch" => (format!("{} {}", text("batch"), text("phase")), false),
        "scan" if b["staff"] == 1 => (format!("badge {} at {name}", text("uid")), false),
        "scan" if b["staff"] == 0 => (format!("batch tag {} at {name}", text("uid")), false),
        "scan" => (format!("unreadable tag at {name}"), true),
        "peer" if b["online"] == 0 => (format!("{} lost contact", text("peer")), true),
        "peer" => (format!("{} connected", text("peer")), false),
        "samples" => {
            let t: Vec<f64> = b["centi_c"]
                .as_array()?
                .iter()
                .filter_map(|v| Some(v.as_f64()? / 100.0))
                .collect();
            let (low, high) = t
                .iter()
                .fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
            let range = if t.is_empty() {
                String::new()
            } else {
                format!(", {low:.2} to {high:.2} C")
            };
            (
                format!(
                    "{name}: {} s from {} s before excursion{range}",
                    t.len(),
                    -b["from_s"].as_i64().unwrap_or(0)
                ),
                true,
            )
        }
        _ => return None,
    })
}

fn refresh(
    mut commands: Commands,
    live: Res<Live>,
    open: Res<PanelOpen>,
    screen: Single<Entity, With<ScreenRoot>>,
    mut panel: Single<&mut Node, With<PanelRoot>>,
    content: Single<Entity, With<PanelContent>>,
    mut last: Local<Option<Shown>>,
) {
    if !live.is_changed() && !open.is_changed() && last.is_some() {
        return;
    }
    let shown = Shown::new(live.0.as_ref(), open.0.clone());
    if last.as_ref() == Some(&shown) {
        return;
    }
    panel.display = if shown.open != Panel::Closed {
        Display::Flex
    } else {
        Display::None
    };
    commands
        .entity(*screen)
        .despawn_children()
        .with_children(|p| draw_screen(p, &shown));
    commands
        .entity(*content)
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
    if let Some((text, bad)) = &s.event {
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
        button(p, "Badge", vec![scan()]);
        close_button(p);
    });
    for i in 0..usize::from(s.delivered) {
        let name = batches::name(i);
        if matches!(&s.open, Panel::Batch(only) if *only != name) {
            continue;
        }
        let batch = s.batches.iter().find(|b| b.name == name);
        let (text, color) = batch.map_or(("DELIVERED", MUTED), verdict);
        p.spawn(row()).with_children(|p| {
            p.spawn(label(&name, 15.0, Color::WHITE));
            p.spawn(label(text, 13.0, color));
            if batch.is_none() {
                button(p, "Check in", operate("checkin", &name));
            }
            for (text, cmd) in batch.map_or(&[][..], choices) {
                button(p, text, operate(cmd, &name));
            }
        });
    }
    if s.open == Panel::All {
        let carrier = match (s.transit, s.at_receiver) {
            (1, false) => "BOX-1 on the way",
            (1, _) => "BOX-1 at the receiver",
            (2, _) => "BOX-1 arrived",
            _ => "BOX-1",
        };
        p.spawn(row()).with_children(|p| {
            p.spawn(label(carrier, 14.0, MUTED));
            // The person at the box presents the badge there.
            let commands: &[(&str, &str, &str)] = match (s.transit, s.receiver) {
                (1, _) if !s.at_receiver => &[],
                (1, _) => &[("Arrive", "arrive", "handed over")],
                (2, _) if s.batches.iter().all(|b| b.phase != "pending") => {
                    &[("Return", "return", "")]
                }
                (2, _) => &[],
                (_, false) => &[("Receiver ready", "receiver", "receiver confirmed")],
                (_, true) => &[("Depart", "depart", "")],
            };
            for (text, cmd, note) in commands {
                let node = NodeId::Carrier;
                button(p, text, vec![badge(node), command(node, cmd, "", note)]);
            }
        });
        carrier_status(p, s);
    }
    for (text, bad) in &s.records {
        p.spawn(label(text, 12.0, if *bad { WARNING } else { MUTED }));
    }
}

/// Departure conditions before leaving, upload progress after.
fn carrier_status(p: &mut ChildSpawnerCommands, s: &Shown) {
    if !s.carrier_online {
        let text = format!("BOX-1 offline, last known {} records to upload", s.backlog);
        p.spawn(label(&text, 12.0, WARNING));
        return;
    }
    if s.transit != 0 {
        let (text, color) = match s.backlog {
            0 => ("All transport records on file".into(), GOOD),
            n => (format!("Uploading transport records, {n} left"), WARNING),
        };
        p.spawn(label(&text, 12.0, color));
        return;
    }
    let stable = match s.stable_left_s {
        Some(0) => "Temperature stable 2-8 C".into(),
        Some(left) => format!("Temperature stable 2-8 C, {} to go", minutes(left)),
        None => "Temperature stable 2-8 C, unknown".into(),
    };
    for (key, text) in [
        ("receiver", "Receiver confirmed".into()),
        ("stable", stable),
        ("battery", "Battery sufficient".into()),
        ("safe_time", "Holds for packing, travel and margin".into()),
        ("lid", "Lid closed".into()),
    ] {
        let met = !s.unmet.iter().any(|u| u == key);
        let (mark, color) = if met { ("OK ", GOOD) } else { ("NO ", BAD) };
        p.spawn(label(&format!("{mark} {text}"), 12.0, color));
    }
}

fn chip(text: &str) -> impl Bundle + use<> {
    (
        Button,
        Node {
            padding: UiRect::axes(px(10), px(5)),
            border_radius: BorderRadius::all(px(6)),
            ..default()
        },
        BackgroundColor(IDLE),
        children![label(text, 13.0, Color::WHITE)],
    )
}

fn close_button(p: &mut ChildSpawnerCommands) {
    p.spawn(chip("Close"))
        .observe(|_: On<Activate>, mut open: ResMut<PanelOpen>| open.0 = Panel::Closed);
}

/// Offers only what may apply. The firmware decides.
fn choices(b: &Batch) -> &'static [(&'static str, &'static str)] {
    match (b.status.as_str(), b.phase.as_str()) {
        ("dnu", "stored") if b.place == "fridge" => &[
            ("Quarantine", "quarantine"),
            ("Release", "release"),
            ("Discard", "discard"),
        ],
        ("dnu", _) => &[("Release", "release"), ("Discard", "discard")],
        ("ok", "pending") => &[("Accept", "accept")],
        ("ok", "stored") if b.place == "fridge" => &[("Check out", "checkout"), ("Load", "load")],
        ("ok", "out") => &[("Check in", "checkin")],
        ("ok", "stored") => &[("Check out", "checkout")],
        _ => &[],
    }
}

/// The batch tag goes past the reader before the command.
fn operate(cmd: &str, batch: &str) -> Vec<Action> {
    let node = NodeId::Inventory;
    let tag = Action::scan(node, batch_tag(batch));
    vec![tag, command(node, cmd, batch, "")]
}

/// Units a command reaches into.
fn doors(cmd: &str) -> &'static [NodeId] {
    match cmd {
        "checkin" | "checkout" | "quarantine" | "release" | "discard" => &[NodeId::Fridge],
        "load" => &[NodeId::Fridge, NodeId::Carrier],
        "accept" => &[NodeId::Carrier],
        _ => &[],
    }
}

fn close_doors(live: Res<Live>, link: Res<Link>, mut doors: ResMut<Doors>) {
    let Some(f) = &live.0 else { return };
    doors.0.retain(|(node, at)| {
        let wait = f.world.time_ms < *at && f.world.time_ms + DOOR_MS >= *at;
        if !wait {
            link.act(Action::Door {
                node: *node,
                open: false,
            });
        }
        wait
    });
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
    p.spawn(chip(text)).observe(
        move |_: On<Activate>,
              live: Res<Live>,
              link: Res<Link>,
              mut sent: ResMut<Sent>,
              mut opened: ResMut<Doors>,
              mut basis: Single<&mut EditableText, With<Basis>>| {
            let Some(f) = &live.0 else { return };
            let now = f.world.time_ms;
            for action in &actions {
                let mut action = action.clone();
                if let Action::Operate { cmd, note, .. } = &mut action
                    && NEEDS_BASIS.contains(&cmd.as_str())
                {
                    *note = basis.value().to_string().trim().into();
                    basis.clear();
                }
                link.act(action.clone());
                let Action::Operate { cmd, .. } = &action else {
                    continue;
                };
                sent.0 = Some(now);
                for id in doors(cmd) {
                    let held = opened.0.iter_mut().find(|(n, _)| n == id);
                    if let Some(held) = held {
                        held.1 = now + DOOR_MS;
                    } else if !f.world.node(*id).door_open() {
                        link.act(Action::Door {
                            node: *id,
                            open: true,
                        });
                        opened.0.push((*id, now + DOOR_MS));
                    }
                }
            }
        },
    );
}
