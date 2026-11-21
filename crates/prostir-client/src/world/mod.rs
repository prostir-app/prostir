//! The room: where everything the headset shows stands, and the rules it is arranged
//! by. The presets, "Line up", the magnet, "move all together", following the head,
//! cinema apart from the desk arrangement, the easing of what is drawn towards where
//! it goes, the placeholder of a monitor being added, and the places of the menu, its
//! button and the keyboard window; and what a ray points at, by what is drawn.
//!
//! It keeps two places of each display: where it goes (`placements`) and where it is
//! drawn now (`shown`), which eases after it. What a ray hits and what a hand takes is
//! what is drawn: a monitor still on its way is hit where the person sees it, and taken
//! from there (its place becomes the drawn one), so it never jumps under the hand.
//!
//! Only the app changes it, through these methods: with what the PC and the streams
//! say (the facts it keeps of them: which displays there are, what each is, which
//! stream shows), the menu's actions, the hands and the frame. The time comes in as a
//! parameter, and so does the head: nothing here is the person's, so more people in
//! the room would be more poses passed in. The geometry itself is [`crate::layout`].

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use glam::{Quat, Vec2, Vec3};
use log::info;
use prostir_proto::state::{DisplayId, DisplayKind, DisplayState, Displays, Mode, ReqId, Row};

use crate::ids::{DisplayKey, PcId, Target, Thing};
use crate::layout::{self, Desk, Desktop, DesktopMemo, Hit, PixelSize, Placement, Ray};
use crate::pending::{Made, ProfileSwitch};
use crate::persist::{Dirty, File};
use crate::profiles::{self, Profile};
use crate::settings::{Preset, Settings};
use crate::{physical, ui};

/// The menu's width in the room (see [`panel_width`]).
pub const PANEL_WIDTH: f32 = 0.62;
/// The round button that opens the menu: its size in the room.
pub const BUTTON_SIZE: f32 = 0.075;
/// The keyboard window's depth as a share of its width (a keyboard's shape).
pub const KEYBOARD_ASPECT: f32 = 0.44;
/// The "Attach" button under a pointed-at detached window: size in the room and in
/// pixels, and how far below the window.
pub const ATTACH_WIDTH: f32 = 0.11;
pub const ATTACH_PX: (u32, u32) = (352, 96);
const ATTACH_GAP: f32 = 0.03;
/// Width of the wrist menu in the room.
pub const WRIST_MENU_WIDTH: f32 = 0.13;
/// A placeholder that waited this long for its monitor goes (the monitor may not
/// start; it then stands failed in the list, and the menu says why).
pub const PLACEHOLDER_FOR: Duration = Duration::from_secs(15);
/// How long a changed desktop must hold before the PC is told it: monitors arriving
/// one by one or being laid out again pass through states the desktop should not be
/// shown.
pub const ARRANGE_AFTER: Duration = Duration::from_millis(800);
/// Focus mode: how bright a monitor without the mouse is.
const DIMMED: f32 = 0.4;

/// The menu's width in the room, larger with the "Large" menu setting.
pub fn panel_width(large: bool) -> f32 {
    let scale: f32 = if large { 1.3 } else { 1.0 };
    PANEL_WIDTH * scale
}

pub fn panel_height(large: bool) -> f32 {
    panel_width(large) * ui::HEIGHT as f32 / ui::WIDTH as f32
}

pub fn attach_height() -> f32 {
    ATTACH_WIDTH * ATTACH_PX.1 as f32 / ATTACH_PX.0 as f32
}

pub fn wrist_menu_height() -> f32 {
    WRIST_MENU_WIDTH * ui::WRIST_HEIGHT as f32 / ui::WRIST_WIDTH as f32
}

/// Lying flat (facing up) with its far edge pointing away from `head`, like a
/// keyboard on a desk.
pub fn lying_flat(position: Vec3, head: Vec3) -> Quat {
    let away = Vec3::new(position.x - head.x, 0.0, position.z - head.z).normalize_or(-Vec3::Z);
    Quat::from_rotation_y((-away.x).atan2(-away.z))
        * Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)
}

/// What a display of the PC is, as far as the arrangement goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A virtual monitor.
    Monitor,
    /// A window panel: placed on its own, never kept.
    Window,
    /// A display of the PC itself.
    Physical,
}

/// One display as the PC's list says.
#[derive(Debug, Clone, Copy)]
struct Listed {
    kind: Kind,
    /// It could not start.
    failed: bool,
}

/// A monitor being added: where it will appear (shown at once as a placeholder), its
/// size, its request, and its number once the PC made it.
#[derive(Debug, Clone, Copy)]
pub struct Adding {
    pub req: ReqId,
    /// The PC it is asked of.
    pub pc: PcId,
    pub place: Placement,
    pub size: (u32, u32),
    pub id: Option<DisplayKey>,
    since: Instant,
}

impl Adding {
    /// What it is in the room.
    pub fn thing(&self) -> Thing {
        Thing::Adding {
            pc: self.pc,
            req: self.req,
        }
    }

    /// Its height in the room.
    pub fn height(&self) -> f32 {
        self.place.width * self.size.1 as f32 / self.size.0.max(1) as f32
    }
}

/// A monitor a ray meets, as drawn: where, how high, on which cylinder (none: flat).
#[derive(Debug, Clone, Copy)]
struct Seen {
    id: DisplayKey,
    hit: Hit,
    place: Placement,
    height: f32,
    curve: Option<f32>,
}

/// What a ray may point at besides what always stands: the menu while it is open, and
/// the "Attach" button of a detached window while it shows.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pickable {
    pub menu: bool,
    pub attach: Option<DisplayKey>,
}

/// Everything that stands in the room and how it is arranged (see the module).
pub struct World {
    // What there is, as the PC and the streams say.
    /// The PC of this session, or of the last one. Its displays are known by it and
    /// their numbers (see [`World::key`]).
    pc: Option<PcId>,
    /// In a session with it.
    connected: bool,
    /// Cinema: one screen shown, the desk arrangement kept aside (see `set_cinema`).
    cinema: bool,
    /// The PC's displays as its list last said (none outside a session).
    listed: HashMap<DisplayKey, Listed>,
    /// The PC said its list in this session.
    has_list: bool,
    /// The PC's monitors left to right, shared with its desktop.
    order: Vec<DisplayKey>,
    primary: Option<DisplayKey>,
    /// The display of the PC itself shown (see [`physical::mirror`]).
    mirror: Option<DisplayKey>,
    /// Every stream shown, and its size in pixels.
    streams: HashMap<DisplayKey, PixelSize>,
    /// The runtime draws curved layers.
    curve_supported: bool,
    /// The monitors left to right (`order` first, those it does not have after, by
    /// number), and the same with the laptop screen: made again whenever what they
    /// are made of changes, read every frame.
    lineup: Vec<(DisplayKey, u32, u32)>,
    sizes: Vec<(DisplayKey, u32, u32)>,

    // The arrangement.
    placements: HashMap<DisplayKey, Placement>,
    /// Where each display is drawn: follows `placements`, easing into new spots.
    shown: HashMap<DisplayKey, Placement>,
    /// How far each monitor has faded in (0 to 1) since it appeared.
    appear: HashMap<DisplayKey, f32>,
    /// Current brightness of each monitor, easing towards 1 or the dimmed level.
    brightness: HashMap<DisplayKey, f32>,
    /// Where layouts are centred and which way they face (head position, yaw).
    center: (Vec3, f32),
    /// The circle around the head the arrangement stands on (it grows for many
    /// monitors, see `layout::preset_radius`).
    radius: f32,
    /// How far up the arrangement's frame is tilted (see `layout::frame`): lying down,
    /// a band overhead seen from below. The arrangement's own, as a profile keeps it:
    /// a preset sets it when it lays the monitors out.
    tilt: f32,
    /// The stream size each desk placement was made for (see `fit_to_stream`). Not
    /// kept for the cinema view.
    placed_for: HashMap<DisplayKey, PixelSize>,
    /// The desk arrangement as last kept, in memory (`layout.txt` follows it and is
    /// never read again); see `save_layout`. In cinema it waits here under the view.
    desk: Desk,
    /// The desktop last sent to the PC.
    sent_layout: Option<Desktop<DisplayKey>>,
    /// A changed desktop waiting to settle before it is sent.
    pending_layout: Option<(Desktop<DisplayKey>, Instant)>,
    /// The desktop for the arrangement now, kept while nothing moves.
    desktop_memo: DesktopMemo<DisplayKey>,
    /// The rows the PC is to be told, until the app sends them.
    arrange: Option<Vec<Row>>,
    /// The monitors far to near as drawn this frame and the frame before (see
    /// `layout::draw_order`), kept to be filled again rather than made anew.
    draw_order: Vec<(f32, DisplayKey)>,
    drawn: Vec<DisplayKey>,
    /// What moves with the hands this frame (see `moving_now`), kept likewise.
    moving: Vec<DisplayKey>,
    /// Follow mode is moving the monitors right now.
    following: bool,
    /// Follow mode waits for the head to come back first: a monitor was just placed
    /// with the head turned, and the arrangement must not swing after it.
    follow_hold: bool,
    /// A monitor being added, shown as a placeholder where it will appear.
    adding: Option<Adding>,
    /// A profile being switched to, until its monitors stand where it says.
    switch: Option<ProfileSwitch>,

    // The person's things.
    menu: Placement,
    button: Option<Placement>,
    /// Where the keyboard window lies (set the first time it is shown).
    keyboard: Option<Placement>,
    /// The palm menu, while the palm is turned to the face.
    wrist: Option<Placement>,

    /// What the hands hold (two at most), and the monitor resized with both.
    held: [Option<Thing>; 2],
    resizing: Option<DisplayKey>,

    /// The magnet's memory (see `stick_of`), asked every frame of a drag.
    stick: layout::StickScratch<DisplayKey>,
    /// Displays the last list no longer has (see [`World::listed`]), and streams it
    /// placed: kept to be filled again.
    gone: Vec<DisplayKey>,
    waiting: Vec<DisplayKey>,
    /// The files this changed since the app last took them.
    saves: Dirty,
}

/// A window panel, as `listed` says.
fn window(listed: &HashMap<DisplayKey, Listed>, id: DisplayKey) -> bool {
    listed.get(&id).is_some_and(|l| l.kind == Kind::Window)
}

impl World {
    /// The room at the start: the desk arrangement and the keyboard window's place as
    /// the files kept them. In cinema the view is built once the PC's monitors are here
    /// (see `build_cinema_view_if_due`) and the desk arrangement waits until cinema
    /// ends.
    pub fn new(
        desk: Desk,
        keyboard: Option<Placement>,
        curve_supported: bool,
        s: &Settings,
    ) -> Self {
        let mut world = Self {
            pc: None,
            connected: false,
            cinema: s.cinema,
            listed: HashMap::new(),
            has_list: false,
            order: Vec::new(),
            primary: None,
            mirror: None,
            streams: HashMap::new(),
            curve_supported,
            lineup: Vec::new(),
            sizes: Vec::new(),
            placements: HashMap::new(),
            shown: HashMap::new(),
            appear: HashMap::new(),
            brightness: HashMap::new(),
            center: (Vec3::ZERO, 0.0),
            radius: layout::RADIUS,
            tilt: layout::tilt(s.preset),
            placed_for: HashMap::new(),
            desk,
            sent_layout: None,
            pending_layout: None,
            desktop_memo: DesktopMemo::default(),
            arrange: None,
            draw_order: Vec::new(),
            drawn: Vec::new(),
            moving: Vec::new(),
            following: false,
            follow_hold: false,
            adding: None,
            switch: None,
            menu: Placement {
                position: Vec3::new(0.0, -0.12, -0.7),
                rotation: Quat::IDENTITY,
                width: panel_width(s.large_menu),
            },
            button: None,
            keyboard,
            wrist: None,
            held: [None, None],
            resizing: None,
            stick: layout::StickScratch::default(),
            gone: Vec::new(),
            waiting: Vec::new(),
            saves: Dirty::default(),
        };
        if !world.cinema {
            let saved = world.desk.arrangement().clone();
            world.adopt_desk(saved);
        }
        world
    }

    // ---- What there is ----

    /// The PC of this session, or of the last one.
    pub fn pc(&self) -> Option<PcId> {
        self.pc
    }

    /// Display `id` of the PC of this session (or of the last one).
    pub fn key(&self, id: DisplayId) -> Option<DisplayKey> {
        self.pc.map(|pc| DisplayKey::new(pc, id))
    }

    /// The number display `k` has on the PC of this session; none for a display of
    /// another PC.
    pub fn on_pc(&self, k: DisplayKey) -> Option<DisplayId> {
        (self.pc == Some(k.pc)).then_some(k.id)
    }

    /// A window panel.
    pub fn is_window(&self, id: DisplayKey) -> bool {
        window(&self.listed, id)
    }

    /// The display of the PC itself shown in the headset, if one is (see
    /// [`physical::mirror`]).
    pub fn mirror(&self) -> Option<DisplayKey> {
        self.mirror
    }

    /// The primary display, as the PC says.
    pub fn primary(&self) -> Option<DisplayKey> {
        self.primary
    }

    /// A stream of display `id` shows.
    pub fn streams(&self, id: DisplayKey) -> bool {
        self.streams.contains_key(&id)
    }

    /// The monitors left to right: in the PC's order, unknown ones at the end, with
    /// their stream sizes.
    pub fn monitors(&self) -> &[(DisplayKey, u32, u32)] {
        &self.lineup
    }

    /// The laptop screen with its stream size, while it streams.
    fn mirror_size(&self) -> Option<(DisplayKey, u32, u32)> {
        let id = self.mirror?;
        self.streams.get(&id).map(|&(w, h)| (id, w, h))
    }

    /// The monitors and the laptop screen kept left to right again, after what they
    /// are made of changed (nothing is allocated once their buffers are large enough).
    fn relist(&mut self) {
        self.lineup.clear();
        for &id in &self.order {
            if let Some(&(w, h)) = self.streams.get(&id) {
                self.lineup.push((id, w, h));
            }
        }
        let known = self.lineup.len();
        for (&id, &(w, h)) in &self.streams {
            if self
                .listed
                .get(&id)
                .is_some_and(|l| l.kind == Kind::Monitor)
                && !self.order.contains(&id)
            {
                self.lineup.push((id, w, h));
            }
        }
        if let Some(rest) = self.lineup.get_mut(known..) {
            rest.sort_unstable();
        }
        self.sizes.clear();
        self.sizes.extend_from_slice(&self.lineup);
        self.sizes.extend(self.mirror_size());
    }

    /// A session with PC `pc` began: its desktop is told afresh.
    pub fn connected(&mut self, pc: PcId) {
        self.pc = Some(pc);
        self.connected = true;
        self.sent_layout = None;
    }

    /// The session ended. The places stay (the same PC comes back with the same
    /// numbers); the next session's list is news, not displays removed, and a profile
    /// being switched to is not any more.
    pub fn disconnected(&mut self) {
        self.connected = false;
        self.order.clear();
        self.listed.clear();
        self.has_list = false;
        self.mirror = None;
        self.switch = None;
        self.relist();
    }

    /// The PC listed its displays anew: the one source of which there are. Within a
    /// session, a display that left the list is gone, and its place goes with it (a
    /// display of the PC itself keeps its place: it comes back where it was), from the
    /// desk arrangement too, which cinema keeps aside; what a hand held of it is let
    /// go. Streams that came before the list said what they are are placed now, and
    /// monitors without a place get one. `head`: where it is and where it looks. The
    /// displays gone, whose streams the app ends.
    pub fn listed(
        &mut self,
        displays: &Displays,
        s: &Settings,
        head: (Vec3, Vec3),
        now: Instant,
    ) -> &[DisplayKey] {
        self.gone.clear();
        let Some(pc) = self.pc else {
            return &self.gone;
        };
        let key = |id: DisplayId| DisplayKey::new(pc, id);
        for (k, l) in &self.listed {
            if l.kind != Kind::Physical && displays.get(k.id).is_none() {
                self.gone.push(*k);
            }
        }
        self.listed.clear();
        for d in &displays.displays {
            let kind = match &d.kind {
                DisplayKind::Virtual => Kind::Monitor,
                DisplayKind::Window { .. } => Kind::Window,
                DisplayKind::Physical { .. } => Kind::Physical,
            };
            let failed = matches!(d.state, DisplayState::Failed { .. });
            self.listed.insert(key(d.id), Listed { kind, failed });
        }
        self.has_list = true;
        self.mirror = physical::mirror(displays).map(key);
        self.order.clear();
        self.order.extend(
            displays
                .displays
                .iter()
                .filter(|d| d.is_virtual())
                .map(|d| key(d.id)),
        );
        if self.desk.forget(&self.gone) {
            self.saves.mark(File::Desk);
        }
        for id in &self.gone {
            self.placements.remove(id);
            self.streams.remove(id);
            for h in &mut self.held {
                if *h == Some(Thing::Display(*id)) {
                    *h = None;
                }
            }
        }
        let primary = displays.primary.map(key);
        if primary != self.primary {
            info!("primary display: {primary:?}");
            self.primary = primary;
        }
        self.relist();
        if !self.gone.is_empty() {
            self.save_layout();
        }
        // A monitor of a profile being switched to that could not start.
        self.profile_streams(s, now);
        // Streams that came before the list said what they are.
        let mut waiting = std::mem::take(&mut self.waiting);
        waiting.clear();
        waiting.extend(
            self.streams
                .keys()
                .filter(|id| !self.placements.contains_key(id)),
        );
        // By number: the same list placed the same way every time.
        waiting.sort_unstable();
        for &id in &waiting {
            self.stream_shown(id, s, head, now);
        }
        self.waiting = waiting;
        self.place_missing(s, head.1, now);
        self.build_cinema_view_if_due(s, now);
        &self.gone
    }

    /// A stream of display `id` started, `size` pixels: it gets its place (a display
    /// not listed yet waits for the list, unseen).
    pub fn stream_started(
        &mut self,
        id: DisplayKey,
        size: PixelSize,
        s: &Settings,
        head: (Vec3, Vec3),
        now: Instant,
    ) {
        self.streams.insert(id, size);
        self.relist();
        self.stream_shown(id, s, head, now);
    }

    /// The stream of display `id` ended; the display keeps its place (only the list
    /// says it is gone).
    pub fn stream_ended(&mut self, id: DisplayKey) {
        if self.streams.remove(&id).is_some() {
            self.relist();
        }
    }

    /// A stream of display `id` shows: it gets its place.
    fn stream_shown(&mut self, id: DisplayKey, s: &Settings, head: (Vec3, Vec3), now: Instant) {
        let Some(kind) = self.listed.get(&id).map(|l| l.kind) else {
            return;
        };
        let size = self.streams.get(&id).copied();
        if let (Kind::Monitor, Some(size)) = (kind, size)
            && !self.cinema
        {
            self.fit_to_stream(id, size);
        }
        match kind {
            Kind::Window => {
                if !self.placements.contains_key(&id) {
                    self.place_window_panel(id, head);
                }
            }
            _ if self.switching() => self.profile_streams(s, now),
            // A monitor the arrangement has no place for: next to the others, which
            // stay. One it knows stays where it is.
            _ if !self.placements.contains_key(&id) => self.place_missing(s, head.1, now),
            _ => {}
        }
        self.build_cinema_view_if_due(s, now);
    }

    // ---- What the app reads ----

    /// Where display `id` goes.
    pub fn placement(&self, id: DisplayKey) -> Option<&Placement> {
        self.placements.get(&id)
    }

    /// Where display `id` is drawn now.
    pub fn shown(&self, id: DisplayKey) -> Option<&Placement> {
        self.shown.get(&id)
    }

    /// How far monitor `id` has faded in, 0 to 1.
    pub fn appear(&self, id: DisplayKey) -> f32 {
        self.appear.get(&id).copied().unwrap_or(1.0)
    }

    /// How bright monitor `id` is now (focus mode), 0 to 1.
    pub fn brightness(&self, id: DisplayKey) -> f32 {
        self.brightness.get(&id).copied().unwrap_or(1.0)
    }

    /// The desk arrangement as kept, and the screen cinema shows.
    pub fn desk(&self) -> &Desk {
        &self.desk
    }

    /// The menu's place.
    pub fn menu(&self) -> Placement {
        self.menu
    }

    /// The menu button's place, if it shows.
    pub fn button(&self) -> Option<Placement> {
        self.button
    }

    /// The keyboard window's place, once it has one.
    pub fn keyboard(&self) -> Option<Placement> {
        self.keyboard
    }

    /// The palm menu's place, while it shows.
    pub fn wrist(&self) -> Option<Placement> {
        self.wrist
    }

    /// The monitor being added, shown as a placeholder.
    pub fn adding(&self) -> Option<&Adding> {
        self.adding.as_ref()
    }

    /// A hand holds `thing`.
    pub fn holds(&self, thing: Thing) -> bool {
        self.held.contains(&Some(thing))
    }

    /// A hand holds something.
    pub fn holding(&self) -> bool {
        self.held.iter().any(Option::is_some)
    }

    /// The monitor resized with both hands.
    pub fn resizing(&self) -> Option<DisplayKey> {
        self.resizing
    }

    /// The files changed since this was last asked, to be written after this pass.
    pub fn take_saves(&mut self) -> Dirty {
        std::mem::take(&mut self.saves)
    }

    /// The rows the PC is to be told now (see [`World::settle`]), once.
    pub fn take_arrange(&mut self) -> Option<Vec<Row>> {
        self.arrange.take()
    }

    /// The monitors in the order they are drawn this frame, far to near, as
    /// [`World::order_drawing`] made it.
    pub fn draw_list(&self) -> &[(f32, DisplayKey)] {
        &self.draw_order
    }

    /// The cylinder curved monitors are drawn on, times the arrangement's radius.
    pub fn curve_factor(&self, s: &Settings) -> Option<f32> {
        self.curve_supported
            .then(|| s.curve.radius_factor())
            .flatten()
    }

    /// The runtime draws curved layers.
    pub fn curve_supported(&self) -> bool {
        self.curve_supported
    }

    pub fn curve_radius(&self, s: &Settings) -> Option<f32> {
        self.curve_factor(s).map(|f| f * self.radius)
    }

    /// The height of display `id` where it goes.
    pub fn height_of(&self, id: DisplayKey) -> f32 {
        match (self.placements.get(&id), self.streams.get(&id)) {
            (Some(p), Some(&(w, h))) => p.width * (h as f32 / w as f32),
            _ => 0.0,
        }
    }

    /// The height of display `id` drawn at `p`.
    fn height_at(&self, id: DisplayKey, p: &Placement) -> Option<f32> {
        let &(w, h) = self.streams.get(&id)?;
        Some(p.width * (h as f32 / w as f32))
    }

    // ---- The desktop of the PC ----

    /// Tells the PC how the monitors stand around the user (rows, left to right), so
    /// the desktop's arrangement matches: the mouse leaves a monitor where the next
    /// one is, sideways, up or down. Only an arrangement that held for
    /// [`ARRANGE_AFTER`], and only when it changed (see [`World::take_arrange`]).
    fn send_layout(&mut self, now: Instant) {
        let Some(pc) = self.pc.filter(|_| !self.cinema && self.connected) else {
            return;
        };
        // Wait until every monitor the PC has is placed: a partial picture would make
        // the desktop rearrange (and blink) once per arriving monitor. One whose stream could
        // not start keeps its place and does not hold the others up.
        let complete = self.order.iter().any(|id| self.streams.contains_key(id))
            && self.order.iter().all(|id| self.placements.contains_key(id));
        if !complete {
            return;
        }
        // The laptop screen too: the PC puts it where it stands here. Monitors stay in
        // the rows the PC has until clearly moved out of them, and its desktop stays
        // while it fits as well as a new one would.
        let desktop = {
            let desktop = self.desktop_memo.get_ref(
                &self.placements,
                &self.sizes,
                self.center,
                self.tilt,
                self.primary,
                self.sent_layout.as_ref(),
            );
            if desktop.rows.is_empty() || self.sent_layout.as_ref() == Some(&*desktop) {
                self.pending_layout = None;
                return;
            }
            // Only an arrangement that has held for a moment.
            match &self.pending_layout {
                Some((pending, since)) if *pending == *desktop => {
                    if now.saturating_duration_since(*since) < ARRANGE_AFTER {
                        return;
                    }
                }
                _ => {
                    self.pending_layout = Some((desktop.clone(), now));
                    return;
                }
            }
            desktop.clone()
        };
        self.pending_layout = None;
        let rows = desktop.wire(|k| (k.pc == pc).then_some(k.id));
        info!(
            "desktop arrangement: {}",
            rows.iter()
                .map(|r| {
                    let ids: Vec<u32> = r.ids().map(|id| id.0).collect();
                    let centers: Vec<Option<i32>> = r.entries.iter().map(|e| e.center).collect();
                    format!("{ids:?}@{centers:?}")
                })
                .collect::<Vec<_>>()
                .join(" / ")
        );
        self.order.clear();
        let listed = &self.listed;
        self.order.extend(
            rows.iter()
                .flat_map(Row::ids)
                .map(|id| DisplayKey::new(pc, id))
                .filter(|id| listed.get(id).is_some_and(|l| l.kind == Kind::Monitor)),
        );
        self.relist();
        self.sent_layout = Some(desktop);
        self.arrange = Some(rows);
    }

    /// Keeps the desktop's arrangement in step once nothing is being moved: no monitor
    /// in a hand, none resized, no profile being switched to.
    pub fn settle(&mut self, now: Instant) {
        let carried = self.held.iter().flatten().any(|t| t.display().is_some());
        if !carried && self.resizing.is_none() && !self.switching() {
            self.send_layout(now);
        }
    }

    /// The monitors on the map: the desktop the PC gets (see `layout::Desktop`).
    pub fn map_spots(&self) -> HashMap<DisplayKey, [f32; 4]> {
        self.desktop_memo
            .get_ref(
                &self.placements,
                &self.sizes,
                self.center,
                self.tilt,
                self.primary,
                self.sent_layout.as_ref(),
            )
            .spots()
    }

    /// The rows the PC's desktop is laid out by, by id: those last sent, or those a
    /// profile switched to was saved with until its desktop is sent (see `rows_from`).
    /// Their neighbours stay neighbours until clearly moved apart (`rows::rows_of`).
    fn sent_rows(&self) -> &[Vec<DisplayKey>] {
        self.sent_layout.as_ref().map_or(&[], |d| &d.rows)
    }

    /// A profile switched to is another arrangement: its rows are those it was saved
    /// with (by the PC's numbers now), or, for a profile without them, start afresh
    /// (see `layout::Desktop::history`).
    fn rows_from(&mut self, rows: Vec<Vec<DisplayKey>>) {
        self.sent_layout = Desktop::history(rows);
        self.pending_layout = None;
    }

    // ---- Laying out ----

    /// Layouts are worked out around the origin facing -Z; this moves them to where
    /// the user sat and looked when they last asked to bring everything in front.
    fn around_user(
        &self,
        mut placements: HashMap<DisplayKey, Placement>,
    ) -> HashMap<DisplayKey, Placement> {
        for p in placements.values_mut() {
            *p = layout::carried(p, (Vec3::ZERO, 0.0), self.center);
        }
        placements
    }

    /// Puts the laptop mirror back under the primary monitor, leaving the rest alone.
    fn place_mirror(&mut self, s: &Settings) {
        let all = self.around_user(layout::arrange(
            s.preset,
            &self.lineup,
            self.primary,
            self.mirror_size(),
            self.curve_factor(s),
        ));
        if let Some(id) = self.mirror
            && let Some(p) = all.get(&id)
        {
            self.placements.insert(id, *p);
        }
    }

    /// Lays the monitors of this session's PC (and its own display shown) out by the
    /// chosen preset, or in cinema its one screen. Only the places it lays out change:
    /// window panels, another PC's displays and a display of this PC that shows
    /// nothing now (a closed laptop's panel) stay where they are. With nothing to lay
    /// out (no PC yet, or no monitor of it streams) nothing changes and nothing is
    /// saved, so the arrangement waiting for its PC is not lost. What the hands held
    /// is let go.
    pub fn arrange_all(&mut self, s: &Settings, now: Instant) {
        if self.cinema {
            // The view takes the place of the monitors here, which it does not show (the
            // desk keeps theirs).
            let view = self.around_user(layout::cinema(&self.lineup, self.desk.cinema()));
            let (listed, pc) = (&self.listed, self.pc);
            self.placements
                .retain(|k, _| window(listed, *k) || pc.is_some_and(|pc| pc != k.pc));
            self.placements.extend(view);
            self.held = [None, None];
            return;
        }
        if self.lineup.is_empty() {
            return;
        }
        self.radius = layout::preset_radius(s.preset, &self.lineup);
        self.tilt = layout::tilt(s.preset);
        let laid = self.around_user(layout::arrange(
            s.preset,
            &self.lineup,
            self.primary,
            self.mirror_size(),
            self.curve_factor(s),
        ));
        self.placements.extend(laid);
        self.held = [None, None];
        self.save_layout();
        self.send_layout(now);
    }

    /// Another preset was chosen (`s` says which): the arrangement's tilt is its, and
    /// everything is laid out by it.
    pub fn preset_chosen(&mut self, s: &Settings, now: Instant) {
        self.tilt = layout::tilt(s.preset);
        self.arrange_all(s, now);
    }

    /// Another curve was chosen: flat, the laptop screen hangs tilted, curved it
    /// continues the cylinder.
    pub fn curve_chosen(&mut self, s: &Settings) {
        if !self.cinema {
            self.place_mirror(s);
        }
    }

    /// Monitors without a place get one: all by the preset when none has any (the
    /// first start), else each next to the others (see `layout::place_new`), which
    /// stay where they are. The laptop screen goes under the primary monitor. `gaze`:
    /// where the head looks.
    fn place_missing(&mut self, s: &Settings, gaze: Vec3, now: Instant) {
        if self.cinema || self.switching() {
            return;
        }
        if self.lineup.is_empty() {
            return;
        }
        if !self
            .lineup
            .iter()
            .any(|m| self.placements.contains_key(&m.0))
        {
            self.arrange_all(s, now);
            return;
        }
        let mut placed = false;
        for i in 0..self.lineup.len() {
            let Some(&m) = self.lineup.get(i) else {
                break;
            };
            if self.placements.contains_key(&m.0) {
                continue;
            }
            // Where its placeholder stood, for the monitor being added.
            let at = match &self.adding {
                Some(a) if a.id == Some(m.0) => a.place,
                _ => self.place_for_new((m.1, m.2), gaze),
            };
            info!("monitor {} placed next to the others", m.0);
            self.placements.insert(m.0, at);
            placed = true;
        }
        if let Some(id) = self.mirror
            && self.streams.contains_key(&id)
            && !self.placements.contains_key(&id)
        {
            self.place_mirror(s);
            placed = true;
        }
        if placed {
            self.save_layout();
        }
    }

    /// Where a new monitor of `size` pixels goes next to the others (it has no place
    /// yet), near where the head looks (`gaze`).
    fn place_for_new(&self, size: (u32, u32), gaze: Vec3) -> Placement {
        layout::place_new(
            &self.placements,
            &self.sizes,
            size,
            self.center,
            self.tilt,
            gaze,
            self.sent_rows(),
            self.radius,
        )
    }

    /// A new window panel: in front of the user, a little closer than the monitors and
    /// below eye level, beside panels already there. Flat, at the monitors' sharpness.
    fn place_window_panel(&mut self, id: DisplayKey, (head, forward): (Vec3, Vec3)) {
        let Some(&(width_px, _)) = self.streams.get(&id) else {
            return;
        };
        const DISTANCE: f32 = 0.8;
        let flat = Vec3::new(forward.x, 0.0, forward.z).normalize_or(-Vec3::Z);
        let right = Vec3::new(-flat.z, 0.0, flat.x);
        let others = self
            .placements
            .keys()
            .filter(|k| self.is_window(**k) && **k != id)
            .count();
        let width =
            (width_px as f32 * layout::METRES_PER_PIXEL * DISTANCE / self.radius).clamp(0.2, 1.2);
        let position = head + flat * DISTANCE - Vec3::Y * 0.12 + right * (0.15 * others as f32);
        self.placements.insert(
            id,
            Placement {
                position,
                rotation: layout::facing(position, Vec3::new(head.x, position.y, head.z)),
                width,
            },
        );
    }

    // ---- The desk and cinema ----

    /// Keeps the arrangement as the desk layout, and `layout.txt` is saved after this
    /// pass. In cinema mode `placements` is the cinema view, which is not kept: the
    /// desk layout stays as the user left it. Window panels are placed afresh when
    /// they appear, so they are not kept.
    pub fn save_layout(&mut self) {
        let listed = &self.listed;
        let kept = self.desk.keep(
            self.placements
                .iter()
                .filter(|(id, _)| !window(listed, **id))
                .map(|(id, p)| (*id, *p)),
            &self.placed_for,
            (self.center, self.radius, self.tilt),
            self.cinema,
        );
        if kept {
            self.saves.mark(File::Desk);
        }
    }

    /// Takes `saved` as the desk arrangement. Window panels stay where they are; a
    /// monitor whose stream changed size since keeps the size of its pixels; layouts
    /// keep facing the way this one was laid out (from a file that does not say, the
    /// way its monitors face).
    fn adopt_desk(&mut self, saved: layout::Arrangement) {
        let listed = &self.listed;
        self.placements.retain(|id, _| window(listed, *id));
        self.placements.extend(saved.placements);
        self.placed_for = saved.sizes;
        self.radius = saved.radius.unwrap_or(layout::RADIUS);
        // A desk never kept (none saved yet) keeps the tilt of the preset.
        if saved.center.is_some() {
            self.tilt = saved.tilt;
        }
        for (&id, &size) in &self.streams {
            if !window(&self.listed, id) {
                if let (Some(p), Some(&was)) =
                    (self.placements.get_mut(&id), self.placed_for.get(&id))
                {
                    *p = layout::refit(*p, was, size);
                }
                self.placed_for.insert(id, size);
            }
        }
        self.center = saved.center.unwrap_or_else(|| {
            let desk: HashMap<DisplayKey, Placement> = self
                .placements
                .iter()
                .filter(|(id, _)| !self.is_window(**id))
                .map(|(id, p)| (*id, *p))
                .collect();
            (
                Vec3::ZERO,
                layout::facing_yaw(&desk, Vec3::ZERO).unwrap_or(0.0),
            )
        });
    }

    /// The desk placement of monitor `id` follows its stream, now `size` pixels: the
    /// same size of a pixel as before (see `layout::refit`).
    fn fit_to_stream(&mut self, id: DisplayKey, size: PixelSize) {
        if let (Some(p), Some(&was)) = (self.placements.get_mut(&id), self.placed_for.get(&id)) {
            *p = layout::refit(*p, was, size);
        }
        self.placed_for.insert(id, size);
    }

    /// Turns cinema mode on or off (`s`: the settings, for the preset it lays out
    /// with). The one place that switches it: the
    /// desk layout is kept before the cinema view replaces it (follow mode may have
    /// moved it), and comes back after from memory (not from its file, which may not
    /// be written yet), window panels staying where they are.
    pub fn set_cinema(&mut self, s: &Settings, on: bool, gaze: Vec3, now: Instant) {
        if on == self.cinema {
            return;
        }
        if on {
            self.save_layout();
            self.cinema = true;
            self.arrange_all(s, now);
            return;
        }
        self.cinema = false;
        let saved = self.desk.arrangement().clone();
        self.adopt_desk(saved);
        self.place_missing(s, gaze, now);
    }

    /// Cinema shows its one screen, the desk arrangement waiting under it.
    pub fn cinema(&self) -> bool {
        self.cinema
    }

    /// Cinema shows `screen` from now on (and now, in cinema).
    pub fn pick_cinema(&mut self, screen: DisplayKey, s: &Settings, now: Instant) {
        if self.desk.pick_cinema(Some(screen)) {
            self.saves.mark(File::Desk);
        }
        if self.cinema {
            self.arrange_all(s, now);
        }
    }

    /// Cinema mode with no view yet (started in cinema, or switched to a PC in it):
    /// built once every monitor the PC has is here, so the chosen screen is among
    /// them. A view that exists, moved by hand perhaps, is left alone.
    fn build_cinema_view_if_due(&mut self, s: &Settings, now: Instant) {
        let due = self.cinema
            && !self.order.is_empty()
            && self.order.iter().all(|id| self.streams.contains_key(id))
            && !self.order.iter().any(|id| self.placements.contains_key(id));
        if due {
            self.arrange_all(s, now);
        }
    }

    /// PC `pc` was forgotten: the places of its displays go (from the arrangement, and
    /// from the desk arrangement kept aside in cinema), so does the cinema's pick of
    /// one. Paired again later, its displays are laid out afresh.
    pub fn forget_pc(&mut self, pc: PcId) {
        let of_pc = |k: &DisplayKey| k.pc == pc;
        self.placements.retain(|k, _| !of_pc(k));
        self.placed_for.retain(|k, _| !of_pc(k));
        let gone: Vec<DisplayKey> = self
            .desk
            .arrangement()
            .placements
            .keys()
            .copied()
            .chain(self.desk.cinema())
            .filter(of_pc)
            .collect();
        if self.desk.forget(&gone) {
            self.saves.mark(File::Desk);
        }
    }

    // ---- Profiles ----

    /// The setup as it is now (see `Profile::of`): the monitors left to right with
    /// their scales (`scale` of each, as the PC says), the primary, the PC's own
    /// displays `shown`, and where they stand (in cinema, the desk arrangement as kept
    /// before it).
    pub fn profile(&self, shown: Vec<DisplayKey>, scale: impl Fn(DisplayKey) -> u16) -> Profile {
        let live = layout::Arrangement {
            placements: self.placements.clone(),
            sizes: HashMap::new(),
            center: Some(self.center),
            radius: Some(self.radius),
            tilt: self.tilt,
        };
        let monitors: Vec<(DisplayKey, Mode)> = self
            .lineup
            .iter()
            .map(|&(k, w, h)| {
                (
                    k,
                    Mode {
                        w,
                        h,
                        scale: scale(k),
                    },
                )
            })
            .collect();
        Profile::of(
            &monitors,
            self.primary,
            shown,
            &live,
            self.cinema.then(|| self.desk.arrangement()),
            self.sent_rows(),
        )
    }

    /// A profile is being switched to (`None`: none any more): the monitors are not
    /// laid out one by one, nor the desktop's arrangement sent, until it stands.
    pub fn switch_to(&mut self, switch: Option<ProfileSwitch>) {
        self.switch = switch;
    }

    /// A profile is being switched to.
    pub fn switching(&self) -> bool {
        self.switch.is_some()
    }

    /// The profile being switched to is the one of request `req`.
    pub fn switching_with(&self, req: ReqId) -> bool {
        self.switch.as_ref().is_some_and(|s| s.req == req)
    }

    /// The PC made a profile's monitors (`ids`, in the order of its monitors): they
    /// take the profile's places, or, for a profile without places, the layout of the
    /// preset once all of them stream. In cinema the profile's places become the desk
    /// arrangement under the cinema view, shown once cinema ends.
    pub fn profile_made(&mut self, ids: Vec<DisplayKey>, s: &Settings, gaze: Vec3, now: Instant) {
        let Some(switch) = &mut self.switch else {
            return;
        };
        let (frame, rows) = (switch.frame, switch.rows.clone());
        let radius = frame.map_or(layout::RADIUS, |f| f.radius);
        match switch.made(ids, now) {
            Made::Places(made) if self.cinema => {
                self.switch = None;
                let mut desk = self.desk.arrangement().clone();
                let at = desk.center.unwrap_or(self.center);
                let (placed, rows) = profiles::standing(&made, frame, &rows, at);
                for (id, p) in placed {
                    desk.placements.insert(id, p);
                    desk.sizes.remove(&id);
                }
                let tilt = frame.map_or(desk.tilt, |f| f.tilt);
                self.desk
                    .keep(desk.placements, &desk.sizes, (at, radius, tilt), false);
                self.saves.mark(File::Desk);
                self.rows_from(rows);
            }
            Made::Places(made) => {
                // Seen from the user's eye now as they were from the eye they were
                // saved around. Worked out here, once the monitors are made: following
                // the head meanwhile moves the center.
                let (placed, rows) = profiles::standing(&made, frame, &rows, self.center);
                for (id, p) in placed {
                    self.placements.insert(id, p);
                    self.placed_for.remove(&id);
                }
                self.radius = radius;
                // The profile's arrangement is seen as it was, whatever the preset.
                if let Some(f) = frame {
                    self.tilt = f.tilt;
                }
                self.switch = None;
                self.rows_from(rows);
                self.save_layout();
                self.place_missing(s, gaze, now);
            }
            Made::Wait => self.profile_streams(s, now),
        }
    }

    /// The request of the profile being switched to went without an answer: it is not
    /// switched to.
    pub fn switch_given_up(&mut self, req: ReqId) {
        if self.switching_with(req) {
            self.switch = None;
        }
    }

    /// A profile whose monitors the PC made, laid out by its preset once they stand
    /// (see `profile_streams`), also when no stream comes.
    pub fn switch_due(&mut self, s: &Settings, now: Instant) {
        if self.switch.as_ref().is_some_and(|s| s.ids.is_some()) {
            self.profile_streams(s, now);
        }
    }

    /// A profile laid out by its preset, once every monitor it made streams (or could
    /// not start: it keeps its place in the list), or once it waited long enough
    /// (`pending::PROFILE_STANDS_WITHIN`), with what streams then.
    fn profile_streams(&mut self, s: &Settings, now: Instant) {
        let settled = |k: DisplayKey| {
            self.streams.contains_key(&k)
                || (self.has_list && self.listed.get(&k).is_none_or(|l| l.failed))
        };
        let stands = self
            .switch
            .as_ref()
            .is_some_and(|sw| sw.stands(now, settled));
        if stands {
            self.switch = None;
            self.rows_from(Vec::new());
            self.arrange_all(s, now);
        }
    }

    // ---- A monitor being added ----

    /// Monitor `size` was asked for with request `req` (none: not asked): shown at once
    /// as a placeholder where it will appear, near where the head looks (`gaze`), and
    /// the monitor takes that place once its picture comes (not in cinema, where new
    /// monitors are not placed).
    pub fn adding_started(
        &mut self,
        req: Option<ReqId>,
        size: (u32, u32),
        gaze: Vec3,
        now: Instant,
    ) {
        self.adding = req.filter(|_| !self.cinema).and_then(|req| {
            Some(Adding {
                req,
                pc: self.pc?,
                place: self.place_for_new(size, gaze),
                size,
                id: None,
                since: now,
            })
        });
    }

    /// The PC made the monitor of request `req`: display `id`.
    pub fn adding_made(&mut self, req: ReqId, id: Option<DisplayKey>) {
        if let Some(a) = &mut self.adding
            && a.req == req
        {
            a.id = id;
        }
    }

    /// The placeholder of a monitor being added goes once its picture is there, it
    /// failed, the connection ended, its request is not waited for (`waits`), or it
    /// waited too long.
    pub fn adding_due(&mut self, now: Instant, waits: bool) {
        let Some(a) = &self.adding else {
            return;
        };
        let failed =
            a.id.is_some_and(|k| self.has_list && self.listed.get(&k).is_none_or(|l| l.failed));
        let shown = a.id.is_some_and(|id| self.streams.contains_key(&id));
        let asked = waits || a.id.is_some();
        if shown
            || failed
            || !asked
            || !self.connected
            || now.saturating_duration_since(a.since) >= PLACEHOLDER_FOR
        {
            self.adding = None;
        }
    }

    // ---- Moving it all ----

    /// Follow mode: once the head has turned (or moved) well away from where the
    /// monitors face, the whole arrangement glides after it until it is in front again.
    /// Small head movements leave it still, so reading is not disturbed.
    pub fn follow_head(&mut self, s: &Settings, dt: f32, (head, forward): (Vec3, Vec3)) {
        const START_TURN: f32 = 0.35; // about 20°
        const START_MOVE: f32 = 0.25; // metres
        if !s.follow || self.placements.is_empty() || self.holding() {
            self.following = false;
            return;
        }
        let flat = Vec3::new(forward.x, 0.0, forward.z).normalize_or(-Vec3::Z);
        let yaw = (-flat.x).atan2(-flat.z);
        let (pos, center_yaw) = self.center;
        let mut turn = yaw - center_yaw;
        turn =
            (turn + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        let shift = Vec3::new(head.x - pos.x, 0.0, head.z - pos.z);
        if self.follow_hold {
            if turn.abs() < START_TURN && shift.length() < START_MOVE {
                self.follow_hold = false;
            }
            return;
        }
        if !self.following && (turn.abs() > START_TURN || shift.length() > START_MOVE) {
            self.following = true;
        }
        if !self.following {
            return;
        }
        // Ease towards the head: about a third of a second to catch up.
        let k = (dt * 3.0).min(1.0);
        let step_turn = turn * k;
        let step_shift = shift * k;
        let new_pos = pos + step_shift;
        for p in self.placements.values_mut() {
            // Its rotation unit after every step (see `layout::carried`): thousands of
            // steps of following must not leave one that the last bits of a float have
            // made other than a turn, nor a seam "Line up" closed no longer seen closed.
            *p = layout::carried(p, (pos, 0.0), (new_pos, step_turn));
        }
        self.center = (new_pos, center_yaw + step_turn);
        if turn.abs() < 0.02 && shift.length() < 0.02 {
            self.following = false;
            self.save_layout();
        }
    }

    /// Follow mode was switched (`s` says how): it starts without waiting, and the
    /// arrangement where following left it is kept.
    pub fn follow_chosen(&mut self, s: &Settings) {
        self.follow_hold = false;
        if !s.follow {
            self.save_layout();
        }
    }

    /// Turns the whole arrangement around the user so the primary monitor is straight
    /// ahead, keeping how the monitors stand to each other. In cinema, or with nothing
    /// placed, lays it all out in front.
    pub fn recenter(&mut self, s: &Settings, (head, forward): (Vec3, Vec3), now: Instant) {
        let flat = Vec3::new(forward.x, 0.0, forward.z).normalize_or(-Vec3::Z);
        let yaw = (-flat.x).atan2(-flat.z);
        if self.cinema || self.placements.is_empty() {
            self.center = (head, yaw);
            self.arrange_all(s, now);
            return;
        }
        let (old_pos, _) = self.center;
        // Straight ahead goes the primary monitor (else the middle of the monitors).
        let ids: Vec<DisplayKey> = self.lineup.iter().map(|m| m.0).collect();
        let ahead =
            layout::ahead(&self.placements, &ids, self.primary, old_pos).unwrap_or(self.center.1);
        for p in self.placements.values_mut() {
            *p = layout::carried(p, (old_pos, ahead), (head, yaw));
        }
        self.center = (head, yaw);
        self.save_layout();
        self.send_layout(now);
    }

    /// Brings the monitors closer (`steps` below 0) or further, a step at a time, as
    /// they stand around the user: each keeps its size, the arrangement its shape.
    pub fn closer(&mut self, steps: i8, now: Instant) {
        const STEP: f32 = 1.15;
        let (center, _) = self.center;
        let wanted = self.radius * STEP.powi(i32::from(steps));
        let k = wanted.clamp(0.5, 6.0) / self.radius.max(1e-3);
        for (id, p) in &mut self.placements {
            if !window(&self.listed, *id) {
                p.position = center + (p.position - center) * k;
            }
        }
        self.radius *= k;
        self.save_layout();
        self.send_layout(now);
    }

    /// The A button: lines up what the user arranged by hand, row by row (see
    /// `layout::line_up`), instead of replacing it with the preset: every row keeps its
    /// place and the way it stands, tilted back or overhead too. Cinema rebuilds its
    /// own view. What the hands held is let go.
    pub fn tidy_up(&mut self, s: &Settings, now: Instant) {
        if self.cinema || self.placements.is_empty() {
            self.arrange_all(s, now);
            return;
        }
        let lid = self.mirror.filter(|id| self.placements.contains_key(id));
        let lined = layout::line_up(
            &self.placements,
            &self.sizes,
            self.center,
            self.tilt,
            self.primary,
            self.sent_rows(),
            lid,
        );
        // Window panels (and anything not laid out) stay as they are.
        self.placements.extend(lined);
        self.held = [None, None];
        self.save_layout();
        self.send_layout(now);
    }

    /// Swaps the places of monitor `id` and its neighbour `delta` along in the order;
    /// the others stay. Each takes the other's place at the size of its own pixels.
    pub fn swap(&mut self, id: DisplayKey, delta: i32) {
        let Some(i) = self.lineup.iter().position(|m| m.0 == id) else {
            return;
        };
        let j = (i as i32 + delta).clamp(0, self.lineup.len() as i32 - 1) as usize;
        let (Some(&(_, aw, ah)), Some(&(other, bw, bh))) = (self.lineup.get(i), self.lineup.get(j))
        else {
            return;
        };
        if let (Some(a), Some(b)) = (
            self.placements.get(&id).copied(),
            self.placements.get(&other).copied(),
        ) {
            let (sa, sb) = ((aw, ah), (bw, bh));
            self.placements.insert(id, layout::refit(b, sb, sa));
            self.placements.insert(other, layout::refit(a, sa, sb));
            self.save_layout();
        }
    }

    // ---- Hands ----

    /// "Move all together": monitor `id` went from `before` to `after` in the user's
    /// hand, and the rest of the arrangement (monitors and the laptop screen, not
    /// window panels) goes along as one piece: moved, turned and scaled with it.
    fn carry_all(&mut self, s: &Settings, id: DisplayKey, before: &Placement, after: &Placement) {
        if !s.move_all || self.cinema || self.is_window(id) || !self.streams.contains_key(&id) {
            return;
        }
        let turn = after.rotation * before.rotation.inverse();
        // One scale for all, kept so no monitor goes past the size limits: clamping
        // each on its own would pull the arrangement apart.
        let mut scale = after.width / before.width.max(1e-3);
        for (other, p) in &self.placements {
            if *other != id && !window(&self.listed, *other) {
                scale = scale.clamp(0.2 / p.width.max(1e-3), 8.0 / p.width.max(1e-3));
            }
        }
        for (other, p) in &mut self.placements {
            if *other == id || window(&self.listed, *other) {
                continue;
            }
            p.position = after.position + turn * (p.position - before.position) * scale;
            p.rotation = (turn * p.rotation).normalize();
            p.width *= scale;
        }
    }

    /// What moves with the hands now: the held monitors, and with "move all together"
    /// every monitor that goes along (drawn where it is, not catching up, or the
    /// arrangement would tear apart while it moves).
    fn moving_now(&mut self, s: &Settings) {
        self.moving.clear();
        self.moving
            .extend(self.held.iter().flatten().filter_map(|t| t.display()));
        let carrying = s.move_all
            && !self.cinema
            && self
                .moving
                .iter()
                .chain(self.resizing.iter())
                .any(|id| self.streams.contains_key(id) && !window(&self.listed, *id));
        if carrying {
            let listed = &self.listed;
            self.moving
                .extend(self.placements.keys().filter(|id| !window(listed, **id)));
        }
    }

    /// The magnet: where monitor `id`, let go now, would stick (not on a wall, where
    /// monitors stay in its plane, nor for the laptop screen or window panels).
    pub fn stick_of(&mut self, s: &Settings, id: DisplayKey) -> Option<layout::Stick<DisplayKey>> {
        // Lying down the monitors are a tilted band the presets keep exact; window
        // panels float on their own.
        if self.cinema
            || !s.magnet
            || s.move_all
            || self.tilt != 0.0
            || self.is_window(id)
            || !self.streams.contains_key(&id)
        {
            return None;
        }
        if s.preset == Preset::Wall {
            layout::stick_on_wall(
                &self.placements,
                &self.sizes,
                self.center,
                id,
                &mut self.stick,
            )
        } else {
            layout::stick(
                &self.placements,
                &self.sizes,
                self.center.0,
                id,
                &mut self.stick,
            )
        }
    }

    /// Where `thing` goes now, for a hand to move it from (the menu, the keyboard
    /// window, a display).
    pub fn place(&self, thing: Thing) -> Option<Placement> {
        match thing {
            Thing::Menu => Some(self.menu),
            Thing::Keyboard => self.keyboard,
            Thing::Display(id) => self.placements.get(&id).copied(),
            Thing::MenuButton | Thing::Wrist | Thing::Attach(_) | Thing::Adding { .. } => None,
        }
    }

    /// A hand takes `thing`, where it is drawn: a monitor still easing towards its
    /// place goes on from there (its place becomes the drawn one), so it does not jump
    /// under the hand. With "move all together" the same goes for every monitor that
    /// goes along with it (see `moving_now`), or they would jump to where they were
    /// going. Where it is taken at; `None` for what cannot be taken, for what a hand
    /// holds already, and while both hands hold something.
    pub fn hold(&mut self, thing: Thing, s: &Settings) -> Option<Placement> {
        if self.holds(thing) {
            return None;
        }
        let free = self.held.iter().position(Option::is_none)?;
        let at = match thing {
            Thing::Display(id) => {
                let carries = s.move_all
                    && !self.cinema
                    && self.streams.contains_key(&id)
                    && !self.is_window(id);
                let target = self.placements.get_mut(&id)?;
                if let Some(drawn) = self.shown.get(&id) {
                    *target = *drawn;
                }
                let at = *target;
                if carries {
                    let (listed, shown) = (&self.listed, &self.shown);
                    for (other, p) in &mut self.placements {
                        if !window(listed, *other)
                            && let Some(drawn) = shown.get(other)
                        {
                            *p = *drawn;
                        }
                    }
                }
                at
            }
            _ => self.place(thing)?,
        };
        *self.held.get_mut(free)? = Some(thing);
        Some(at)
    }

    /// `thing`, in a hand, is now at `p` (worked out from where it was, see
    /// [`World::place`]): the keyboard window stays flat, a monitor on a wall stays
    /// parallel to it, and with "move all together" the rest goes along. `head`: where
    /// the head is.
    pub fn drag(&mut self, s: &Settings, thing: Thing, mut p: Placement, head: Vec3) {
        let wall = s.preset == Preset::Wall && !self.cinema;
        if thing == Thing::Keyboard {
            // It follows the hand but stays flat, like the desk.
            p.rotation = lying_flat(p.position, head);
        } else if wall && thing != Thing::Menu {
            // On a wall, monitors stay parallel to it wherever they go.
            p.rotation = Quat::from_rotation_y(self.center.1);
        }
        let target = match thing {
            Thing::Menu => &mut self.menu,
            Thing::Keyboard => match &mut self.keyboard {
                Some(k) => k,
                None => return,
            },
            Thing::Display(id) => match self.placements.get_mut(&id) {
                Some(t) => t,
                None => return,
            },
            Thing::MenuButton | Thing::Wrist | Thing::Attach(_) | Thing::Adding { .. } => return,
        };
        let before = *target;
        *target = p;
        if let Some(id) = thing.display() {
            self.carry_all(s, id, &before, &p);
        }
    }

    /// A hand let go of `thing`. A monitor put somewhere new stays there until the
    /// head comes back near where it was (the menu and the keyboard do not count);
    /// let go next to another monitor, it sticks to it, edge to edge (true then), and
    /// the desktop's arrangement follows what the eyes see. Resizing ends too. In
    /// cinema nothing is kept.
    pub fn let_go(&mut self, thing: Thing, s: &Settings, now: Instant) -> bool {
        for h in &mut self.held {
            if *h == Some(thing) {
                *h = None;
            }
        }
        self.follow_hold = s.follow
            && thing
                .display()
                .is_some_and(|id| self.placements.contains_key(&id));
        self.resizing = None;
        let mut stuck = false;
        match thing {
            Thing::Keyboard => {
                if self.keyboard.is_some() {
                    self.saves.mark(File::Keyboard);
                }
            }
            Thing::Menu if !self.cinema => self.save_layout(),
            Thing::Display(id) if !self.cinema => {
                if let Some(st) = self.stick_of(s, id) {
                    self.placements.insert(id, st.placement);
                    stuck = true;
                }
                self.save_layout();
                self.send_layout(now);
            }
            Thing::Menu
            | Thing::Display(_)
            | Thing::MenuButton
            | Thing::Wrist
            | Thing::Attach(_)
            | Thing::Adding { .. } => {}
        }
        stuck
    }

    /// The other hand took hold of monitor `id` too: both resize it. Its width then.
    pub fn resize_start(&mut self, id: DisplayKey) -> Option<f32> {
        let width = self.placements.get(&id)?.width;
        self.resizing = Some(id);
        Some(width)
    }

    /// The monitor resized with both hands is `width` wide now (the rest goes along
    /// with "move all together").
    pub fn resize(&mut self, s: &Settings, width: f32) {
        let Some(id) = self.resizing else {
            return;
        };
        let Some(p) = self.placements.get_mut(&id) else {
            return;
        };
        let before = *p;
        p.width = width;
        let after = *p;
        self.carry_all(s, id, &before, &after);
    }

    /// The second hand let go: resizing ends, and the arrangement is kept.
    pub fn resize_end(&mut self) {
        self.resizing = None;
        self.save_layout();
    }

    /// The hands are not the app's any more (the system menu over it): what they held
    /// is let go, and the arrangement is kept.
    pub fn cancel_holds(&mut self) {
        self.held = [None, None];
        self.resizing = None;
        self.save_layout();
    }

    // ---- The person's things ----

    /// Opens the menu in front of the user: within arm's reach, a little lower, when
    /// hands are in view (to `touch` it), further away for controllers.
    pub fn open_menu(&mut self, (head, forward): (Vec3, Vec3), touch: bool, s: &Settings) {
        let flat = Vec3::new(forward.x, 0.0, forward.z).normalize_or(-Vec3::Z);
        let position = if touch {
            head + flat * 0.5 - Vec3::Y * 0.22
        } else {
            head + flat * 0.7 - Vec3::Y * 0.12
        };
        self.menu = Placement {
            position,
            rotation: layout::facing(position, head),
            width: panel_width(s.large_menu),
        };
    }

    /// The menu, dragged by a fingertip, is at `position`, facing `head`.
    pub fn move_menu(&mut self, position: Vec3, head: Vec3) {
        self.menu.position = position;
        self.menu.rotation = layout::facing(position, head);
    }

    /// The menu is large or not.
    pub fn menu_size(&mut self, large: bool) {
        self.menu.width = panel_width(large);
    }

    /// Where the palm menu is (`None`: not shown).
    pub fn set_wrist(&mut self, place: Option<Placement>) {
        self.wrist = place;
    }

    /// The keyboard window's place: in front of the user at desk height the first
    /// time, then wherever it was moved.
    pub fn keyboard_here(&mut self, (head, forward): (Vec3, Vec3)) -> Placement {
        *self.keyboard.get_or_insert_with(|| {
            let flat = Vec3::new(forward.x, 0.0, forward.z).normalize_or(-Vec3::Z);
            let position = head + flat * 0.42 - Vec3::Y * 0.42;
            Placement {
                position,
                rotation: lying_flat(position, head),
                width: 0.5,
            }
        })
    }

    /// Puts the menu button just under the primary monitor's bottom-left corner, and
    /// left of the laptop mirror hanging there so it never covers it. When the primary
    /// is not shown (cinema on another screen), under the first monitor that is: with
    /// hands, the button is the only way back to the menu. Hidden in the settings, the
    /// menu opens from the palm menu, the hand gesture or the controller.
    pub fn place_button(&mut self, s: &Settings) {
        if !s.menu_button {
            self.button = None;
            return;
        }
        let shown = |id: &DisplayKey| self.placements.contains_key(id);
        let anchor = self
            .primary
            .filter(shown)
            .or_else(|| self.lineup.iter().map(|m| m.0).find(|id| shown(id)));
        let mirror = self
            .mirror
            .filter(|id| self.streams.contains_key(id))
            .and_then(|id| Some((*self.placements.get(&id)?, self.height_of(id))));
        self.button = anchor.and_then(|id| {
            let p = self.placements.get(&id)?;
            let h = self.height_of(id);
            let mut x = -p.width / 2.0 + BUTTON_SIZE;
            if let Some((m, mh)) = mirror {
                let corner = m.position + m.rotation * Vec3::new(-m.width / 2.0, mh / 2.0, 0.0);
                let local = p.rotation.inverse() * (corner - p.position);
                // Only when the mirror actually hangs under this monitor.
                if local.y < -h / 2.0 + 0.05 && local.y > -h / 2.0 - 0.3 {
                    x = x.min(local.x - BUTTON_SIZE * 0.8);
                }
            }
            let offset = Vec3::new(x, -h / 2.0 - BUTTON_SIZE * 0.9, 0.02);
            Some(Placement {
                position: p.position + p.rotation * offset,
                rotation: p.rotation,
                width: BUTTON_SIZE,
            })
        });
    }

    /// Where the "Attach" button under detached window `id` is (as drawn, while it
    /// glides).
    pub fn attach_place(&self, id: DisplayKey) -> Option<Placement> {
        let p = self.shown.get(&id).or_else(|| self.placements.get(&id))?;
        let down = self.height_at(id, p)? / 2.0 + ATTACH_GAP;
        Some(Placement {
            position: p.position + p.rotation * Vec3::new(0.0, -down, 0.005),
            rotation: p.rotation,
            width: ATTACH_WIDTH,
        })
    }

    // ---- What is seen and pointed at ----

    /// What the ray points at, as drawn. The compositor stacks its layers with no
    /// depth of their own, so of the monitors the one on top is what the person sees
    /// where they overlap, even a little further away (one glued over a row and kept
    /// over it, see `layout::DRAW_HOLD`): the first hit from the top of the stack wins.
    /// Of the rest and that monitor the nearest hit wins, and of two as near the one
    /// higher in the stack: the palm menu, "Attach", the menu, its button, the
    /// placeholder, the monitors (flat ones over curved ones, each far to near), the
    /// keyboard window (under the monitors). A monitor is hit where it is drawn (see
    /// `shown`). `hidden`: monitors not shown (asked to go). Nothing is allocated.
    pub fn pick(
        &self,
        ray: Ray,
        s: &Settings,
        can: Pickable,
        hidden: impl Fn(DisplayKey) -> bool,
    ) -> Option<Target> {
        let mut best: Option<Target> = None;
        let mut consider = |thing: Thing, hit: Option<Hit>| {
            if let Some(hit) = hit
                && best.is_none_or(|b| hit.t < b.hit.t)
            {
                best = Some(Target { thing, hit });
            }
        };
        if let Some(w) = &self.wrist {
            consider(
                Thing::Wrist,
                layout::hit_surface(ray, w, wrist_menu_height(), None),
            );
        }
        if let Some(id) = can.attach
            && let Some(b) = self.attach_place(id)
        {
            consider(
                Thing::Attach(id),
                layout::hit_surface(ray, &b, attach_height(), None),
            );
        }
        if can.menu {
            consider(
                Thing::Menu,
                layout::hit_surface(ray, &self.menu, panel_height(s.large_menu), None),
            );
        }
        if let Some(b) = &self.button {
            consider(
                Thing::MenuButton,
                layout::hit_surface(ray, b, BUTTON_SIZE, None),
            );
        }
        if let Some(a) = &self.adding {
            consider(
                a.thing(),
                layout::hit_surface(ray, &a.place, a.height(), None),
            );
        }
        if let Some(m) = self.top_monitor(ray, s, &hidden) {
            consider(Thing::Display(m.id), Some(m.hit));
        }
        if s.keyboard_window
            && let Some(k) = &self.keyboard
        {
            consider(
                Thing::Keyboard,
                layout::hit_surface(ray, k, k.width * KEYBOARD_ASPECT, None),
            );
        }
        best
    }

    /// The monitor the ray meets highest in the compositor's stack: the flat ones (all
    /// of them, or the window panels when the monitors are curved) over the curved
    /// ones, each drawn far to near (see `order_drawing`), so walked from the last
    /// drawn. As drawn last (see `drawn_at`). Nothing is allocated.
    fn top_monitor(
        &self,
        ray: Ray,
        s: &Settings,
        hidden: impl Fn(DisplayKey) -> bool,
    ) -> Option<Seen> {
        let curve = self.curve_radius(s);
        let flat = |id: DisplayKey| curve.is_none() || self.is_window(id);
        [true, false].into_iter().find_map(|quads| {
            self.drawn
                .iter()
                .rev()
                .filter(|id| flat(**id) == quads)
                .find_map(|&id| {
                    let (p, height) = self.drawn_at(id, &hidden)?;
                    let curve = curve.filter(|_| !quads);
                    let hit = layout::hit_surface(ray, p, height, curve)?;
                    Some(Seen {
                        id,
                        hit,
                        place: *p,
                        height,
                        curve,
                    })
                })
        })
    }

    /// Monitor `id` as drawn last, if it still stands (streams, has a place, is not
    /// `hidden`): where, and how high.
    fn drawn_at(
        &self,
        id: DisplayKey,
        hidden: impl Fn(DisplayKey) -> bool,
    ) -> Option<(&Placement, f32)> {
        if hidden(id) || !self.placements.contains_key(&id) {
            return None;
        }
        let p = self.shown.get(&id)?;
        Some((p, self.height_at(id, p)?))
    }

    /// The monitor looked at along `ray` (the gaze), as drawn and stacked (see
    /// `top_monitor`), and the point looked at across its image (0..=1 from the top
    /// left); none when the open menu (`menu`) is in front of it. Nothing is
    /// allocated.
    pub fn looked_at(
        &self,
        ray: Ray,
        s: &Settings,
        menu: bool,
        hidden: impl Fn(DisplayKey) -> bool,
    ) -> Option<(DisplayKey, Vec2)> {
        let looked = self.top_monitor(ray, s, &hidden);
        // Looking at the menu, not past it.
        let at_menu = menu
            && layout::hit_surface(ray, &self.menu, panel_height(s.large_menu), None)
                .is_some_and(|m| looked.is_none_or(|l| m.t < l.hit.t));
        let m = looked.filter(|_| !at_menu)?;
        Some((
            m.id,
            layout::image_uv(&m.place, m.height, m.curve, m.hit.local),
        ))
    }

    /// The monitor most straight ahead of the head (not a window panel, nor one
    /// `hidden`); of two as straight ahead, the one of the smaller number.
    pub fn monitor_in_front(
        &self,
        (head, forward): (Vec3, Vec3),
        hidden: impl Fn(DisplayKey) -> bool,
    ) -> Option<DisplayKey> {
        self.placements
            .iter()
            .filter(|(id, _)| {
                !self.is_window(**id) && self.streams.contains_key(*id) && !hidden(**id)
            })
            .max_by(|a, b| {
                let score = |p: &Placement| (p.position - head).normalize_or_zero().dot(forward);
                score(a.1).total_cmp(&score(b.1)).then(b.0.cmp(a.0))
            })
            .map(|(id, _)| *id)
    }

    /// Where the map looks from, and how to turn the room so it looks straight ahead:
    /// the eyes (`eye`, else where the arrangement is centred), facing the
    /// arrangement; lying down, along the band tilted up overhead (seen from below,
    /// right over the head, left and right would make no sense), as the desktop does
    /// it.
    fn map_view(&self, eye: Option<Vec3>) -> (Vec3, Quat) {
        (
            eye.unwrap_or(self.center.0),
            Quat::from_rotation_x(-self.tilt) * Quat::from_rotation_y(-self.center.1),
        )
    }

    /// Where monitor `id` stands as seen from `eye`, for the menu.
    pub fn place_of(&self, id: DisplayKey, eye: Option<Vec3>) -> ui::Place {
        let Some(p) = self.placements.get(&id) else {
            return ui::Place::Unknown;
        };
        let (eye, unturn) = self.map_view(eye);
        let local = unturn * (p.position - eye);
        let side = local.x.atan2(-local.z);
        let up = local.y.atan2(Vec2::new(local.x, local.z).length());
        if up > 0.35 {
            ui::Place::Above
        } else if up < -0.35 {
            ui::Place::Below
        } else if side < -0.26 {
            ui::Place::Left
        } else if side > 0.26 {
            ui::Place::Right
        } else {
            ui::Place::Center
        }
    }

    // ---- A frame ----

    /// Moves the drawn monitors towards their places (held ones, and with "move all
    /// together" the ones going along, follow at once) and fades new ones in.
    pub fn animate(&mut self, s: &Settings, dt: f32) {
        self.moving_now(s);
        // About a quarter of a second to settle.
        let k = 1.0 - (-dt * 12.0).exp();
        for (id, target) in &self.placements {
            match self.shown.get_mut(id) {
                Some(drawn) if !self.moving.contains(id) => {
                    drawn.position = drawn.position.lerp(target.position, k);
                    drawn.rotation = drawn.rotation.slerp(target.rotation, k);
                    drawn.width += (target.width - drawn.width) * k;
                    if drawn.position.distance(target.position) < 0.0005 {
                        *drawn = *target;
                    }
                }
                Some(drawn) => *drawn = *target,
                None => {
                    self.shown.insert(*id, *target);
                }
            }
        }
        let placements = &self.placements;
        self.shown.retain(|id, _| placements.contains_key(id));
        for id in self.streams.keys() {
            let a = self.appear.entry(*id).or_insert(0.0);
            *a = (*a + dt / 0.3).min(1.0);
        }
        let streams = &self.streams;
        self.appear.retain(|id, _| streams.contains_key(id));
    }

    /// The monitors in the order to draw them, far from `head` to near, two at about
    /// one distance (a monitor glued over a row and that row) in the order they had,
    /// so where they overlap never flickers; one in a hand last. `hidden`: not drawn
    /// (asked to go). Kept in memory from frame to frame.
    pub fn order_drawing(
        &mut self,
        head: Vec3,
        hidden: impl Fn(DisplayKey) -> bool,
    ) -> &[(f32, DisplayKey)] {
        let mut order = std::mem::take(&mut self.draw_order);
        order.clear();
        let held = self.held;
        order.extend(
            self.streams
                .keys()
                // Gone from view as soon as asked to go; back if the PC does not remove it.
                .filter(|id| !hidden(**id))
                .filter_map(|id| {
                    let d = self.shown.get(id)?.position.distance(head);
                    let in_hand = held.contains(&Some(Thing::Display(*id)));
                    Some((if in_hand { -1.0 } else { d }, *id))
                }),
        );
        layout::draw_order(&mut order, &self.drawn);
        self.drawn.clear();
        self.drawn.extend(order.iter().map(|o| o.1));
        self.draw_order = order;
        &self.draw_order
    }

    /// Focus mode: monitors without the mouse (on `focus`, as the PC says) fade to a
    /// dim level, the one in a hand never.
    pub fn brighten(&mut self, s: &Settings, dt: f32, focus: Option<DisplayKey>) {
        for id in self.streams.keys() {
            let in_hand = self.held.contains(&Some(Thing::Display(*id)));
            let target = match focus {
                Some(f) if s.focus && f != *id && !in_hand => DIMMED,
                _ => 1.0,
            };
            let b = self.brightness.entry(*id).or_insert(1.0);
            let step = dt * 5.0;
            *b = if *b < target {
                (*b + step).min(target)
            } else {
                (*b - step).max(target)
            };
        }
    }
}
