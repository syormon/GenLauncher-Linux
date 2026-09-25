//! Theme colours. The WPF version stored brushes; here we resolve the same hex
//! strings into `egui::Color32` and keep the palette in one struct.

use egui::Color32;
use serde::{Deserialize, Serialize};

use crate::config::Game;

/// Raw palette exactly as it appears in `Colors.yaml` and in mod manifests.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorsInfoString {
    #[serde(rename = "GenLauncherBorderColor")]
    pub border: String,
    #[serde(rename = "GenLauncherInactiveBorder")]
    pub inactive_border: String,
    #[serde(rename = "GenLauncherInactiveBorder2")]
    pub inactive_border2: String,
    #[serde(rename = "GenLauncherActiveColor")]
    pub active: String,
    #[serde(rename = "GenLauncherDarkFillColor")]
    pub dark_fill: String,
    #[serde(rename = "GenLauncherDarkBackGround")]
    pub dark_background: String,
    #[serde(rename = "GenLauncherLightBackGround")]
    pub light_background: String,
    #[serde(rename = "GenLauncherDefaultTextColor")]
    pub default_text: String,
    #[serde(rename = "GenLauncherDownloadTextColor")]
    pub download_text: String,
    #[serde(rename = "GenLauncherListBoxSelectionColor2")]
    pub list_selection2: String,
    #[serde(rename = "GenLauncherListBoxSelectionColor1")]
    pub list_selection1: String,
    #[serde(rename = "GenLauncherButtonSelectionColor")]
    pub button_selection: String,
    #[serde(rename = "GenLauncherBackgroundImageLink")]
    pub background_image_link: String,
}

/// Resolved palette used by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub border: Color32,
    pub inactive_border: Color32,
    pub inactive_border2: Color32,
    pub active: Color32,
    pub dark_fill: Color32,
    pub dark_background: Color32,
    pub light_background: Color32,
    pub default_text: Color32,
    pub download_text: Color32,
    pub list_selection1: Color32,
    pub list_selection2: Color32,
    pub button_selection: Color32,
}

impl Palette {
    pub fn zero_hour() -> Self {
        Self::from_hex_set(
            "#00e3ff", "DarkGray", "#7a7db0", "#baff0c", "#232977", "#090502", "#B3000000",
            "White", "#090502", "#F21d2057", "#F21d2057", "#2534ff",
        )
    }

    pub fn generals() -> Self {
        Self::from_hex_set(
            "#ffbb00", "DarkGray", "#ffbb00", "#ffbb00", "#e24c17", "#090502", "#B3000000",
            "White", "#090502", "#5a210d", "#8a2e0d", "#e24c17",
        )
    }

    pub fn for_game(game: Game) -> Self {
        match game {
            Game::ZeroHour => Self::zero_hour(),
            Game::Generals => Self::generals(),
        }
    }

    /// Argument order matches the 12-argument `ColorsInfo` constructor in C#.
    #[allow(clippy::too_many_arguments)]
    fn from_hex_set(
        border: &str,
        inactive_border: &str,
        inactive_border2: &str,
        active: &str,
        dark_fill: &str,
        dark_background: &str,
        light_background: &str,
        text: &str,
        text2: &str,
        s_color2: &str,
        s_color1: &str,
        b_color: &str,
    ) -> Self {
        Self {
            border: parse_color(border).unwrap_or(Color32::GRAY),
            inactive_border: parse_color(inactive_border).unwrap_or(Color32::DARK_GRAY),
            inactive_border2: parse_color(inactive_border2).unwrap_or(Color32::DARK_GRAY),
            active: parse_color(active).unwrap_or(Color32::WHITE),
            dark_fill: parse_color(dark_fill).unwrap_or(Color32::BLACK),
            dark_background: parse_color(dark_background).unwrap_or(Color32::BLACK),
            light_background: parse_color(light_background).unwrap_or(Color32::BLACK),
            default_text: parse_color(text).unwrap_or(Color32::WHITE),
            download_text: parse_color(text2).unwrap_or(Color32::WHITE),
            // The C# constructor swaps these two; keep the same mapping.
            list_selection2: parse_color(s_color2).unwrap_or(Color32::DARK_BLUE),
            list_selection1: parse_color(s_color1).unwrap_or(Color32::DARK_BLUE),
            button_selection: parse_color(b_color).unwrap_or(Color32::BLUE),
        }
    }

    /// Overlay whichever entries a mod-supplied palette actually defines.
    pub fn with_overrides(mut self, raw: &ColorsInfoString) -> Self {
        let set = |field: &mut Color32, hex: &str| {
            if let Some(c) = parse_color(hex) {
                *field = c;
            }
        };
        set(&mut self.border, &raw.border);
        set(&mut self.inactive_border, &raw.inactive_border);
        set(&mut self.inactive_border2, &raw.inactive_border2);
        set(&mut self.active, &raw.active);
        set(&mut self.dark_fill, &raw.dark_fill);
        set(&mut self.dark_background, &raw.dark_background);
        set(&mut self.light_background, &raw.light_background);
        set(&mut self.default_text, &raw.default_text);
        set(&mut self.download_text, &raw.download_text);
        set(&mut self.list_selection2, &raw.list_selection1);
        set(&mut self.list_selection1, &raw.list_selection2);
        set(&mut self.button_selection, &raw.button_selection);
        self
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::zero_hour()
    }
}

/// Accepts `#RGB`, `#RRGGBB`, `#AARRGGBB` and the handful of WPF colour names
/// the stock palettes use.
pub fn parse_color(spec: &str) -> Option<Color32> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }

    if let Some(hex) = spec.strip_prefix('#') {
        let d = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        return match hex.len() {
            3 => {
                let v: Vec<u8> = hex
                    .chars()
                    .filter_map(|c| c.to_digit(16).map(|d| (d * 17) as u8))
                    .collect();
                (v.len() == 3).then(|| Color32::from_rgb(v[0], v[1], v[2]))
            }
            6 => Some(Color32::from_rgb(d(0)?, d(2)?, d(4)?)),
            // WPF uses #AARRGGBB.
            8 => Some(Color32::from_rgba_unmultiplied(d(2)?, d(4)?, d(6)?, d(0)?)),
            _ => None,
        };
    }

    match spec.to_ascii_lowercase().as_str() {
        "white" => Some(Color32::WHITE),
        "black" => Some(Color32::BLACK),
        "gray" | "grey" => Some(Color32::from_rgb(128, 128, 128)),
        "darkgray" | "darkgrey" => Some(Color32::from_rgb(169, 169, 169)),
        "lightgray" | "lightgrey" => Some(Color32::from_rgb(211, 211, 211)),
        "red" => Some(Color32::RED),
        "green" => Some(Color32::GREEN),
        "blue" => Some(Color32::BLUE),
        "yellow" => Some(Color32::YELLOW),
        "orange" => Some(Color32::from_rgb(255, 165, 0)),
        "transparent" => Some(Color32::TRANSPARENT),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wpf_color_forms() {
        assert_eq!(parse_color("#00e3ff"), Some(Color32::from_rgb(0, 0xe3, 0xff)));
        assert_eq!(
            parse_color("#B3000000"),
            Some(Color32::from_rgba_unmultiplied(0, 0, 0, 0xB3))
        );
        assert_eq!(parse_color("DarkGray"), Some(Color32::from_rgb(169, 169, 169)));
        assert_eq!(parse_color(""), None);
    }
}
