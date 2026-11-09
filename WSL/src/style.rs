//! The app's look: one palette, one type family, one shape for every control,
//! installed once on the context so every tab gets it without asking.
//!
//! Status owns the loud colours (green healthy, amber warning, red error,
//! blue in progress), so the accent -- selection, text cursor, links, the one
//! primary button -- is iris violet, a hue no status colour uses. A selected
//! row must never read as healthy.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow,
    Stroke, TextStyle, Theme, Vec2,
};
use std::sync::Arc;

/// The status colours the accent must stay clear of. Healthy, warning, error,
/// in progress, and the muted "unknown".
pub mod status {
    use eframe::egui::Color32;
    pub const HEALTHY: Color32 = Color32::from_rgb(0x4C, 0xAF, 0x50);
    pub const WARNING: Color32 = Color32::from_rgb(0xE6, 0xA2, 0x3C);
    pub const ERROR: Color32 = Color32::from_rgb(0xE5, 0x53, 0x4F);
    pub const PROGRESS: Color32 = Color32::from_rgb(0x4A, 0x9E, 0xD8);
}

/// The dark palette, darkest to lightest.
pub mod palette {
    use eframe::egui::Color32;
    /// Text inputs and other wells: below the surface they sit on.
    pub const INSET: Color32 = Color32::from_rgb(0x11, 0x13, 0x18);
    /// The central panel.
    pub const GROUND: Color32 = Color32::from_rgb(0x16, 0x18, 0x1F);
    /// Every other row of a striped table.
    pub const STRIPE: Color32 = Color32::from_rgb(0x1A, 0x1D, 0x24);
    /// The side and top panels, a step up from the ground.
    pub const PANEL: Color32 = Color32::from_rgb(0x1B, 0x1E, 0x26);
    /// Windows, menus and tooltips.
    pub const WINDOW: Color32 = Color32::from_rgb(0x1F, 0x22, 0x2B);
    /// A button at rest.
    pub const RAISED: Color32 = Color32::from_rgb(0x26, 0x2A, 0x35);
    pub const HOVER: Color32 = Color32::from_rgb(0x2F, 0x34, 0x41);
    pub const PRESSED: Color32 = Color32::from_rgb(0x39, 0x3F, 0x4E);
    /// Separators and panel edges.
    pub const LINE: Color32 = Color32::from_rgb(0x2C, 0x30, 0x3B);
    /// A control's edge.
    pub const EDGE: Color32 = Color32::from_rgb(0x3A, 0x3F, 0x4D);
    pub const EDGE_HOVER: Color32 = Color32::from_rgb(0x52, 0x58, 0x6C);
    pub const TEXT: Color32 = Color32::from_rgb(0xD3, 0xD7, 0xE0);
    pub const STRONG: Color32 = Color32::from_rgb(0xF1, 0xF3, 0xF7);
    pub const MUTED: Color32 = Color32::from_rgb(0x8D, 0x93, 0xA3);
    /// Selection, focus, the cursor, links. No status uses this hue.
    pub const ACCENT: Color32 = Color32::from_rgb(0x8C, 0x7C, 0xF0);
    pub const LINK: Color32 = Color32::from_rgb(0xA9, 0x9C, 0xF7);
    /// A selected row or tab: the accent sunk into the ground.
    pub const SELECTED: Color32 = Color32::from_rgb(0x35, 0x2F, 0x5E);
    pub const SELECTED_TEXT: Color32 = Color32::from_rgb(0xEC, 0xE8, 0xFF);
}

/// The type scale, in points.
pub mod size {
    pub const SMALL: f32 = 11.5;
    pub const BODY: f32 = 13.0;
    pub const BUTTON: f32 = 13.0;
    pub const MONO: f32 = 12.5;
    pub const HEADING: f32 = 17.0;
}

const SANS: &str = "IBM Plex Sans";
const SANS_MEDIUM: &str = "IBM Plex Sans Medium";
const SANS_SEMIBOLD: &str = "IBM Plex Sans SemiBold";
const MONO: &str = "IBM Plex Mono";

/// Buttons and tabs: Plex Medium.
pub fn medium() -> FontFamily {
    FontFamily::Name(SANS_MEDIUM.into())
}

/// Headings: Plex SemiBold. egui's `strong()` only changes colour.
pub fn semibold() -> FontFamily {
    FontFamily::Name(SANS_SEMIBOLD.into())
}

/// Plex first in each family, egui's own fonts behind it for symbols.
pub fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let add = |fonts: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.to_string(), Arc::new(FontData::from_static(bytes)));
    };
    add(&mut fonts, SANS, include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"));
    add(&mut fonts, SANS_MEDIUM, include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf"));
    add(&mut fonts, SANS_SEMIBOLD, include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"));
    add(&mut fonts, MONO, include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"));

    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let with = |first: &str| {
        std::iter::once(first.to_string())
            .chain(fallbacks.iter().cloned())
            .collect::<Vec<_>>()
    };
    fonts.families.insert(FontFamily::Proportional, with(SANS));
    fonts.families.insert(medium(), with(SANS_MEDIUM));
    fonts.families.insert(semibold(), with(SANS_SEMIBOLD));
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, MONO.to_string());
    fonts
}

/// Sizes and spacing, the same in both themes.
fn shape(style: &mut egui::Style) {
    style.text_styles = [
        (TextStyle::Small, FontId::new(size::SMALL, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(size::BODY, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(size::BUTTON, medium())),
        (TextStyle::Heading, FontId::new(size::HEADING, semibold())),
        (TextStyle::Monospace, FontId::new(size::MONO, FontFamily::Monospace)),
    ]
    .into();

    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(8.0, 4.0);
    s.button_padding = Vec2::new(10.0, 4.0);
    s.interact_size = Vec2::new(40.0, 24.0);
    s.window_margin = Margin::same(14);
    s.menu_margin = Margin::same(6);
    s.indent = 16.0;
    s.icon_width = 15.0;
    s.icon_width_inner = 9.0;
    s.icon_spacing = 6.0;
    s.scroll = egui::style::ScrollStyle::thin();

    let v = &mut style.visuals;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        // 4, not more: a checkbox shares this radius, and anything rounder
        // reads as a radio button.
        w.corner_radius = CornerRadius::same(4);
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.corner_radius = CornerRadius::same(6);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = true;
}

/// The dark palette onto egui's visuals.
fn paint_dark(v: &mut egui::Visuals) {
    use palette::*;
    let edge = |c| Stroke::new(1.0_f32, c);

    v.panel_fill = GROUND;
    v.window_fill = WINDOW;
    v.window_stroke = edge(EDGE);
    v.extreme_bg_color = INSET;
    v.faint_bg_color = STRIPE;
    v.code_bg_color = RAISED;
    v.hyperlink_color = LINK;
    v.weak_text_color = Some(MUTED);
    v.warn_fg_color = status::WARNING;
    v.error_fg_color = status::ERROR;
    v.selection.bg_fill = SELECTED;
    v.selection.stroke = Stroke::new(1.0_f32, SELECTED_TEXT);
    v.text_cursor.stroke = Stroke::new(2.0_f32, ACCENT);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(130),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PANEL;
    w.noninteractive.weak_bg_fill = PANEL;
    w.noninteractive.bg_stroke = edge(LINE);
    w.noninteractive.fg_stroke = edge(TEXT);
    w.inactive.bg_fill = RAISED;
    w.inactive.weak_bg_fill = RAISED;
    w.inactive.bg_stroke = edge(EDGE);
    w.inactive.fg_stroke = edge(TEXT);
    w.hovered.bg_fill = HOVER;
    w.hovered.weak_bg_fill = HOVER;
    w.hovered.bg_stroke = edge(EDGE_HOVER);
    w.hovered.fg_stroke = edge(STRONG);
    // Pressed: the one place the accent edges a control.
    w.active.bg_fill = PRESSED;
    w.active.weak_bg_fill = PRESSED;
    w.active.bg_stroke = edge(ACCENT);
    w.active.fg_stroke = edge(STRONG);
    w.open.bg_fill = HOVER;
    w.open.weak_bg_fill = HOVER;
    w.open.bg_stroke = edge(EDGE_HOVER);
    w.open.fg_stroke = edge(STRONG);
}

/// The light theme keeps egui's colours; only the accent is shared.
fn paint_light(v: &mut egui::Visuals) {
    v.selection.bg_fill = Color32::from_rgb(0xDD, 0xD8, 0xFC);
    v.selection.stroke = Stroke::new(1.0_f32, Color32::from_rgb(0x2E, 0x22, 0x8A));
    v.hyperlink_color = Color32::from_rgb(0x5B, 0x47, 0xD6);
    v.text_cursor.stroke = Stroke::new(2.0_f32, Color32::from_rgb(0x5B, 0x47, 0xD6));
}

/// Fonts, sizes and both palettes. Called once, before the first frame.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.all_styles_mut(shape);
    ctx.style_mut_of(Theme::Dark, |s| paint_dark(&mut s.visuals));
    ctx.style_mut_of(Theme::Light, |s| paint_light(&mut s.visuals));
}

/// The one thing on a row to do next. Accent edge and text on the ordinary
/// fill, never a solid accent block. Use sparingly.
pub fn primary<'a>(ui: &egui::Ui, text: &str) -> egui::Button<'a> {
    let (edge, ink) = if ui.visuals().dark_mode {
        (palette::ACCENT, palette::SELECTED_TEXT)
    } else {
        (
            Color32::from_rgb(0x5B, 0x47, 0xD6),
            Color32::from_rgb(0x2E, 0x22, 0x8A),
        )
    };
    egui::Button::new(egui::RichText::new(text).color(ink)).stroke(Stroke::new(1.0_f32, edge))
}

/// The side and top panels: a step up from the ground, edged by a line.
pub fn side_panel_frame(ctx: &egui::Context) -> egui::Frame {
    let frame = egui::Frame::side_top_panel(&ctx.style());
    if ctx.style().visuals.dark_mode {
        frame.fill(palette::PANEL)
    } else {
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels(c: Color32) -> [u8; 3] {
        [c.r(), c.g(), c.b()]
    }

    fn hue(c: Color32) -> f32 {
        let [r, g, b] = channels(c).map(|x| x as f32 / 255.0);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let d = max - min;
        let h = if d == 0.0 {
            0.0
        } else if max == r {
            60.0 * (((g - b) / d) % 6.0)
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        (h + 360.0) % 360.0
    }

    fn distance(a: f32, b: f32) -> f32 {
        let d = (a - b).abs() % 360.0;
        d.min(360.0 - d)
    }

    #[test]
    fn the_accent_is_no_status_colour() {
        // The amber/red/green/blue the app already paints its status text with,
        // plus the constants above.
        let statuses = [
            status::HEALTHY,
            status::WARNING,
            status::ERROR,
            status::PROGRESS,
            Color32::from_rgb(220, 80, 80),
            Color32::from_rgb(220, 150, 60),
            Color32::from_rgb(120, 180, 120),
            Color32::from_rgb(255, 205, 0),
        ];
        for status in statuses {
            assert!(
                distance(hue(palette::ACCENT), hue(status)) >= 30.0,
                "accent too close to {:?}",
                channels(status)
            );
        }
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let lum = |c: Color32| {
            let lin = |x: u8| {
                let x = x as f32 / 255.0;
                if x <= 0.03928 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
            };
            let [r, g, b] = channels(c);
            0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn text_is_readable_on_every_surface_it_sits_on() {
        use palette::*;
        for surface in [INSET, GROUND, STRIPE, PANEL, WINDOW, RAISED, HOVER] {
            assert!(contrast(TEXT, surface) >= 7.0, "body text on {:?}", channels(surface));
        }
        for surface in [INSET, GROUND, STRIPE, PANEL, WINDOW, RAISED] {
            assert!(contrast(MUTED, surface) >= 4.5, "muted text on {:?}", channels(surface));
        }
        assert!(contrast(SELECTED_TEXT, SELECTED) >= 7.0);
        assert!(contrast(LINK, GROUND) >= 4.5);
    }

    #[test]
    fn the_surfaces_step_up_in_order() {
        use palette::*;
        let lightness = |c: Color32| channels(c).iter().map(|&x| x as u32).sum::<u32>();
        for pair in [INSET, GROUND, PANEL, WINDOW, RAISED, HOVER, PRESSED].windows(2) {
            assert!(lightness(pair[0]) < lightness(pair[1]));
        }
    }

    #[test]
    fn every_font_named_in_a_family_is_loaded() {
        let fonts = fonts();
        for (family, names) in &fonts.families {
            for name in names {
                assert!(
                    fonts.font_data.contains_key(name),
                    "{family:?} names {name}, which is not loaded"
                );
            }
        }
        assert_eq!(fonts.families[&FontFamily::Proportional][0], SANS);
        assert_eq!(fonts.families[&FontFamily::Monospace][0], MONO);
        assert!(
            fonts.families[&FontFamily::Proportional].len() > 1,
            "egui's fonts stay behind Plex for symbols"
        );
    }

    #[test]
    fn installed_controls_do_not_grow_under_the_pointer() {
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.set_theme(Theme::Dark);
        let style = ctx.style();
        assert_eq!(style.visuals.widgets.hovered.expansion, 0.0);
        assert_eq!(style.visuals.panel_fill, palette::GROUND);
        assert_eq!(style.text_styles[&TextStyle::Heading].family, semibold());
    }
}
