//! The window's colours and its egui styles, dark and light.
//!
//! The look is a dark cockpit: normal states are calm neutrals, cyan marks
//! what can be acted on, amber appears only when something needs the user,
//! and red only when SLC is about to lose speech.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Shadow, Stroke, TextStyle, Theme,
    ThemePreference,
};

use super::fonts::STRONG;

/// The colour tokens of one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// The window's background.
    pub panel: Color32,
    /// Grouped rows.
    pub surface: Color32,
    /// Inputs, secondary buttons and pressed rows.
    pub surface_2: Color32,
    /// 1 px strokes and dividers.
    pub line: Color32,
    /// Text.
    pub ink: Color32,
    /// Secondary text, and the trace's spoken bars.
    pub ink_dim: Color32,
    /// Primary buttons, the selected tab, the speaking bar and links.
    pub act: Color32,
    /// The ready lamp, and nothing else.
    pub ok: Color32,
    /// Failed announcements and downloads, and the update banner's edge.
    pub caution: Color32,
    /// The background behind caution text, where the theme needs one.
    pub caution_bg: Option<Color32>,
    /// Text that warns SLC is about to lose speech.
    pub warn: Color32,
}

/// `Color32::from_rgb` written as one hex number, `0xRRGGBB`, so the
/// tables below read like the design's own.
const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const DARK: Palette = Palette {
    panel: hex(0x20262C),
    surface: hex(0x29313A),
    surface_2: hex(0x313A44),
    line: hex(0x3A444F),
    ink: hex(0xE6EBEF),
    ink_dim: hex(0xA0ACB6),
    act: hex(0x56C6E3),
    ok: hex(0x5FCF8F),
    caution: hex(0xF2AD45),
    caution_bg: None,
    warn: hex(0xF27166),
};

pub const LIGHT: Palette = Palette {
    panel: hex(0xF3F4F6),
    surface: hex(0xFFFFFF),
    surface_2: hex(0xEEF1F4),
    line: hex(0xD9DEE3),
    ink: hex(0x1A2228),
    ink_dim: hex(0x56636D),
    act: hex(0x00708A),
    ok: hex(0x1F8A4C),
    caution: hex(0x9A5A00),
    caution_bg: Some(hex(0xFFF3DC)),
    warn: hex(0xC0362C),
};

/// Corner radius of buttons and inputs.
pub const WIDGET_RADIUS: u8 = 4;
/// Corner radius of grouped rows.
pub const ROW_RADIUS: u8 = 6;

/// Body text's size, in points. Body strong is the same size, semibold.
const BODY_SIZE: f32 = 14.0;

/// Body strong: body-sized text in the semibold family, for labels that
/// head a group. (`RichText::strong` only changes the colour.)
pub fn body_strong() -> FontId {
    FontId::new(BODY_SIZE, FontFamily::Name(STRONG.into()))
}

/// `text` in the caution colour. The light theme puts caution text on a
/// pale amber background too, so it stays readable.
pub fn caution_text(text: egui::RichText, p: &Palette) -> egui::RichText {
    let text = text.color(p.caution);
    match p.caution_bg {
        Some(background) => text.background_color(background),
        None => text,
    }
}

/// The palette for dark (true) or light mode.
pub fn palette(dark: bool) -> &'static Palette {
    if dark { &DARK } else { &LIGHT }
}

/// egui's style for `p`. `dark` picks egui's dark or light defaults to
/// start from, for everything the palette doesn't set.
pub fn style(p: &Palette, dark: bool) -> egui::Style {
    let mut style = egui::Style::default();
    let v = &mut style.visuals;
    *v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.window_stroke = Stroke::new(1.0, p.line);
    v.extreme_bg_color = p.surface_2;
    v.faint_bg_color = p.surface;
    v.code_bg_color = p.surface_2;
    v.hyperlink_color = p.act;
    v.warn_fg_color = p.caution;
    v.error_fg_color = p.warn;
    v.weak_text_color = Some(p.ink_dim);
    // A blinking cursor repaints the window twice a second while a text
    // field has focus, and nothing on the window moves but a download bar.
    v.text_cursor.blink = false;
    // Selected tabs and selected text: the action colour, with text in the
    // window's background colour on top of it.
    v.selection.bg_fill = p.act;
    v.selection.stroke = Stroke::new(1.0, p.panel);
    // No drop shadows: Windows draws the window's own.
    v.window_shadow = Shadow::NONE;
    v.popup_shadow = Shadow::NONE;
    v.window_corner_radius = CornerRadius::same(ROW_RADIUS);
    v.menu_corner_radius = CornerRadius::same(ROW_RADIUS);

    let radius = CornerRadius::same(WIDGET_RADIUS);
    let w = &mut v.widgets;
    // Labels, separators and frames.
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.ink);
    w.noninteractive.corner_radius = radius;
    // Buttons and inputs at rest, under the pointer, pressed, and open.
    for (widget, fill) in [
        (&mut w.inactive, p.surface_2),
        (&mut w.hovered, p.line),
        (&mut w.active, p.line),
        (&mut w.open, p.surface_2),
    ] {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.fg_stroke = Stroke::new(1.0, p.ink);
        widget.corner_radius = radius;
        // No growing under the pointer: nothing moves.
        widget.expansion = 0.0;
    }
    w.inactive.bg_stroke = Stroke::NONE;
    w.hovered.bg_stroke = Stroke::new(1.0, p.line);
    w.active.bg_stroke = Stroke::new(1.0, p.act);
    w.open.bg_stroke = Stroke::new(1.0, p.line);

    let strong = FontFamily::Name(STRONG.into());
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(12.0)),
        (TextStyle::Body, FontId::proportional(BODY_SIZE)),
        (TextStyle::Button, FontId::proportional(BODY_SIZE)),
        (TextStyle::Heading, FontId::new(20.0, strong)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
    ]
    .into();
    style
}

/// Installs both styles and follows Windows' light or dark app mode.
pub fn install(ctx: &egui::Context) {
    ctx.set_style_of(Theme::Dark, style(&DARK, true));
    ctx.set_style_of(Theme::Light, style(&LIGHT, false));
    ctx.set_theme(ThemePreference::System);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_match_design() {
        let dark = [
            (DARK.panel, 0x20262C),
            (DARK.surface, 0x29313A),
            (DARK.surface_2, 0x313A44),
            (DARK.line, 0x3A444F),
            (DARK.ink, 0xE6EBEF),
            (DARK.ink_dim, 0xA0ACB6),
            (DARK.act, 0x56C6E3),
            (DARK.ok, 0x5FCF8F),
            (DARK.caution, 0xF2AD45),
            (DARK.warn, 0xF27166),
        ];
        let light = [
            (LIGHT.panel, 0xF3F4F6),
            (LIGHT.surface, 0xFFFFFF),
            (LIGHT.surface_2, 0xEEF1F4),
            (LIGHT.line, 0xD9DEE3),
            (LIGHT.ink, 0x1A2228),
            (LIGHT.ink_dim, 0x56636D),
            (LIGHT.act, 0x00708A),
            (LIGHT.ok, 0x1F8A4C),
            (LIGHT.caution, 0x9A5A00),
            (LIGHT.warn, 0xC0362C),
        ];
        for (color, rgb) in dark.into_iter().chain(light) {
            let [r, g, b, a] = color.to_array();
            assert_eq!(
                (r, g, b, a),
                ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255),
                "{rgb:06X}"
            );
        }
        assert_eq!(DARK.caution_bg, None);
        assert_eq!(LIGHT.caution_bg, Some(Color32::from_rgb(0xFF, 0xF3, 0xDC)));
    }

    #[test]
    fn style_maps_tokens() {
        for (p, dark) in [(&DARK, true), (&LIGHT, false)] {
            let s = style(p, dark);
            let v = &s.visuals;
            assert_eq!(v.dark_mode, dark);
            assert_eq!(v.panel_fill, p.panel);
            assert_eq!(v.window_fill, p.panel);
            assert_eq!(v.extreme_bg_color, p.surface_2);
            assert_eq!(v.widgets.inactive.bg_fill, p.surface_2);
            assert_eq!(v.hyperlink_color, p.act);
            // A blinking cursor would repaint the window twice a second
            // while a field has focus.
            assert!(!v.text_cursor.blink);
            assert_eq!(v.selection.bg_fill, p.act);
            assert_eq!(v.widgets.noninteractive.bg_stroke, Stroke::new(1.0, p.line));
            assert_eq!(v.widgets.inactive.corner_radius, CornerRadius::same(4));
            assert_eq!(v.widgets.hovered.corner_radius, CornerRadius::same(4));
            assert_eq!(v.window_shadow, Shadow::NONE);

            let size = |text_style: TextStyle| s.text_styles[&text_style].size;
            assert_eq!(size(TextStyle::Body), 14.0);
            assert_eq!(size(TextStyle::Button), 14.0);
            assert_eq!(size(TextStyle::Small), 12.0);
            assert_eq!(size(TextStyle::Heading), 20.0);
            assert_eq!(
                s.text_styles[&TextStyle::Heading].family,
                FontFamily::Name(STRONG.into())
            );
        }
    }

    #[test]
    fn body_strong_is_semibold_body() {
        assert_eq!(
            body_strong(),
            FontId::new(14.0, FontFamily::Name(STRONG.into()))
        );
    }
}
