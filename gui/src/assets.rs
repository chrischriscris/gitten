//! The shell's asset source: gitten's own icons in front of the component
//! library's.
//!
//! The design's chrome glyphs are stroke SVGs with a 1.6px weight and a
//! `currentColor` stroke, so they tint through `text_color` exactly as the
//! library's built-ins do. They live under the `gitten/` prefix and are
//! embedded here; every other path falls through to
//! [`gpui_component_assets::Assets`], so the widgets that ship their own
//! icons keep working and a path nobody registered still resolves to the
//! library's own answer.
//!
//! This is the seam for chrome art, not a second registry: adding an icon is
//! a file under `gui/assets/icons/` and one arm below. A client that wants
//! different art supplies its own [`gpui::AssetSource`] at the same place the
//! window is built.

use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

/// The shell's asset source. Zero-sized: every icon is embedded at compile
/// time, so a frame spends no syscall on art.
pub struct Assets;

/// One embedded icon. The path is relative to this file, so the icon dir and
/// the arm below cannot drift.
macro_rules! icon {
    ($name:literal) => {
        Cow::Borrowed(&include_bytes!(concat!("../assets/icons/", $name, ".svg"))[..])
    };
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes = match path {
            "gitten/branch.svg" => icon!("branch"),
            "gitten/changes.svg" => icon!("changes"),
            "gitten/history.svg" => icon!("history"),
            "gitten/stash.svg" => icon!("stash"),
            "gitten/search.svg" => icon!("search"),
            "gitten/file.svg" => icon!("file"),
            "gitten/chevron.svg" => icon!("chevron"),
            "gitten/palette.svg" => icon!("palette"),
            _ => return gpui_component_assets::Assets.load(path),
        };
        Ok(Some(bytes))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}

#[cfg(test)]
mod tests {
    use super::Assets;
    use gpui::AssetSource;

    /// Every arm above resolves, and the bytes are the SVG the path names —
    /// a missing file is a compile error, but a wrong arm is not.
    #[test]
    fn gitten_icons_resolve_to_their_own_bytes() {
        for (path, needle) in [
            ("gitten/branch.svg", b"circle".as_slice()),
            ("gitten/changes.svg", b"rect".as_slice()),
            ("gitten/history.svg", b"path".as_slice()),
            ("gitten/stash.svg", b"path".as_slice()),
            ("gitten/search.svg", b"circle".as_slice()),
            ("gitten/file.svg", b"path".as_slice()),
            ("gitten/chevron.svg", b"path".as_slice()),
            ("gitten/palette.svg", b"circle".as_slice()),
        ] {
            let bytes = Assets.load(path).expect("loads").expect("present");
            assert!(
                bytes.windows(needle.len()).any(|w| w == needle),
                "{path} did not contain its mark"
            );
        }
    }

    /// An unknown path is not swallowed: the library's own loader answers,
    /// which is what keeps widget icons working through this source.
    #[test]
    fn unknown_paths_fall_through_to_the_component_assets() {
        let known = Assets.load("icons/settings.svg").expect("library icon");
        assert!(known.is_some(), "the library's settings icon vanished");
    }
}
