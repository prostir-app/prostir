//! What the menu shows ([`UiState`]), made from the model only when the model changed
//! (step 7, 4.1): in a frame where nothing happened the menu's state is neither made
//! nor compared again.

#![forbid(unsafe_code)]

use crate::app::Model;
use crate::session::Attempt;
use crate::ui::{self, MonitorEntry, UiState};

/// The menu's state as last made, and the model's [`Model::rev`] it was made at.
#[derive(Debug, Default)]
pub struct View {
    made: Option<(u64, UiState)>,
}

impl View {
    pub fn new() -> Self {
        Self::default()
    }

    /// What the menu shows now: made again only if the model changed since.
    pub fn state(&mut self, model: &Model) -> &UiState {
        let rev = model.rev();
        if self.made.as_ref().is_none_or(|(at, _)| *at != rev) {
            self.made = None;
        }
        &self.made.get_or_insert_with(|| (rev, build(model))).1
    }

    /// The model's change the state was made at (`None`: not made yet).
    pub fn rev(&self) -> Option<u64> {
        self.made.as_ref().map(|(rev, _)| *rev)
    }
}

/// What the menu shows, from `m`.
pub fn build(m: &Model) -> UiState {
    let world = m.world();
    let session = m.session();
    let menu = m.menu();
    let headset = m.headset();
    let now = m.now();
    let shown_for = |at: std::time::Instant, d| now.saturating_duration_since(at) < d;
    let spots = world.map_spots();
    let search = session.search();
    let mut windows: Vec<MonitorEntry> = world
        .windows()
        .map(|(id, (width, height))| MonitorEntry {
            id,
            label: m.label_of(id).unwrap_or_default(),
            width,
            height,
            place: ui::Place::Unknown,
            seen: None,
        })
        .collect();
    windows.sort_unstable_by_key(|w| w.id);
    UiState {
        host: session.host().map(str::to_owned),
        found: search
            .found
            .iter()
            .map(|h| (h.name.clone(), h.addr.ip().to_string()))
            .collect(),
        ip_input: menu.ip_input.clone(),
        pairing: match &search.attempt {
            Some(Attempt::Pairing { view, .. }) => Some(view.clone()),
            _ => None,
        },
        pair_offer: match &search.attempt {
            Some(Attempt::PairOffer { label, why, .. }) => Some((label.clone(), *why)),
            _ => None,
        },
        unreachable: search.unreachable.clone(),
        pc: m.pc().encoding.clone(),
        limits: m.pc().limits.clone(),
        can_move_mouse: m.can_move_mouse(),
        monitors: world
            .monitors()
            .iter()
            .enumerate()
            .map(|(i, &(id, width, height))| MonitorEntry {
                id,
                label: m.label_of(id).unwrap_or_else(|| (i + 1).to_string()),
                width,
                height,
                place: world.place_of(id, m.view_eye()),
                seen: spots.get(&id).copied(),
            })
            .collect(),
        windows,
        primary: world.primary(),
        cinema_screen: world.desk().cinema(),
        selected: menu.selected.filter(|id| world.streams(*id)),
        mirror_seen: world
            .mirror()
            .filter(|id| world.streams(*id))
            .and_then(|id| spots.get(&id).copied()),
        mirror_wanted: headset.mirror_wanted,
        audio_wanted: m.pc().sound.as_ref().is_none_or(|s| s.to_headset),
        mic_wanted: headset.mic_wanted,
        mic_denied: headset.mic_wanted && headset.mic_denied,
        mirror_name: m.physical_name(),
        settings: *m.settings(),
        room_available: headset.can.room,
        panorama_available: headset.can.panorama,
        custom_available: headset.can.custom_panorama,
        curve_available: world.curve_supported(),
        keyboard_available: headset.can.keyboard_window,
        error: menu
            .error
            .as_ref()
            .filter(|(_, at)| shown_for(*at, crate::app::ERROR_SHOWN_FOR))
            .map(|(t, _)| t.clone()),
        confirm_remove: menu
            .confirm_remove
            .filter(|(_, at)| shown_for(*at, crate::app::CONFIRM_FOR))
            .map(|(id, _)| id),
        confirm_save: menu
            .confirm_save
            .filter(|(_, at)| shown_for(*at, crate::app::CONFIRM_FOR))
            .map(|(i, _)| i),
        paired: session.paired().iter().map(|p| p.name.clone()).collect(),
        battery: headset.battery,
        clock: m.clock().clock.clone(),
        date: m.clock().date.clone(),
        confirm_forget: menu
            .confirm_forget
            .filter(|(_, at)| shown_for(*at, crate::app::CONFIRM_FOR))
            .map(|(i, _)| i),
        tab: menu.tab,
        texts: m.texts().clone(),
        languages: m.lang().known().clone(),
        language_page: menu.language_page,
        intro: !m.settings().intro_seen,
        update: search.update.clone(),
        update_from: headset.can.update_from,
        saved_profiles: m.profiles().each_ref().map(Option::is_some),
        active_profile: menu.active_profile,
        note: menu.notes.shown().cloned(),
    }
}
