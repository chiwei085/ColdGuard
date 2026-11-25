//! Batch boxes. Each one stands where the firmware says it is.

use crate::{
    Live,
    hud::Hint,
    scene::node,
    stock::{Panel, PanelOpen},
};
use bevy::{platform::collections::HashMap, prelude::*};
use sim_core::{Batch, Frame, NodeId};

const MOVE_S: f32 = 1.8;
const ARC_M: f32 = 0.45;
/// In front of the fridge and the counter.
const FRONT_Z: f32 = 1.05;

pub fn plugin(app: &mut App) {
    app.init_resource::<Spots>()
        .add_systems(Update, (arrange, tags));
}

pub fn name(index: usize) -> String {
    format!("VAX-{:02}", index + 1)
}

/// Positions by place and index.
#[derive(Resource, Default)]
pub struct Spots(HashMap<(String, usize), Entity>);

/// Parses names such as spot_fridge_1 or batch_1.
fn numbered<'a>(text: &'a str, prefix: &str) -> Option<(&'a str, usize)> {
    let (rest, number) = text.strip_prefix(prefix)?.rsplit_once('_')?;
    Some((rest, number.parse::<usize>().ok()?.checked_sub(1)?))
}

#[derive(Component)]
struct Carton {
    index: usize,
    place: Option<&'static str>,
    from: Option<&'static str>,
    /// Progress of the current move.
    moved: f32,
}

#[derive(Component)]
struct UnusableTag(usize);

/// Returns false for nodes that belong to someone else.
pub fn bind(entity: Entity, text: &str, commands: &mut Commands, spots: &mut Spots) -> bool {
    if let Some((place, index)) = numbered(text, "spot_") {
        spots.0.insert((place.into(), index), entity);
    } else if let Some((_, index)) = numbered(text, "dnu").filter(|(rest, _)| rest.is_empty()) {
        commands
            .entity(entity)
            .insert((UnusableTag(index), Visibility::Hidden));
    } else if let Some((_, index)) = numbered(text, "batch").filter(|(rest, _)| rest.is_empty()) {
        commands
            .entity(entity)
            .insert((
                Carton {
                    index,
                    place: None,
                    from: None,
                    moved: 1.0,
                },
                Visibility::Hidden,
            ))
            .observe(move |_: On<PointerClick>, mut open: ResMut<PanelOpen>| {
                open.0 = Panel::Batch(name(index));
            })
            .observe(|_: On<PointerOver>, mut hint: ResMut<Hint>| {
                hint.0 = Some("Batch  [click]");
            })
            .observe(|_: On<PointerOut>, mut hint: ResMut<Hint>| hint.0 = None);
    } else {
        return false;
    }
    true
}

fn record(f: &Frame, index: usize) -> Option<&Batch> {
    let name = name(index);
    let batches = &f.fleet.as_ref()?.batches;
    batches.iter().find(|b| b.name == name)
}

/// The one place of a batch, or none when it does not exist.
fn place(f: &Frame, index: usize) -> Option<&'static str> {
    if index >= usize::from(f.world.delivered) {
        return None;
    }
    let Some(b) = record(f, index) else {
        return Some("delivery");
    };
    Some(match (b.phase.as_str(), b.place.as_str()) {
        ("out", _) => "out",
        (_, "carrier") => "carrier",
        (_, "quarantine") => "quarantine",
        (_, "receiver") => "receiver",
        (_, "discard") => "discard",
        _ => "fridge",
    })
}

/// Where a box goes first so that it clears the furniture.
fn exit(place: &str, p: Vec3) -> Vec3 {
    match place {
        "fridge" | "quarantine" => p.with_z(FRONT_Z),
        "carrier" => p + Vec3::Y * 0.5,
        _ => (p + Vec3::Y * 0.3).with_z(FRONT_Z),
    }
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Pull out, carry over, put down.
fn route(a: (&str, Vec3), b: (&str, Vec3), t: f32) -> Vec3 {
    let (out, over) = (exit(a.0, a.1), exit(b.0, b.1));
    match t {
        t if t < 0.25 => a.1.lerp(out, smooth(t * 4.0)),
        t if t < 0.75 => {
            let s = smooth((t - 0.25) * 2.0);
            out.lerp(over, s) + Vec3::Y * ARC_M * (s * std::f32::consts::PI).sin()
        }
        t => over.lerp(b.1, smooth(t * 4.0 - 3.0)),
    }
}

fn arrange(
    live: Res<Live>,
    time: Res<Time>,
    spots: Res<Spots>,
    places: Query<&GlobalTransform, Without<Carton>>,
    mut cartons: Query<(&mut Carton, &mut Transform, &mut Visibility)>,
    mut last_ms: Local<u64>,
) {
    let Some(f) = &live.0 else { return };
    // A reset clears unfinished moves.
    let restarted = f.world.time_ms < *last_ms;
    *last_ms = f.world.time_ms;
    let at = |place: &str, index: usize| {
        let spot = spots.0.get(&(place.to_string(), index))?;
        places.get(*spot).ok().map(GlobalTransform::translation)
    };
    for (mut c, mut transform, mut visibility) in &mut cartons {
        let goal = place(f, c.index);
        if goal != c.place {
            c.from = c.place;
            c.place = goal;
            c.moved = if c.from.is_some() { 0.0 } else { 1.0 };
        }
        if restarted {
            c.moved = 1.0;
        } else if !f.paused {
            c.moved = (c.moved + time.delta_secs() / MOVE_S).min(1.0);
        }
        let end = c.place.and_then(|p| Some((p, at(p, c.index)?)));
        let Some(end) = end else {
            *visibility = Visibility::Hidden;
            continue;
        };
        *visibility = Visibility::Inherited;
        let start = c.from.and_then(|p| Some((p, at(p, c.index)?)));
        transform.translation = match start {
            Some(start) if c.moved < 1.0 => route(start, end, c.moved),
            _ => end.1,
        };
    }
}

fn tags(live: Res<Live>, mut tags: Query<(&UnusableTag, &mut Visibility)>) {
    let Some(f) = &live.0 else { return };
    for (tag, mut visibility) in &mut tags {
        let locked = record(f, tag.0).is_some_and(|b| b.status != "ok");
        *visibility = if locked {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
}

/// Whether a door or lid is shut.
pub fn closed(f: &Frame, id: NodeId) -> bool {
    node(f, id).thermal.as_ref().is_some_and(|t| !t.door_open)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::{Config, Fleet, World};

    fn frame(batches: &[(&str, &str, &str)]) -> Frame {
        let batches = batches.iter().map(|(name, place, phase)| Batch {
            name: name.to_string(),
            place: place.to_string(),
            phase: phase.to_string(),
            status: "ok".into(),
        });
        Frame {
            paused: false,
            finished: false,
            speed: 1.0,
            caption: None,
            world: World::new(Config::default()).unwrap(),
            fleet: Some(Fleet {
                batches: batches.collect(),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn boxes_follow_the_records() {
        let f = frame(&[
            ("VAX-01", "carrier", "transit"),
            ("VAX-02", "fridge", "out"),
        ]);
        let places: Vec<_> = (0..4).map(|i| place(&f, i)).collect();
        assert_eq!(
            places,
            [Some("carrier"), Some("out"), Some("delivery"), None]
        );
    }

    #[test]
    fn names_parse() {
        assert_eq!(numbered("spot_fridge_3", "spot_"), Some(("fridge", 2)));
        assert_eq!(numbered("batch_1", "batch"), Some(("", 0)));
        assert_eq!(numbered("spot_fridge_0", "spot_"), None);
    }

    #[test]
    fn route_starts_and_ends_at_the_spots() {
        let (a, b) = (Vec3::new(-1.0, 1.0, 0.5), Vec3::new(0.5, 0.9, 0.2));
        assert_eq!(route(("fridge", a), ("out", b), 0.0), a);
        assert_eq!(route(("fridge", a), ("out", b), 1.0), b);
    }
}
