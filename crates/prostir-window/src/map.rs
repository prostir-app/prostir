//! The monitor map. Flat (the main view): the displays as the PC's desktop has them,
//! moved by dragging or with the arrow keys, each landing against a neighbour
//! ([`crate::snap`]). Around you: the same displays on an arc around the viewer, to
//! look at only; dragging an empty spot turns the view.

use eframe::egui::{
    self, Color32, CornerRadius, Key, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2,
    vec2,
};

use prostir_lang::shortened;

use crate::snap;
use crate::theme::mix;
use crate::widgets::{Tk, faded};

/// One display as the map draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    pub id: u32,
    pub name: String,
    /// Width and height as written under the name.
    pub size: String,
    pub rect: snap::Rect,
    /// A screen of the PC itself (drawn with an outline: hardware, not a stream).
    pub pc: bool,
    /// Shown in the headset; dimmed when not.
    pub shown: bool,
    pub main: bool,
}

/// What happened on the map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MapEvent {
    Select(u32),
    Place { id: u32, x: i32, y: i32 },
}

/// The tile being dragged: its id, where the pointer has it, where it would land.
type Ghost = (u32, (i32, i32), Option<(i32, i32)>);

struct Drag {
    id: u32,
    /// Where the pointer took the tile, in desktop pixels from its top-left.
    grab: Vec2,
    scale: f32,
    origin: Pos2,
    at: Pos2,
}

/// Pulled into line within this many points on screen.
const MAGNET_POINTS: f32 = 10.0;

pub struct MapState {
    drag: Option<Drag>,
    yaw: f32,
    pitch: f32,
}

impl Default for MapState {
    fn default() -> Self {
        Self {
            drag: None,
            yaw: 0.0,
            pitch: 0.28,
        }
    }
}

fn bounds(tiles: &[Tile]) -> Rect {
    tiles.iter().fold(Rect::NOTHING, |b, t| {
        b.union(Rect::from_min_size(
            pos2(t.rect.x as f32, t.rect.y as f32),
            vec2(t.rect.w as f32, t.rect.h as f32),
        ))
    })
}

/// The smallest font a tile's text goes down to before less of it is shown.
const MIN_FONT: f32 = 10.0;
/// Between the name and the size line.
const LINE_GAP: f32 = 2.0;

/// How a tile's text fits it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabelFit {
    /// The name's font size.
    pub name: f32,
    /// The size line's font size; none: the size line is left out.
    pub size: Option<f32>,
    /// The name is shortened with an ellipsis.
    pub elided: bool,
}

/// The largest text that fits `room` (width, height): the name (at `body` down to
/// [`MIN_FONT`]) over the size line (`body - small` smaller, not under
/// [`MIN_FONT`]); where both do not fit even at the smallest, the name alone as large
/// as it fits; then the name shortened at the smallest; nothing where not even one
/// line of it fits. `measure(name, font)`: the size of the name (true) or of the size
/// line (false) at that font size.
pub fn fit_label(
    room: Vec2,
    body: f32,
    small: f32,
    has_size: bool,
    measure: impl Fn(bool, f32) -> Vec2,
) -> Option<LabelFit> {
    let body = body.max(MIN_FONT);
    let smaller = (body - small).max(0.0);
    let steps = || {
        let n = (body - MIN_FONT).ceil() as i32;
        (0..=n).map(move |i| (body - i as f32).max(MIN_FONT))
    };
    if has_size {
        for font in steps() {
            let size_font = (font - smaller).max(MIN_FONT);
            let (n, s) = (measure(true, font), measure(false, size_font));
            if n.x <= room.x && s.x <= room.x && n.y + LINE_GAP + s.y <= room.y {
                return Some(LabelFit {
                    name: font,
                    size: Some(size_font),
                    elided: false,
                });
            }
        }
    }
    for font in steps() {
        let n = measure(true, font);
        if n.x <= room.x && n.y <= room.y {
            return Some(LabelFit {
                name: font,
                size: None,
                elided: false,
            });
        }
    }
    // Shortened: at least a letter and the ellipsis across.
    let n = measure(true, MIN_FONT);
    (n.y <= room.y && room.x >= MIN_FONT * 1.2).then_some(LabelFit {
        name: MIN_FONT,
        size: None,
        elided: true,
    })
}

/// A tile's name and size, in full.
fn hover_text(t: &Tile) -> String {
    if t.size.is_empty() {
        t.name.clone()
    } else {
        format!("{}\n{}", t.name, t.size)
    }
}

fn label(ui: &Ui, tk: &Tk<'_>, r: Rect, t: &Tile, color: Color32) {
    // Inside the tile, above the main monitor's bar.
    let mut room = r.shrink2(vec2(6.0, 4.0));
    if t.main {
        room.max.y = (r.bottom() - 11.0).max(room.min.y);
    }
    if room.width() <= 0.0 || room.height() <= 0.0 {
        return;
    }
    let painter = ui.painter().with_clip_rect(r.intersect(ui.clip_rect()));
    let font = |size: f32, name: bool| tk.font(size, name);
    let galley = |text: &str, size: f32, name: bool| {
        painter.layout_no_wrap(text.to_owned(), font(size, name), color)
    };
    let fit = fit_label(
        room.size(),
        tk.th.type_.size_body,
        tk.th.type_.size_small,
        !t.size.is_empty(),
        |name, size| {
            let text = if name { &t.name } else { &t.size };
            galley(text, size, name).size()
        },
    );
    let Some(fit) = fit else {
        return;
    };
    let name = if fit.elided {
        let short = shortened(&t.name, |s| {
            galley(s, fit.name, true).size().x <= room.width()
        });
        let Some(short) = short else {
            return;
        };
        galley(&short, fit.name, true)
    } else {
        galley(&t.name, fit.name, true)
    };
    let size = fit
        .size
        .map(|s| painter.layout_no_wrap(t.size.clone(), font(s, false), faded(color, 0.85)));
    let height = name.size().y + size.as_ref().map_or(0.0, |g| LINE_GAP + g.size().y);
    let mut y = room.center().y - height / 2.0;
    let x = |w: f32| room.center().x - w / 2.0;
    let name_height = name.size().y;
    painter.galley(pos2(x(name.size().x), y), name, color);
    y += name_height + LINE_GAP;
    if let Some(g) = size {
        painter.galley(pos2(x(g.size().x), y), g, color);
    }
}

fn colors(tk: &Tk<'_>, t: &Tile, selected: bool, hover: f32) -> (Color32, Color32) {
    let p = tk.p;
    if selected {
        (p.accent, p.accent_text)
    } else if t.pc {
        (p.screen_pc, p.screen_text)
    } else {
        (mix(p.screen, p.control_hover, hover * 0.5), p.screen_text)
    }
}

/// The flat map.
pub fn flat(
    ui: &mut Ui,
    tk: &Tk<'_>,
    st: &mut MapState,
    tiles: &[Tile],
    selected: Option<u32>,
    height: f32,
) -> Vec<MapEvent> {
    let mut events = Vec::new();
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let p = tk.p;
    ui.painter().rect_filled(rect, tk.r_large(), p.well);
    if tiles.is_empty() {
        return events;
    }
    if st
        .drag
        .as_ref()
        .is_some_and(|d| !tiles.iter().any(|t| t.id == d.id))
    {
        st.drag = None;
    }
    // Fit while at rest; keep the scale while dragging so the map does not swim.
    let (scale, origin) = if let Some(d) = &st.drag {
        (d.scale, d.origin)
    } else {
        let bb = bounds(tiles);
        let pad = tk.th.shape.padding * 2.0;
        let s = ((rect.width() - pad * 2.0) / bb.width())
            .min((rect.height() - pad * 2.0) / bb.height())
            .min(0.12);
        (
            s,
            rect.center() - bb.size() * s / 2.0 - bb.min.to_vec2() * s,
        )
    };
    let magnet = (MAGNET_POINTS / scale).round() as i32;
    let to_screen = |r: snap::Rect| {
        Rect::from_min_size(
            origin + vec2(r.x as f32, r.y as f32) * scale,
            vec2(r.w as f32, r.h as f32) * scale,
        )
    };
    // Tiles are drawn with a gap between them, so touching monitors still read as two.
    let gap = 3.0;
    let others = |id: u32| -> Vec<snap::Rect> {
        tiles
            .iter()
            .filter(|t| t.id != id)
            .map(|t| t.rect)
            .collect()
    };

    // Where the dragged tile is under the pointer, and where it would land.
    let ghost: Option<Ghost> = if let Some(d) = &st.drag
        && let Some(t) = tiles.iter().find(|t| t.id == d.id)
    {
        let want = ((d.at - origin) / scale - d.grab).round();
        let want = (want.x as i32, want.y as i32);
        let land = snap::snap(&others(t.id), (t.rect.w, t.rect.h), want, magnet);
        Some((t.id, want, land))
    } else {
        None
    };

    let mut order: Vec<&Tile> = tiles.iter().collect();
    if let Some((id, ..)) = ghost {
        order.sort_by_key(|t| t.id == id);
    }
    for t in order {
        let id = ui.scope_id().with(("monitor", t.id));
        let dragging = ghost.filter(|g| g.0 == t.id);
        let r = match dragging {
            Some((_, want, _)) => to_screen(t.rect.at(want)),
            None => to_screen(t.rect),
        }
        .shrink(gap);
        let mut resp = ui.interact(r, id, Sense::click_and_drag());
        if st.drag.is_none() {
            // The whole name and size, also where the tile has room for less.
            resp = resp.on_hover_text(hover_text(t));
        }
        let h = ui.ctx().animate_bool_with_time(
            id.with("h"),
            resp.hovered(),
            tk.secs(tk.th.motion.hover_ms),
        );
        let on = selected == Some(t.id);
        let (fill, fg) = colors(tk, t, on, h);
        if dragging.is_some() {
            // The one lifted thing on the page: it is in the hand, not on the desk.
            ui.painter().add(
                egui::epaint::RectShape::filled(r.translate(vec2(0.0, 6.0)), tk.r(), p.shadow)
                    .with_blur_width(18.0),
            );
        }
        let k = if t.shown { 1.0 } else { 0.5 };
        ui.painter().rect(
            r,
            tk.r(),
            faded(fill, k),
            if t.pc {
                Stroke::new(
                    tk.th.shape.stroke * 1.5,
                    mix(p.control_border, p.accent_ink, h),
                )
            } else {
                Stroke::NONE
            },
            StrokeKind::Inside,
        );
        if t.main {
            // The panel lives on the main monitor: a short bar where it sits.
            let bar = Rect::from_center_size(
                pos2(r.center().x, r.bottom() - 7.0),
                vec2(r.width() * 0.4, 3.0),
            );
            ui.painter()
                .rect_filled(bar, CornerRadius::same(2), faded(fg, 0.7));
        }
        label(ui, tk, r, t, faded(fg, k));
        tk.focus_ring(ui, &resp, tk.th.shape.radius);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, &t.name));

        if resp.clicked() {
            events.push(MapEvent::Select(t.id));
        }
        if resp.has_focus() {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    },
                );
            });
            let dir = ui.input(|i| {
                [
                    (Key::ArrowLeft, (-1, 0)),
                    (Key::ArrowRight, (1, 0)),
                    (Key::ArrowUp, (0, -1)),
                    (Key::ArrowDown, (0, 1)),
                ]
                .into_iter()
                .find(|(k, _)| i.key_pressed(*k))
                .map(|(_, d)| d)
            });
            if let Some(d) = dir {
                if !on {
                    events.push(MapEvent::Select(t.id));
                }
                if let Some((x, y)) = snap::step(&others(t.id), t.rect, d, 0) {
                    events.push(MapEvent::Place { id: t.id, x, y });
                }
            }
        }
        if resp.drag_started()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            if !on {
                events.push(MapEvent::Select(t.id));
            }
            let grab = (pos - origin) / scale - vec2(t.rect.x as f32, t.rect.y as f32);
            st.drag = Some(Drag {
                id: t.id,
                grab,
                scale,
                origin,
                at: pos,
            });
        }
        if resp.dragged()
            && let (Some(d), Some(pos)) = (&mut st.drag, resp.interact_pointer_pos())
        {
            d.at = pos;
        }
        if resp.drag_stopped() {
            if let Some((id, _, Some((x, y)))) = ghost
                && (x, y) != (t.rect.x, t.rect.y)
            {
                events.push(MapEvent::Place { id, x, y });
            }
            st.drag = None;
        }
    }
    if let Some((id, _, Some(land))) = ghost
        && let Some(t) = tiles.iter().find(|t| t.id == id)
    {
        let r = to_screen(t.rect.at(land)).shrink(gap);
        ui.painter().rect_stroke(
            r,
            tk.r(),
            Stroke::new(tk.th.shape.focus_width, p.focus),
            StrokeKind::Inside,
        );
    }
    if st.drag.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    events
}

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot3(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(a: V3) -> V3 {
    let l = dot3(a, a).sqrt();
    [a[0] / l, a[1] / l, a[2] / l]
}

fn inside(pts: &[Pos2], at: Pos2) -> bool {
    let n = pts.len();
    let side = |i: usize| {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        (b - a).x * (at - a).y - (b - a).y * (at - a).x
    };
    (0..n).all(|i| side(i) >= 0.0) || (0..n).all(|i| side(i) <= 0.0)
}

/// The displays on an arc around the viewer, seen from behind and above.
pub fn arc(
    ui: &mut Ui,
    tk: &Tk<'_>,
    st: &mut MapState,
    tiles: &[Tile],
    selected: Option<u32>,
    height: f32,
) -> Vec<MapEvent> {
    let mut events = Vec::new();
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
    let p = tk.p;
    ui.painter().rect_filled(rect, tk.r_large(), p.well);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, tk.t("mon-arc")));
    if resp.dragged() {
        let d = resp.drag_delta();
        st.yaw = (st.yaw - d.x * 0.006).clamp(-1.2, 1.2);
        st.pitch = (st.pitch + d.y * 0.004).clamp(0.05, 1.1);
    }
    if resp.double_clicked() {
        *st = MapState::default();
    }
    let Some(centre) = tiles.iter().find(|t| t.main).or_else(|| tiles.first()) else {
        return events;
    };

    // Desktop pixels to metres: a 2560 wide monitor is about 0.95 m on a 1.25 m arc.
    let k = 0.95 / 2560.0;
    let radius = 1.25;
    let cx0 = centre.rect.x as f32 + centre.rect.w as f32 / 2.0;
    let cy0 = centre.rect.y as f32 + centre.rect.h as f32 / 2.0;
    let quad = |t: &Tile| -> [V3; 4] {
        let r = t.rect;
        // A little more angle, so neighbours on the arc do not cut into each other.
        let s = (r.x as f32 + r.w as f32 / 2.0 - cx0) * k * 1.04;
        let th = s / radius;
        let y = -(r.y as f32 + r.h as f32 / 2.0 - cy0) * k;
        let c = [radius * th.sin(), y, -radius * th.cos()];
        let tangent = [th.cos(), 0.0, th.sin()];
        let (hw, hh) = (r.w as f32 * k / 2.0, r.h as f32 * k / 2.0);
        let at = |a: f32, b: f32| [c[0] + tangent[0] * a, c[1] + b, c[2] + tangent[2] * a];
        [at(-hw, hh), at(hw, hh), at(hw, -hh), at(-hw, -hh)]
    };
    let target = [0.0, -0.1, -0.8];
    let dist = 2.3;
    let cam = [
        target[0] + dist * st.yaw.sin() * st.pitch.cos(),
        target[1] + dist * st.pitch.sin(),
        target[2] + dist * st.yaw.cos() * st.pitch.cos(),
    ];
    let f = norm(sub(target, cam));
    let right = norm(cross(f, [0.0, 1.0, 0.0]));
    let up = cross(right, f);
    let proj = |pt: V3| -> (Vec2, f32) {
        let v = sub(pt, cam);
        let z = dot3(v, f);
        (vec2(dot3(v, right) / z, -dot3(v, up) / z), z)
    };

    let floor_y = tiles
        .iter()
        .map(|t| -(t.rect.y as f32 + t.rect.h as f32 - cy0) * k)
        .fold(0.0, f32::min)
        - 0.12;
    let floor: Vec<V3> = (0..=40)
        .map(|i| {
            let a = (-80.0 + 160.0 * i as f32 / 40.0f32).to_radians();
            [radius * a.sin(), floor_y, -radius * a.cos()]
        })
        .collect();
    let quads: Vec<[V3; 4]> = tiles.iter().map(quad).collect();
    let mut bb = Rect::NOTHING;
    for v in quads.iter().flatten().chain(&floor) {
        bb.extend_with(proj(*v).0.to_pos2());
    }
    let pad = tk.th.shape.padding * 2.0;
    let s =
        ((rect.width() - pad * 2.0) / bb.width()).min((rect.height() - pad * 2.0) / bb.height());
    let off = rect.center() - bb.center().to_vec2() * s;
    let scr = |pt: V3| off + proj(pt).0 * s;

    // The floor arc and the viewer: what "around you" is around.
    let line: Vec<Pos2> = floor.iter().map(|v| scr(*v)).collect();
    ui.painter().add(Shape::line(
        line,
        Stroke::new(tk.th.shape.stroke * 1.5, p.border),
    ));
    let head = scr([0.0, floor_y, 0.0]);
    let facing = scr([0.0, floor_y, -0.3]);
    ui.painter().line_segment(
        [head, facing],
        Stroke::new(tk.th.shape.focus_width, p.text_muted),
    );
    ui.painter()
        .circle_filled(head, tk.th.type_.size_body * 0.45, p.text_muted);

    let depth = |q: &[V3; 4]| {
        let c = [
            (q[0][0] + q[2][0]) / 2.0,
            (q[0][1] + q[2][1]) / 2.0,
            (q[0][2] + q[2][2]) / 2.0,
        ];
        proj(c).1
    };
    let mut order: Vec<usize> = (0..tiles.len()).collect();
    order.sort_by(|&a, &b| depth(&quads[b]).total_cmp(&depth(&quads[a])));
    let shapes: Vec<Vec<Pos2>> = quads
        .iter()
        .map(|q| q.iter().map(|v| scr(*v)).collect())
        .collect();
    let hit = resp
        .hover_pos()
        .and_then(|h| order.iter().rev().copied().find(|&i| inside(&shapes[i], h)));
    for &i in &order {
        let t = &tiles[i];
        let pts = &shapes[i];
        let on = selected == Some(t.id);
        let hov = hit == Some(i) && !resp.dragged();
        let (fill, fg) = colors(tk, t, on, if hov { 1.0 } else { 0.0 });
        let kk = if t.shown { 1.0 } else { 0.5 };
        let stroke = if t.pc || hov {
            Stroke::new(
                tk.th.shape.stroke * 1.5,
                if hov { p.accent_ink } else { p.control_border },
            )
        } else {
            Stroke::NONE
        };
        ui.painter()
            .add(Shape::convex_polygon(pts.clone(), faded(fill, kk), stroke));
        let c = pts.iter().fold(Vec2::ZERO, |a, p| a + p.to_vec2()) / 4.0;
        // The name only where it fits on the screen's face, across and down.
        let face = pts[0].distance(pts[1]).min(pts[3].distance(pts[2])) - 8.0;
        let face_height = pts[0].distance(pts[3]).min(pts[1].distance(pts[2])) - 4.0;
        for font in [tk.body_m(), tk.small()] {
            let g = ui
                .painter()
                .layout_no_wrap(t.name.clone(), font, faded(fg, kk));
            if g.size().x <= face && g.size().y <= face_height {
                ui.painter().galley(c.to_pos2() - g.size() / 2.0, g, fg);
                break;
            }
        }
    }
    if resp.clicked()
        && let Some(i) = hit
    {
        events.push(MapEvent::Select(tiles[i].id));
    }
    // The whole name and size of the display under the pointer.
    if let Some(i) = hit
        && !resp.dragged()
    {
        resp.clone().on_hover_text(hover_text(&tiles[i]));
    }
    if resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if hit.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    } else if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    /// About as the window's font measures: 0.55 of the size across a letter, 1.25 of
    /// it a line.
    fn measure(name: &str, size: &str) -> impl Fn(bool, f32) -> Vec2 {
        let (name, size) = (name.chars().count(), size.chars().count());
        move |is_name, font| {
            let n = if is_name { name } else { size };
            vec2(n as f32 * 0.55 * font, font * 1.25)
        }
    }

    fn fit(room: (f32, f32), name: &str, size: &str) -> Option<LabelFit> {
        fit_label(
            vec2(room.0, room.1),
            15.0,
            13.0,
            !size.is_empty(),
            measure(name, size),
        )
    }

    /// Whatever is shown stays inside the room.
    fn inside(room: (f32, f32), name: &str, size: &str, f: LabelFit) {
        let m = measure(name, size);
        let n = m(true, f.name);
        let mut height = n.y;
        if !f.elided {
            assert!(n.x <= room.0, "{name} at {f:?} in {room:?}");
        }
        if let Some(s) = f.size {
            let s = m(false, s);
            assert!(s.x <= room.0, "{size} at {f:?} in {room:?}");
            height += LINE_GAP + s.y;
        }
        assert!(height <= room.1, "{f:?} in {room:?}");
        assert!(f.name >= MIN_FONT && f.size.is_none_or(|s| s >= MIN_FONT));
    }

    #[test]
    fn a_wide_tile_has_both_lines_at_full_size() {
        let f = fit((200.0, 100.0), "Prostir-1", "2560 × 1440").unwrap();
        assert_eq!(
            f,
            LabelFit {
                name: 15.0,
                size: Some(13.0),
                elided: false
            }
        );
    }

    /// A portrait or a scaled monitor: smaller text first, then the name alone, then
    /// shortened; never outside the tile.
    #[test]
    fn a_narrow_tile_shrinks_the_text_then_drops_the_size_then_shortens() {
        let (name, size) = ("Prostir-3", "1440 × 2560 · 125 %");
        let shrunk = fit((110.0, 100.0), name, size).unwrap();
        assert!(shrunk.name < 15.0 && shrunk.size.is_some() && !shrunk.elided);
        inside((110.0, 100.0), name, size, shrunk);

        let alone = fit((60.0, 100.0), name, size).unwrap();
        assert_eq!(alone.size, None);
        assert!(!alone.elided);
        inside((60.0, 100.0), name, size, alone);

        let short = fit((30.0, 100.0), name, size).unwrap();
        assert!(short.elided);
        assert_eq!(short.name, MIN_FONT);
        inside((30.0, 100.0), name, size, short);
    }

    #[test]
    fn a_low_tile_keeps_one_line_and_a_tiny_one_none() {
        let (name, size) = ("Prostir-2", "2560 × 1440");
        let one = fit((200.0, 20.0), name, size).unwrap();
        assert_eq!(one.size, None);
        inside((200.0, 20.0), name, size, one);
        assert_eq!(fit((200.0, 8.0), name, size), None);
        assert_eq!(fit((6.0, 100.0), name, size), None);
    }

    #[test]
    fn every_room_gets_text_that_fits_it() {
        let (name, size) = ("Prostir-12", "3840 × 2160 · 150 %");
        for w in (4..300).step_by(7) {
            for h in (4..120).step_by(5) {
                let room = (w as f32, h as f32);
                if let Some(f) = fit(room, name, size) {
                    inside(room, name, size, f);
                }
            }
        }
    }
}
