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

use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;
use std::time::{Duration, Instant};

use glam::{Quat, Vec2, Vec3};
use log::info;
use prostir_proto::state::{DisplayId, DisplayKind, DisplayState, Displays, Mode, ReqId, Row};

use crate::clarity::{self, Dev, Quality};
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
/// A monitor with more than this share of it hidden behind another is hidden (1 %;
/// a seam seen at a slant hides less).
const HIDDEN: f64 = 0.01;

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

/// What the world keeps of one PC (step 7 note, 4.5: what belongs to each PC): whether
/// it is in a session, its list in that session, its monitors' order and its primary as
/// it says, its own display shown, and the desktop it was told and is to be told.
#[derive(Default)]
struct PcView {
    /// In a session with it.
    connected: bool,
    /// It said its list in this session.
    has_list: bool,
    /// Its monitors left to right, shared with its desktop.
    order: Vec<DisplayKey>,
    primary: Option<DisplayKey>,
    /// Its own displays shown, in its list's order, with where each is on its desktop
    /// (x, y, width, height, where known): placed as one group as they stand there.
    physical: Vec<(DisplayKey, Option<[i32; 4]>)>,
    /// Its monitors and its own displays shown that stream, left to right, with their
    /// stream sizes: what its desktop is made of (see `relist`).
    sizes: Vec<(DisplayKey, u32, u32)>,
    /// The desktop last sent to it.
    sent_layout: Option<Desktop<DisplayKey>>,
    /// A changed desktop waiting to settle before it is sent.
    pending_layout: Option<(Desktop<DisplayKey>, Instant)>,
    /// The desktop for the arrangement now, kept while nothing moves.
    desktop_memo: DesktopMemo<DisplayKey>,
    /// The rows it is to be told, until the app sends them.
    arrange: Option<Vec<Row>>,
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
    /// Its monitor goes into the main row, whose monitors make room only once it is
    /// there: `place` is only where it waits (see `World::waiting_place`).
    into_row: bool,
    since: Instant,
    /// Where the head looked when it was asked for: its place is near there.
    gaze: Vec3,
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

/// How sharp a monitor's layer is shown (see [`World::layer_quality`]): worked out
/// again only when its place, its stream's width, the eye, its curve or the choices of
/// "For development" change.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Clear {
    /// What it was worked out for.
    made_for: (Placement, u32, Vec3, Option<f32>, Dev),
    /// Headset pixels per stream pixel at its middle (`None`: not known).
    scale: Option<f32>,
    quality: Quality,
}

/// The arrangement before a size of "For development" laid the monitors out again,
/// put back by its "Reset".
#[cfg(feature = "preview")]
#[derive(Debug, Clone)]
struct BeforeDev {
    placements: HashMap<DisplayKey, Placement>,
    radius: f32,
    tilt: f32,
    sized: layout::Sizes,
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
    // What there is, as the PCs and the streams say.
    /// Each PC's own (see [`PcView`]).
    pcs: BTreeMap<PcId, PcView>,
    /// The PC of the last session that began: the main one while none is connected
    /// (see [`World::pc`]).
    last: Option<PcId>,
    /// Cinema: one screen shown, the desk arrangement kept aside (see `set_cinema`).
    cinema: bool,
    /// The PCs' displays as their lists last said (none of a PC outside its session).
    listed: HashMap<DisplayKey, Listed>,
    /// Every stream shown, and its size in pixels.
    streams: HashMap<DisplayKey, PixelSize>,
    /// The runtime draws curved layers.
    curve_supported: bool,
    /// The monitors left to right (each PC's order first, by PC, those no order has
    /// after, by number), and the same with the main PC's laptop screen: made again
    /// whenever what they are made of changes, read every frame.
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
    /// The headset's pixels per degree, where its model is known (see
    /// `clarity::pixels_per_degree`): monitors are made "1:1" by them.
    ppd: Option<f32>,
    /// How large new monitors are made: as the last preset made them (see
    /// `clarity::preset_sizes`), or as those standing are when the arrangement came
    /// from the file or a profile (see `World::sized_from_places`).
    sized: layout::Sizes,
    /// The choices of "For development" (see `clarity::Dev`).
    dev: Dev,
    /// The arrangement before a size of "For development" laid it out again.
    #[cfg(feature = "preview")]
    before_dev: Option<BeforeDev>,
    /// How sharp each monitor's layer is (see [`World::layer_quality`]).
    clear: HashMap<DisplayKey, Clear>,
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
    /// The PC its request went to: its answers are known by that PC and the number.
    switch_pc: Option<PcId>,

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
    /// A display in a hand moved, or the one resized changed size, since it was taken.
    dragged: bool,
    /// The arrangement was changed by hand since the app last asked: a display moved
    /// or resized, "Line up", two swapped, a layout chosen (see
    /// [`World::take_rearranged`]).
    rearranged: bool,

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
    /// ends. `ppd`: the headset's pixels per degree, where its model is known.
    pub fn new(
        desk: Desk,
        keyboard: Option<Placement>,
        curve_supported: bool,
        ppd: Option<f32>,
        s: &Settings,
    ) -> Self {
        let dev = Dev::default();
        let mut world = Self {
            pcs: BTreeMap::new(),
            last: None,
            cinema: s.cinema,
            listed: HashMap::new(),
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
            ppd,
            sized: clarity::start_sizes(ppd, dev.size),
            dev,
            #[cfg(feature = "preview")]
            before_dev: None,
            clear: HashMap::new(),
            tilt: layout::tilt(s.preset),
            placed_for: HashMap::new(),
            desk,
            draw_order: Vec::new(),
            drawn: Vec::new(),
            moving: Vec::new(),
            following: false,
            follow_hold: false,
            adding: None,
            switch: None,
            switch_pc: None,
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
            dragged: false,
            rearranged: false,
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

    /// The main PC: the first one in a session, else the one of the last session.
    /// Its primary and its own display shown are those the presets lay out around.
    pub fn pc(&self) -> Option<PcId> {
        self.pcs
            .iter()
            .find(|(_, v)| v.connected)
            .map(|(pc, _)| *pc)
            .or(self.last)
    }

    fn main(&self) -> Option<&PcView> {
        self.pcs.get(&self.pc()?)
    }

    fn main_mut(&mut self) -> Option<&mut PcView> {
        let pc = self.pc()?;
        self.pcs.get_mut(&pc)
    }

    /// PC `pc` is in a session and said its list in it.
    fn has_list(&self, pc: PcId) -> bool {
        self.pcs.get(&pc).is_some_and(|v| v.has_list)
    }

    /// A window panel.
    pub fn is_window(&self, id: DisplayKey) -> bool {
        window(&self.listed, id)
    }

    /// The main PC's own displays shown in the headset, in its list's order, with
    /// where each is on its desktop.
    pub fn physical(&self) -> &[(DisplayKey, Option<[i32; 4]>)] {
        self.main().map_or(&[], |v| v.physical.as_slice())
    }

    /// The main PC's primary display, as it says.
    pub fn primary(&self) -> Option<DisplayKey> {
        self.main()?.primary
    }

    /// A stream of display `id` shows.
    pub fn streams(&self, id: DisplayKey) -> bool {
        self.streams.contains_key(&id)
    }

    /// The displays of PC `pc` whose streams show.
    pub fn streams_of(&self, pc: PcId) -> impl Iterator<Item = DisplayKey> + '_ {
        self.streams.keys().copied().filter(move |k| k.pc == pc)
    }

    /// The window panels whose streams show, with their sizes in pixels (in no order).
    pub fn windows(&self) -> impl Iterator<Item = (DisplayKey, PixelSize)> + '_ {
        self.streams
            .iter()
            .filter(|(id, _)| self.is_window(**id))
            .map(|(id, size)| (*id, *size))
    }

    /// The monitors left to right: in the PC's order, unknown ones at the end, with
    /// their stream sizes.
    pub fn monitors(&self) -> &[(DisplayKey, u32, u32)] {
        &self.lineup
    }

    /// The main PC's own displays shown whose streams show, with their stream sizes
    /// and where each is on its desktop.
    fn physical_sizes(&self) -> Vec<layout::Physical<DisplayKey>> {
        self.physical()
            .iter()
            .filter_map(|&(id, desk)| self.streams.get(&id).map(|&(w, h)| (id, w, h, desk)))
            .collect()
    }

    /// The monitors and the PC's own displays kept left to right again, after what they
    /// are made of changed (nothing is allocated once their buffers are large enough).
    fn relist(&mut self) {
        self.lineup.clear();
        for view in self.pcs.values() {
            for &id in &view.order {
                if let Some(&(w, h)) = self.streams.get(&id) {
                    self.lineup.push((id, w, h));
                }
            }
        }
        let known = self.lineup.len();
        for (&id, &(w, h)) in &self.streams {
            if self
                .listed
                .get(&id)
                .is_some_and(|l| l.kind == Kind::Monitor)
                && !self.pcs.get(&id.pc).is_some_and(|v| v.order.contains(&id))
            {
                self.lineup.push((id, w, h));
            }
        }
        if let Some(rest) = self.lineup.get_mut(known..) {
            rest.sort_unstable();
        }
        self.sizes.clear();
        self.sizes.extend_from_slice(&self.lineup);
        let main = self.pc();
        let streams = &self.streams;
        if let Some(view) = main.and_then(|pc| self.pcs.get(&pc)) {
            for (id, _) in &view.physical {
                if let Some(&(w, h)) = streams.get(id) {
                    self.sizes.push((*id, w, h));
                }
            }
        }
        // Each PC's desktop is made of its own displays.
        for (pc, view) in &mut self.pcs {
            view.sizes.clear();
            view.sizes
                .extend(self.lineup.iter().filter(|m| m.0.pc == *pc).copied());
            for (id, _) in &view.physical {
                if let Some(&(w, h)) = streams.get(id) {
                    view.sizes.push((*id, w, h));
                }
            }
        }
    }

    /// A session with PC `pc` began: its desktop is told afresh, by the rows it had (the
    /// last session's, or after a start those `layout.txt` keeps): nothing moved, the
    /// same rows, so its desktop is not rearranged.
    pub fn connected(&mut self, pc: PcId) {
        let kept = self.desk.rows(pc);
        let view = self.pcs.entry(pc).or_default();
        view.connected = true;
        let rows = view
            .sent_layout
            .take()
            .map_or_else(|| kept.to_vec(), |d| d.rows);
        view.sent_layout = Desktop::history(rows);
        view.pending_layout = None;
        self.last = Some(pc);
    }

    /// The session with PC `pc` ended. The places stay (the same PC comes back with
    /// the same numbers); its next session's list is news, not displays removed, and a
    /// profile being switched to is not any more.
    pub fn disconnected(&mut self, pc: PcId) {
        if let Some(view) = self.pcs.get_mut(&pc) {
            view.connected = false;
            view.order.clear();
            view.has_list = false;
            view.physical.clear();
            view.pending_layout = None;
            view.arrange = None;
        }
        self.listed.retain(|k, _| k.pc != pc);
        // Its streams went with the connection (the app drops their decoders).
        self.streams.retain(|k, _| k.pc != pc);
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
        pc: PcId,
        displays: &Displays,
        s: &Settings,
        head: (Vec3, Vec3),
        now: Instant,
    ) -> &[DisplayKey] {
        self.gone.clear();
        if !self.pcs.get(&pc).is_some_and(|v| v.connected) {
            return &self.gone;
        }
        let key = |id: DisplayId| DisplayKey::new(pc, id);
        for (k, l) in &self.listed {
            if k.pc == pc && l.kind != Kind::Physical && displays.get(k.id).is_none() {
                self.gone.push(*k);
            }
        }
        self.listed.retain(|k, _| k.pc != pc);
        for d in &displays.displays {
            let kind = match &d.kind {
                DisplayKind::Virtual => Kind::Monitor,
                DisplayKind::Window { .. } => Kind::Window,
                DisplayKind::Physical { .. } => Kind::Physical,
            };
            let failed = matches!(d.state, DisplayState::Failed { .. });
            self.listed.insert(key(d.id), Listed { kind, failed });
        }
        let view = self.pcs.entry(pc).or_default();
        view.has_list = true;
        view.physical.clear();
        view.physical.extend(
            displays
                .displays
                .iter()
                .filter(|d| d.is_shown_physical())
                .map(|d| (key(d.id), d.desk.map(physical::desk_rect))),
        );
        view.order.clear();
        view.order.extend(
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
        if primary != view.primary {
            info!("primary display: {primary:?}");
            view.primary = primary;
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
            && self.fit_to_stream(id, size)
        {
            // Larger now: the neighbours it reaches into make room along the row.
            let group = self.placed_group();
            layout::make_room(
                &mut self.placements,
                &self.sizes,
                (self.center, self.tilt),
                s.preset == Preset::Wall,
                &group,
                id,
            );
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

    /// Follow mode is moving the monitors after the head right now.
    pub fn following(&self) -> bool {
        self.following
    }

    /// The monitor resized with both hands.
    pub fn resizing(&self) -> Option<DisplayKey> {
        self.resizing
    }

    /// Whether the arrangement was changed by hand since this was last asked: the
    /// profile it came from is not how it stands any more.
    pub fn take_rearranged(&mut self) -> bool {
        std::mem::take(&mut self.rearranged)
    }

    /// The files changed since this was last asked, to be written after this pass.
    pub fn take_saves(&mut self) -> Dirty {
        std::mem::take(&mut self.saves)
    }

    /// The rows a PC is to be told now (see [`World::settle`]), once: which PC, and
    /// the rows of its own displays.
    pub fn take_arrange(&mut self) -> Option<(PcId, Vec<Row>)> {
        self.pcs
            .iter_mut()
            .find_map(|(pc, v)| v.arrange.take().map(|rows| (*pc, rows)))
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
        if self.cinema {
            return;
        }
        // Each PC in a session in turn, by number (nothing allocated in the frame).
        let mut changed = false;
        let mut after = None;
        loop {
            let from = after.map_or(Bound::Unbounded, Bound::Excluded);
            let Some(pc) = self
                .pcs
                .range((from, Bound::Unbounded))
                .find(|(_, v)| v.connected)
                .map(|(pc, _)| *pc)
            else {
                break;
            };
            changed |= self.send_layout_of(pc, now);
            after = Some(pc);
        }
        if changed {
            self.relist();
        }
    }

    /// [`World::send_layout`] for PC `pc`, with its own displays: whether its order
    /// changed.
    fn send_layout_of(&mut self, pc: PcId, now: Instant) -> bool {
        let Self {
            pcs,
            placements,
            streams,
            center,
            tilt,
            listed,
            desk,
            saves,
            ..
        } = self;
        let Some(view) = pcs.get_mut(&pc) else {
            return false;
        };
        // Wait until every monitor the PC has is placed: a partial picture would make
        // the desktop rearrange (and blink) once per arriving monitor. One that could
        // not start does not hold the others up: it keeps its place if it had one, and
        // one that never had a place gets one only once it streams.
        let due =
            |id: &DisplayKey| streams.contains_key(id) || listed.get(id).is_some_and(|l| !l.failed);
        let complete = view.order.iter().any(|id| streams.contains_key(id))
            && view
                .order
                .iter()
                .filter(|id| due(id))
                .all(|id| placements.contains_key(id));
        if !complete {
            return false;
        }
        // The laptop screen too: the PC puts it where it stands here. Monitors stay in
        // the rows the PC has until clearly moved out of them, and its desktop stays
        // while it fits as well as a new one would.
        let desktop = {
            let desktop = view.desktop_memo.get_ref(
                placements,
                &view.sizes,
                *center,
                *tilt,
                view.primary,
                view.sent_layout.as_ref(),
            );
            if desktop.rows.is_empty() || view.sent_layout.as_ref() == Some(&*desktop) {
                view.pending_layout = None;
                return false;
            }
            // Only an arrangement that has held for a moment.
            match &view.pending_layout {
                Some((pending, since)) if *pending == *desktop => {
                    if now.saturating_duration_since(*since) < ARRANGE_AFTER {
                        return false;
                    }
                }
                _ => {
                    view.pending_layout = Some((desktop.clone(), now));
                    return false;
                }
            }
            desktop.clone()
        };
        view.pending_layout = None;
        let rows = desktop.wire(|k| (k.pc == pc).then_some(k.id));
        info!(
            "desktop arrangement of PC {pc}: {}",
            rows.iter()
                .map(|r| {
                    let ids: Vec<u32> = r.ids().map(|id| id.0).collect();
                    let centers: Vec<Option<i32>> = r.entries.iter().map(|e| e.center).collect();
                    format!("{ids:?}@{centers:?}")
                })
                .collect::<Vec<_>>()
                .join(" / ")
        );
        view.order.clear();
        view.order.extend(
            rows.iter()
                .flat_map(Row::ids)
                .map(|id| DisplayKey::new(pc, id))
                .filter(|id| listed.get(id).is_some_and(|l| l.kind == Kind::Monitor)),
        );
        // Kept for the next start too.
        if desk.keep_rows(pc, &desktop.rows) {
            saves.mark(File::Desk);
        }
        view.sent_layout = Some(desktop);
        view.arrange = Some(rows);
        true
    }

    /// Keeps the desktops' arrangements in step once nothing is being moved: no
    /// monitor in a hand, none resized, no profile being switched to.
    pub fn settle(&mut self, now: Instant) {
        if !self.carried() && !self.switching() {
            self.send_layout(now);
        }
    }

    /// The monitors on the map: the desktop the main PC gets (see `layout::Desktop`;
    /// look one up with `Desktop::spot`). While a monitor is in a hand or resized the
    /// PC is not told anything new, and the map shows the desktop as it was until the
    /// hand lets go (nothing is worked out, nor allocated, in the frames of a drag).
    pub fn map_spots(&self) -> Option<std::cell::Ref<'_, Desktop<DisplayKey>>> {
        let view = self.main()?;
        if self.carried()
            && let Some(last) = view.desktop_memo.last()
        {
            return Some(last);
        }
        Some(view.desktop_memo.get_ref(
            &self.placements,
            &view.sizes,
            self.center,
            self.tilt,
            view.primary,
            view.sent_layout.as_ref(),
        ))
    }

    /// A monitor is in a hand, or resized with both.
    fn carried(&self) -> bool {
        self.held.iter().flatten().any(|t| t.display().is_some()) || self.resizing.is_some()
    }

    /// The rows the main PC's desktop is laid out by, by id: those last sent, or those
    /// a profile switched to was saved with until its desktop is sent (see
    /// `rows_from`). Their neighbours stay neighbours until clearly moved apart
    /// (`rows::rows_of`).
    fn sent_rows(&self) -> &[Vec<DisplayKey>] {
        self.main()
            .and_then(|v| v.sent_layout.as_ref())
            .map_or(&[], |d| &d.rows)
    }

    /// A profile switched to is another arrangement: its rows are those it was saved
    /// with (by the PC's numbers now), or, for a profile without them, start afresh
    /// (see `layout::Desktop::history`).
    fn rows_from(&mut self, rows: Vec<Vec<DisplayKey>>) {
        if let Some(view) = self.main_mut() {
            view.sent_layout = Desktop::history(rows);
            view.pending_layout = None;
        }
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

    /// Puts the PC's own displays shown back under the primary monitor as one group
    /// (see `layout::arrange`), leaving the rest alone: all of them, as they stand to
    /// each other on the PC's desktop (one more of them shown changes the group).
    fn place_physical(&mut self, s: &Settings) {
        let laid = self.physical_laid(s);
        self.placements.extend(laid);
    }

    /// The PC's own displays that show, laid out as one group under the primary
    /// monitor (see `layout::arrange`).
    fn physical_laid(&self, s: &Settings) -> Vec<(DisplayKey, Placement)> {
        let physical = self.physical_sizes();
        let laid = self.around_user(layout::arrange_sized(
            s.preset,
            &self.lineup,
            self.primary(),
            &physical,
            self.curve_factor(s),
            self.sized,
        ));
        physical
            .iter()
            .filter_map(|(id, ..)| Some((*id, *laid.get(id)?)))
            .collect()
    }

    /// The PC's own displays that show and have no place yet get one in the group: all
    /// of them where none has one (under the primary monitor, and further down if a
    /// monitor stands there: they go through none); else each where the group puts it
    /// next to one placed already, wherever the person moved that one (see
    /// `layout::kept_to`). Those placed stay where they are.
    fn place_new_physical(&mut self, s: &Settings) {
        let mut laid = self.physical_laid(s);
        let placed = laid
            .iter()
            .find_map(|(id, at)| Some((*id, *at, *self.placements.get(id)?)));
        let anchor = placed.map(|(_, from, to)| (from, to));
        let frame = layout::frame(self.center, self.tilt);
        let height = |id: &DisplayKey, p: &Placement| self.height_at(*id, p).unwrap_or(0.0);
        let others = self.placed_heights(&self.lineup);
        let anchor_clear = placed.is_some_and(|(id, _, to)| {
            let h = height(&id, &to);
            !others.iter().any(|(o, oh)| layout::cuts(&to, h, o, *oh))
        });
        if anchor.is_none() {
            let down = laid
                .iter()
                .map(|(id, p)| layout::clear_along_up(*p, height(id, p), &others, &frame, -1.0))
                .fold(0.0f32, f32::max);
            for (_, p) in &mut laid {
                *p = layout::lifted(*p, &frame, -down);
            }
        }
        for (id, at) in laid {
            if self.placements.contains_key(&id) {
                continue;
            }
            let mut p = anchor.map_or(at, |(from, to)| layout::kept_to(at, from, to));
            // Next to one of the group that goes through no monitor, where a monitor
            // stands: further down, out of its way. A group put over monitors by hand
            // is the person's to move.
            if anchor_clear {
                let h = self.height_at(id, &p).unwrap_or(0.0);
                let down = layout::clear_along_up(p, h, &others, &frame, -1.0);
                p = layout::lifted(p, &frame, -down);
            }
            self.placements.insert(id, p);
        }
    }

    /// The main PC's own displays that show and have a place: the group they stand in,
    /// moved as one piece (see `layout::make_room`).
    fn placed_group(&self) -> Vec<DisplayKey> {
        self.physical()
            .iter()
            .map(|p| p.0)
            .filter(|id| self.streams.contains_key(id) && self.placements.contains_key(id))
            .collect()
    }

    /// Where each of `monitors` that has a place stands, and how high it is.
    fn placed_heights(&self, monitors: &[(DisplayKey, u32, u32)]) -> Vec<(Placement, f32)> {
        monitors
            .iter()
            .filter_map(|m| {
                let p = *self.placements.get(&m.0)?;
                Some((p, self.height_at(m.0, &p)?))
            })
            .collect()
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
            let (listed, pc) = (&self.listed, self.pc());
            self.placements
                .retain(|k, _| window(listed, *k) || pc.is_some_and(|pc| pc != k.pc));
            self.placements.extend(view);
            self.held = [None, None];
            return;
        }
        if self.lineup.is_empty() {
            return;
        }
        let physical = self.physical_sizes();
        let (primary, curve) = (self.primary(), self.curve_factor(s));
        self.sized = clarity::preset_sizes(self.ppd, self.dev.size, |sized| {
            layout::largest_scale(s.preset, &self.lineup, primary, &physical, curve, sized)
        });
        self.radius = layout::preset_radius_sized(s.preset, &self.lineup, primary, self.sized);
        self.tilt = layout::tilt(s.preset);
        let laid = self.around_user(layout::arrange_sized(
            s.preset,
            &self.lineup,
            primary,
            &physical,
            curve,
            self.sized,
        ));
        self.placements.extend(laid);
        self.held = [None, None];
        self.save_layout();
        self.send_layout(now);
    }

    /// Another preset was chosen (`s` says which): the arrangement's tilt is its, and
    /// everything is laid out by it.
    pub fn preset_chosen(&mut self, s: &Settings, now: Instant) {
        self.rearranged = true;
        self.tilt = layout::tilt(s.preset);
        self.arrange_all(s, now);
    }

    /// Another curve was chosen: flat, the PC's own displays hang tilted, curved they
    /// continue the cylinder.
    pub fn curve_chosen(&mut self, s: &Settings) {
        if !self.cinema {
            self.place_physical(s);
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
            // Where its placeholder stood, for the monitor being added, while nothing
            // took that place meanwhile.
            let (at, push) = match &self.adding {
                Some(a)
                    if a.id == Some(m.0)
                        && !a.into_row
                        && !layout::covers_any(
                            &self.placements,
                            &self.sizes,
                            m.0,
                            (m.1, m.2),
                            &a.place,
                            (self.center, self.tilt),
                        ) =>
                {
                    (a.place, false)
                }
                _ => self.place_for_new((m.1, m.2), gaze),
            };
            info!("monitor {} placed next to the others", m.0);
            self.placements.insert(m.0, at);
            if push {
                // Into the main row: its neighbours there make room along it. What
                // that lifts out of its way out of view (a row over them, out from
                // behind it), the whole further away brings back.
                self.make_room_for(s, m.0);
                self.further_into_view(s, &[]);
            }
            placed = true;
            // A placeholder still waiting (another monitor asked for meanwhile) moves
            // to where its monitor goes now, out of the way of this one.
            if let Some(a) = self.adding.filter(|a| a.id != Some(m.0)) {
                let (place, into_row) = self.waiting_place(a.size, a.gaze);
                if let Some(a) = &mut self.adding {
                    a.place = place;
                    a.into_row = into_row;
                }
            }
        }
        if self
            .physical()
            .iter()
            .any(|(id, _)| self.streams.contains_key(id) && !self.placements.contains_key(id))
        {
            self.place_new_physical(s);
            placed = true;
        }
        if placed {
            self.save_layout();
        }
    }

    /// Where a new monitor of `size` pixels goes next to the others (it has no place
    /// yet), near where the head looks (`gaze`), in view; `true` when that is in the
    /// main row and its neighbours there are to make room (see `layout::place_in_view`):
    /// any monitor, as over a row it may be out of view.
    fn place_for_new(&self, size: (u32, u32), gaze: Vec3) -> (Placement, bool) {
        self.place_in_view(size, gaze, true)
    }

    /// Where the placeholder of a new monitor of `size` pixels waits for its picture:
    /// where the monitor goes (see [`Self::place_for_new`]); but where that is in the
    /// main row, whose monitors make room only once it comes, where it covers nothing
    /// (see `layout::place_waiting`), and then `true`.
    fn waiting_place(&self, size: (u32, u32), gaze: Vec3) -> (Placement, bool) {
        let (place, into_row) = self.place_for_new(size, gaze);
        if !into_row {
            return (place, false);
        }
        let waits = layout::place_waiting(
            &self.placements,
            &self.sizes,
            size,
            self.center,
            self.tilt,
            gaze,
            self.sent_rows(),
            self.circle(),
            place,
        );
        (waits, true)
    }

    /// [`Self::place_for_new`]; `any`: any monitor may go into the main row, not only
    /// one too wide for its circle.
    fn place_in_view(&self, size: (u32, u32), gaze: Vec3, any: bool) -> (Placement, bool) {
        layout::place_in_view(
            &self.placements,
            &self.sizes,
            size,
            self.center,
            self.tilt,
            gaze,
            self.sent_rows(),
            self.circle(),
            any,
        )
    }

    /// The circle new monitors go on, and how large they are made.
    fn circle(&self) -> layout::Circle {
        layout::Circle {
            radius: self.radius,
            sizes: self.sized,
        }
    }

    /// The monitors beside `id` in its row that it covers are pushed aside along the
    /// row, the PC's own displays as one piece, and those over or under the row that
    /// then go through one of them or are seen behind one are lifted clear (see
    /// `layout::make_room`).
    fn make_room_for(&mut self, s: &Settings, id: DisplayKey) {
        let group = self.placed_group();
        layout::make_room(
            &mut self.placements,
            &self.sizes,
            (self.center, self.tilt),
            s.preset == Preset::Wall,
            &group,
            id,
        );
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
        let per_pixel = self.sized.others.metres_per_pixel(width_px, self.radius);
        let width = (width_px as f32 * per_pixel * DISTANCE / self.radius).clamp(0.2, 1.2);
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
        self.sized = self.sized_from_places();
    }

    /// How large new monitors are made beside an arrangement that came from elsewhere
    /// (the file, a profile): the standard size where every monitor standing has it
    /// (or the headset's pixels are not known, or the standard size is chosen); else
    /// seen at the scale the middle one of them (by scale) is shown at, for all. With
    /// nothing standing, as at the start (1:1).
    fn sized_from_places(&self) -> layout::Sizes {
        let Some(ppd) = self
            .ppd
            .filter(|_| self.dev.size != clarity::MonitorSize::Standard)
        else {
            return layout::Sizes::default();
        };
        let eye = self.center.0;
        let standing: Vec<(f32, f32)> = self
            .placements
            .iter()
            .filter(|(id, _)| !self.is_window(**id))
            .filter_map(|(id, p)| {
                let (w, _) = self
                    .streams
                    .get(id)
                    .or_else(|| self.placed_for.get(id))
                    .copied()?;
                let per_pixel = p.width / w.max(1) as f32;
                Some((per_pixel, clarity::shown(p, w, eye, None, ppd)))
            })
            .collect();
        if standing.is_empty() {
            return clarity::start_sizes(self.ppd, self.dev.size);
        }
        let standard = layout::METRES_PER_PIXEL;
        if standing
            .iter()
            .all(|(m, _)| (m / standard - 1.0).abs() < 0.005)
        {
            return layout::Sizes::default();
        }
        let mut scales: Vec<f32> = standing.iter().map(|(_, s)| *s).collect();
        scales.sort_by(f32::total_cmp);
        layout::Sizes::all(layout::Size::Seen {
            ppd,
            scale: scales[scales.len() / 2],
        })
    }

    /// The desk placement of monitor `id` follows its stream, now `size` pixels: the
    /// same size of a pixel as before (see `layout::refit`). Whether its size changed.
    fn fit_to_stream(&mut self, id: DisplayKey, size: PixelSize) -> bool {
        let mut changed = false;
        if let (Some(p), Some(&was)) = (self.placements.get_mut(&id), self.placed_for.get(&id)) {
            let now = layout::refit(*p, was, size);
            changed = now != *p;
            *p = now;
        }
        self.placed_for.insert(id, size);
        changed
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
        let order = self.main().map_or(&[][..], |v| &v.order);
        let due = self.cinema
            && !order.is_empty()
            && order.iter().all(|id| self.streams.contains_key(id))
            && !order.iter().any(|id| self.placements.contains_key(id));
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
        self.pcs.remove(&pc);
        if self.last == Some(pc) {
            self.last = None;
        }
        let gone: Vec<DisplayKey> = self
            .desk
            .arrangement()
            .placements
            .keys()
            .copied()
            .chain(self.desk.cinema())
            .chain(self.desk.rows(pc).iter().flatten().copied())
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
            self.primary(),
            shown,
            &live,
            self.cinema.then(|| self.desk.arrangement()),
            self.sent_rows(),
        )
    }

    /// A profile is being switched to (`None`: none any more): the monitors are not
    /// laid out one by one, nor the desktop's arrangement sent, until it stands.
    /// `switch`: the PC asked, and how it stands.
    pub fn switch_to(&mut self, switch: Option<(PcId, ProfileSwitch)>) {
        let (pc, switch) = switch.unzip();
        self.switch = switch;
        self.switch_pc = pc;
    }

    /// A profile is being switched to.
    pub fn switching(&self) -> bool {
        self.switch.is_some()
    }

    /// The profile being switched to is the one of request `req` to PC `pc` (two
    /// PCs number their requests each on their own).
    pub fn switching_with(&self, pc: PcId, req: ReqId) -> bool {
        self.switch_pc == Some(pc) && self.switch.as_ref().is_some_and(|s| s.req == req)
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
                self.sized = self.sized_from_places();
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
    pub fn switch_given_up(&mut self, pc: PcId, req: ReqId) {
        if self.switching_with(pc, req) {
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
                || (self.has_list(k.pc) && self.listed.get(&k).is_none_or(|l| l.failed))
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
    /// monitors are not placed). One that goes into the main row, whose monitors make
    /// room for it only then, waits where it covers none of them.
    pub fn adding_started(
        &mut self,
        req: Option<ReqId>,
        size: (u32, u32),
        gaze: Vec3,
        now: Instant,
    ) {
        self.adding = req.filter(|_| !self.cinema).and_then(|req| {
            let (place, into_row) = self.waiting_place(size, gaze);
            Some(Adding {
                req,
                pc: self.pc()?,
                place,
                size,
                id: None,
                into_row,
                since: now,
                gaze,
            })
        });
    }

    /// PC `pc` made the monitor of its request `req`: display `id`.
    pub fn adding_made(&mut self, pc: PcId, req: ReqId, id: Option<DisplayKey>) {
        if let Some(a) = &mut self.adding
            && a.pc == pc
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
        let failed = a
            .id
            .is_some_and(|k| self.has_list(k.pc) && self.listed.get(&k).is_none_or(|l| l.failed));
        let shown = a.id.is_some_and(|id| self.streams.contains_key(&id));
        let asked = waits || a.id.is_some();
        if shown
            || failed
            || !asked
            || !self.pcs.get(&a.pc).is_some_and(|v| v.connected)
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
            layout::ahead(&self.placements, &ids, self.primary(), old_pos).unwrap_or(self.center.1);
        for p in self.placements.values_mut() {
            *p = layout::carried(p, (old_pos, ahead), (head, yaw));
        }
        self.center = (head, yaw);
        self.save_layout();
        self.send_layout(now);
    }

    /// The A button: lines up what the user arranged by hand, row by row (see
    /// `layout::line_up`), instead of replacing it with the preset: every row keeps its
    /// place and the way it stands, tilted back or overhead too; but a monitor out of
    /// view is brought into view (see [`Self::bring_into_view`] and
    /// [`Self::further_into_view`]). Cinema rebuilds its own view. What the hands held
    /// is let go.
    pub fn tidy_up(&mut self, s: &Settings, now: Instant) {
        if self.cinema || self.placements.is_empty() {
            self.arrange_all(s, now);
            return;
        }
        let lined = self.line_up_rows();
        let glued = if self.bring_into_view(s) {
            self.line_up_rows()
        } else {
            lined
        };
        self.further_into_view(s, &glued);
        self.rearranged = true;
        self.held = [None, None];
        self.save_layout();
        self.send_layout(now);
    }

    /// Every monitor alone in a row whose middle is out of view (right over the head,
    /// say) placed again as a new one would be, near where it was around the user: by
    /// another row, or into the main row with its neighbours making room (see
    /// `layout::place_in_view`). The PC's own displays stay with their group. Whether
    /// any moved.
    fn bring_into_view(&mut self, s: &Settings) -> bool {
        let group: Vec<DisplayKey> = self.physical().iter().map(|p| p.0).collect();
        let monitors: Vec<(DisplayKey, u32, u32)> = self
            .sizes
            .iter()
            .filter(|m| !group.contains(&m.0))
            .copied()
            .collect();
        let at = (self.center, self.tilt);
        let out = layout::alone_out_of_view(&self.placements, &monitors, at, self.sent_rows());
        let mut moved = false;
        for id in out {
            let (Some(was), Some(&(_, w, h))) = (
                self.placements.remove(&id),
                monitors.iter().find(|m| m.0 == id),
            ) else {
                continue;
            };
            let gaze = layout::level_towards(&was, at);
            let (p, push) = self.place_in_view((w, h), gaze, true);
            // Only for a place in view: else it stays where the person left it.
            if !push && !layout::in_view(&p, at) {
                self.placements.insert(id, was);
                continue;
            }
            info!("monitor {id} out of view, placed again");
            self.placements.insert(id, p);
            if push {
                self.make_room_for(s, id);
            }
            moved = true;
        }
        moved
    }

    /// A monitor still out of view (rows of ordinary monitors stacked high, say): the
    /// whole arrangement further away, as a preset takes it (see
    /// `prostir_layout::circle`), each monitor where it was around the user and so seen
    /// nearer eye level (see `layout::further`), and lined up again, the rows glued as
    /// it stood kept glued (their seams, opened in the room by the carry, close again);
    /// the PC's own displays carried as one piece, window panels left where they are.
    /// At the least radius at which everything carried is in view (found by halving:
    /// carrying is cheap) and 2 cm more, for rows lining up pushes apart, lined up
    /// once: there if every monitor is in view, nothing goes through anything, none is
    /// seen behind another where it was not (but for a seam kept closed) and no seam
    /// seen closed before (to within a few pixels) is open; no further than
    /// `layout::VIEW_RADIUS`. Else everything stays as it was. `glued`: the pairs
    /// lining up kept glued as it stands now (see [`Self::line_up_rows`]), kept glued
    /// too.
    fn further_into_view(&mut self, s: &Settings, glued: &[(DisplayKey, DisplayKey)]) {
        /// Past the least radius in view as carried, how much further it is lined up
        /// again (lined up, rows pushed apart may come a little out of view again);
        /// once, as a lining up costs some milliseconds on the headset.
        const MORE: f32 = 0.02;
        /// How much a seam closed as seen may open, as seen (radians: half a millimetre
        /// a metre away).
        const OPEN: f32 = 5e-4;
        /// A seam seen closed to within this is kept closed (radians: about three pixels
        /// a metre away).
        const MEETING: f32 = 1.5e-3;
        let group = self.placed_group();
        let at = (self.center, self.tilt);
        let laid: Vec<(DisplayKey, u32, u32)> = self
            .sizes
            .iter()
            .filter(|m| self.placements.contains_key(&m.0))
            .copied()
            .collect();
        let unseen = |placements: &HashMap<DisplayKey, Placement>| {
            laid.iter().filter(|m| !group.contains(&m.0)).any(|m| {
                placements
                    .get(&m.0)
                    .is_some_and(|p| !layout::in_view(p, at))
            })
        };
        if !unseen(&self.placements) || self.radius <= 0.0 || self.radius >= layout::VIEW_RADIUS {
            return;
        }
        let frame = layout::frame(self.center, self.tilt);
        let wall = s.preset == Preset::Wall;
        let (was, was_radius) = (self.placements.clone(), self.radius);
        let lead = group.first().and_then(|id| was.get(id)).copied();
        // Every monitor of `was` carried as if its circle were `radius` (see above).
        let carry = |radius: f32, to: &mut HashMap<DisplayKey, Placement>| {
            let k = radius / was_radius;
            let lead_by = lead.map_or(Vec3::ZERO, |p| {
                layout::further(p, &frame, k, wall).position - p.position
            });
            for m in &laid {
                let (Some(from), Some(to)) = (was.get(&m.0), to.get_mut(&m.0)) else {
                    continue;
                };
                *to = if group.contains(&m.0) {
                    Placement {
                        position: from.position + lead_by,
                        ..*from
                    }
                } else {
                    layout::further(*from, &frame, k, wall)
                };
            }
        };
        // The least radius in view as carried, halved down to 5 mm: carried
        // further, every monitor is seen nearer eye level.
        let mut next = was.clone();
        carry(layout::VIEW_RADIUS, &mut next);
        if unseen(&next) {
            return;
        }
        let (mut lo, mut hi) = (was_radius, layout::VIEW_RADIUS);
        while hi - lo > 0.005 {
            let mid = (lo + hi) / 2.0;
            carry(mid, &mut next);
            if unseen(&next) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        // The seams to keep closed: those lining up keeps glued, and every other one
        // seen closed as it stands (a monitor over another that lining up does not take
        // for glued, a row resting on a row of other widths).
        let mut pairs = layout::meeting_pairs(&was, &laid, at, MEETING);
        for g in glued {
            if !pairs.contains(g) {
                pairs.push(*g);
            }
        }
        let gaps_was = layout::seam_gaps(&was, &laid, at, &pairs);
        let radius = (hi + MORE).min(layout::VIEW_RADIUS);
        carry(radius, &mut self.placements);
        self.line_up_keeping(&pairs);
        let gaps = layout::seam_gaps(&self.placements, &laid, at, &pairs);
        // Open, or no longer one over the other.
        let opened = gaps
            .iter()
            .zip(&gaps_was)
            .any(|(now, was)| match (now, was) {
                (Some(now), Some(was)) => now - was > OPEN,
                (None, Some(_)) => true,
                _ => false,
            });
        // Seen over or behind another where it was not, but for a seam kept closed (a
        // row glued over a row may overlap it a little, as "Line up" leaves it).
        let hidden_was = layout::hidden_pairs(&was, &laid, self.center.0, HIDDEN);
        let hides = layout::hidden_pairs(&self.placements, &laid, self.center.0, HIDDEN)
            .into_iter()
            .any(|(a, b)| {
                !hidden_was.contains(&(a, b))
                    && !pairs.contains(&(a, b))
                    && !pairs.contains(&(b, a))
            });
        if !opened
            && !hides
            && !unseen(&self.placements)
            && !layout::cut_any(&self.placements, &laid)
        {
            info!("out of view: the arrangement taken {radius:.2} m away");
            self.radius = radius;
            return;
        }
        self.placements = was;
    }

    /// The pairs of monitors "Line up" keeps glued as they stand (see
    /// `layout::glued_pairs`), with what it lines up (see [`Self::line_up_rows`]).
    #[cfg(test)]
    fn glued_now(&self) -> Vec<(DisplayKey, DisplayKey)> {
        let (lid, sizes) = self.lined_up_ones();
        layout::glued_pairs(
            &self.placements,
            &sizes,
            (self.center, self.tilt),
            self.primary(),
            self.sent_rows(),
            lid,
        )
    }

    /// What "Line up" lines up: the first display of the PC itself that shows, as a
    /// laptop's screen (see [`Self::line_up_rows`]), and the sizes of it and the
    /// monitors.
    fn lined_up_ones(&self) -> (Option<DisplayKey>, Vec<(DisplayKey, u32, u32)>) {
        let lid = self
            .physical()
            .iter()
            .map(|p| p.0)
            .find(|id| self.streams.contains_key(id) && self.placements.contains_key(id));
        let sizes = self
            .sizes
            .iter()
            .filter(|m| Some(m.0) == lid || !self.physical().iter().any(|p| p.0 == m.0))
            .copied()
            .collect();
        (lid, sizes)
    }

    /// Every row of the desk arrangement lined up (see `layout::line_up`), the PC's own
    /// displays with the first of them.
    fn line_up_rows(&mut self) -> Vec<(DisplayKey, DisplayKey)> {
        self.line_up_keeping(&[])
    }

    /// [`Self::line_up_rows`], each of `glued` kept glued as well (see
    /// `layout::line_up_keeping`). The pairs it kept glued.
    fn line_up_keeping(
        &mut self,
        glued: &[(DisplayKey, DisplayKey)],
    ) -> Vec<(DisplayKey, DisplayKey)> {
        let mut kept = Vec::new();
        // The first display of the PC itself that shows is lined up as a laptop's
        // screen; the others go with it, standing to it as they stood (they are not
        // lined up, and so cover no monitor the first one does not).
        let (lid, sizes) = self.lined_up_ones();
        let others: Vec<DisplayKey> = self
            .physical()
            .iter()
            .map(|p| p.0)
            .filter(|id| Some(*id) != lid)
            .collect();
        let lid_was = lid.and_then(|id| self.placements.get(&id).copied());
        let lined = layout::line_up_keeping(
            &self.placements,
            &sizes,
            self.center,
            self.tilt,
            self.primary(),
            self.sent_rows(),
            lid,
            glued,
            Some(&mut kept),
        );
        // Window panels (and anything not laid out) stay as they are.
        self.placements.extend(lined);
        if let (Some(from), Some(to)) = (lid_was, lid.and_then(|id| self.placements.get(&id))) {
            let to = *to;
            for id in &others {
                if let Some(p) = self.placements.get_mut(id) {
                    *p = layout::kept_to(*p, from, to);
                }
            }
        }
        // Where one of them then goes through a monitor lined up next to it (a wide one
        // beside the group), the whole group goes down out of its way, as one piece.
        let group: Vec<DisplayKey> = lid.into_iter().chain(others).collect();
        let rest: Vec<(DisplayKey, u32, u32)> = self
            .sizes
            .iter()
            .filter(|m| !group.contains(&m.0))
            .copied()
            .collect();
        let rest = self.placed_heights(&rest);
        let frame = layout::frame(self.center, self.tilt);
        let down = group
            .iter()
            .filter_map(|id| {
                let p = *self.placements.get(id)?;
                let h = self.height_at(*id, &p)?;
                Some(layout::clear_along_up(p, h, &rest, &frame, -1.0))
            })
            .fold(0.0f32, f32::max);
        if down > 0.0 {
            for id in &group {
                if let Some(p) = self.placements.get_mut(id) {
                    *p = layout::lifted(*p, &frame, -down);
                }
            }
        }
        kept
    }

    /// Swaps the places of monitor `id` and its neighbour `delta` along in the order.
    /// Each takes the other's place at the size of its own pixels, the two edge to edge
    /// where the pair stood (the one going left from the left edge of the pair, the
    /// other from its right edge); neighbours either reaches into are pushed clear along
    /// the row (see `layout::make_room`), the others stay.
    pub fn swap(&mut self, s: &Settings, id: DisplayKey, delta: i32) {
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
            let frame = layout::frame(self.center, self.tilt);
            let wall = s.preset == Preset::Wall;
            // Into `b`'s place goes `id`, into `a`'s the other, each from the pair's
            // outer edge on its side.
            let side = if layout::left_of(&a, &b, &frame) {
                1.0
            } else {
                -1.0
            };
            let to_b = layout::edge_to(layout::refit(b, sb, sa), &b, side, &frame, wall);
            let to_a = layout::edge_to(layout::refit(a, sa, sb), &a, -side, &frame, wall);
            self.placements.insert(id, to_b);
            self.placements.insert(other, to_a);
            self.rearranged = true;
            let group = self.placed_group();
            for moved in [other, id] {
                layout::make_room(
                    &mut self.placements,
                    &self.sizes,
                    (self.center, self.tilt),
                    wall,
                    &group,
                    moved,
                );
            }
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
                self.center,
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
            self.dragged |= before != p;
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
                let mut moved = std::mem::take(&mut self.dragged);
                if let Some(st) = self.stick_of(s, id) {
                    // Stuck where it stood already (taken and let go): no change.
                    let was = self.placements.insert(id, st.placement);
                    moved |= was.is_none_or(|w| {
                        w.position.distance(st.placement.position) > 1e-4
                            || w.rotation.angle_between(st.placement.rotation) > 1e-4
                    });
                    stuck = true;
                }
                self.rearranged |= moved;
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
        self.dragged |= before != after;
        self.carry_all(s, id, &before, &after);
    }

    /// The second hand let go: resizing ends, and the arrangement is kept.
    pub fn resize_end(&mut self) {
        self.resizing = None;
        self.rearranged |= std::mem::take(&mut self.dragged);
        self.save_layout();
    }

    /// The hands are not the app's any more (the system menu over it): what they held
    /// is let go, and the arrangement is kept.
    pub fn cancel_holds(&mut self) {
        // Saved only if something was held: nothing moved otherwise.
        let held = self.held.iter().any(Option::is_some) || self.resizing.is_some();
        self.held = [None, None];
        self.resizing = None;
        if held {
            self.save_layout();
        }
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
            .primary()
            .filter(shown)
            .or_else(|| self.lineup.iter().map(|m| m.0).find(|id| shown(id)));
        let hanging = |id: &DisplayKey| {
            self.streams
                .contains_key(id)
                .then(|| Some((*self.placements.get(id)?, self.height_of(*id))))
                .flatten()
        };
        self.button = anchor.and_then(|id| {
            let p = self.placements.get(&id)?;
            let h = self.height_of(id);
            let mut x = -p.width / 2.0 + BUTTON_SIZE;
            for (m, mh) in self.physical().iter().filter_map(|(id, _)| hanging(id)) {
                let corner = m.position + m.rotation * Vec3::new(-m.width / 2.0, mh / 2.0, 0.0);
                let local = p.rotation.inverse() * (corner - p.position);
                // Only a display that actually hangs under this monitor.
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
        self.clarify(s);
    }

    /// How sharp each monitor's layer is shown, worked out again for those whose place
    /// (where it goes, not where it is drawn on its way), stream width, curve, the eye
    /// or the choices of "For development" changed since (see [`Clear`]). Nothing is
    /// allocated once each monitor has its entry.
    fn clarify(&mut self, s: &Settings) {
        let curve = self.curve_radius(s);
        let (eye, ppd, dev) = (self.center.0, self.ppd, self.dev);
        let Self {
            placements,
            streams,
            listed,
            clear,
            ..
        } = self;
        for (id, at) in placements.iter() {
            let Some(&(width, _)) = streams.get(id) else {
                continue;
            };
            let curve = curve.filter(|_| !window(listed, *id));
            let made_for = (*at, width, eye, curve, dev);
            if clear.get(id).is_some_and(|c| c.made_for == made_for) {
                continue;
            }
            let scale = ppd.map(|ppd| clarity::shown(at, width, eye, curve, ppd));
            // Supersampled before by the scale, or not: it changes only well past the
            // line (see `clarity::quality`).
            let was = clear
                .get(id)
                .filter(|c| c.made_for.4.super_sampling == clarity::Filter::Auto)
                .map(|c| c.quality.super_sampling);
            let c = Clear {
                made_for,
                scale,
                quality: clarity::quality(scale, &dev, was),
            };
            match clear.get_mut(id) {
                Some(kept) => *kept = c,
                None => {
                    clear.insert(*id, c);
                }
            }
        }
        clear.retain(|id, _| placements.contains_key(id) && streams.contains_key(id));
    }

    /// The compositor's filters for monitor `id`'s layer, by the scale it is shown at
    /// (see `clarity::quality`); both until its place is known.
    pub fn layer_quality(&self, id: DisplayKey) -> Quality {
        self.clear.get(&id).map_or(Quality::BOTH, |c| c.quality)
    }

    /// How many of the headset's pixels a pixel of monitor `id`'s stream takes at its
    /// middle, where it goes, seen from the arrangement's eye (where the head was when
    /// it was laid out or brought in front, not where it is now); `None`: not known.
    pub fn shown_scale(&self, id: DisplayKey) -> Option<f32> {
        self.clear.get(&id).and_then(|c| c.scale)
    }

    // ---- For development ----

    /// Whether a display other than `except`, as drawn, stands where a rectangle of
    /// `size` would, centred at `at` in the plane of `p` (x right, y up from its middle),
    /// seen square to that plane. Nothing is allocated.
    #[cfg(feature = "preview")]
    pub fn stands_at(&self, p: &Placement, at: Vec2, size: Vec2, except: DisplayKey) -> bool {
        let turn = p.rotation.inverse();
        let (lo, hi) = (at - size / 2.0, at + size / 2.0);
        self.streams
            .iter()
            .filter(|(id, _)| **id != except)
            .any(|(id, &(w, h))| {
                let Some(q) = self.shown.get(id) else {
                    return false;
                };
                let corners = q.corners(q.width * h as f32 / w.max(1) as f32);
                let (mut qlo, mut qhi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
                for c in corners {
                    let l = (turn * (c - p.position)).truncate();
                    qlo = qlo.min(l);
                    qhi = qhi.max(l);
                }
                qlo.x < hi.x && lo.x < qhi.x && qlo.y < hi.y && lo.y < qhi.y
            })
    }

    /// The choices of "For development".
    pub fn dev(&self) -> &Dev {
        &self.dev
    }

    /// The headset's pixels per degree, where its model is known.
    pub fn pixels_per_degree(&self) -> Option<f32> {
        self.ppd
    }

    /// The choices of "For development" are `dev` from now on. Another size of the
    /// monitors lays them out again by the preset at once (as choosing a preset does);
    /// the arrangement before the first such change is kept for "Reset" (see
    /// [`Self::reset_size`]). The filters take effect with the next frame.
    #[cfg(feature = "preview")]
    pub fn set_dev(&mut self, s: &Settings, dev: Dev, now: Instant) {
        let resized = dev.size != self.dev.size;
        self.dev = dev;
        if !resized {
            return;
        }
        if self.cinema || self.lineup.is_empty() {
            self.sized = clarity::start_sizes(self.ppd, dev.size);
            return;
        }
        if self.before_dev.is_none() {
            self.before_dev = Some(BeforeDev {
                placements: self.placements.clone(),
                radius: self.radius,
                tilt: self.tilt,
                sized: self.sized,
            });
        }
        self.rearranged = true;
        self.arrange_all(s, now);
    }

    /// Whether "Reset" of the monitors' size has something to put back.
    #[cfg(feature = "preview")]
    pub fn can_reset_size(&self) -> bool {
        self.before_dev.is_some() || self.dev.size != clarity::MonitorSize::default()
    }

    /// "Reset": the size of new monitors as by default, and the monitors back where
    /// they stood before the first size of "For development" laid them out again: those
    /// still here. A monitor added in between stays where it was put, and nothing else
    /// moves.
    #[cfg(feature = "preview")]
    pub fn reset_size(&mut self, now: Instant) {
        self.dev.size = clarity::MonitorSize::default();
        // Cinema shows its own view: the desk arrangement is not the one to put back.
        let Some(before) = self.before_dev.take().filter(|_| !self.cinema) else {
            self.sized = clarity::start_sizes(self.ppd, self.dev.size);
            return;
        };
        for (id, p) in before.placements {
            if let Some(at) = self.placements.get_mut(&id) {
                *at = p;
            }
        }
        self.radius = before.radius;
        self.tilt = before.tilt;
        self.sized = before.sized;
        self.held = [None, None];
        self.rearranged = true;
        self.save_layout();
        self.send_layout(now);
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
