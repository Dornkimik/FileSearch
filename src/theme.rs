//! Colors, spacing and fonts for a calm, minimal look in both light and dark mode.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle,
    Theme, Visuals, style::ScrollStyle, vec2,
};

pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub field: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub error: Color32,
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: Color32::from_rgb(0x0F, 0x11, 0x15),
            surface: Color32::from_rgb(0x15, 0x18, 0x1E),
            field: Color32::from_rgb(0x1B, 0x1F, 0x27),
            border: Color32::from_rgb(0x25, 0x2A, 0x33),
            text: Color32::from_rgb(0xE4, 0xE7, 0xEC),
            muted: Color32::from_rgb(0x84, 0x8B, 0x98),
            accent: Color32::from_rgb(0x7C, 0x8C, 0xFF),
            accent_soft: Color32::from_rgb(0x26, 0x2E, 0x55),
            error: Color32::from_rgb(0xF2, 0x6D, 0x6D),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(0xFA, 0xFA, 0xFB),
            surface: Color32::from_rgb(0xF2, 0xF3, 0xF5),
            field: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            border: Color32::from_rgb(0xE1, 0xE3, 0xE8),
            text: Color32::from_rgb(0x1D, 0x20, 0x26),
            muted: Color32::from_rgb(0x6B, 0x72, 0x80),
            accent: Color32::from_rgb(0x4F, 0x5B, 0xD5),
            accent_soft: Color32::from_rgb(0xE3, 0xE7, 0xFF),
            error: Color32::from_rgb(0xD0, 0x3B, 0x3B),
        }
    }
}

fn visuals(dark: bool) -> Visuals {
    let p = palette(dark);
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    let radius = CornerRadius::same(7);

    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.field;
    v.text_edit_bg_color = Some(p.field);
    v.faint_bg_color = p.surface;
    v.hyperlink_color = p.accent;
    v.error_fg_color = p.error;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(if dark { 110 } else { 30 }) };
    v.popup_shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if dark { 100 } else { 25 }) };
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.0, if dark { Color32::from_rgb(0xC9, 0xD0, 0xFF) } else { p.accent });

    let hover = if dark { Color32::from_rgb(0x23, 0x28, 0x32) } else { Color32::from_rgb(0xE8, 0xEA, 0xEE) };
    let active = if dark { Color32::from_rgb(0x2B, 0x31, 0x3D) } else { Color32::from_rgb(0xDD, 0xE0, 0xE6) };

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = p.field;
    w.inactive.weak_bg_fill = p.field;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = radius;

    w.hovered.bg_fill = hover;
    w.hovered.weak_bg_fill = hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border);
    w.hovered.fg_stroke = Stroke::new(1.5, p.text);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = active;
    w.active.weak_bg_fill = active;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.5, p.text);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open = w.hovered;
    v
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_visuals_of(Theme::Dark, visuals(true));
    ctx.set_visuals_of(Theme::Light, visuals(false));
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
        let s = &mut style.spacing;
        s.item_spacing = vec2(8.0, 6.0);
        s.button_padding = vec2(10.0, 5.0);
        s.interact_size.y = 28.0;
        s.menu_margin = Margin::same(6);
        s.scroll = ScrollStyle::floating();
        style.interaction.selectable_labels = false;
    });
}

pub const SEMIBOLD: &str = "semibold";

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();

    let mut load = |key: &str, file: &str| -> bool {
        match std::fs::read(format!(r"{dir}\Fonts\{file}")) {
            Ok(bytes) => {
                fonts.font_data.insert(key.into(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            Err(_) => false,
        }
    };
    let regular = load("segoe", "segoeui.ttf");
    let semibold = load("segoe-sb", "seguisb.ttf");

    let mut prop = fallbacks.clone();
    if regular {
        prop.insert(0, "segoe".into());
    }
    let mut strong = fallbacks;
    if semibold {
        strong.insert(0, "segoe-sb".into());
    } else if regular {
        strong.insert(0, "segoe".into());
    }
    fonts.families.insert(FontFamily::Proportional, prop);
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), strong);
    ctx.set_fonts(fonts);
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}
