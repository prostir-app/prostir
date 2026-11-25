//! What the headset is called, as the system says it: shown and logged (the PC's list
//! of headsets, pairing), and looked up in one table, the pixels per degree of known
//! displays (`platform::pixels`, for how large monitors are made); never used to decide
//! what the headset can do, which is [`crate::caps::Caps`].

#![forbid(unsafe_code)]

use prostir_proto::state::{MAX_SHORT, clip};

/// The headset's name from its maker and model as the system gives them
/// (`Build.MANUFACTURER`, `Build.MODEL`): "Maker Model 3", or the model alone when it
/// already names its maker ("Maker 4" from "Maker"). Short enough for the protocol;
/// empty when the system said nothing.
pub fn model_name(manufacturer: &str, model: &str) -> String {
    let (manufacturer, model) = (manufacturer.trim(), model.trim());
    let named = model
        .get(..manufacturer.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(manufacturer));
    let name = if manufacturer.is_empty() || named {
        model.to_owned()
    } else if model.is_empty() {
        manufacturer.to_owned()
    } else {
        format!("{manufacturer} {model}")
    };
    // Shown on the PC: no control characters from the system's strings.
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let mut name = clip(&name, MAX_SHORT);
    name.truncate(name.trim_end().len());
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maker_and_model() {
        assert_eq!(model_name("Maker", "Model 3"), "Maker Model 3");
        assert_eq!(model_name("Maker", "Maker 4"), "Maker 4");
        assert_eq!(model_name("MAKER", "maker View 2"), "maker View 2");
        assert_eq!(model_name(" maker ", " "), "maker");
        assert_eq!(model_name("", "Model 3"), "Model 3");
        assert_eq!(model_name("", ""), "");
        assert_eq!(model_name("Ma\u{1b}ker", "Model\n3\t"), "Maker Model3");
    }

    #[test]
    fn never_longer_than_the_protocol_takes() {
        let long = model_name("A maker with a long name", "and a model with a longer one");
        assert!(long.len() <= MAX_SHORT);
        assert!(long.starts_with("A maker with a long name and"));
        // Cut on a character, not inside one.
        let wide = model_name("Виробник", "Модель з довгою назвою");
        assert!(wide.len() <= MAX_SHORT);
        assert!(wide.starts_with("Виробник Модель"));
    }
}
