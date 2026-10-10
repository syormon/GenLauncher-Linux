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
    let mut style = (*ctx.global_style()).clone();
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

    ctx.set_global_style(style);
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
/// A selectable list row whose size does not depend on its state.
///
/// egui draws a selectable label with no frame until it is hovered or
/// selected, and a frame's 1px border takes up space. So the row under the
/// pointer grew by 2px each way, and whatever was sized to the list, a dialog
/// or a drop-down, twitched as the pointer moved over it. Here the frame is
/// always there; on an idle row it is simply invisible.
pub fn selectable_row<'a>(ui: &mut Ui, selected: bool, text: impl egui::IntoAtoms<'a>) -> egui::Response {
    ui.scope(|ui| {
        let idle = &mut ui.visuals_mut().widgets.inactive;
        idle.bg_stroke.color = Color32::TRANSPARENT;
        idle.bg_fill = Color32::TRANSPARENT;
        idle.weak_bg_fill = Color32::TRANSPARENT;

        ui.add(egui::Button::selectable(selected, text).frame_when_inactive(true))
    })
    .inner
}

/// A progress bar for a job whose steps cannot always say how far along they
/// are. With a fraction it fills; without one a block slides across, so the
/// user can still see that work is going on.
pub fn stage_progress_bar(
    ui: &mut Ui,
    palette: &Palette,
    fraction: Option<f32>,
    label: &str,
    height: f32,
) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());

    let painter = ui.painter();
    let radius = CornerRadius::same(2);
    painter.rect_filled(rect, radius, palette.button_selection);

    let fill = palette.active.gamma_multiply(0.85);
    match fraction {
        Some(fraction) => {
            let mut filled = rect;
            filled.set_width(rect.width() * fraction.clamp(0.0, 1.0));
            painter.rect_filled(filled, radius, fill);
        }
        None => {
            // A quarter-width block, crossing the bar every two seconds.
            const BLOCK: f32 = 0.25;
            let phase = (ui.input(|i| i.time) / 2.0).fract() as f32;
            let left = rect.left() + (rect.width() * (1.0 + BLOCK)) * phase - rect.width() * BLOCK;
            let block = egui::Rect::from_min_max(
                egui::pos2(left.max(rect.left()), rect.top()),
                egui::pos2((left + rect.width() * BLOCK).min(rect.right()), rect.bottom()),
            );
            painter.rect_filled(block, radius, fill);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    painter.rect_stroke(rect, radius, Stroke::new(1.0_f32, palette.border), egui::StrokeKind::Inside);

    if !label.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.0),
            palette.download_text,
        );
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Game;

    /// Draw the bar headless in a 400-wide slot. Returns the filled
    /// rectangles painted (the track first, then what fills it) as
    /// `(left edge relative to the bar, width)`, and the text drawn.
    fn painted(fraction: Option<f32>, time: f64) -> (Vec<(f32, f32)>, Vec<String>) {
        let palette = Palette::for_game(Game::ZeroHour);
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 200.0));
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(time), ..Default::default() };

        let output = ctx.run_ui(input, |ui| {
            ui.allocate_ui(Vec2::new(400.0, 22.0), |ui| {
                ui.set_width(400.0);
                stage_progress_bar(ui, &palette, fraction, "1/3  Unpacking... 25%", 22.0);
            });
        });

        fn walk(shape: &egui::Shape, rects: &mut Vec<egui::Rect>, texts: &mut Vec<String>) {
            match shape {
                egui::Shape::Rect(rect) if rect.fill != Color32::TRANSPARENT => rects.push(rect.rect),
                egui::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, rects, texts)),
                _ => {}
            }
        }
        let (mut rects, mut texts) = (Vec::new(), Vec::new());
        output.shapes.iter().for_each(|clipped| walk(&clipped.shape, &mut rects, &mut texts));
        output.drop_without_applying_deltas();

        let origin = rects.first().map_or(0.0, |track| track.left());
        (rects.iter().map(|r| (r.left() - origin, r.width())).collect(), texts)
    }

    /// The size a list row comes out at, headless, in a given state.
    /// `plain` uses egui's own selectable label instead of ours.
    fn row_size(selected: bool, hovered: bool, plain: bool) -> Vec2 {
        let ctx = egui::Context::default();
        let palette = Palette::for_game(Game::ZeroHour);
        apply(&ctx, &palette);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let mut rect = egui::Rect::NOTHING;

        // Hover takes a frame to register: the pointer is placed on the row
        // once its position is known, and the row reacts on the next pass.
        for _ in 0..4 {
            let mut input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
            if hovered && rect.is_positive() {
                input.events.push(egui::Event::PointerMoved(rect.center()));
            }
            let output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let text = egui::RichText::new("Rise of the Reds").size(14.0);
                    rect = if plain {
                        ui.selectable_label(selected, text).rect
                    } else {
                        selectable_row(ui, selected, text).rect
                    };
                });
            });
            output.drop_without_applying_deltas();
        }
        rect.size()
    }

    #[test]
    fn a_list_row_keeps_its_size_when_hovered_or_selected() {
        let idle = row_size(false, false, false);
        assert_eq!(row_size(false, true, false), idle, "the row changed size under the pointer");
        assert_eq!(row_size(true, false, false), idle, "the row changed size when selected");
        assert_eq!(row_size(true, true, false), idle, "the selected row changed size under the pointer");

        // What this replaces does change, which is the whole reason it exists.
        // Should egui stop doing that, this fails and the helper can go.
        assert_ne!(row_size(false, true, true), row_size(false, false, true));
    }

    #[test]
    fn a_known_fraction_fills_that_share_of_the_bar() {
        let (rects, texts) = painted(Some(0.25), 0.0);
        assert_eq!(rects.len(), 2, "expected a track and a fill: {rects:?}");
        let (track, fill) = (rects[0], rects[1]);
        assert!((track.1 - 400.0).abs() < 0.5, "the track is {} wide", track.1);
        assert!(fill.0.abs() < 0.5, "the fill starts at {}", fill.0);
        assert!((fill.1 - 100.0).abs() < 0.5, "the fill is {} wide", fill.1);
        assert_eq!(texts, ["1/3  Unpacking... 25%"]);
    }

    #[test]
    fn an_unknown_fraction_shows_a_block_that_moves_across() {
        let block_at = |time: f64| {
            let (rects, _) = painted(None, time);
            assert_eq!(rects.len(), 2, "expected a track and a block: {rects:?}");
            rects[1]
        };

        // Mid-sweep the block is a quarter of the bar, wholly inside it.
        let (early_left, early_width) = block_at(0.8);
        let (later_left, later_width) = block_at(1.2);
        assert!((early_width - 100.0).abs() < 0.5, "the block is {early_width} wide");
        assert!((later_width - 100.0).abs() < 0.5, "the block is {later_width} wide");
        assert!(early_left >= 0.0 && later_left + later_width <= 400.5);

        // And it has moved to the right in between.
        assert!(later_left > early_left + 50.0, "block at {early_left}, then {later_left}");
    }
}
