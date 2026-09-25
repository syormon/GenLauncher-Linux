//! Reads and writes the game's own `Options.ini`. Port of `GameOptionsHandler`.
//!
//! The file is a flat `Key = value` list; values keep their leading space
//! because the game is picky about the format it wrote itself.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::{self, Game};

const DEFAULT_OPTIONS: &str = include_str!("../../assets/options.ini");

/// Keys the options screen exposes as on/off toggles.
pub const TOGGLE_KEYS: &[(&str, &str)] = &[
    ("UseShadowVolumes", "3DShadows"),
    ("BuildingOcclusion", "DisplayUnits"),
    ("UseShadowDecals", "2DShadows"),
    ("ShowTrees", "ShowProps"),
    ("UseCloudMap", "CloudShadows"),
    ("ExtraAnimations", "ExtraAnimation"),
    ("UseLightMap", "ExtraGroundLighting"),
    ("ShowSoftWaterEdge", "SmoothWaterBorders"),
    ("HeatEffects", "HeatEffects"),
    ("UseAlternateMouse", "AlternateMouseSetup"),
];

pub const RESOLUTIONS: &[&str] = &[
    "1024×768", "1152×864", "1176×664", "1280×720", "1280×768", "1280×800", "1280×960",
    "1280×1024", "1360×768", "1366×768", "1440×900", "1600×900", "1600×1024", "1600×1200",
    "1680×1050", "1920×1080", "1920×1200", "1920×1440", "2560×1440", "3620×2036",
];

pub struct GameOptions {
    /// Ordered so saving produces a stable file.
    pub values: BTreeMap<String, String>,
    path: PathBuf,
}

impl GameOptions {
    pub fn load(game: Game) -> Result<Self> {
        let folder = config::user_data_dir(game);
        std::fs::create_dir_all(&folder)?;
        let path = folder.join("Options.ini");

        if !path.exists() {
            std::fs::write(&path, DEFAULT_OPTIONS)?;
        }

        let mut options = GameOptions { values: BTreeMap::new(), path };
        options.read()?;
        options.validate();
        Ok(options)
    }

    fn read(&mut self) -> Result<()> {
        let text = std::fs::read_to_string(&self.path)?;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let key = key.replace(' ', "");
            if value.is_empty() {
                continue;
            }
            self.values.entry(key).or_insert_with(|| value.to_owned());
        }
        Ok(())
    }

    /// Fill in the keys the options screen expects, so it never reads a blank.
    fn validate(&mut self) {
        let mut ensure = |k: &str, v: &str| {
            self.values.entry(k.to_owned()).or_insert_with(|| v.to_owned());
        };

        ensure("Resolution", " 1024 768");
        ensure("MaxParticleCount", " 2500");
        ensure("TextureReduction", " 1");
        ensure("UseShadowVolumes", " no");
        ensure("BuildingOcclusion", " no");
        ensure("UseShadowDecals", " no");
        ensure("ShowTrees", " no");
        ensure("UseCloudMap", " no");
        ensure("ExtraAnimations", " no");
        ensure("UseLightMap", " no");
        ensure("DynamicLOD", " yes");
        ensure("ShowSoftWaterEdge", " no");
        ensure("HeatEffects", " no");
        ensure("UseAlternateMouse", " no");
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body: String = self
            .values
            .iter()
            .map(|(k, v)| {
                let v = if v.starts_with(' ') { v.clone() } else { format!(" {v}") };
                format!("{k} ={v}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&self.path, body + "\n")?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> &str {
        self.values.get(key).map(String::as_str).unwrap_or("")
    }

    pub fn set(&mut self, key: &str, value: &str) {
        let value = if value.starts_with(' ') { value.to_owned() } else { format!(" {value}") };
        self.values.insert(key.to_owned(), value);
    }

    pub fn is_yes(&self, key: &str) -> bool {
        self.get(key).trim().eq_ignore_ascii_case("yes")
    }

    pub fn set_bool(&mut self, key: &str, on: bool) {
        self.set(key, if on { "yes" } else { "no" });
    }

    pub fn int(&self, key: &str, fallback: i32) -> i32 {
        self.get(key).trim().parse().unwrap_or(fallback)
    }

    /// Resolution in the `1920×1080` form the combo box uses.
    pub fn resolution(&self) -> String {
        self.get("Resolution").trim().replace(' ', "×")
    }

    pub fn set_resolution(&mut self, display: &str) {
        self.set("Resolution", &display.replace('×', " "));
    }

    /// The screen quality preset applied by "Set recommended settings".
    pub fn apply_defaults(&mut self, screen: (u32, u32)) -> Result<()> {
        self.set("Resolution", &format!("{} {}", screen.0, screen.1));
        self.set("HeatEffects", "no");
        self.set("UseCloudMap", "no");
        self.set("UseShadowVolumes", "no");
        self.set("MaxParticleCount", "2500");
        self.set("UseShadowDecals", "yes");
        self.set("DynamicLOD", "yes");
        self.save()
    }
}

/// The options screen shows texture quality inverted relative to the file.
pub fn invert_texture_reduction(value: i32) -> i32 {
    (value - 2).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_reduction_round_trips() {
        for v in 0..=2 {
            assert_eq!(invert_texture_reduction(invert_texture_reduction(v)), v);
        }
    }
}
