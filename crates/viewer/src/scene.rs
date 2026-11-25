//! Clinic model, lighting and camera.

use crate::{Live, link::Link};
use bevy::{
    anti_alias::{
        contrast_adaptive_sharpening::ContrastAdaptiveSharpening, taa::TemporalAntiAliasing,
    },
    asset::RenderAssetUsages,
    camera::Hdr,
    gltf::GltfMaterialName,
    input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll},
    light::{CascadeShadowConfigBuilder, GeneratedEnvironmentMapLight, NotShadowCaster, RectLight},
    pbr::{ContactShadows, ScreenSpaceAmbientOcclusion, ScreenSpaceTransmission},
    post_process::bloom::Bloom,
    prelude::*,
    render::render_resource::{
        Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
    },
    world_serialization::WorldInstanceReady,
};
use sim_core::{Action, Node as SimNode, NodeId};

pub fn plugin(app: &mut App) {
    app.insert_resource(ClearColor(Color::BLACK))
        .insert_resource(GlobalAmbientLight::NONE)
        .add_systems(Startup, (model, lights, camera))
        .add_systems(Update, (hinges, lamps, interior, glow, fan, orbit))
        .add_observer(bind);
}

/// Rotates toward the physical door state.
#[derive(Component)]
struct Hinge {
    node: NodeId,
    axis: Vec3,
    open: f32,
    angle: f32,
}

#[derive(Component, Clone, Copy)]
struct Lamp {
    node: NodeId,
    kind: LampKind,
}

#[derive(Clone, Copy)]
enum LampKind {
    Temperature,
    Power,
    Network,
    Screen,
}

#[derive(Component)]
struct Interior(NodeId);

/// Light strip inside the fridge.
#[derive(Component, Clone)]
struct FridgeGlow;

#[derive(Component)]
struct Fan;

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

/// Demo staff badge at the inventory reader.
pub fn scan() -> Action {
    Action::Scan {
        node: NodeId::Inventory,
        uid: "04A1B2C3".into(),
    }
}

fn model(mut commands: Commands, assets: Res<AssetServer>) {
    commands.spawn(WorldAssetRoot(
        assets.load(GltfAssetLabel::Scene(0).from_asset("clinic.glb")),
    ));
}

/// Adds behavior to the model nodes by name.
fn bind(
    ready: On<WorldInstanceReady>,
    mut commands: Commands,
    names: Query<&Name>,
    children: Query<&Children>,
    surfaces: Query<(&GltfMaterialName, &MeshMaterial3d<StandardMaterial>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for entity in children.iter_descendants(ready.entity) {
        if let Ok((name, handle)) = surfaces.get(entity) {
            if name.0 == "glass" {
                // Blended surfaces read the normals of whatever is behind them.
                if let Some(mut glass) = materials.get_mut(&handle.0) {
                    glass.alpha_mode = AlphaMode::Opaque;
                    glass.base_color = Color::srgb(0.94, 0.97, 0.96);
                    glass.specular_transmission = 0.96;
                }
                commands.entity(entity).insert(NotShadowCaster);
            }
            continue;
        }
        let Ok(name) = names.get(entity) else {
            continue;
        };
        let hinge = |node, axis, open| Hinge {
            node,
            axis,
            open,
            angle: 0.0,
        };
        match name.as_str() {
            "fridge_door" => {
                commands
                    .entity(entity)
                    .insert(hinge(NodeId::Fridge, Vec3::Y, -1.6))
                    .observe(toggle(NodeId::Fridge));
            }
            "carrier_lid" => {
                commands
                    .entity(entity)
                    .insert(hinge(NodeId::Carrier, Vec3::X, -1.75))
                    .observe(toggle(NodeId::Carrier));
            }
            "carrier_body" => {
                commands.entity(entity).observe(toggle(NodeId::Carrier));
            }
            "fridge_fan" => {
                commands.entity(entity).insert(Fan);
            }
            "reader" => {
                commands
                    .entity(entity)
                    .observe(|_: On<PointerClick>, link: Res<Link>| link.act(scan()));
            }
            "sky" => {
                for part in children.iter_descendants(entity) {
                    commands.entity(part).insert(NotShadowCaster);
                }
            }
            "glow_fridge" => own_materials(
                entity,
                FridgeGlow,
                &mut commands,
                &children,
                &surfaces,
                &mut materials,
            ),
            other => {
                let Some(lamp) = lamp(other) else { continue };
                own_materials(
                    entity,
                    lamp,
                    &mut commands,
                    &children,
                    &surfaces,
                    &mut materials,
                );
            }
        }
    }
}

fn lamp(name: &str) -> Option<Lamp> {
    let (node, kind) = name.strip_prefix("lamp_")?.split_once('_')?;
    Some(Lamp {
        node: match node {
            "fridge" => NodeId::Fridge,
            "carrier" => NodeId::Carrier,
            "inventory" => NodeId::Inventory,
            _ => return None,
        },
        kind: match kind {
            "temperature" => LampKind::Temperature,
            "power" => LampKind::Power,
            "network" => LampKind::Network,
            "screen" => LampKind::Screen,
            _ => return None,
        },
    })
}

/// Gives each lit surface its own material, since the model shares them.
fn own_materials(
    root: Entity,
    tag: impl Component + Clone,
    commands: &mut Commands,
    children: &Query<&Children>,
    surfaces: &Query<(&GltfMaterialName, &MeshMaterial3d<StandardMaterial>)>,
    materials: &mut Assets<StandardMaterial>,
) {
    for part in children.iter_descendants(root) {
        let Ok((_, handle)) = surfaces.get(part) else {
            continue;
        };
        if let Some(copy) = materials.get(&handle.0).cloned() {
            commands
                .entity(part)
                .insert((MeshMaterial3d(materials.add(copy)), tag.clone()));
        }
    }
}

/// Click to open or close the door.
fn toggle(id: NodeId) -> impl Fn(On<PointerClick>, Res<Live>, Res<Link>) {
    move |_, live, link| {
        if let Some(t) = live.0.as_ref().and_then(|f| node(f, id).thermal.as_ref()) {
            link.act(Action::Door {
                node: id,
                open: !t.door_open,
            });
        }
    }
}

/// The window sets the main direction. Panels and interior lamps fill.
fn lights(mut commands: Commands) {
    commands.spawn((
        DirectionalLight {
            illuminance: 21_000.0,
            color: Color::srgb(1.0, 0.94, 0.84),
            shadow_maps_enabled: true,
            contact_shadows_enabled: true,
            soft_shadow_size: Some(6.0),
            ..default()
        },
        CascadeShadowConfigBuilder {
            maximum_distance: 11.0,
            first_cascade_far_bound: 4.5,
            ..default()
        }
        .build(),
        Transform::from_xyz(-4.6, 3.1, 3.6).looking_at(vec3(0.2, 0.75, 0.3), Vec3::Y),
    ));
    // A spot, since a RectLight casts no shadows.
    commands.spawn((
        SpotLight {
            color: Color::srgb(0.84, 0.92, 1.0),
            intensity: 620_000.0,
            range: 12.0,
            radius: 0.5,
            inner_angle: 0.5,
            outer_angle: 1.12,
            shadow_maps_enabled: true,
            soft_shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(-2.0, 1.75, 2.3).looking_at(vec3(0.3, 0.9, 0.4), Vec3::Y),
    ));
    for x in [-0.9, 1.9] {
        commands.spawn((
            RectLight {
                color: Color::srgb(1.0, 0.97, 0.92),
                intensity: 48_000.0,
                width: 1.2,
                height: 0.6,
                range: 9.0,
            },
            Transform::from_xyz(x, 2.76, 2.0).looking_to(Vec3::NEG_Y, Vec3::Z),
        ));
    }
    commands.spawn((
        PointLight {
            intensity: 0.0,
            range: 2.2,
            color: Color::srgb(0.93, 0.97, 1.0),
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(-0.87, 1.66, 0.62),
        Interior(NodeId::Fridge),
    ));
}

/// Room seen from its center, for reflections.
fn surroundings() -> Image {
    const N: u32 = 128;
    const LOW: Vec3 = vec3(-2.2, 0.0, 0.0);
    const HIGH: Vec3 = vec3(3.4, 2.8, 5.6);
    let eye = vec3(0.2, 1.3, 1.9);
    let mut data = Vec::with_capacity((N * N * 6 * 16) as usize);
    for face in 0..6 {
        for y in 0..N {
            for x in 0..N {
                let u = (x as f32 + 0.5) / N as f32 * 2.0 - 1.0;
                let v = 1.0 - (y as f32 + 0.5) / N as f32 * 2.0;
                let d = match face {
                    0 => vec3(1.0, v, -u),
                    1 => vec3(-1.0, v, u),
                    2 => vec3(u, 1.0, -v),
                    3 => vec3(u, -1.0, v),
                    4 => vec3(u, v, 1.0),
                    _ => vec3(-u, v, -1.0),
                }
                .normalize();
                // Cubemaps are left handed.
                let d = vec3(d.x, d.y, -d.z);
                let far = Vec3::select(d.cmpgt(Vec3::ZERO), HIGH, LOW);
                let t = ((far - eye) / d).min_element();
                let hit = eye + d * t;
                for c in surface(hit, LOW, HIGH).to_array() {
                    data.extend(c.to_le_bytes());
                }
                data.extend(1.0_f32.to_le_bytes());
            }
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 6,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba32Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..default()
    });
    image
}

/// Radiance of a room surface.
fn surface(p: Vec3, low: Vec3, high: Vec3) -> Vec3 {
    let lit = |x: f32| (p.x - x).abs() < 0.6 && (p.z - 2.0).abs() < 0.3;
    if p.y > high.y - 0.01 {
        return if lit(-0.9) || lit(1.9) {
            vec3(9.0, 8.8, 8.4)
        } else {
            Vec3::splat(0.6)
        };
    }
    if p.y < low.y + 0.01 {
        return vec3(0.28, 0.28, 0.27);
    }
    if p.x < low.x + 0.01 && (1.0..2.25).contains(&p.y) && (1.05..2.75).contains(&p.z) {
        let mullion = (p.z - 1.9).abs() < 0.04;
        return if mullion {
            Vec3::splat(0.5)
        } else {
            vec3(9.0, 10.5, 12.0)
        };
    }
    let wall = if p.y < 1.08 {
        vec3(0.3, 0.35, 0.33)
    } else {
        vec3(0.52, 0.52, 0.5)
    };
    // Farther from the window is darker.
    wall * (1.1 - 0.11 * (p.x - low.x))
}

fn camera(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    commands.spawn((
        Camera3d::default(),
        Projection::from(PerspectiveProjection {
            fov: 0.6,
            near: 0.05,
            ..default()
        }),
        Hdr,
        Msaa::Off,
        TemporalAntiAliasing::default(),
        ContrastAdaptiveSharpening {
            enabled: true,
            sharpening_strength: 0.4,
            ..default()
        },
        ScreenSpaceAmbientOcclusion {
            radius: 0.35,
            ..default()
        },
        ScreenSpaceTransmission::default(),
        ContactShadows {
            length: 0.06,
            thickness: 0.03,
            ..default()
        },
        GeneratedEnvironmentMapLight {
            environment_map: images.add(surroundings()),
            intensity: 430.0,
            ..default()
        },
        Bloom {
            intensity: 0.06,
            ..Bloom::NATURAL
        },
        Orbit {
            focus: vec3(-0.15, 1.06, 0.3),
            yaw: 0.2,
            pitch: 0.06,
            distance: 4.75,
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
                LampKind::Screen => Color::WHITE,
                LampKind::Network => on(n.network_up, Color::srgb(0.2, 0.9, 0.4)),
            }
        });
        if let Some(mut material) = materials.get_mut(&handle.0) {
            material.base_color = color;
            material.emissive = color.to_linear()
                * if matches!(lamp.kind, LampKind::Screen) {
                    1.0
                } else {
                    4.0
                };
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
            (true, false) => 32_000.0,
            (true, true) => 42_000.0,
        };
    }
}

fn glow(
    live: Res<Live>,
    strips: Query<&MeshMaterial3d<StandardMaterial>, With<FridgeGlow>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let on = live.0.as_ref().is_some_and(|f| f.world.fridge.mains);
    for handle in &strips {
        if let Some(mut material) = materials.get_mut(&handle.0) {
            material.emissive = LinearRgba::WHITE * if on { 6.0 } else { 0.0 };
        }
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
        o.yaw = o.yaw.clamp(-0.4, 0.6);
        o.pitch = o.pitch.clamp(0.0, 0.33);
        o.distance = (o.distance * (1.0 - scroll.delta.y * 0.08)).clamp(1.2, 5.0);
        let direction = Quat::from_euler(EulerRot::YXZ, o.yaw, -o.pitch, 0.0) * Vec3::Z;
        *transform = Transform::from_translation(o.focus + direction * o.distance)
            .looking_at(o.focus, Vec3::Y);
    }
}
