//! How many pixels a degree is at the middle of each headset's display the app knows,
//! by its model as the system names it (`plans/clarity-2026-11.md`, part 9): monitors
//! are made one pixel to one of the headset's by it (see `crate::clarity`). A table of
//! models, like the controllers' table, rather than the size the runtime suggests for
//! the eyes' buffers, which is not the display's. Builds on any system.

#![forbid(unsafe_code)]

/// Pixels per degree at the middle of the display, by model: the maker's numbers for
/// the Quest 3 (2064x2208 an eye, 25), about for the others.
pub const MODELS: [(&str, f32); 4] = [
    ("quest 3", 25.0),
    ("quest 3s", 20.0),
    ("quest 2", 20.0),
    ("quest pro", 22.0),
];

/// The pixels per degree of the headset `model` (as `crate::device::model_name` names
/// it: maker and model); `None` for a model not in the table, which keeps the standard
/// size.
pub fn per_degree(model: &str) -> Option<f32> {
    let model = model.trim().to_lowercase();
    let at = model.find("quest")?;
    let name: String = model[at..].split_whitespace().collect::<Vec<_>>().join(" ");
    MODELS.iter().find(|(m, _)| *m == name).map(|(_, ppd)| *ppd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The models of the table by the names the system gives them; any other keeps
    /// the standard size.
    #[test]
    fn the_models_of_the_table() {
        assert_eq!(per_degree("Oculus Quest 3"), Some(25.0));
        assert_eq!(per_degree("Meta Quest 3S"), Some(20.0));
        assert_eq!(per_degree("Oculus Quest 2"), Some(20.0));
        assert_eq!(per_degree("Quest  Pro"), Some(22.0));
        assert_eq!(per_degree("Oculus Quest"), None);
        assert_eq!(per_degree("Quest 4"), None);
        assert_eq!(per_degree("Other 4"), None);
        assert_eq!(per_degree(""), None);
    }
}
