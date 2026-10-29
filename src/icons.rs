//! Tiny vector icons painted directly, so they look crisp and never depend on font coverage.

use std::f32::consts::{FRAC_1_SQRT_2, TAU};

use eframe::egui::{Color32, CornerRadius, Painter, Rect, Stroke, StrokeKind, pos2, vec2};

pub fn magnifier(p: &Painter, rect: Rect, color: Color32) {
    let s = rect.width().min(rect.height());
    let c = rect.center() - vec2(s * 0.09, s * 0.09);
    let r = s * 0.3;
    p.circle_stroke(c, r, Stroke::new(1.6, color));
    let dir = vec2(FRAC_1_SQRT_2, FRAC_1_SQRT_2);
    p.line_segment([c + dir * (r + 0.6), c + dir * (r + s * 0.3)], Stroke::new(2.0, color));
}

pub fn folder(p: &Painter, rect: Rect, color: Color32) {
    let body = Rect::from_center_size(rect.center() + vec2(0.0, 1.0), vec2(16.0, 11.0));
    let tab = Rect::from_min_size(body.min - vec2(0.0, 2.5), vec2(7.0, 4.0));
    p.rect_filled(tab, CornerRadius::same(1), color);
    p.rect_filled(body, CornerRadius::same(2), color);
}

pub fn file(p: &Painter, rect: Rect, color: Color32) {
    let r = Rect::from_center_size(rect.center(), vec2(12.0, 15.0));
    p.rect_filled(r, CornerRadius::same(2), color.gamma_multiply(0.2));
    p.rect_stroke(r, CornerRadius::same(2), Stroke::new(1.3, color), StrokeKind::Inside);
    for i in 0..2 {
        let y = r.top() + 5.5 + i as f32 * 3.5;
        p.line_segment([pos2(r.left() + 3.0, y), pos2(r.right() - 3.0, y)], Stroke::new(1.0, color));
    }
}

pub fn sun(p: &Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    p.circle_filled(c, 3.6, color);
    for i in 0..8 {
        let a = i as f32 / 8.0 * TAU;
        let d = vec2(a.cos(), a.sin());
        p.line_segment([c + d * 5.8, c + d * 7.8], Stroke::new(1.5, color));
    }
}

pub fn moon(p: &Painter, rect: Rect, color: Color32, bg: Color32) {
    let c = rect.center();
    p.circle_filled(c, 6.5, color);
    p.circle_filled(c + vec2(3.4, -3.0), 5.6, bg);
}

pub fn close(p: &Painter, rect: Rect, color: Color32) {
    let c = rect.center();
    let s = 4.0;
    let stroke = Stroke::new(1.5, color);
    p.line_segment([c + vec2(-s, -s), c + vec2(s, s)], stroke);
    p.line_segment([c + vec2(-s, s), c + vec2(s, -s)], stroke);
}
