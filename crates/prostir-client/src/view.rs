//! What the menu shows ([`UiState`]), made from the model only when the model changed
//! (step 7, 4.1): in a frame where nothing happened the menu's state is neither made
//! nor compared again.

#![forbid(unsafe_code)]

use crate::app::Model;
use crate::caps::Offer;
use crate::ids::DisplayKey;
use crate::physical;
use crate::session::Attempt;
use crate::ui::{self, MonitorEntry, UiState};
use crate::video::{self, Health};

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

/// A display rate as the menu writes it, in whole hertz.
fn hz(rate: f32) -> u16 {
    rate.round().clamp(0.0, f32::from(u16::MAX)) as u16
}

/// What the menu shows, from `m`.
pub fn build(m: &Model) -> UiState {
    let world = m.world();
    let session = m.session();
    let menu = m.menu();
    let headset = m.headset();
    let caps = &headset.caps;
    let now = m.now();
    let shown_for = |at: std::time::Instant, d| now.saturating_duration_since(at) < d;
    let map = world.map_spots();
    let spot = |id| map.as_ref().and_then(|d| d.spot(id));
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
            scale: 100,
            picture: ui::Picture::Shows,
        })
        .collect();
    windows.sort_unstable_by_key(|w| w.id);
    let videos = m.videos();
    let picture = |id| match videos.health(id) {
        Health::Fine => ui::Picture::Shows,
        Health::Trouble => ui::Picture::Recovering,
        Health::Failed(_) => videos.failure(id).map_or(ui::Picture::Shows, |f| {
            ui::Picture::NotShown(video::shown_text(&f, m.texts()))
        }),
    };
    let monitors: Vec<MonitorEntry> = world
        .monitors()
        .iter()
        .enumerate()
        .map(|(i, &(id, width, height))| MonitorEntry {
            id,
            label: m.label_of(id).unwrap_or_else(|| (i + 1).to_string()),
            width,
            height,
            place: world.place_of(id, m.view_eye()),
            seen: spot(id),
            scale: m.display(id).map_or(100, |d| d.mode.scale),
            picture: picture(id),
        })
        .collect();
    UiState {
        host: session.host().map(str::to_owned),
        found: search
            .found
            .iter()
            // Made when the PC answered (see `Session::heard`).
            .map(|h| (h.label.clone(), h.ip.clone()))
            .collect(),
        ip_input: menu.ip_input.clone(),
        hint: search.hint,
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
        can_scale: m.can_scale(),
        add_scale: menu.add_scale,
        can_auto: m.can_auto(),
        add_auto: menu.add_auto,
        auto: world
            .monitors()
            .iter()
            .map(|m| m.0)
            .filter(|k| m.is_auto(*k))
            .collect(),
        can_move_mouse: m.can_move_mouse(),
        monitors,
        windows,
        primary: world.primary(),
        cinema_screen: world.desk().cinema(),
        selected: menu.selected.filter(|id| world.streams(*id)),
        physical_seen: world
            .physical()
            .iter()
            .filter(|(id, _)| world.streams(*id))
            .filter_map(|(id, _)| {
                let name = physical::label(m.display(*id)?, m.texts());
                Some((spot(*id)?, name))
            })
            .collect(),
        physical_page: menu.physical_page,
        physical: match (session.main(), m.pc().displays.as_ref()) {
            (Some(pc), Some(displays)) => {
                // Asked and not answered yet: as asked.
                let waits = |k: &DisplayKey| session.waits(ui::Asked::Physical(*k));
                let asked: Vec<(DisplayKey, bool)> = menu
                    .physical_asked
                    .iter()
                    .filter(|(k, _)| waits(k))
                    .copied()
                    .collect();
                physical::entries(pc, displays, &asked, m.texts())
            }
            _ => Vec::new(),
        },
        audio_wanted: m.pc().sound.as_ref().is_none_or(|s| s.to_headset),
        mic_wanted: headset.mic_wanted,
        mic_denied: headset.mic_wanted && headset.mic_denied,
        settings: *m.settings(),
        room_available: caps.offers(Offer::RoomBackground),
        panorama_available: caps.offers(Offer::Panorama),
        custom_available: headset.custom_panorama,
        curve_available: world.curve_supported(),
        keyboard_available: caps.offers(Offer::KeyboardWindow),
        hands_available: caps.offers(Offer::Hands),
        display_rates: caps
            .rate_choice()
            .map(|(usual, saver)| (hz(usual), hz(saver))),
        display_rate_now: headset.refresh_hz.map(hz),
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
        confirm_delete: menu
            .confirm_delete
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
        update_from: headset.update_from,
        profiles: (0..m.profiles().len()).map(|i| m.profile_name(i)).collect(),
        profile_page: menu.profile_page,
        active_profile: menu.active_profile,
        note: menu.notes.shown().cloned(),
        #[cfg(feature = "preview")]
        dev: Some(ui::DevView {
            dev: *world.dev(),
            known: world.pixels_per_degree().is_some(),
            can_reset: world.can_reset_size(),
            auto_line: m.auto_line(),
        }),
        #[cfg(not(feature = "preview"))]
        dev: None,
    }
}
