//! The shell's asset source: gitten's own icons in front of the component
//! library's, and every blob a viewer has loaded behind both.
//!
//! The design's chrome glyphs are stroke SVGs with a 1.6px weight and a
//! `currentColor` stroke, so they tint through `text_color` exactly as the
//! library's built-ins do. They live under the `gitten/` prefix and are
//! embedded here; every other path falls through to
//! [`gpui_component_assets::Assets`], so the widgets that ship their own
//! icons keep working and a path nobody registered still resolves to the
//! library's own answer.
//!
//! `gitten/blob/<key>` is the third arm and the interesting one: it is how an
//! in-memory blob reaches a decoder **without a copy of the file anywhere**. A
//! picture on screen is fetched once per content identity through this door,
//! decoded on the renderer's own executor, and cached by it — so the frame
//! that draws it does no work beyond finding the texture it already made. The
//! bytes come from [`crate::views::blob::Store`], which the pane filled from
//! the repository.
//!
//! This is the seam for chrome art, not a second registry: adding an icon is
//! a file under `gui/assets/icons/` and one arm below. A client that wants
//! different art supplies its own [`gpui::AssetSource`] at the same place the
//! window is built.

use crate::views::blob::{self as blobview, Store};
use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

/// The shell's asset source. Zero-sized where it can be: every icon is
/// embedded at compile time, so a frame spends no syscall on art — and a blob
/// costs one map lookup, because the bytes were read before the frame began.
pub struct Assets {
    blobs: Store,
}

impl Assets {
    /// The source over the process's blob store. One store for the process and
    /// not one per window: the asset source is the only door a blob has into a
    /// decoder, and GPUI installs one of those per application.
    pub fn new(blobs: Store) -> Self {
        Self { blobs }
    }
}

impl Default for Assets {
    fn default() -> Self {
        Self::new(blobview::store())
    }
}

/// One embedded icon. The path is relative to this file, so the icon dir and
/// the arm below cannot drift.
macro_rules! icon {
    ($name:literal) => {
        Cow::Borrowed(&include_bytes!(concat!("../assets/icons/", $name, ".svg"))[..])
    };
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        // Blobs first: a key is a path and a path is a lookup. The bytes are
        // cloned once per load — the decoder takes an owned buffer — and the
        // load itself happens once per content identity, off the frame.
        if let Some(key) = path.strip_prefix(blobview::ASSET_PREFIX) {
            let bytes = self
                .blobs
                .lock()
                .expect("blob store")
                .get(key)
                .map(|blob| blob.bytes.to_vec());
            return Ok(bytes.map(Cow::Owned));
        }
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
            let bytes = Assets::default()
                .load(path)
                .expect("loads")
                .expect("present");
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
        let known = Assets::default()
            .load("icons/settings.svg")
            .expect("library icon");
        assert!(known.is_some(), "the library's settings icon vanished");
    }

    /// A stored blob is served from the store, by the key the pane draws with
    /// — and a key nobody stored is `None` rather than an error, because the
    /// renderer treats the two the same way: nothing to draw here.
    #[test]
    fn a_stored_blob_is_served_by_its_key() {
        use crate::views::blob::{asset_path, Store};
        use gitten_core::blob::{Blob, Store as Blobs};
        use std::sync::{Arc, Mutex};

        let store: Store = Arc::new(Mutex::new(Blobs::new(1 << 20)));
        assert!(store.lock().unwrap().insert_at(
            "blob-beef",
            Blob::new(None, b"\x89PNG\r\n\x1a\nbytes".to_vec().into())
        ));
        let assets = Assets::new(store);

        let served = assets
            .load(&asset_path("blob-beef"))
            .expect("a lookup")
            .expect("the bytes");
        assert_eq!(&served[..8], b"\x89PNG\r\n\x1a\n");
        assert!(
            assets
                .load(&asset_path("blob-nobody"))
                .expect("a lookup")
                .is_none(),
            "a key nothing stored is nothing to draw"
        );
    }
}
