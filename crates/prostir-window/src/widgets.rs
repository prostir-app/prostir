//! Widgets drawn from theme tokens. The few numbers left here are proportions of a
//! token (a dot's size against the text, a track's thickness), not looks of their own.
//!
//! Every control names itself for AccessKit (screen readers, and the tests that find
//! controls by name), draws the theme's focus ring when the keyboard is on it, and
//! answers Enter and Space like a click.

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Pos2, Rect, Response, Role,
    Sense, Shape, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, pos2, vec2,
};
use prostir_lang::{Arg, Strings};

use crate::fonts;
use crate::theme::{Palette, Theme, mix};

/// What every widget draws with: the colours shown now, the theme, the texts.
pub struct Tk<'a> {
    pub p: &'a Palette,
    pub th: &'a Theme,
    pub s: &'a Strings,
}

/// `c` with its alpha scaled by `k`.
pub fn faded(c: Color32, k: f32) -> Color32 {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, (a as f32 * k.clamp(0.0, 1.0)) as u8)
}

pub fn cr(r: f32) -> CornerRadius {
    CornerRadius::same(r.round().clamp(0.0, 255.0) as u8)
}

/// How strongly a disabled control shows: readable, clearly not for now.
const DISABLED: f32 = 0.45;

impl Tk<'_> {
    pub fn t(&self, id: &str) -> String {
        self.s.t(id)
    }

    pub fn f(&self, id: &str, args: &[(&str, Arg)]) -> String {
        self.s.f(id, args)
    }

    pub fn font(&self, size: f32, medium: bool) -> FontId {
        fonts::font(size, medium)
    }

    pub fn body(&self) -> FontId {
        self.font(self.th.type_.size_body, false)
    }

    pub fn body_m(&self) -> FontId {
        self.font(self.th.type_.size_body, true)
    }

    pub fn small(&self) -> FontId {
        self.font(self.th.type_.size_small, false)
    }

    pub fn heading(&self) -> FontId {
        self.font(self.th.type_.size_heading, true)
    }

    pub fn r(&self) -> CornerRadius {
        cr(self.th.shape.radius)
    }

    pub fn r_large(&self) -> CornerRadius {
        cr(self.th.shape.radius_large)
    }

    pub fn secs(&self, ms: f32) -> f32 {
        ms / 1000.0
    }

    fn hover(&self, ui: &Ui, resp: &Response) -> f32 {
        ui.ctx().animate_bool_with_time(
            resp.id.with("hover"),
            resp.hovered() || resp.is_pointer_button_down_on(),
            self.secs(self.th.motion.hover_ms),
        )
    }

    /// The keyboard focus ring, outside the widget so it never hides its edge.
    pub fn focus_ring(&self, ui: &Ui, resp: &Response, radius: f32) {
        if resp.has_focus() {
            let w = self.th.shape.focus_width;
            ui.painter().rect_stroke(
                resp.rect.expand(w * 1.5),
                cr(radius + w * 1.5),
                Stroke::new(w, self.p.focus),
                StrokeKind::Outside,
            );
        }
    }
}

/// Text that wraps to the width there is.
pub fn text(ui: &mut Ui, s: impl Into<String>, font: FontId, color: Color32) -> Response {
    ui.add(egui::Label::new(egui::RichText::new(s).font(font).color(color)).selectable(false))
}

pub fn page_header(ui: &mut Ui, tk: &Tk<'_>, title: &str, sub: Option<&str>) {
    text(ui, title, tk.font(tk.th.type_.size_title, true), tk.p.text);
    if let Some(sub) = sub {
        ui.add_space(tk.th.shape.spacing * 0.5);
        text(ui, sub, tk.body(), tk.p.text_muted);
    }
    ui.add_space(tk.th.shape.section_gap);
}

pub fn section_title(ui: &mut Ui, tk: &Tk<'_>, title: &str) {
    text(ui, title, tk.heading(), tk.p.text);
    ui.add_space(tk.th.shape.spacing * 1.25);
}

/// A group of settings that belong together.
pub fn card<R>(ui: &mut Ui, tk: &Tk<'_>, add: impl FnOnce(&mut Ui) -> R) -> R {
    card_framed(ui, tk, Stroke::new(tk.th.shape.stroke, tk.p.border), add)
}

/// A card that asks for the person's attention: its border in the accent colour.
pub fn card_accent<R>(ui: &mut Ui, tk: &Tk<'_>, add: impl FnOnce(&mut Ui) -> R) -> R {
    let width = (tk.th.shape.stroke * 2.0).max(1.5);
    card_framed(ui, tk, Stroke::new(width, tk.p.accent), add)
}

fn card_framed<R>(ui: &mut Ui, tk: &Tk<'_>, border: Stroke, add: impl FnOnce(&mut Ui) -> R) -> R {
    let pad = tk.th.shape.padding.round().clamp(0.0, 127.0) as i8;
    egui::Frame::new()
        .fill(tk.p.surface)
        .stroke(border)
        .corner_radius(tk.r_large())
        .inner_margin(egui::Margin::same(pad))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

pub fn divider(ui: &mut Ui, tk: &Tk<'_>) {
    let gap = tk.th.shape.padding * 0.6;
    ui.add_space(gap);
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), tk.th.shape.stroke.max(1.0)),
        Sense::hover(),
    );
    ui.painter().rect_filled(rect, 0.0, tk.p.border);
    ui.add_space(gap);
}

/// A title and a description on the left, a control on the right, the two centred on
/// each other. The control's size comes from the frame before, so a short switch next
/// to two lines of text sits in the middle too.
pub fn row(
    ui: &mut Ui,
    tk: &Tk<'_>,
    title: &str,
    desc: Option<&str>,
    control: impl FnOnce(&mut Ui),
) {
    let w = ui.available_width();
    let id = ui.next_auto_id().with("row");
    let prev = ui
        .data(|d| d.get_temp::<Vec2>(id))
        .unwrap_or_else(|| vec2(w * 0.4, tk.th.shape.control_height));
    let lw = (w - prev.x - tk.th.shape.padding).max(w * 0.3);
    let title_h = ui
        .painter()
        .layout(title.to_owned(), tk.body_m(), tk.p.text, lw)
        .size()
        .y;
    let desc_h = desc.map_or(0.0, |d| {
        2.0 + ui
            .painter()
            .layout(d.to_owned(), tk.small(), tk.p.text, lw)
            .size()
            .y
    });
    let block = title_h + desc_h;
    ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::right_to_left(Align::Min), |ui| {
        let c = ui
            .allocate_ui_with_layout(vec2(0.0, 0.0), Layout::top_down(Align::Max), |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.add_space(((block - prev.y) / 2.0).max(0.0));
                ui.horizontal(control).response.rect
            })
            .inner;
        ui.data_mut(|d| d.insert_temp(id, c.size()));
        ui.add_space(tk.th.shape.padding);
        ui.with_layout(Layout::top_down(Align::Min), |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(((c.height() - block) / 2.0).max(0.0));
            text(ui, title, tk.body_m(), tk.p.text);
            if let Some(d) = desc {
                ui.add_space(2.0);
                text(ui, d, tk.small(), tk.p.text_muted);
            }
        });
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The one main action of a card: filled with the accent.
    Primary,
    Secondary,
    /// Cancel, stop: no fill until hovered.
    Quiet,
    /// Forget, remove: red text, no fill until hovered.
    Danger,
}

/// How wide [`button`] makes a button with `label`.
pub fn button_width(ui: &Ui, tk: &Tk<'_>, label: &str) -> f32 {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), tk.body_m(), Color32::PLACEHOLDER);
    galley.size().x + tk.th.shape.padding * 2.0
}

pub fn button(ui: &mut Ui, tk: &Tk<'_>, label: &str, kind: Kind, enabled: bool) -> Response {
    button_named(ui, tk, label, label, kind, enabled)
}

/// A button whose accessible name says more than its text ("Forget Quest 3" for a
/// "Forget" in a list of headsets).
pub fn button_named(
    ui: &mut Ui,
    tk: &Tk<'_>,
    label: &str,
    name: &str,
    kind: Kind,
    enabled: bool,
) -> Response {
    button_at_least(ui, tk, label, name, 0.0, kind, enabled)
}

/// Buttons side by side that are equal choices (Yes and No): the same `kind` and the
/// same width, so none stands out over the other. Each is a label and the accessible
/// name that says more ("Yes, count this PC"). Returns which one was clicked.
pub fn equal_buttons(
    ui: &mut Ui,
    tk: &Tk<'_>,
    buttons: &[(&str, &str)],
    kind: Kind,
) -> Option<usize> {
    let width = buttons
        .iter()
        .map(|(l, _)| button_width(ui, tk, l))
        .fold(0.0, f32::max);
    let mut clicked = None;
    for (i, (label, name)) in buttons.iter().enumerate() {
        if button_at_least(ui, tk, label, name, width, kind, true).clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// [`button_named`] at least `width` wide.
fn button_at_least(
    ui: &mut Ui,
    tk: &Tk<'_>,
    label: &str,
    name: &str,
    width: f32,
    kind: Kind,
    enabled: bool,
) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), tk.body_m(), Color32::PLACEHOLDER);
    let pad = tk.th.shape.padding;
    let size = vec2(
        (galley.size().x + pad * 2.0).max(width),
        tk.th.shape.control_height,
    );
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let h = if enabled { tk.hover(ui, &resp) } else { 0.0 };
    let p = tk.p;
    let (fill, fg) = match kind {
        Kind::Primary => (mix(p.accent, p.accent_hover, h), p.accent_text),
        Kind::Secondary => (mix(p.control, p.control_hover, h), p.text),
        Kind::Quiet => (mix(faded(p.control, 0.0), p.control, h), p.text),
        Kind::Danger => (mix(faded(p.control, 0.0), p.control, h), p.danger),
    };
    let k = if enabled { 1.0 } else { DISABLED };
    ui.painter().rect_filled(rect, tk.r(), faded(fill, k));
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, faded(fg, k));
    tk.focus_ring(ui, &resp, tk.th.shape.radius);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, name));
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Choices side by side, the chosen one filled with the accent: the headset menu's
/// main control, so both read the same. Returns the newly chosen index.
pub fn segmented(
    ui: &mut Ui,
    tk: &Tk<'_>,
    options: &[&str],
    selected: usize,
    enabled: bool,
) -> Option<usize> {
    segmented_within(ui, tk, options, selected, enabled, f32::INFINITY)
}

/// [`segmented`], at most `within` wide: the choices narrower (less room around their
/// labels) where they would not fit.
pub fn segmented_within(
    ui: &mut Ui,
    tk: &Tk<'_>,
    options: &[&str],
    selected: usize,
    enabled: bool,
    within: f32,
) -> Option<usize> {
    let font = tk.body_m();
    let pad = tk.th.shape.padding;
    let gap = tk.th.shape.spacing * 0.75;
    let n = options.len() as f32;
    // Equal widths read as one control rather than a row of separate buttons.
    let label_w = options
        .iter()
        .map(|o| {
            ui.painter()
                .layout_no_wrap((*o).to_owned(), font.clone(), Color32::PLACEHOLDER)
                .size()
                .x
        })
        .fold(0.0, f32::max);
    let fits = ((within - gap * (n - 1.0)) / n.max(1.0)).max(label_w + pad * 0.5);
    let w = (label_w + pad * 2.0).min(fits);
    // One allocation for the whole control keeps its order in right-to-left rows.
    let (whole, base) = ui.allocate_exact_size(
        vec2(w * n + gap * (n - 1.0), tk.th.shape.control_height),
        Sense::hover(),
    );
    let mut chosen = None;
    for (i, label) in options.iter().enumerate() {
        let sense = if enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let rect = Rect::from_min_size(
            whole.min + vec2(i as f32 * (w + gap), 0.0),
            vec2(w, whole.height()),
        );
        let resp = ui.interact(rect, base.id.with(i), sense);
        let on = selected == i;
        let h = if enabled { tk.hover(ui, &resp) } else { 0.0 };
        let p = tk.p;
        let (fill, fg) = if on {
            (mix(p.accent, p.accent_hover, h), p.accent_text)
        } else {
            (mix(p.control, p.control_hover, h), p.text)
        };
        let k = if enabled { 1.0 } else { DISABLED };
        ui.painter().rect_filled(rect, tk.r(), faded(fill, k));
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            *label,
            font.clone(),
            faded(fg, k),
        );
        tk.focus_ring(ui, &resp, tk.th.shape.radius);
        resp.widget_info(|| WidgetInfo::selected(Role::RadioButton, enabled, on, *label));
        if resp.clicked() && !on {
            chosen = Some(i);
        }
        if enabled {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
    }
    chosen
}

/// On and off. Returns the new value when it was flipped.
pub fn switch(ui: &mut Ui, tk: &Tk<'_>, label: &str, on: bool, enabled: bool) -> Option<bool> {
    let hgt = tk.th.shape.control_height * 0.6;
    let size = vec2(hgt * 1.8, hgt);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let flipped = resp.clicked().then_some(!on);
    let shown = flipped.unwrap_or(on);
    let t =
        ui.ctx()
            .animate_bool_with_time(resp.id.with("on"), shown, tk.secs(tk.th.motion.state_ms));
    let h = if enabled { tk.hover(ui, &resp) } else { 0.0 };
    let p = tk.p;
    let k = if enabled { 1.0 } else { DISABLED };
    let off_fill = mix(p.control, p.control_hover, h);
    let on_fill = mix(p.accent, p.accent_hover, h);
    ui.painter().rect(
        rect,
        cr(hgt / 2.0),
        faded(mix(off_fill, on_fill, t), k),
        Stroke::new(
            tk.th.shape.stroke,
            faded(mix(p.control_border, on_fill, t), k),
        ),
        StrokeKind::Inside,
    );
    let r = hgt / 2.0 - 3.0;
    let x = egui::lerp(rect.left() + hgt / 2.0..=rect.right() - hgt / 2.0, t);
    ui.painter().circle_filled(
        pos2(x, rect.center().y),
        r,
        faded(mix(p.text_muted, p.accent_text, t), k),
    );
    tk.focus_ring(ui, &resp, hgt / 2.0);
    resp.widget_info(|| WidgetInfo::selected(Role::Switch, enabled, shown, label));
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    flipped
}

/// A level from 0 to 100; the arrow keys move it by 5. Returns the level while it is
/// being moved and whether the move is over (released, or a key press).
pub fn slider(
    ui: &mut Ui,
    tk: &Tk<'_>,
    label: &str,
    value: u8,
    width: f32,
    enabled: bool,
) -> Option<(u8, bool)> {
    let hgt = tk.th.shape.control_height;
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, hgt), sense);
    let knob = hgt * 0.22;
    let track = Rect::from_min_max(
        pos2(rect.left() + knob, rect.center().y - 3.0),
        pos2(rect.right() - knob, rect.center().y + 3.0),
    );
    let mut out = if let Some(pos) = resp.interact_pointer_pos()
        && (resp.dragged() || resp.clicked() || resp.drag_stopped())
    {
        let v = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
        Some((
            (v * 100.0).round() as u8,
            resp.clicked() || resp.drag_stopped(),
        ))
    } else {
        None
    };
    if resp.has_focus() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    ..Default::default()
                },
            );
        });
        let step = ui.input(|i| {
            i.num_presses(Key::ArrowRight) as i32 - i.num_presses(Key::ArrowLeft) as i32
        });
        if step != 0 {
            let v = ((value as i32 / 5 + step) * 5).clamp(0, 100) as u8;
            out = Some((v, true));
        }
    }
    let shown = out.map_or(value, |(v, _)| v) as f32 / 100.0;
    let p = tk.p;
    let k = if enabled { 1.0 } else { DISABLED };
    let h = if enabled { tk.hover(ui, &resp) } else { 0.0 };
    ui.painter()
        .rect_filled(track, CornerRadius::same(3), faded(p.control_hover, k));
    let mut fill = track;
    fill.set_right(track.left() + track.width() * shown);
    ui.painter()
        .rect_filled(fill, CornerRadius::same(3), faded(p.accent, k));
    let c = pos2(fill.right(), track.center().y);
    ui.painter().circle(
        c,
        knob + h * 1.5,
        faded(p.surface, k),
        Stroke::new(tk.th.shape.focus_width, faded(p.accent, k)),
    );
    if resp.has_focus() {
        ui.painter()
            .circle_stroke(c, knob + 5.0, Stroke::new(tk.th.shape.focus_width, p.focus));
    }
    resp.widget_info(|| WidgetInfo::slider(enabled, shown as f64 * 100.0, label));
    out
}

pub fn dot(ui: &Ui, tk: &Tk<'_>, center: Pos2, color: Color32) {
    ui.painter()
        .circle_filled(center, tk.th.type_.size_body * 0.3, color);
}

/// A dot for the state and the text in the normal text colour: the colour carries the
/// state, the words stay readable in both themes.
pub fn status(ui: &mut Ui, tk: &Tk<'_>, color: Color32, s: &str) {
    ui.horizontal(|ui| {
        let (r, _) =
            ui.allocate_exact_size(vec2(12.0, tk.th.type_.size_body * 1.3), Sense::hover());
        dot(ui, tk, r.center(), color);
        ui.add(
            egui::Label::new(egui::RichText::new(s).font(tk.body()).color(tk.p.text))
                .selectable(false)
                .wrap(),
        );
    });
}

/// A turning part of a ring, only while something is going on.
pub fn spinner(ui: &mut Ui, tk: &Tk<'_>, size: f32) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let t = ui.input(|i| i.time) as f32;
    let c = rect.center();
    let r = size * 0.4;
    let start = t * 4.0;
    let pts: Vec<Pos2> = (0..=24)
        .map(|i| {
            let a = start + i as f32 / 24.0 * std::f32::consts::PI * 1.4;
            c + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    ui.painter()
        .circle_stroke(c, r, Stroke::new(size * 0.09, tk.p.control_hover));
    ui.painter()
        .add(Shape::line(pts, Stroke::new(size * 0.09, tk.p.accent_ink)));
    resp.widget_info(|| WidgetInfo::new(Role::ProgressIndicator));
    // About 16 turns of the ring a second look smooth; the display's rate is not needed.
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(60));
}

pub fn battery(ui: &mut Ui, tk: &Tk<'_>, level: f32, charging: bool) {
    let h = tk.th.type_.size_title * 0.8;
    let (rect, _) = ui.allocate_exact_size(vec2(h * 2.1, h), Sense::hover());
    let body = Rect::from_min_max(rect.min, pos2(rect.right() - h * 0.18, rect.bottom()));
    let cap = Rect::from_center_size(
        pos2(rect.right() - h * 0.09, rect.center().y),
        vec2(h * 0.18, h * 0.42),
    );
    let p = tk.p;
    ui.painter().rect_stroke(
        body,
        cr(tk.th.shape.radius_small),
        Stroke::new(tk.th.shape.stroke * 1.5, p.text_muted),
        StrokeKind::Inside,
    );
    ui.painter()
        .rect_filled(cap, CornerRadius::same(2), p.text_muted);
    let inner = body.shrink(4.0);
    let mut fill = inner;
    fill.set_right(inner.left() + inner.width() * level.clamp(0.0, 1.0));
    let color = if level < 0.2 && !charging {
        p.warning
    } else {
        p.ok
    };
    ui.painter()
        .rect_filled(fill, cr(tk.th.shape.radius_small * 0.5), color);
}

/// A link to a page on the web: the accent's ink, underlined while hovered or
/// focused, named as a link.
pub fn link(ui: &mut Ui, tk: &Tk<'_>, label: &str) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), tk.body_m(), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::click());
    let color = tk.p.accent_ink;
    ui.painter().galley(rect.min, galley, color);
    if resp.hovered() || resp.has_focus() {
        let y = rect.bottom() - 1.0;
        ui.painter().line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(tk.th.shape.stroke.max(1.0), color),
        );
    }
    tk.focus_ring(ui, &resp, tk.th.shape.radius_small);
    resp.widget_info(|| WidgetInfo::labeled(Role::Link, true, label));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Counting: three rising bars in the accent's ink, `size` square.
pub fn count_icon(ui: &mut Ui, tk: &Tk<'_>, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let bar = size * 0.22;
    let gap = (size - bar * 3.0) / 2.0;
    let radius = cr(bar * 0.3);
    for (i, height) in [0.45, 0.7, 1.0].into_iter().enumerate() {
        let left = rect.left() + i as f32 * (bar + gap);
        let r = Rect::from_min_max(
            pos2(left, rect.bottom() - size * height),
            pos2(left + bar, rect.bottom()),
        );
        ui.painter().rect_filled(r, radius, tk.p.accent_ink);
    }
}

/// The app icon (packaging/prostir.svg): three screens on an arc around you.
pub fn brand_mark(ui: &mut Ui, tk: &Tk<'_>, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let s = rect.width() / 64.0;
    let at = |x: f32, y: f32| rect.min + vec2(x, y) * s;
    let b = &tk.th.brand;
    let painter = ui.painter();
    painter.rect_filled(
        Rect::from_min_size(at(2.0, 2.0), Vec2::splat(60.0 * s)),
        cr(14.0 * s),
        b.tile,
    );
    let side = faded(b.screen_side, 0.85);
    painter.add(Shape::convex_polygon(
        vec![at(8.0, 22.0), at(21.0, 19.0), at(21.0, 41.0), at(8.0, 44.0)],
        side,
        Stroke::NONE,
    ));
    painter.add(Shape::convex_polygon(
        vec![
            at(43.0, 19.0),
            at(56.0, 22.0),
            at(56.0, 44.0),
            at(43.0, 41.0),
        ],
        side,
        Stroke::NONE,
    ));
    painter.rect_filled(
        Rect::from_min_size(at(24.0, 17.0), vec2(16.0, 26.0) * s),
        cr(2.0 * s),
        b.screen,
    );
    painter.rect_filled(
        Rect::from_min_size(at(25.0, 47.0), vec2(14.0, 4.0) * s),
        cr(2.0 * s),
        b.base,
    );
}

fn code_cells(ui: &mut Ui, tk: &Tk<'_>, sense: Sense) -> (Rect, Response, Vec<Rect>) {
    let cell = tk.th.shape.control_height * 1.15;
    let gap = tk.th.shape.spacing;
    let size = vec2(cell * 6.0 + gap * 5.0 + gap * 1.5, cell * 1.2);
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let cells = (0..6)
        .map(|i| {
            // A wider gap after three digits: easier to read off and to type.
            let x = rect.left() + i as f32 * (cell + gap) + if i >= 3 { gap * 1.5 } else { 0.0 };
            Rect::from_min_size(pos2(x, rect.top()), vec2(cell, rect.height()))
        })
        .collect();
    (rect, resp, cells)
}

/// Six digits typed into one field drawn as six cells. Returns true when Enter was
/// pressed with all six there.
pub fn code_field(ui: &mut Ui, tk: &Tk<'_>, label: &str, code: &mut String, enabled: bool) -> bool {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (_, resp, cells) = code_cells(ui, tk, sense);
    if resp.clicked() {
        resp.request_focus();
    }
    let mut submit = false;
    if resp.has_focus() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                egui::EventFilter {
                    tab: false,
                    ..Default::default()
                },
            );
        });
        for e in ui.input(|i| i.events.clone()) {
            match e {
                egui::Event::Text(t) => {
                    for ch in t.chars().filter(char::is_ascii_digit) {
                        if code.len() < 6 {
                            code.push(ch);
                        }
                    }
                }
                egui::Event::Paste(t) => {
                    code.clear();
                    code.extend(t.chars().filter(char::is_ascii_digit).take(6));
                }
                egui::Event::Key {
                    key: Key::Backspace,
                    pressed: true,
                    ..
                } => {
                    code.pop();
                }
                egui::Event::Key {
                    key: Key::Enter,
                    pressed: true,
                    ..
                } => submit = code.len() == 6,
                _ => {}
            }
        }
    }
    let p = tk.p;
    let focused = resp.has_focus();
    for (i, r) in cells.iter().enumerate() {
        let current = focused && (i == code.len() || (i == 5 && code.len() == 6));
        let stroke = if current {
            Stroke::new(tk.th.shape.focus_width, p.focus)
        } else {
            Stroke::new(tk.th.shape.stroke, p.control_border)
        };
        ui.painter()
            .rect(*r, tk.r(), p.well, stroke, StrokeKind::Inside);
        if let Some(ch) = code.chars().nth(i) {
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                ch,
                tk.font(tk.th.type_.size_display, true),
                p.text,
            );
        }
    }
    let value = code.clone();
    resp.widget_info(|| {
        let mut info = WidgetInfo::text_edit(enabled, String::new(), value.clone(), "");
        info.label = Some(label.to_owned());
        info
    });
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::Text);
    }
    submit
}

/// A code made on the PC, shown big to be typed in the headset.
pub fn code_display(ui: &mut Ui, tk: &Tk<'_>, code: &str) {
    let (_, resp, cells) = code_cells(ui, tk, Sense::hover());
    let p = tk.p;
    for (i, r) in cells.iter().enumerate() {
        ui.painter().rect(
            *r,
            tk.r(),
            p.well,
            Stroke::new(tk.th.shape.stroke, p.control_border),
            StrokeKind::Inside,
        );
        if let Some(ch) = code.chars().nth(i) {
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                ch,
                tk.font(tk.th.type_.size_display, true),
                p.text,
            );
        }
    }
    let spoken = format!(
        "{} {}",
        code.get(..3).unwrap_or(code),
        code.get(3..).unwrap_or("")
    );
    resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, spoken.clone()));
}
