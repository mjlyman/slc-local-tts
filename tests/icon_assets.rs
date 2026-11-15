//! `assets/slc-local-tts.ico`, the exe's icon, is generated from the same
//! drawing code as the tray icon. To regenerate it after changing that code:
//!
//! ```text
//! cargo test --test icon_assets -- --ignored
//! ```

use std::path::PathBuf;

use slc_local_tts::tray::icon::render;
use slc_local_tts::tray::model::Badge;

/// The sizes in the file: Explorer's views and the taskbar at every common
/// display scale.
const SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

fn ico_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("slc-local-tts.ico")
}

/// An `.ico` file holding one 32-bit BMP image per size.
///
/// The layout: a 6-byte header, a 16-byte directory entry per image, then
/// each image as a `BITMAPINFOHEADER` followed by its pixels, bottom row
/// first in blue-green-red-alpha order, and a 1-bit transparency mask that
/// Windows still expects after them (all zeros here, since alpha does that
/// job).
fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&size| bmp_entry(size)).collect();

    let mut out = Vec::new();
    out.extend_from_slice(&0_u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1_u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());

    let mut offset = 6 + 16 * sizes.len();
    for (&size, image) in sizes.iter().zip(&images) {
        // A size of 256 is written as 0: the field is one byte.
        let side = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[side, side, 0, 0]); // width, height, no palette, reserved
        out.extend_from_slice(&1_u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32_u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

fn bmp_entry(size: u32) -> Vec<u8> {
    let rgba = render(size, Badge::None);
    // Each mask row is padded to a whole number of 4-byte words.
    let mask_row = size.div_ceil(32) * 4;

    let mut out = Vec::new();
    out.extend_from_slice(&40_u32.to_le_bytes()); // header size
    out.extend_from_slice(&(size as i32).to_le_bytes());
    // The height counts the colour image and the mask together.
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes()); // planes
    out.extend_from_slice(&32_u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&0_u32.to_le_bytes()); // no compression
    out.extend_from_slice(&0_u32.to_le_bytes()); // image size (may be 0)
    out.extend_from_slice(&[0; 16]); // resolution and palette: unused

    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    out.resize(out.len() + (mask_row * size) as usize, 0);
    out
}

#[test]
#[ignore = "writes assets/slc-local-tts.ico; run it after changing the icon"]
fn regenerate_ico() {
    let path = ico_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, ico(&SIZES)).unwrap();
}

#[test]
fn ico_asset_is_current() {
    let committed = std::fs::read(ico_path()).expect(
        "assets/slc-local-tts.ico is missing; run: cargo test --test icon_assets -- --ignored",
    );
    assert!(
        committed == ico(&SIZES),
        "assets/slc-local-tts.ico doesn't match the icon code; run: cargo test --test icon_assets -- --ignored"
    );
}
