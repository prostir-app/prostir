//! Cinema: one monitor big and further away, the others not shown.

use std::collections::HashMap;

use glam::Vec3;

use super::{Key, Placement, facing};

/// The monitor shown in cinema mode: `pick` if it is among `monitors`, else the
/// largest landscape one (films are wide), else the first.
pub fn cinema_choice<K: Key>(monitors: &[(K, u32, u32)], pick: Option<K>) -> Option<K> {
    pick.and_then(|k| monitors.iter().find(|m| m.0 == k))
        .or_else(|| {
            monitors
                .iter()
                .filter(|m| m.1 > m.2)
                .max_by_key(|m| u64::from(m.1) * u64::from(m.2))
        })
        .or_else(|| monitors.first())
        .map(|m| m.0)
}

/// Cinema: one monitor big and further away, straight ahead at eye level; the others
/// are not shown.
pub fn cinema<K: Key>(monitors: &[(K, u32, u32)], pick: Option<K>) -> HashMap<K, Placement> {
    const DISTANCE: f32 = 3.0;
    /// About 56° wide, like a good seat in a cinema.
    const WIDTH: f32 = 3.2;
    let id = cinema_choice(monitors, pick);
    let mut out = HashMap::new();
    if let Some(id) = id {
        let position = Vec3::new(0.0, -0.05, -DISTANCE);
        out.insert(
            id,
            Placement {
                position,
                rotation: facing(position, Vec3::new(0.0, -0.05, 0.0)),
                width: WIDTH,
            },
        );
    }
    out
}
