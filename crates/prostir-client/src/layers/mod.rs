//! The frame's layers (step 7, 4.1, 5 and 9): what the compositor is given, in which
//! order, and where each thing is drawn, within the number of layers the compositor
//! takes in a frame (`Caps::layers_max`). The shell turns each [`Layer`] into its
//! OpenXR layer as it is; the core knows no swapchain, only whether the shell has a
//! picture for each thing (see [`Ready`]).
//!
//! The order, back to front:
//! 1. what stands behind everything ([`Layer::Background`]: the room through the
//!    cameras, the black layer that dims a room blended with the frame, or the
//!    panorama);
//! 2. the window onto the keyboard;
//! 3. the monitors on cylinders, then the flat ones, far to near (as
//!    `World::draw_list` has them, the one in a hand last), of every PC together,
//!    each PC's mouse pointer right after its monitor (so a nearer monitor covers it
//!    as it covers that monitor);
//! 4. what is wrong with a monitor's picture: over it while it recovers, in its place
//!    while it cannot be shown;
//! 5. the placeholder of a monitor being added, the numbers next to the monitor looked
//!    at (the preview build's "For development");
//! 6. the menu's button, the menu, the toast, "Attach", the palm menu;
//! 7. the hands and rays, then the pointer dots.
//!
//! Room is left for what is planned: the monitors' frames drawn together in one layer
//! go right after the keyboard's window, ranked with the background; a frame around
//! the menu is drawn in the menu's own picture.
//!
//! **Over budget** (more wanted than the compositor takes), what goes first:
//! 1. the numbers of "For development", then what is wrong with a monitor that still
//!    shows its picture (the words over it);
//! 2. the monitors, the farthest first (one that cannot be shown and has its reason in
//!    its place counts as a monitor there; the one in a hand is the nearest);
//! 3. the placeholder, "Attach", the toast: a small sign goes before the room;
//! 4. the window onto the keyboard, then the background;
//! 5. last, and with the compositor's least ([`crate::caps::LAYERS_AT_LEAST`]) never:
//!    the person's own, the pointer dots and the PCs' mouse pointers, the menu's
//!    button, the palm menu, the menu, the hands and rays. A PC's pointer goes with
//!    its monitor all the same.
//!
//! The streams the PC is told the headset takes leave [`crate::caps::RESERVED_LAYERS`]
//! for the background, the menu, its button, the hands, the dots of the two rays, the
//! toast and one PC's mouse pointer, so pointing, a message or the pointer never
//! pushes a monitor out; only what comes on top of those (the palm menu, "Attach", the
//! keyboard's window, a dot under a fingertip, a second PC's pointer...) can. The
//! model hears how many monitors are hidden when that changes (see [`Plan::news`]) and
//! says so once. That number is counted as if there were no toast, no dots and no
//! pointers: the toast saying so never hides one more monitor and so says more, and a
//! dot or a pointer that comes and goes says nothing.
//!
//! Nothing is allocated once the plan's lists are long enough: it is kept from frame
//! to frame and filled again.

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use glam::{Vec2, Vec3};

use crate::app::{Model, TOAST_WIDTH};
use crate::clarity::Quality;
use crate::cursors::Shown;
use crate::ids::{DisplayKey, PcId, Target};
use crate::interaction::Hands;
use crate::layout::{Placement, facing, image_frame, image_width};
use crate::world::{
    ATTACH_WIDTH, BUTTON_SIZE, WRIST_MENU_WIDTH, attach_height, panel_height, panel_width,
    wrist_menu_height,
};

/// The toast's picture, in pixels: its layer has its shape.
pub const TOAST_PX: (u32, u32) = (1024, 132);
/// The picture of the numbers of "For development", in pixels.
#[cfg(feature = "preview")]
pub const METRICS_PX: (u32, u32) = (1024, 88);

/// A flat picture where it is drawn: its middle, how it faces, how wide
/// (`at.width`) and how high, in metres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    pub at: Placement,
    pub height: f32,
}

/// A monitor's picture where it is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Monitor {
    pub id: DisplayKey,
    pub quad: Quad,
    /// On a cylinder of this radius around the arrangement, in metres; `None`: flat.
    pub curve: Option<f32>,
    /// How bright: focus dimming, the brightness setting and fading in, together (0
    /// to 1, the colour scale).
    pub bright: f32,
    /// How far it faded in (0 to 1).
    pub alpha: f32,
    /// The compositor's filters for it, by the scale it is shown at (see
    /// `World::layer_quality`).
    pub quality: Quality,
}

/// A layer of the frame (see the module for the order).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Layer {
    /// The room through the cameras, its dimming, or the panorama: the shell has one.
    Background,
    /// The window onto the keyboard.
    KeyboardWindow,
    Monitor(Monitor),
    /// What is wrong with monitor `id`'s picture, in words.
    Trouble {
        id: DisplayKey,
        quad: Quad,
    },
    /// The monitor being added.
    Placeholder(Quad),
    /// The numbers next to the monitor looked at ("For development").
    #[cfg(feature = "preview")]
    Metrics(Quad),
    MenuButton(Quad),
    Menu(Quad),
    Toast(Quad),
    /// "Attach" under a detached window.
    Attach(Quad),
    /// The palm menu.
    Palm(Quad),
    /// The hands and rays, drawn on top of everything.
    Hands,
    /// A pointer dot: where a ray meets something, or under a fingertip near the menu.
    Cursor(Quad),
    /// PC `pc`'s mouse pointer in its look `shape`, over its monitor (the layer
    /// before it).
    PcCursor {
        pc: PcId,
        shape: u32,
        quad: Quad,
    },
}

/// What the shell has a picture for this frame.
pub trait Ready {
    /// Something to stand behind everything (the room, its dimming, the panorama).
    fn background(&self) -> bool;
    /// The window onto the keyboard.
    fn keyboard_window(&self) -> bool;
    /// Monitor `id`'s picture, with its size in pixels.
    fn picture(&self, id: DisplayKey) -> Option<(u32, u32)>;
    /// What is wrong with monitor `id`'s picture, drawn in words.
    fn trouble(&self, id: DisplayKey) -> bool;
    /// The placeholder's picture.
    fn placeholder(&self) -> bool;
    /// The menu's picture, drawn at least once.
    fn menu(&self) -> bool;
    /// The palm menu's picture, drawn at least once.
    fn palm(&self) -> bool;
    /// "Attach" drawn.
    fn attach(&self) -> bool;
    /// The numbers of "For development" drawn.
    #[cfg(feature = "preview")]
    fn metrics(&self) -> bool;
    /// The hands and rays drawn this frame.
    fn hands(&self) -> bool;
    /// The picture of look `shape` of PC `pc`'s mouse pointer.
    fn pc_cursor(&self, pc: PcId, shape: u32) -> bool;
}

/// What a layer is worth when not all fit: the higher one stays (see the module).
/// The variants go from what goes first to what goes last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// What is wrong with a monitor that still shows, by its monitor's nearness.
    Over(u32),
    /// A monitor, or the reason in its place, by nearness (the higher the nearer).
    Monitor(u32),
    /// The placeholder (0), "Attach" (1), the toast ([`Rank::TOAST`]).
    Said(u8),
    /// The window onto the keyboard (0), the background (1).
    Room(u8),
    /// The pointer dots ([`Rank::DOT`]), the menu's button (1), the palm menu (2), the
    /// menu (3), the hands and rays (4).
    Own(u8),
}

impl Rank {
    const TOAST: Self = Self::Said(2);
    const DOT: Self = Self::Own(0);

    /// Left out when the hidden monitors are counted (see the module): the toast that
    /// says so, and the dots that come and go with the hands.
    fn uncounted(self) -> bool {
        self == Self::TOAST || self == Self::DOT
    }
}

/// How near a monitor `distance` metres from the head is, for [`Rank`]: the nearer,
/// the higher (the one in a hand, at -1, the highest).
fn nearness(distance: f32) -> u32 {
    // The bits of a float that is not negative grow as it does.
    u32::MAX - distance.max(0.0).to_bits()
}

/// The frame's layers, kept from frame to frame (see [`plan`]).
#[derive(Debug, Default)]
pub struct Plan {
    /// Back to front, the ones that fit.
    layers: Vec<Layer>,
    /// Each layer's rank and place in the list wanted, for the budget.
    ranks: Vec<(Rank, u16)>,
    /// Whether each layer wanted stays.
    stays: Vec<bool>,
    /// How many layers were wanted.
    wanted: usize,
    /// The monitors hidden this frame, and as the model was last told.
    hidden: usize,
    told: usize,
}

impl Plan {
    pub fn new() -> Self {
        Self::default()
    }

    /// The layers to give the compositor, back to front.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// How many layers the frame wanted, before the budget.
    pub fn wanted(&self) -> usize {
        self.wanted
    }

    /// How many monitors (or the reasons in their places) do not fit, counted as if
    /// there were no toast and no pointer dots (see the module).
    pub fn hidden(&self) -> usize {
        self.hidden
    }

    /// How many monitors are hidden, when that changed since the last time asked:
    /// for the model (`PlatformEvent::LayersHidden`).
    pub fn news(&mut self) -> Option<usize> {
        (self.hidden != self.told).then(|| {
            self.told = self.hidden;
            self.hidden
        })
    }

    fn clear(&mut self) {
        self.layers.clear();
        self.ranks.clear();
        self.wanted = 0;
        self.hidden = 0;
    }

    fn push(&mut self, layer: Layer, rank: Rank) {
        let at = u16::try_from(self.layers.len()).unwrap_or(u16::MAX);
        self.layers.push(layer);
        self.ranks.push((rank, at));
    }

    /// Whether layer `i` is a PC's pointer whose monitor (the layer before it) does
    /// not stay.
    fn lone_pointer(&self, i: usize) -> bool {
        matches!(self.layers[i], Layer::PcCursor { .. }) && (i == 0 || !self.stays[i - 1])
    }

    /// No more than `max` layers: the ones ranked lowest go (see the module), the
    /// rest keep their order.
    fn budget(&mut self, max: usize) {
        self.wanted = self.layers.len();
        if self.layers.len() <= max {
            return;
        }
        // Highest first; of two alike the later one (drawn on top, nearer) first.
        self.ranks.sort_unstable_by(|a, b| b.cmp(a));
        self.stays.clear();
        self.stays.resize(self.layers.len(), false);
        for &(_, at) in &self.ranks[..max] {
            self.stays[usize::from(at)] = true;
        }
        // A PC's pointer goes with its monitor (the layer before it); its place goes
        // to the next layer that fits, so a pointer over a monitor left out takes no place.
        let mut next = max;
        loop {
            let mut freed = 0;
            for i in 1..self.layers.len() {
                if self.stays[i] && self.lone_pointer(i) {
                    self.stays[i] = false;
                    freed += 1;
                }
            }
            if freed == 0 {
                break;
            }
            while freed > 0 && next < self.ranks.len() {
                let at = usize::from(self.ranks[next].1);
                next += 1;
                if !self.lone_pointer(at) {
                    self.stays[at] = true;
                    freed -= 1;
                }
            }
        }
        // The monitors that would not fit without the toast and the dots.
        self.hidden = self
            .ranks
            .iter()
            .filter(|(rank, _)| !rank.uncounted())
            .skip(max)
            .filter(|(rank, _)| matches!(rank, Rank::Monitor(_)))
            .count();
        let mut i = 0;
        let stays = &self.stays;
        self.layers.retain(|_| {
            i += 1;
            stays[i - 1]
        });
    }
}

/// The frame's layers into `plan`: what the model shows, where it is drawn now, of
/// what the shell has `ready`, within what the compositor takes. Called once a frame,
/// after the hands and `Model::animate`, so what is drawn is the model of this very
/// frame (step 7, 7: a monitor dragged is drawn where the hand put it, without a
/// frame of delay).
pub fn plan(model: &Model, hands: &Hands, ready: &impl Ready, plan: &mut Plan) {
    plan.clear();
    let world = model.world();
    let settings = model.settings();
    if ready.background() {
        plan.push(Layer::Background, Rank::Room(1));
    }
    if ready.keyboard_window() {
        plan.push(Layer::KeyboardWindow, Rank::Room(0));
    }
    // The monitors: curved ones first, then the flat ones (windows always flat),
    // each group far to near.
    let curve = world.curve_radius(settings);
    for curved in [true, false] {
        for &(distance, id) in world.draw_list() {
            let on = curve.filter(|_| !world.is_window(id));
            if on.is_some() != curved {
                continue;
            }
            let (Some((w, h)), Some(p)) = (ready.picture(id), world.shown(id)) else {
                continue;
            };
            // Fading in: premultiplied, so colour and alpha scale together.
            let t = world.appear(id);
            let t = t * t * (3.0 - 2.0 * t);
            let bright = world.brightness(id) * t * f32::from(settings.brightness) / 100.0;
            let quad = Quad {
                at: *p,
                height: p.width * h as f32 / w.max(1) as f32,
            };
            plan.push(
                Layer::Monitor(Monitor {
                    id,
                    quad,
                    curve: on,
                    bright,
                    alpha: t,
                    quality: world.layer_quality(id),
                }),
                Rank::Monitor(nearness(distance)),
            );
            // Its PC's pointer, if it is on it, right over it.
            if let Some(shown) = model.cursors().on(id, model.now())
                && ready.pc_cursor(id.pc, shown.shape)
            {
                let quad = pointer_quad(p, quad.height, on, (w, h), &shown);
                let layer = Layer::PcCursor {
                    pc: id.pc,
                    shape: shown.shape,
                    quad,
                };
                plan.push(layer, Rank::DOT);
            }
        }
    }
    for &(distance, id) in world.draw_list() {
        if !ready.trouble(id) {
            continue;
        }
        let Some(p) = world.shown(id) else {
            continue;
        };
        let quad = Quad {
            at: *p,
            height: world.height_of(id),
        };
        let near = nearness(distance);
        // Over the picture it has, or in the place of the one it has not.
        let rank = if ready.picture(id).is_some() {
            Rank::Over(near)
        } else {
            Rank::Monitor(near)
        };
        plan.push(Layer::Trouble { id, quad }, rank);
    }
    if let Some(a) = world.adding().filter(|_| ready.placeholder()) {
        let quad = Quad {
            at: a.place,
            height: a.height(),
        };
        plan.push(Layer::Placeholder(quad), Rank::Said(0));
    }
    #[cfg(feature = "preview")]
    if let Some(quad) = model.metrics_place().filter(|_| ready.metrics()) {
        plan.push(Layer::Metrics(quad), Rank::Over(0));
    }
    if let Some(at) = world.button() {
        let quad = Quad {
            at: Placement {
                width: BUTTON_SIZE,
                ..at
            },
            height: BUTTON_SIZE,
        };
        plan.push(Layer::MenuButton(quad), Rank::Own(1));
    }
    if model.menu().open && ready.menu() {
        let quad = Quad {
            at: Placement {
                width: panel_width(settings.large_menu),
                ..world.menu()
            },
            height: panel_height(settings.large_menu),
        };
        plan.push(Layer::Menu(quad), Rank::Own(3));
    }
    if let Some(at) = model.toast().and_then(|t| t.place) {
        let quad = Quad {
            at: Placement {
                width: TOAST_WIDTH,
                ..at
            },
            height: TOAST_WIDTH * TOAST_PX.1 as f32 / TOAST_PX.0 as f32,
        };
        plan.push(Layer::Toast(quad), Rank::TOAST);
    }
    if let Some(at) = hands
        .attach_for()
        .filter(|_| ready.attach())
        .and_then(|id| world.attach_place(id))
    {
        let quad = Quad {
            at: Placement {
                width: ATTACH_WIDTH,
                ..at
            },
            height: attach_height(),
        };
        plan.push(Layer::Attach(quad), Rank::Said(1));
    }
    if let Some(at) = world.wrist().filter(|_| ready.palm()) {
        let quad = Quad {
            at: Placement {
                width: WRIST_MENU_WIDTH,
                ..at
            },
            height: wrist_menu_height(),
        };
        plan.push(Layer::Palm(quad), Rank::Own(2));
    }
    if ready.hands() {
        plan.push(Layer::Hands, Rank::Own(4));
    }
    // Where a ray meets something: a dot facing out of it, as big to the eye near
    // and far.
    for &Target { hit, .. } in hands.picks().iter().flatten() {
        let size = 0.011 * hit.t.clamp(0.3, 4.0);
        plan.push(
            Layer::Cursor(dot(hit.point, hit.normal, 0.004, size)),
            Rank::DOT,
        );
    }
    // Under a fingertip near the menu: a dot that shrinks as the finger gets closer.
    let menu = world.menu();
    let normal = menu.rotation * Vec3::Z;
    for (_, at, z, _) in hands.pokes() {
        let point = menu.position + menu.rotation * Vec3::new(at.x, at.y, 0.0);
        let size = 0.006 + z.max(0.0) * 0.15;
        plan.push(Layer::Cursor(dot(point, normal, 0.002, size)), Rank::DOT);
    }
    plan.budget(usize::from(model.headset().caps.layers_max));
}

/// Where a PC's pointer `shown` is drawn over its monitor, which stands at `p`,
/// `height` high, on a cylinder of `curve` (none: flat), its picture `picture` pixels:
/// the pixel of the look it points with over the pixel of the picture the PC says,
/// in the plane touching the monitor there, as large as it is in the picture.
pub fn pointer_quad(
    p: &Placement,
    height: f32,
    curve: Option<f32>,
    (w, h): (u32, u32),
    shown: &Shown<'_>,
) -> Quad {
    // The middle of the pixel the PC points at: `MouseTo` and `CursorAt` say pixels
    // from the first (0) to the last (1).
    let to_middle = |f: f32, n: u32| (f * (n.max(1) - 1) as f32 + 0.5) / n.max(1) as f32;
    let uv = Vec2::new(to_middle(shown.uv.x, w), to_middle(shown.uv.y, h));
    let (point, rotation) = image_frame(p, height, curve, uv);
    // Metres a pixel of the picture takes.
    let pixel = image_width(p, curve) / w.max(1) as f32;
    let look = shown.look;
    let (lw, lh) = (f32::from(look.w), f32::from(look.h));
    // From the middle of the pixel it points with to the middle of the look, right and
    // up.
    let right = (lw / 2.0 - (f32::from(look.hot_x) + 0.5)) * pixel;
    let up = ((f32::from(look.hot_y) + 0.5) - lh / 2.0) * pixel;
    Quad {
        at: Placement {
            position: point + rotation * Vec3::new(right, up, 0.0),
            rotation,
            width: lw * pixel,
        },
        height: lh * pixel,
    }
}

/// A round dot `size` across, `lift` out of a surface at `point` that faces `normal`.
fn dot(point: Vec3, normal: Vec3, lift: f32, size: f32) -> Quad {
    let at = point + normal * lift;
    Quad {
        at: Placement {
            position: at,
            rotation: facing(at, at + normal),
            width: size,
        },
        height: size,
    }
}
