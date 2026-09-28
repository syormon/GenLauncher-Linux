//! Lazily decoded textures for mod artwork and window backgrounds.
//!
//! The WPF build wrote a second greyscale copy of every logo to disk; here the
//! desaturated variant is produced in memory and cached alongside the colour one.

use egui::{ColorImage, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct ImageCache {
    entries: HashMap<(PathBuf, bool), Option<TextureHandle>>,
}

impl ImageCache {
    /// Texture for `path`, decoding it on first use. `grayscale` gives the
    /// dimmed variant used for unselected rows.
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        path: &Path,
        grayscale: bool,
    ) -> Option<TextureHandle> {
        let key = (path.to_path_buf(), grayscale);
        if let Some(cached) = self.entries.get(&key) {
            return cached.clone();
        }

        let texture = decode(ctx, path, grayscale);
        self.entries.insert(key, texture.clone());
        texture
    }

    /// Forget a path so the next frame re-reads it from disk.
    pub fn invalidate(&mut self, path: &Path) {
        self.entries.remove(&(path.to_path_buf(), true));
        self.entries.remove(&(path.to_path_buf(), false));
    }
}

fn decode(ctx: &egui::Context, path: &Path, grayscale: bool) -> Option<TextureHandle> {
    if !path.is_file() {
        return None;
    }

    // Mod logos are downloaded without an extension, so sniff the format.
    let image = match image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode() {
        Ok(image) => image,
        Err(e) => {
            log::debug!("cannot decode {}: {e}", path.display());
            return None;
        }
    };

    let name = format!("{}{}", path.display(), if grayscale { "#bw" } else { "" });
    Some(ctx.load_texture(name, to_color_image(&image, grayscale), TextureOptions::LINEAR))
}

fn to_color_image(image: &image::DynamicImage, grayscale: bool) -> ColorImage {
    let rgba = image.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let mut pixels = rgba.into_raw();

    if grayscale {
        // The same flat average the C# generator used.
        for px in pixels.as_chunks_mut::<4>().0 {
            let avg = ((px[0] as u32 + px[1] as u32 + px[2] as u32) / 3) as u8;
            px[0] = avg;
            px[1] = avg;
            px[2] = avg;
        }
    }

    ColorImage::from_rgba_unmultiplied(size, &pixels)
}
