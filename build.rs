//! Embeds the application icon and version details into the Windows executable,
//! so Explorer, the taskbar and shortcuts show GenLauncher's icon rather than
//! the generic one. Other targets need nothing from this script.
//!
//! The `.ico` is generated here from `assets/icon.png` rather than committed,
//! so the PNG stays the single source of truth for every icon size.

use std::path::{Path, PathBuf};

use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::imageops::FilterType;
use image::ExtendedColorType;

const ICON_SOURCE: &str = "assets/icon.png";

/// The sizes Windows asks for across Explorer views, the taskbar and Alt+Tab
/// at the usual DPI scales.
const ICON_SIZES: &[u32] = &[16, 20, 24, 32, 40, 48, 64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed={ICON_SOURCE}");
    println!("cargo:rerun-if-changed=build.rs");

    // CARGO_CFG_TARGET_OS is the target being built, not the machine building it.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let ico = out_dir.join("genlauncher.ico");
    write_ico(Path::new(ICON_SOURCE), &ico);

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().expect("OUT_DIR is valid UTF-8"))
        .set("ProductName", "GenLauncher")
        .set("FileDescription", "GenLauncher")
        .set("OriginalFilename", "GenLauncher.exe")
        .set("InternalName", "GenLauncher");

    if let Err(e) = res.compile() {
        panic!("could not embed the Windows icon and version resource: {e}");
    }
}

/// Scale the source PNG to every size in `ICON_SIZES` and pack them into one
/// `.ico`. Each entry is PNG-compressed, which Windows has read since Vista.
fn write_ico(source: &Path, target: &Path) {
    let image = image::open(source)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", source.display()))
        .into_rgba8();

    let encoded: Vec<Vec<u8>> = ICON_SIZES
        .iter()
        .map(|&size| image::imageops::resize(&image, size, size, FilterType::Lanczos3).into_raw())
        .collect();

    let frames: Vec<IcoFrame> = ICON_SIZES
        .iter()
        .zip(&encoded)
        .map(|(&size, pixels)| {
            IcoFrame::as_png(pixels, size, size, ExtendedColorType::Rgba8)
                .unwrap_or_else(|e| panic!("cannot encode the {size}px icon: {e}"))
        })
        .collect();

    let file = std::fs::File::create(target)
        .unwrap_or_else(|e| panic!("cannot create {}: {e}", target.display()));
    IcoEncoder::new(std::io::BufWriter::new(file))
        .encode_images(&frames)
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", target.display()));
}
