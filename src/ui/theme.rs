//! Maps the launcher's palette onto egui's style, and provides the handful of
//! custom widgets the WPF build styled by hand.

use egui::{Color32, CornerRadius, Stroke, Ui, Vec2};

use crate::model::colors::Palette;

pub const WINDOW_SIZE: Vec2 = Vec2::new(1000.0, 720.0);
/// Mod artwork is authored at 500x100; the row scales it to fit the panel but
/// never taller than this, so more cards stay on screen.
pub const MOD_IMAGE_ASPECT: f32 = 5.0;
pub const MOD_IMAGE_MAX_HEIGHT: f32 = 76.0;

pub fn apply(ctx: &egui::Context, palette: &Palette) {
    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;

    v.dark_mode = true;
    v.override_text_color = Some(palette.default_text);
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = palette.dark_background;
    v.window_stroke = Stroke::new(1.0_f32, palette.border);
    v.extreme_bg_color = palette.dark_background;
    v.faint_bg_color = palette.light_background;

    let radius = CornerRadius::same(2);

    v.widgets.noninteractive.bg_fill = palette.dark_background;
    v.widgets.noninteractive.weak_bg_fill = palette.dark_background;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, palette.inactive_border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, palette.default_text);
    v.widgets.noninteractive.corner_radius = radius;

    v.widgets.inactive.bg_fill = palette.dark_background;
    v.widgets.inactive.weak_bg_fill = palette.dark_background;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, palette.border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, palette.default_text);
    v.widgets.inactive.corner_radius = radius;

    v.widgets.hovered.bg_fill = palette.button_selection;
    v.widgets.hovered.weak_bg_fill = palette.button_selection;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, palette.active);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, palette.default_text);
    v.widgets.hovered.corner_radius = radius;

    v.widgets.active.bg_fill = palette.button_selection;
    v.widgets.active.weak_bg_fill = palette.button_selection;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, palette.active);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, palette.default_text);
    v.widgets.active.corner_radius = radius;

    v.widgets.open.bg_fill = palette.dark_background;
    v.widgets.open.weak_bg_fill = palette.dark_background;
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, palette.border);
    v.widgets.open.corner_radius = radius;

    v.selection.bg_fill = palette.list_selection1;
    v.selection.stroke = Stroke::new(1.0_f32, palette.active);

    style.spacing.item_spacing = Vec2::new(6.0, 6.0);
    style.spacing.button_padding = Vec2::new(8.0, 4.0);

    ctx.set_style(style);
}

/// The blinking accent the WPF build used to draw attention to an update.
/// Returns a colour oscillating between `base` and `accent`.
pub fn blink(ctx: &egui::Context, base: Color32, accent: Color32) -> Color32 {
    // Two-second cycle, matching the 0.5s-each-way WPF storyboard.
    let t = ctx.input(|i| i.time);
    ctx.request_repaint_after(std::time::Duration::from_millis(50));
    let phase = ((t * std::f64::consts::PI).sin().abs()) as f32;
    lerp_color(base, accent, phase)
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgba_unmultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

/// A progress bar drawn in the launcher's own colours, with the status line
/// centred on top the way the WPF `InfoTextBlock` sat over the bar.
pub fn progress_bar(
    ui: &mut Ui,
    palette: &Palette,
    fraction: f32,
    label: &str,
    active: bool,
    height: f32,
) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());

    let (background, border, text_color) = if active {
        (palette.button_selection, palette.border, palette.download_text)
    } else {
        (palette.dark_background, palette.inactive_border, palette.default_text)
    };

    let painter = ui.painter();
    let radius = CornerRadius::same(2);
    painter.rect_filled(rect, radius, background);

    if fraction > 0.0 {
        let mut filled = rect;
        filled.set_width(rect.width() * fraction.clamp(0.0, 1.0));
        painter.rect_filled(filled, radius, palette.active.gamma_multiply(0.85));
    }

    painter.rect_stroke(rect, radius, Stroke::new(1.0_f32, border), egui::StrokeKind::Inside);

    if !label.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.0),
            text_color,
        );
    }
}

/// A button that pulses until the user interacts with it.
pub fn blinking_button(
    ui: &mut Ui,
    palette: &Palette,
    text: &str,
    size: Vec2,
    blinking: bool,
) -> egui::Response {
    let fill = if blinking {
        blink(ui.ctx(), palette.dark_background, palette.button_selection)
    } else {
        palette.dark_background
    };

    ui.add_sized(
        size,
        egui::Button::new(egui::RichText::new(text).size(15.0))
            .fill(fill)
            .stroke(Stroke::new(1.0_f32, palette.border)),
    )
}

/// A slider with a visible track.
///
/// egui draws the rail with `widgets.inactive.bg_fill`, which this theme sets
/// to the near-black panel colour — so the rail has to be lightened locally or
/// the control renders as a lone floating handle.
pub fn slider<T>(
    ui: &mut Ui,
    palette: &Palette,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
) -> egui::Response
where
    T: egui::emath::Numeric,
{
    let mut scoped = ui.new_child(
        egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).layout(*ui.layout()),
    );
    scoped.visuals_mut().widgets.inactive.bg_fill = palette.inactive_border;
    scoped.visuals_mut().widgets.hovered.bg_fill = palette.active;
    scoped.visuals_mut().widgets.active.bg_fill = palette.active;

    let response = scoped.add(egui::Slider::new(value, range).trailing_fill(true));
    ui.advance_cursor_after_rect(scoped.min_rect());
    response
}

/// Section heading used throughout the options screen.
pub fn heading(ui: &mut Ui, palette: &Palette, text: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(text).size(16.0).strong().color(palette.active));
    ui.add_space(2.0);
}
