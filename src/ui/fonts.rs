//! The window's fonts: Windows' own Segoe UI, its semibold cut, and its
//! icon font, each loaded from the Windows fonts folder when it's there.
//!
//! egui's bundled fonts stay underneath as the fallback, so a missing or
//! damaged Windows font never stops the window from opening.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, Id};

/// The semibold family: headings and strong text.
pub const STRONG: &str = "strong";
/// The icon family: the glyphs in [`glyph`].
pub const ICONS: &str = "icons";

/// Segoe UI, the body text.
const SEGOE_UI: &str = "segoeui";
/// Segoe UI Semibold.
const SEGOE_SEMIBOLD: &str = "seguisb";
/// The icon fonts, best first: Segoe Fluent Icons (Windows 11), then Segoe
/// MDL2 Assets (Windows 10).
const ICON_FONTS: [&str; 2] = ["SegoeIcons", "segmdl2"];

/// The icons' code points in Segoe's icon fonts. They're in Unicode's
/// private-use area, so no other font draws them.
pub mod glyph {
    pub const COPY: char = '\u{E8C8}';
    pub const PLAY: char = '\u{E768}';
    pub const DOWNLOAD: char = '\u{E896}';
    pub const DELETE: char = '\u{E74D}';
    /// A folder, in both icon fonts.
    pub const FOLDER: char = '\u{E8B7}';
}

/// The fonts to install, and whether an icon font was found.
pub struct Fonts {
    pub defs: FontDefinitions,
    /// False when no icon font loaded: buttons then show words instead.
    pub has_icons: bool,
}

/// The font set: egui's bundled fonts, plus whatever of Segoe UI, its
/// semibold and an icon font `fonts_dir` holds. Never fails; a file that's
/// missing or doesn't parse is skipped.
pub fn definitions(fonts_dir: &Path) -> Fonts {
    let mut defs = FontDefinitions::default();

    // egui's own fonts stay at the end of every family, for characters
    // Segoe lacks.
    let mut proportional = defs.families[&FontFamily::Proportional].clone();
    if add_font(&mut defs, fonts_dir, SEGOE_UI) {
        proportional.insert(0, SEGOE_UI.to_string());
    }
    defs.families
        .insert(FontFamily::Proportional, proportional.clone());

    let mut strong = proportional.clone();
    if add_font(&mut defs, fonts_dir, SEGOE_SEMIBOLD) {
        strong.insert(0, SEGOE_SEMIBOLD.to_string());
    }
    defs.families
        .insert(FontFamily::Name(STRONG.into()), strong);

    // The first icon font that loads; any later one isn't read.
    let icons = ICON_FONTS
        .into_iter()
        .find(|name| add_font(&mut defs, fonts_dir, name));
    let mut icon_family = proportional;
    if let Some(name) = icons {
        icon_family.insert(0, name.to_string());
    }
    // Without an icon font the family still exists, because egui panics on
    // a family it doesn't know. Buttons then show words instead of icons.
    defs.families
        .insert(FontFamily::Name(ICONS.into()), icon_family);

    Fonts {
        defs,
        has_icons: icons.is_some(),
    }
}

/// Loads `<name>.ttf` from `fonts_dir` into `defs`, under the key `name`.
/// False, leaving `defs` as it was, if the file is missing or isn't a font.
fn add_font(defs: &mut FontDefinitions, fonts_dir: &Path, name: &str) -> bool {
    let path = fonts_dir.join(format!("{name}.ttf"));
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::debug!("the font {} wasn't loaded ({e})", path.display());
            return false;
        }
    };
    // egui stops the program on a font it can't parse, so the file is
    // checked here first, with the font library egui itself uses.
    if let Err(e) = skrifa::FontRef::new(&bytes) {
        tracing::warn!(
            "the font {} is damaged, so it isn't used ({e})",
            path.display()
        );
        return false;
    }
    defs.font_data
        .insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
    true
}

/// `%WINDIR%\Fonts`, where Windows keeps its fonts.
pub fn windows_fonts_dir() -> PathBuf {
    let windows = std::env::var_os("WINDIR").unwrap_or_else(|| r"C:\Windows".into());
    PathBuf::from(windows).join("Fonts")
}

/// Where [`install`] records `has_icons` in the context's memory.
fn has_icons_id() -> Id {
    Id::unique("slc_local_tts_has_icons")
}

/// Installs `fonts` in `ctx`. They take effect from the next frame.
pub fn install(ctx: &egui::Context, fonts: Fonts) {
    ctx.set_fonts(fonts.defs);
    // Temporary data lives in the context's memory until the window closes;
    // it's never saved anywhere.
    ctx.data_mut(|data| data.insert_temp(has_icons_id(), fonts.has_icons));
}

/// Whether [`install`] found an icon font. False before it runs.
pub fn has_icons(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp(has_icons_id()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp folder holding copies of these files from Windows' fonts
    /// folder, or `None` (with a note) when this PC lacks one of them: not
    /// every Windows edition ships every font, and CI's may not. The test
    /// then has nothing to check and returns early.
    fn dir_with(files: &[&str]) -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            let source = windows_fonts_dir().join(file);
            if !source.is_file() {
                eprintln!("skipped: this PC has no {}", source.display());
                return None;
            }
            std::fs::copy(source, dir.path().join(file)).unwrap();
        }
        Some(dir)
    }

    fn family(fonts: &Fonts, name: FontFamily) -> &Vec<String> {
        &fonts.defs.families[&name]
    }

    fn named(name: &str) -> FontFamily {
        FontFamily::Name(name.into())
    }

    #[test]
    fn present_fonts_are_used() {
        let Some(dir) = dir_with(&["segoeui.ttf", "seguisb.ttf"]) else {
            return;
        };
        let fonts = definitions(dir.path());
        assert!(fonts.defs.font_data.contains_key("segoeui"));
        assert_eq!(family(&fonts, FontFamily::Proportional)[0], "segoeui");
        assert_eq!(family(&fonts, named(STRONG))[0], "seguisb");
        // egui's own fonts stay behind them, for characters Segoe lacks.
        let defaults = &FontDefinitions::default().families[&FontFamily::Proportional];
        assert!(family(&fonts, FontFamily::Proportional).ends_with(defaults));
        assert!(!fonts.has_icons);
    }

    #[test]
    fn icons_prefer_segoe_icons_over_mdl2() {
        if let Some(dir) = dir_with(&["SegoeIcons.ttf", "segmdl2.ttf"]) {
            let fonts = definitions(dir.path());
            assert!(fonts.has_icons);
            assert_eq!(family(&fonts, named(ICONS))[0], "SegoeIcons");
            assert!(!fonts.defs.font_data.contains_key("segmdl2"));
        }

        if let Some(dir) = dir_with(&["segmdl2.ttf"]) {
            let fonts = definitions(dir.path());
            assert!(fonts.has_icons);
            assert_eq!(family(&fonts, named(ICONS))[0], "segmdl2");
        }
    }

    #[test]
    fn missing_fonts_fall_back_to_bundled() {
        let dir = tempfile::tempdir().unwrap();
        let fonts = definitions(dir.path());
        let defaults = FontDefinitions::default();
        assert_eq!(
            family(&fonts, FontFamily::Proportional),
            &defaults.families[&FontFamily::Proportional]
        );
        // egui panics on a family it doesn't know, so both always exist.
        assert!(fonts.defs.families.contains_key(&named(STRONG)));
        assert!(fonts.defs.families.contains_key(&named(ICONS)));
        assert!(!fonts.has_icons);

        let ctx = egui::Context::default();
        install(&ctx, fonts);
        assert!(!has_icons(&ctx));
    }

    #[test]
    fn unreadable_font_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("segoeui.ttf"), b"not a font").unwrap();
        let fonts = definitions(dir.path());
        assert!(!fonts.defs.font_data.contains_key("segoeui"));
        assert_eq!(
            family(&fonts, FontFamily::Proportional),
            &FontDefinitions::default().families[&FontFamily::Proportional]
        );
    }

    #[test]
    fn install_records_icons() {
        // Any icon font will do; Segoe MDL2 Assets is on every Windows 10
        // and 11.
        let Some(dir) = dir_with(&["segmdl2.ttf"]) else {
            return;
        };
        let ctx = egui::Context::default();
        assert!(!has_icons(&ctx));
        install(&ctx, definitions(dir.path()));
        assert!(has_icons(&ctx));
    }
}
