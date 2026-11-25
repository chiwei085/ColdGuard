//! Clinic room, devices and camera.

use crate::{Live, link::Link};
use bevy::{
    camera::Hdr,
    input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll},
    post_process::bloom::Bloom,
    prelude::*,
};
use sim_core::{Action, Node as SimNode, NodeId};
use std::f32::consts::{FRAC_PI_2, PI};

pub fn plugin(app: &mut App) {
    app.insert_resource(ClearColor(Color::srgb(0.16, 0.22, 0.26)))
        .insert_resource(GlobalAmbientLight {
            color: Color::srgb(0.85, 0.92, 1.0),
            brightness: 140.0,
            ..default()
        })
        .add_systems(Startup, (room, fridge, carrier, terminal, staff, camera))
        .add_systems(Update, (hinges, lamps, interior, fan, walk, orbit));
}

/// Rotates toward the physical door state.
#[derive(Component)]
struct Hinge {
    node: NodeId,
    axis: Vec3,
    open: f32,
    angle: f32,
}

#[derive(Component)]
struct Lamp {
    node: NodeId,
    kind: LampKind,
}

#[derive(Clone, Copy)]
enum LampKind {
    Temperature,
    Power,
    Network,
}

#[derive(Component)]
struct Interior(NodeId);

#[derive(Component)]
struct Fan;

#[derive(Component)]
struct Staff {
    scans: u64,
    /// Seconds left at the terminal.
    visit: f32,
}

#[derive(Component)]
struct Orbit {
    focus: Vec3,
    yaw: f32,
    pitch: f32,
    distance: f32,
}

pub fn node(frame: &sim_core::Frame, id: NodeId) -> &SimNode {
    match id {
        NodeId::Fridge => &frame.world.fridge,
        NodeId::Carrier => &frame.world.carrier,
        NodeId::Inventory => &frame.world.inventory,
    }
}

/// Display color for the sensor reading. Alarms belong to the firmware.
pub fn temperature_color(reading: Option<f64>) -> Color {
    match reading {
        None => Color::srgb(0.45, 0.45, 0.5),
        Some(t) if t <= 0.0 => Color::srgb(0.65, 0.3, 1.0),
        Some(t) if t < 2.0 => Color::srgb(0.2, 0.5, 1.0),
        Some(t) if t > 8.0 => Color::srgb(1.0, 0.2, 0.15),
        Some(_) => Color::srgb(0.1, 0.9, 0.45),
    }
}

/// Spawning helper for boxes.
struct Kit<'a, 'w, 's> {
    commands: &'a mut Commands<'w, 's>,
    meshes: &'a mut Assets<Mesh>,
    materials: &'a mut Assets<StandardMaterial>,
}

impl Kit<'_, '_, '_> {
    fn paint(&mut self, r: f32, g: f32, b: f32, roughness: f32) -> Handle<StandardMaterial> {
        self.materials.add(StandardMaterial {
            base_color: Color::srgb(r, g, b),
            perceptual_roughness: roughness,
            ..default()
        })
    }

    fn glow(&mut self) -> Handle<StandardMaterial> {
        self.materials.add(StandardMaterial {
            base_color: Color::BLACK,
            ..default()
        })
    }

    fn block(&mut self, size: Vec3, at: Vec3, material: &Handle<StandardMaterial>) -> Entity {
        self.part(Cuboid::from_size(size), at, material)
    }

    fn part(
        &mut self,
        mesh: impl Into<Mesh>,
        at: Vec3,
        material: &Handle<StandardMaterial>,
    ) -> Entity {
        self.commands
            .spawn((
                Mesh3d(self.meshes.add(mesh)),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(at),
            ))
            .id()
    }

    fn lamp(&mut self, node: NodeId, kind: LampKind, size: Vec3, at: Vec3) -> Entity {
        let material = self.glow();
        let lamp = self.block(size, at, &material);
        self.commands.entity(lamp).insert(Lamp { node, kind });
        lamp
    }

    fn lamps(&mut self, node: NodeId, at: Vec3) {
        for (i, kind) in [LampKind::Power, LampKind::Network].into_iter().enumerate() {
            self.lamp(node, kind, Vec3::splat(0.05), at + Vec3::X * 0.1 * i as f32);
        }
    }

    /// Click to open or close the door.
    fn clickable(&mut self, entity: Entity, toggle: NodeId) {
        self.commands.entity(entity).observe(
            move |_: On<PointerClick>, live: Res<Live>, link: Res<Link>| {
                if let Some(t) = live
                    .0
                    .as_ref()
                    .and_then(|f| node(f, toggle).thermal.as_ref())
                {
                    link.act(Action::Door {
                        node: toggle,
                        open: !t.door_open,
                    });
                }
            },
        );
    }
}

fn room(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut k = Kit {
        commands: &mut commands,
        meshes: &mut meshes,
        materials: &mut materials,
    };
    let grout = k.paint(0.5, 0.54, 0.57, 0.9);
    k.block(vec3(12.0, 0.1, 9.0), vec3(0.0, -0.06, 0.0), &grout);
    let tile = k.paint(0.72, 0.76, 0.78, 0.35);
    for x in 0..12 {
        for z in 0..9 {
            let at = vec3(x as f32 - 5.5, 0.0, z as f32 - 4.0);
            k.block(vec3(0.97, 0.02, 0.97), at, &tile);
        }
    }
    let wall = k.paint(0.8, 0.85, 0.85, 0.95);
    let trim = k.paint(0.18, 0.48, 0.5, 0.6);
    k.block(vec3(12.0, 4.0, 0.1), vec3(0.0, 2.0, -4.5), &wall);
    k.block(vec3(0.1, 4.0, 9.0), vec3(-6.0, 2.0, 0.0), &wall);
    k.block(vec3(12.0, 0.35, 0.04), vec3(0.0, 1.2, -4.43), &trim);
    k.block(vec3(0.04, 0.35, 9.0), vec3(-5.93, 1.2, 0.0), &trim);

    // Window on the left wall.
    let sky = k.materials.add(StandardMaterial {
        base_color: Color::srgb(0.5, 0.75, 0.95),
        emissive: LinearRgba::rgb(0.8, 1.3, 1.8),
        ..default()
    });
    let frame = k.paint(0.95, 0.95, 0.95, 0.5);
    k.block(vec3(0.06, 1.5, 2.4), vec3(-5.93, 2.5, 1.0), &frame);
    for z in [0.43, 1.57] {
        k.block(vec3(0.08, 1.3, 1.04), vec3(-5.92, 2.5, z), &sky);
    }

    // Counter under the carrier.
    let cabinet = k.paint(0.88, 0.89, 0.9, 0.6);
    let top = k.paint(0.25, 0.28, 0.32, 0.25);
    k.block(vec3(3.2, 0.9, 1.0), vec3(0.4, 0.45, -3.7), &cabinet);
    k.block(vec3(3.3, 0.06, 1.1), vec3(0.4, 0.93, -3.68), &top);
    let gap = k.paint(0.6, 0.63, 0.65, 0.6);
    for x in [-0.4, 0.4, 1.2] {
        k.block(vec3(0.02, 0.8, 0.02), vec3(x, 0.45, -3.2), &gap);
    }

    // Medical cross.
    let cross = k.materials.add(StandardMaterial {
        base_color: Color::srgb(0.1, 0.7, 0.55),
        emissive: LinearRgba::rgb(0.1, 1.2, 0.8),
        ..default()
    });
    k.block(vec3(0.75, 0.24, 0.06), vec3(0.4, 2.9, -4.4), &cross);
    k.block(vec3(0.24, 0.75, 0.06), vec3(0.4, 2.9, -4.4), &cross);

    // Plant.
    let pot = k.paint(0.75, 0.42, 0.3, 0.8);
    let leaf = k.paint(0.15, 0.5, 0.22, 0.8);
    k.part(Cylinder::new(0.25, 0.5), vec3(-5.3, 0.25, -0.9), &pot);
    for (x, y, z, r) in [
        (0.0, 0.9, 0.0, 0.4),
        (0.2, 1.15, 0.1, 0.28),
        (-0.15, 1.3, -0.1, 0.25),
    ] {
        k.part(Sphere::new(r), vec3(-5.3 + x, y, -0.9 + z), &leaf);
    }

    k.commands.spawn((
        DirectionalLight {
            illuminance: 7_500.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(4.0, 8.0, 6.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    for x in [-3.0, 3.0] {
        k.commands.spawn((
            PointLight {
                intensity: 220_000.0,
                range: 14.0,
                color: Color::srgb(1.0, 0.97, 0.9),
                ..default()
            },
            Transform::from_xyz(x, 3.8, 0.5),
        ));
    }
}

fn fridge(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut k = Kit {
        commands: &mut commands,
        meshes: &mut meshes,
        materials: &mut materials,
    };
    let origin = vec3(-3.2, 0.0, -3.7);
    let shell = k.paint(0.9, 0.92, 0.94, 0.3);
    let inner = k.paint(0.82, 0.9, 0.96, 0.6);
    let steel = k.materials.add(StandardMaterial {
        base_color: Color::srgb(0.7, 0.72, 0.75),
        metallic: 0.9,
        perceptual_roughness: 0.3,
        ..default()
    });
    for (size, at) in [
        (vec3(1.6, 2.3, 0.1), vec3(0.0, 1.15, -0.5)),
        (vec3(0.1, 2.3, 1.0), vec3(-0.75, 1.15, 0.0)),
        (vec3(0.1, 2.3, 1.0), vec3(0.75, 1.15, 0.0)),
        (vec3(1.6, 0.35, 1.0), vec3(0.0, 0.175, 0.0)),
        (vec3(1.6, 0.35, 1.0), vec3(0.0, 2.3, 0.0)),
    ] {
        k.block(size, origin + at, &shell);
    }
    k.block(
        vec3(1.4, 1.8, 0.02),
        origin + vec3(0.0, 1.25, -0.43),
        &inner,
    );

    // Shelves with vaccine boxes.
    let boxes = [
        k.paint(0.95, 0.75, 0.25, 0.7),
        k.paint(0.3, 0.6, 0.9, 0.7),
        k.paint(0.9, 0.45, 0.5, 0.7),
    ];
    for (row, y) in [0.45_f32, 0.95, 1.45].into_iter().enumerate() {
        k.block(vec3(1.4, 0.03, 0.8), origin + vec3(0.0, y, 0.0), &steel);
        for col in 0..5 {
            let size = vec3(0.2, 0.16 + 0.04 * ((row + col) % 3) as f32, 0.3);
            let at = vec3(-0.52 + 0.26 * col as f32, y + 0.015 + size.y / 2.0, 0.1);
            k.block(size, origin + at, &boxes[(row * 2 + col) % 3]);
        }
    }
    // Glycol probe bottle.
    let glycol = k.materials.add(StandardMaterial {
        base_color: Color::srgba(0.3, 0.8, 0.9, 0.7),
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    k.part(
        Cylinder::new(0.06, 0.25),
        origin + vec3(0.6, 1.09, 0.28),
        &glycol,
    );

    let light = k
        .commands
        .spawn((
            PointLight {
                intensity: 0.0,
                range: 4.0,
                color: Color::srgb(0.8, 0.93, 1.0),
                ..default()
            },
            Transform::from_translation(origin + vec3(0.0, 1.9, 0.3)),
        ))
        .id();
    k.commands.entity(light).insert(Interior(NodeId::Fridge));

    // Door hinged on the left edge.
    let glass = k.materials.add(StandardMaterial {
        base_color: Color::srgba(0.65, 0.85, 0.95, 0.22),
        alpha_mode: AlphaMode::Blend,
        perceptual_roughness: 0.08,
        ..default()
    });
    let hinge = k
        .commands
        .spawn((
            Hinge {
                node: NodeId::Fridge,
                axis: Vec3::Y,
                open: -1.9,
                angle: 0.0,
            },
            Transform::from_translation(origin + vec3(-0.75, 1.23, 0.55)),
            Visibility::default(),
        ))
        .id();
    for (size, at, material) in [
        (vec3(1.3, 1.6, 0.03), vec3(0.75, 0.0, 0.0), &glass),
        (vec3(1.5, 0.12, 0.08), vec3(0.75, 0.83, 0.0), &shell),
        (vec3(1.5, 0.12, 0.08), vec3(0.75, -0.83, 0.0), &shell),
        (vec3(0.12, 1.75, 0.08), vec3(0.06, 0.0, 0.0), &shell),
        (vec3(0.12, 1.75, 0.08), vec3(1.44, 0.0, 0.0), &shell),
        (vec3(0.05, 0.6, 0.06), vec3(1.44, 0.0, 0.08), &steel),
    ] {
        let part = k.block(size, at, material);
        k.clickable(part, NodeId::Fridge);
        k.commands.entity(hinge).add_child(part);
    }

    let strip = vec3(1.1, 0.07, 0.03);
    k.lamp(
        NodeId::Fridge,
        LampKind::Temperature,
        strip,
        origin + vec3(0.0, 2.36, 0.51),
    );
    k.lamps(NodeId::Fridge, origin + vec3(0.5, 2.22, 0.51));

    // Compressor fan, spinning while cooling.
    let blades = k
        .commands
        .spawn((
            Fan,
            Transform::from_translation(origin + vec3(0.0, 0.18, 0.53)),
            Visibility::default(),
        ))
        .id();
    for i in 0..4 {
        let blade = k.block(vec3(0.24, 0.05, 0.02), Vec3::ZERO, &steel);
        k.commands
            .entity(blade)
            .insert(Transform::from_rotation(Quat::from_rotation_z(
                i as f32 * PI / 4.0,
            )));
        k.commands.entity(blades).add_child(blade);
    }
}

fn carrier(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut k = Kit {
        commands: &mut commands,
        meshes: &mut meshes,
        materials: &mut materials,
    };
    let origin = vec3(0.4, 0.96, -3.6);
    let shell = k.paint(0.12, 0.4, 0.65, 0.5);
    let lid = k.paint(0.92, 0.94, 0.96, 0.4);
    let pack = k.paint(0.5, 0.8, 0.95, 0.3);
    for (size, at) in [
        (vec3(1.1, 0.06, 0.7), vec3(0.0, 0.03, 0.0)),
        (vec3(1.1, 0.55, 0.07), vec3(0.0, 0.28, 0.32)),
        (vec3(1.1, 0.55, 0.07), vec3(0.0, 0.28, -0.32)),
        (vec3(0.07, 0.55, 0.7), vec3(0.52, 0.28, 0.0)),
        (vec3(0.07, 0.55, 0.7), vec3(-0.52, 0.28, 0.0)),
    ] {
        let part = k.block(size, origin + at, &shell);
        k.clickable(part, NodeId::Carrier);
    }
    for x in [-0.4, 0.4] {
        k.block(vec3(0.1, 0.4, 0.5), origin + vec3(x, 0.26, 0.0), &pack);
    }
    let vial = k.paint(0.95, 0.75, 0.25, 0.7);
    for x in [-0.2, 0.0, 0.2] {
        k.block(vec3(0.16, 0.25, 0.3), origin + vec3(x, 0.19, 0.0), &vial);
    }
    let hinge = k
        .commands
        .spawn((
            Hinge {
                node: NodeId::Carrier,
                axis: Vec3::X,
                open: -1.7,
                angle: 0.0,
            },
            Transform::from_translation(origin + vec3(0.0, 0.56, -0.35)),
            Visibility::default(),
        ))
        .id();
    for (size, at) in [
        (vec3(1.14, 0.1, 0.74), vec3(0.0, 0.05, 0.36)),
        (vec3(0.4, 0.05, 0.06), vec3(0.0, 0.14, 0.36)),
    ] {
        let part = k.block(size, at, &lid);
        k.clickable(part, NodeId::Carrier);
        k.commands.entity(hinge).add_child(part);
    }
    let strip = vec3(0.6, 0.06, 0.02);
    k.lamp(
        NodeId::Carrier,
        LampKind::Temperature,
        strip,
        origin + vec3(-0.1, 0.4, 0.365),
    );
    k.lamps(NodeId::Carrier, origin + vec3(0.32, 0.4, 0.365));
}

fn terminal(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut k = Kit {
        commands: &mut commands,
        meshes: &mut meshes,
        materials: &mut materials,
    };
    let origin = vec3(3.9, 0.0, -2.9);
    let wood = k.paint(0.62, 0.46, 0.3, 0.6);
    let dark = k.paint(0.1, 0.11, 0.13, 0.4);
    let reader = k.paint(0.15, 0.45, 0.75, 0.4);
    k.block(vec3(2.2, 0.08, 1.1), origin + vec3(0.0, 0.8, 0.0), &wood);
    for x in [-1.0, 1.0] {
        k.block(vec3(0.08, 0.8, 1.0), origin + vec3(x, 0.4, 0.0), &wood);
    }
    k.block(
        vec3(0.3, 0.04, 0.25),
        origin + vec3(-0.2, 0.86, -0.2),
        &dark,
    );
    k.block(
        vec3(0.06, 0.3, 0.06),
        origin + vec3(-0.2, 1.0, -0.22),
        &dark,
    );
    k.block(vec3(1.1, 0.7, 0.06), origin + vec3(-0.2, 1.42, -0.2), &dark);
    let screen = vec3(1.0, 0.6, 0.02);
    k.lamp(
        NodeId::Inventory,
        LampKind::Power,
        screen,
        origin + vec3(-0.2, 1.42, -0.16),
    );
    k.block(
        vec3(0.8, 0.03, 0.28),
        origin + vec3(-0.2, 0.86, 0.25),
        &dark,
    );
    let pad = k.block(
        vec3(0.32, 0.06, 0.32),
        origin + vec3(0.7, 0.87, 0.2),
        &reader,
    );
    k.commands
        .entity(pad)
        .observe(|_: On<PointerClick>, link: Res<Link>| link.act(scan()));
    k.lamp(
        NodeId::Inventory,
        LampKind::Network,
        vec3(0.12, 0.02, 0.12),
        origin + vec3(0.7, 0.91, 0.2),
    );
}

/// Demo staff badge at the inventory reader.
pub fn scan() -> Action {
    Action::Scan {
        node: NodeId::Inventory,
        uid: "04A1B2C3".into(),
    }
}

fn staff(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut k = Kit {
        commands: &mut commands,
        meshes: &mut meshes,
        materials: &mut materials,
    };
    let coat = k.paint(0.95, 0.97, 0.98, 0.8);
    let skin = k.paint(0.85, 0.65, 0.5, 0.7);
    let hair = k.paint(0.15, 0.1, 0.08, 0.9);
    let person = k
        .commands
        .spawn((
            Staff {
                scans: 0,
                visit: 0.0,
            },
            Transform::from_translation(REST),
            Visibility::default(),
        ))
        .id();
    let parts = [
        k.part(Capsule3d::new(0.26, 0.75), vec3(0.0, 0.75, 0.0), &coat),
        k.part(Sphere::new(0.2), vec3(0.0, 1.55, 0.0), &skin),
        k.part(Sphere::new(0.21), vec3(0.0, 1.63, 0.03), &hair),
        k.part(Capsule3d::new(0.08, 0.5), vec3(-0.32, 0.9, 0.0), &coat),
        k.part(Capsule3d::new(0.08, 0.5), vec3(0.32, 0.9, 0.0), &coat),
    ];
    k.commands.entity(person).add_children(&parts);
}

fn camera(mut commands: Commands) {
    commands.spawn((
        Camera3d::default(),
        Hdr,
        Bloom::NATURAL,
        Orbit {
            focus: vec3(-0.3, 0.95, -2.8),
            yaw: 0.16,
            pitch: 0.32,
            distance: 8.6,
        },
        Transform::default(),
    ));
}

fn hinges(live: Res<Live>, time: Res<Time>, mut hinges: Query<(&mut Hinge, &mut Transform)>) {
    let Some(frame) = &live.0 else { return };
    for (mut hinge, mut transform) in &mut hinges {
        let open = node(frame, hinge.node)
            .thermal
            .as_ref()
            .is_some_and(|t| t.door_open);
        let goal = if open { hinge.open } else { 0.0 };
        let blend = 1.0 - (-6.0 * time.delta_secs()).exp();
        hinge.angle += (goal - hinge.angle) * blend;
        transform.rotation = Quat::from_axis_angle(hinge.axis, hinge.angle);
    }
}

fn lamps(
    live: Res<Live>,
    lamps: Query<(&Lamp, &MeshMaterial3d<StandardMaterial>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (lamp, handle) in &lamps {
        let color = live.0.as_ref().map_or(Color::BLACK, |frame| {
            let n = node(frame, lamp.node);
            let on = |yes, color| {
                if yes {
                    color
                } else {
                    Color::srgb(0.9, 0.1, 0.1)
                }
            };
            match lamp.kind {
                _ if !n.powered => Color::BLACK,
                LampKind::Temperature => {
                    temperature_color(n.thermal.as_ref().and_then(|t| t.reading_c()))
                }
                LampKind::Power => on(
                    n.mains || n.battery_level > 0.2,
                    Color::srgb(0.1, 0.6, 0.75),
                ),
                LampKind::Network => on(n.network_up, Color::srgb(0.2, 0.9, 0.4)),
            }
        });
        if let Some(mut material) = materials.get_mut(&handle.0) {
            material.base_color = color;
            material.emissive = color.to_linear() * 2.0;
        }
    }
}

fn interior(live: Res<Live>, mut lights: Query<(&Interior, &mut PointLight)>) {
    let Some(frame) = &live.0 else { return };
    for (interior, mut light) in &mut lights {
        let n = node(frame, interior.0);
        let open = n.thermal.as_ref().is_some_and(|t| t.door_open);
        light.intensity = match (n.mains, open) {
            (false, _) => 0.0,
            (true, false) => 25_000.0,
            (true, true) => 80_000.0,
        };
    }
}

fn fan(live: Res<Live>, time: Res<Time>, mut fans: Query<&mut Transform, With<Fan>>) {
    let cooling = live
        .0
        .as_ref()
        .filter(|f| !f.paused)
        .and_then(|f| f.world.fridge.thermal.as_ref())
        .is_some_and(|t| t.cooling_w > 0.0);
    if cooling {
        for mut transform in &mut fans {
            transform.rotate_z(time.delta_secs() * 9.0);
        }
    }
}

const REST: Vec3 = vec3(2.5, 0.0, -2.6);

/// Staff walk to whatever is being operated.
fn walk(live: Res<Live>, time: Res<Time>, mut staff: Query<(&mut Staff, &mut Transform)>) {
    let Some(frame) = &live.0 else { return };
    let w = &frame.world;
    let open = |n: &SimNode| n.thermal.as_ref().is_some_and(|t| t.door_open);
    for (mut staff, mut transform) in &mut staff {
        // A reset lowers the count.
        if w.inventory.scan_count > staff.scans {
            staff.visit = 4.0;
        }
        staff.scans = w.inventory.scan_count;
        staff.visit = (staff.visit - time.delta_secs()).max(0.0);
        let goal = if staff.visit > 0.0 {
            vec3(4.1, 0.0, -1.8)
        } else if open(&w.fridge) {
            vec3(-1.7, 0.0, -2.5)
        } else if open(&w.carrier) {
            vec3(1.4, 0.0, -2.5)
        } else {
            REST
        };
        let offset = goal - transform.translation;
        let distance = offset.length();
        if distance > 0.02 {
            let step = (2.5 * time.delta_secs()).min(distance);
            transform.translation += offset / distance * step;
            let bob = (time.elapsed_secs() * 10.0).sin() * 0.04;
            transform.rotation = Quat::from_rotation_z(bob);
        } else {
            transform.rotation = Quat::IDENTITY;
        }
    }
}

fn orbit(
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut cameras: Query<(&mut Orbit, &mut Transform)>,
) {
    for (mut o, mut transform) in &mut cameras {
        if buttons.pressed(MouseButton::Right) {
            o.yaw -= motion.delta.x * 0.005;
            o.pitch += motion.delta.y * 0.005;
        }
        // Keep the camera inside the open side of the room.
        o.yaw = o.yaw.clamp(-0.5, FRAC_PI_2 - 0.1);
        o.pitch = o.pitch.clamp(0.08, 1.3);
        o.distance = (o.distance * (1.0 - scroll.delta.y * 0.08)).clamp(3.0, 14.0);
        let direction = Quat::from_euler(EulerRot::YXZ, o.yaw, -o.pitch, 0.0) * Vec3::Z;
        *transform = Transform::from_translation(o.focus + direction * o.distance)
            .looking_at(o.focus, Vec3::Y);
    }
}
