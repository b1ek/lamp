use crate::canvas::{difference, Canvas, Color, Rect};
use crate::solar::SunTimes;
use crate::text::Text;

const DAYLIGHT_KELVIN: f64 = 6500.0;
const NIGHT_COLOR: Color = [0, 0, 0];

const NIGHT_TEXT_COLOR: Color = [0x11, 0x11, 0x11];
const DAY_TEXT_COLOR: Color = [0xff, 0xff, 0xff];

const BASE_FONT: f32 = 15.0;
const CLOCK_FONT: f32 = 64.0;
const CLOCK_LETTER_SPACING: f32 = CLOCK_FONT * 0.08;
const LINE_HEIGHT: f32 = 1.5;
const GAP: f32 = BASE_FONT;
const ICON_LABEL_GAP: f32 = BASE_FONT * 0.5;

const BATTERY_FONT: f32 = BASE_FONT * 2.0;
const BATTERY_PERCENTAGE_FONT: f32 = BATTERY_FONT * 0.7;
const BATTERY_ICON_ASPECT: f32 = 6.0 / 3.175;

#[derive(PartialEq, Clone, Copy)]
pub enum Page {
    Clock,
    ConfirmPowerOff,
    PoweringOff,
}

#[derive(PartialEq, Clone, Copy)]
pub enum Action {
    AskForPowerOff,
    Cancel,
    PowerOff,
}

#[derive(Clone, Copy)]
enum Icon {
    Power,
    Back,
}

pub struct Display {
    pub page: Page,
    pub time: String,
    pub dots: usize,
    pub battery: Option<u8>,
}

pub struct Button {
    pub rect: Rect,
    pub action: Action,
    icon: Icon,
    label: &'static str,
    trailing: Option<String>,
}

struct ButtonSpec {
    action: Action,
    icon: Icon,
    label: &'static str,
    trailing: Option<String>,
}

pub fn draw(
    canvas: &mut Canvas,
    display: &Display,
    sun: &SunTimes,
    milliseconds_into_day: f64,
    scale: f32,
    text: &mut Text,
) {
    let logical = (canvas.width as f32 / scale, canvas.height as f32 / scale);
    let background = background_at(sun, milliseconds_into_day);
    let ink = difference(background, text_color_at(sun, milliseconds_into_day));

    canvas.clear(background);

    let clock_size = CLOCK_FONT * scale;
    let clock_line_height = clock_size * LINE_HEIGHT;
    let clock_spacing = CLOCK_LETTER_SPACING * scale;
    let clock_width = text.run_width(&display.time, clock_size, clock_spacing);

    text.draw_run(
        canvas,
        &display.time,
        (canvas.width as f32 - clock_width) / 2.0,
        text.baseline(block_top(logical) * scale, clock_line_height, clock_size),
        clock_size,
        clock_spacing,
        ink,
    );

    for button in buttons(display, logical, scale, text) {
        draw_button(canvas, &button, scale, text, ink);
    }

    if let Some(percentage) = display.battery {
        draw_battery(canvas, percentage, scale, text, ink);
    }
}

pub fn action_at(display: &Display, logical: (f32, f32), point: (f32, f32), text: &mut Text) -> Option<Action> {
    buttons(display, logical, 1.0, text)
        .into_iter()
        .find(|button| button.rect.contains(point.0, point.1))
        .map(|button| button.action)
}

pub fn is_night(sun: &SunTimes, milliseconds_into_day: f64) -> bool {
    milliseconds_into_day < sun.civil_dawn || milliseconds_into_day >= sun.civil_dusk
}

pub fn background_at(sun: &SunTimes, milliseconds_into_day: f64) -> Color {
    if milliseconds_into_day < sun.civil_dawn {
        return NIGHT_COLOR;
    }

    if milliseconds_into_day < sun.sunrise {
        let amount = progress(sun.civil_dawn, sun.sunrise, milliseconds_into_day);
        return mix(NIGHT_COLOR, day_color(), amount);
    }

    if milliseconds_into_day < sun.sunset {
        return day_color();
    }

    if milliseconds_into_day < sun.civil_dusk {
        let amount = progress(sun.sunset, sun.civil_dusk, milliseconds_into_day);
        return mix(day_color(), NIGHT_COLOR, amount);
    }

    NIGHT_COLOR
}

fn text_color_at(sun: &SunTimes, milliseconds_into_day: f64) -> Color {
    if is_night(sun, milliseconds_into_day) {
        NIGHT_TEXT_COLOR
    } else {
        DAY_TEXT_COLOR
    }
}

fn block_top(logical: (f32, f32)) -> f32 {
    let clock_line_height = CLOCK_FONT * LINE_HEIGHT;
    let row_height = BASE_FONT * LINE_HEIGHT;

    (logical.1 - clock_line_height - row_height) / 2.0
}

fn day_color() -> Color {
    kelvin_to_rgb(DAYLIGHT_KELVIN)
}

fn progress(from: f64, to: f64, current: f64) -> f64 {
    (current - from) / (to - from)
}

fn mix(from: Color, to: Color, amount: f64) -> Color {
    [0, 1, 2].map(|index| {
        let start = from[index] as f64;
        let end = to[index] as f64;

        (start + (end - start) * amount).round() as u8
    })
}

fn kelvin_to_rgb(kelvin: f64) -> Color {
    let temperature = kelvin / 100.0;

    let red = if temperature <= 66.0 {
        255.0
    } else {
        329.698727446 * (temperature - 60.0).powf(-0.1332047592)
    };

    let green = if temperature <= 66.0 {
        99.4708025861 * temperature.ln() - 161.1195681661
    } else {
        288.1221695283 * (temperature - 60.0).powf(-0.0755148492)
    };

    let blue = if temperature >= 66.0 {
        255.0
    } else if temperature <= 19.0 {
        0.0
    } else {
        138.5177312231 * (temperature - 10.0).ln() - 305.0447927307
    };

    [red, green, blue].map(|value| value.clamp(0.0, 255.0).round() as u8)
}

fn buttons(display: &Display, logical: (f32, f32), scale: f32, text: &mut Text) -> Vec<Button> {
    let specs = page_buttons(display);
    let row_height = BASE_FONT * LINE_HEIGHT;
    let row_top = block_top(logical) + CLOCK_FONT * LINE_HEIGHT;

    let mut measured: Vec<(ButtonSpec, f32)> = specs
        .into_iter()
        .map(|spec| {
            let label_width = text.run_width(spec.label, BASE_FONT, 0.0);
            let trailing_width = spec
                .trailing
                .as_deref()
                .map(|run| text.run_width(run, BASE_FONT, 0.0))
                .unwrap_or(0.0);

            (spec, BASE_FONT + ICON_LABEL_GAP + label_width + trailing_width)
        })
        .collect();

    let total_width: f32 =
        measured.iter().map(|(_, width)| *width).sum::<f32>() + GAP * measured.len().saturating_sub(1) as f32;

    let mut pen = (logical.0 - total_width) / 2.0;

    measured
        .drain(..)
        .map(|(spec, width)| {
            let rect = Rect::new(pen * scale, row_top * scale, width * scale, row_height * scale);
            pen += width + GAP;

            Button {
                rect,
                action: spec.action,
                icon: spec.icon,
                label: spec.label,
                trailing: spec.trailing,
            }
        })
        .collect()
}

fn page_buttons(display: &Display) -> Vec<ButtonSpec> {
    match display.page {
        Page::Clock => vec![ButtonSpec {
            action: Action::AskForPowerOff,
            icon: Icon::Power,
            label: "Power off",
            trailing: None,
        }],
        Page::ConfirmPowerOff => vec![
            ButtonSpec {
                action: Action::Cancel,
                icon: Icon::Back,
                label: "Cancel",
                trailing: None,
            },
            ButtonSpec {
                action: Action::PowerOff,
                icon: Icon::Power,
                label: "Power off",
                trailing: None,
            },
        ],
        Page::PoweringOff => vec![ButtonSpec {
            action: Action::PowerOff,
            icon: Icon::Power,
            label: "Powering off",
            trailing: Some(".".repeat(display.dots) + &" ".repeat(4 - display.dots)),
        }],
    }
}

fn draw_button(canvas: &mut Canvas, button: &Button, scale: f32, text: &mut Text, ink: Color) {
    let icon_size = BASE_FONT * scale;
    let icon = Rect::new(
        button.rect.x,
        button.rect.y + (button.rect.height - icon_size) / 2.0,
        icon_size,
        icon_size,
    );

    match button.icon {
        Icon::Power => draw_power_icon(canvas, icon, ink),
        Icon::Back => draw_back_icon(canvas, icon, ink),
    }

    let baseline = text.baseline(button.rect.y, button.rect.height, BASE_FONT * scale);
    let label_end = text.draw_run(
        canvas,
        button.label,
        icon.right() + ICON_LABEL_GAP * scale,
        baseline,
        BASE_FONT * scale,
        0.0,
        ink,
    );

    if let Some(trailing) = &button.trailing {
        text.draw_run(canvas, trailing, label_end, baseline, BASE_FONT * scale, 0.0, ink);
    }
}

fn draw_battery(canvas: &mut Canvas, percentage: u8, scale: f32, text: &mut Text, ink: Color) {
    let battery_font = BATTERY_FONT * scale;
    let container = Rect::new(
        battery_font + 1.0,
        canvas.height as f32 - battery_font * 0.5 - battery_font,
        battery_font,
        battery_font,
    );
    let icon = Rect::new(
        container.x,
        container.y + (container.height - container.width / BATTERY_ICON_ASPECT) / 2.0,
        container.width,
        container.width / BATTERY_ICON_ASPECT,
    );

    let percentage = 100;

    draw_battery_icon(canvas, icon, percentage, ink);

    let percentage_font = BATTERY_PERCENTAGE_FONT * scale;
    let label = format!("{}%", percentage);
    let line_height = percentage_font * LINE_HEIGHT;

    text.draw_run(
        canvas,
        &label,
        container.right() + battery_font * 0.3,
        text.baseline(container.y + (container.height - line_height) / 2.0, line_height, percentage_font),
        percentage_font,
        0.0,
        ink,
    );
}

fn draw_battery_icon(canvas: &mut Canvas, icon: Rect, percentage: u8, ink: Color) {
    let unit = icon.height / 3.175;
    let body = Rect::new(icon.x, icon.y, 5.2 * unit, 3.175 * unit);
    let nub = Rect::new(body.right() + 2.0, icon.y + 0.79375 * unit, 13.0, 1.5875 * unit);
    let corner_radius = 0.79 * unit;
    let frame = 0.42 * unit;
    let inner = inset(body, frame);
    let inner_radius = (corner_radius - frame).max(0.0);
    let level = inset(body, frame * 0.6);
    let level_radius = corner_radius - frame * 0.6;
    let fill_right = inner.x + percentage as f32 / 100.0 * (body.right() - inner.x);

    canvas.fill_shaped(level, ink, |x, y| inside_round_rect(x, y, level, level_radius) && x <= fill_right);

    canvas.fill_shaped(
        Rect::new(body.x - unit, body.y - unit, body.width + 2.0 * unit, body.height + 2.0 * unit),
        ink,
        |x, y| inside_round_rect(x, y, body, corner_radius) && !inside_round_rect(x, y, inner, inner_radius),
    );

    canvas.fill_shaped(
        Rect::new(nub.x - unit, nub.y - unit, nub.width, nub.height + 2.0 * unit),
        ink,
        |x, y| inside_round_rect(x, y, nub, 0.35 * unit),
    );
}

fn draw_power_icon(canvas: &mut Canvas, icon: Rect, ink: Color) {
    let unit = icon.width / 24.0;
    let center = (icon.x + 12.0 * unit, icon.y + 12.0 * unit);
    let radius = 9.0 * unit;
    let thickness = 1.9 * unit;
    let gap_half_angle = 32.0_f32.to_radians();
    let top_angle = -std::f32::consts::FRAC_PI_2;

    let bounds = Rect::new(
        center.0 - radius - thickness,
        center.1 - radius - thickness,
        (radius + thickness) * 2.0,
        (radius + thickness) * 2.0,
    );

    canvas.fill_shaped(bounds, ink, |x, y| {
        let distance = ((x - center.0).powi(2) + (y - center.1).powi(2)).sqrt();
        let angle = (y - center.1).atan2(x - center.0);

        (distance - radius).abs() <= thickness / 2.0 && angular_distance(angle, top_angle) > gap_half_angle
    });

    let bar = Rect::new(icon.x + 11.0 * unit, icon.y + 2.0 * unit, 2.0 * unit, 11.0 * unit);

    canvas.fill_shaped(bar, ink, |x, y| bar.contains(x, y));
}

fn draw_back_icon(canvas: &mut Canvas, icon: Rect, ink: Color) {
    let unit = icon.width / 24.0;
    let thickness = 2.0 * unit;
    let shaft_end = (icon.x + 20.0 * unit, icon.y + 12.0 * unit);
    let tip = (icon.x + 4.0 * unit, icon.y + 12.0 * unit);
    let upper = (icon.x + 12.0 * unit, icon.y + 4.0 * unit);
    let lower = (icon.x + 12.0 * unit, icon.y + 20.0 * unit);

    canvas.fill_shaped(icon, ink, |x, y| {
        distance_to_segment(x, y, tip, shaft_end) <= thickness / 2.0
            || distance_to_segment(x, y, upper, tip) <= thickness / 2.0
            || distance_to_segment(x, y, tip, lower) <= thickness / 2.0
    });
}

fn angular_distance(first: f32, second: f32) -> f32 {
    let difference = (first - second).abs() % std::f32::consts::TAU;
    difference.min(std::f32::consts::TAU - difference)
}

fn distance_to_segment(x: f32, y: f32, start: (f32, f32), end: (f32, f32)) -> f32 {
    let segment = (end.0 - start.0, end.1 - start.1);
    let length_squared = segment.0 * segment.0 + segment.1 * segment.1;
    let projection = (((x - start.0) * segment.0 + (y - start.1) * segment.1) / length_squared).clamp(0.0, 1.0);
    let closest = (start.0 + projection * segment.0, start.1 + projection * segment.1);

    ((x - closest.0).powi(2) + (y - closest.1).powi(2)).sqrt()
}

fn inside_round_rect(x: f32, y: f32, rect: Rect, radius: f32) -> bool {
    if !rect.contains(x, y) {
        return false;
    }

    let horizontal = x.clamp(rect.x + radius, rect.right() - radius);
    let vertical = y.clamp(rect.y + radius, rect.bottom() - radius);
    let delta_x = x - horizontal;
    let delta_y = y - vertical;

    delta_x * delta_x + delta_y * delta_y <= radius * radius
}

fn inset(rect: Rect, amount: f32) -> Rect {
    Rect::new(
        rect.x + amount,
        rect.y + amount,
        (rect.width - amount * 2.0).max(0.0),
        (rect.height - amount * 2.0).max(0.0),
    )
}
