//! Seeing a blob: the pane a file with no lines to show gets.
//!
//! A PNG, a PDF or a tarball arrives from acquisition as bytes and nothing
//! else, and the diff pane has no rows for it — which is why this exists: the
//! centre shows the file instead of an empty list, and says what it cannot
//! show rather than nothing at all.
//!
//! Three things about how, and all three are performance decisions:
//!
//! **Nothing decodes here, and nothing decodes per frame.** The bytes are
//! handed to the renderer as an *asset path* — `gitten/blob/<key>`, served by
//! this crate's [`AssetSource`](crate::assets::Assets) out of a store keyed by
//! content — and GPUI decodes them on its own executor with its own codecs and
//! keeps the result in its own cache. A frame of an image is therefore one
//! `img()` element built from a key that is already a hit, one texture the
//! platform draws; no pixel enters this process's render path at all.
//!
//! **A raster is paid for once.** A PDF page comes out of [`gitten_app::blobs`]
//! on a job thread, at a fixed width, and is stored as PNG bytes under the
//! document's own identity — so dragging the window re-samples a texture rather
//! than re-rendering a document, and a second look at the same page is a cache
//! hit.
//!
//! **The pairing of kind and drawing is a registry, not a match.** [`Presenter`]
//! claims kinds and returns an element; the built-ins go through the same
//! [`Presenters::register`] call an extension would use, which is the only way
//! the seam stays honest. The fallback claims everything, so a kind nobody has
//! a presentation for is *described* rather than blank.

use crate::chrome;
use gitten_core::blob::{self, Kind, SideOf};
use gitten_core::host::Host;
use gitten_core::theme::Surface;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use std::sync::{Arc, Mutex, OnceLock};

/// The blobs a viewer has already paid for, shared with the asset source.
///
/// One store for the process: the asset source reads it on a decoder's thread
/// and the pane writes it on the frame's, so it is behind a lock — held only
/// for a lookup or an insert, which is why a `Mutex` is the right primitive
/// and a channel would not be.
pub type Store = Arc<Mutex<blob::Store>>;

/// How many bytes of blobs the process keeps, across every pane and window.
///
/// A viewer's working set is the file under the cursor and the one it came
/// from; 64 MB is a dozen screenshots or two photographs, which is what
/// flipping between two sides of a picture costs. It is *also* the read limit
/// — a blob over this is refused before any body is loaded — so the number is
/// the answer to "how big a file may I look at", asked once.
pub const BUDGET: usize = 64 << 20;

/// The process's store, which is the process's because the *asset source* is.
///
/// GPUI installs one `AssetSource` per application, and that source is how a
/// blob reaches a decoder — so a store per window would be a window whose
/// pictures are invisible to the renderer that has to cache them. A `OnceLock`
/// makes that structural rather than a convention: there is exactly one, and
/// every pane draws from it.
pub fn store() -> Store {
    static STORE: OnceLock<Store> = OnceLock::new();
    STORE
        .get_or_init(|| Arc::new(Mutex::new(blob::Store::new(BUDGET))))
        .clone()
}

/// The prefix every stored blob's path starts with, in the renderer's asset
/// namespace. Chrome art lives under `gitten/` and a blob under this, so one
/// `match` in one `AssetSource` serves both and neither can shadow the other.
pub const ASSET_PREFIX: &str = "gitten/blob/";

/// Where a frontend asks for a stored blob.
///
/// **No scheme, on purpose.** Every asset system treats a string with a scheme
/// as a URL and fetches it, so a path shaped like one would turn a local
/// picture into a network request. The key inside it carries no colon for the
/// same reason; see `blob::Blob::key`.
pub fn asset_path(key: &str) -> String {
    format!("{ASSET_PREFIX}{key}")
}

/// What a presentation is handed: the bytes, and the key they are stored and
/// decoded under.
///
/// Small on purpose. A presenter is *chosen* by kind and by claim, so the kind
/// is the registry's business rather than the argument list's, and what it does
/// with the bytes is drawing — anything else it wants it can hold itself.
pub struct Shown<'a> {
    pub bytes: &'a [u8],
    /// The store key of what to draw — the blob's own, or a rasterized page's.
    pub key: &'a str,
}

/// What draws one kind of blob.
///
/// The whole seam: a name, a claim, and an element. An implementation never
/// touches the store or the repository — it is handed the bytes and a key, and
/// what it does with them is drawing.
pub trait Presenter {
    fn name(&self) -> &'static str;

    /// Whether this presenter draws these bytes. The last registered claim
    /// wins, and the fallback claims everything — the rule `Rows` uses, for the
    /// same reason: a registry with no fallback is a blank pane.
    fn claims(&self, kind: Kind) -> bool;

    fn render(&self, what: &Shown, host: &Host, store: &Store) -> AnyElement;
}

/// The presentations this pane can draw with, in claim order.
pub struct Presenters {
    list: Vec<Box<dyn Presenter>>,
}

impl Presenters {
    /// The built-ins, through the same registration an extension uses.
    ///
    /// **The fallback goes first, and that is not cosmetic.** The last claim
    /// wins — the rule `Rows` uses, so a presentation registered later can
    /// correct an earlier one — which means the presentation that claims
    /// *everything* has to be the earliest registered, or it draws every kind
    /// and the specific ones never run.
    pub fn builtin() -> Self {
        let mut this = Self { list: Vec::new() };
        this.register(Box::new(Hex));
        this.register(Box::new(Image));
        this
    }

    /// Adds a presentation. Registering a name twice replaces it, so a
    /// built-in can be corrected rather than only added to.
    pub fn register(&mut self, presenter: Box<dyn Presenter>) -> Option<Box<dyn Presenter>> {
        match self.list.iter().position(|p| p.name() == presenter.name()) {
            Some(at) => Some(std::mem::replace(&mut self.list[at], presenter)),
            None => {
                self.list.push(presenter);
                None
            }
        }
    }

    /// The presentation that draws these bytes: the last claim wins.
    ///
    /// There is always one — [`Presenters::builtin`] registers a fallback that
    /// claims everything, and `register` can only add or replace — so the last
    /// entry is the floor rather than a panic waiting for a mistake.
    pub fn claim(&self, kind: Kind) -> &dyn Presenter {
        self.list
            .iter()
            .rev()
            .find(|p| p.claims(kind))
            .map(|p| &**p)
            .unwrap_or_else(|| &**self.list.last().expect("the fallback is always registered"))
    }

    /// Every registered name, in registration order — what a picker would
    /// list, and what a test reads to prove a name replaced rather than
    /// doubled. Part of the registry's own surface, like `Panes::names`.
    #[allow(dead_code)]
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.list.iter().map(|p| p.name())
    }
}

/// The built-in: anything the renderer can decode, drawn whole in the pane.
///
/// `ObjectFit::Contain` is the entire scaling policy — the platform's sampler,
/// on the graphics thread, at whatever size the pane happens to be. Nothing
/// here computes a scale factor or resamples anything, and a window dragged
/// from 800 to 1600 pixels costs one layout pass.
pub struct Image;

impl Presenter for Image {
    fn name(&self) -> &'static str {
        "image"
    }

    fn claims(&self, kind: Kind) -> bool {
        matches!(
            kind,
            Kind::Png
                | Kind::Jpeg
                | Kind::Gif
                | Kind::Webp
                | Kind::Bmp
                | Kind::Ico
                | Kind::Tiff
                | Kind::Svg
                | Kind::Pdf
        )
    }

    fn render(&self, what: &Shown, host: &Host, _store: &Store) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(12.0))
            .bg(rgb(host.theme.diff.context_bg))
            .child(
                img(asset_path(what.key))
                    .object_fit(ObjectFit::Contain)
                    .w_full()
                    .h_full(),
            )
            .into_any_element()
    }
}

/// The fallback: the head of the blob, in hex and in text.
///
/// A viewer's answer to "I cannot draw this" has to be better than an empty
/// pane, and hex is: sixteen bytes a row tells a reader immediately whether
/// they are looking at a font, a zip or a database, which is the question a
/// binary file in a diff actually raises. Bounded by [`HEX_ROWS`], so a
/// thousand-megabyte file costs this row list the same as a ten-byte one.
pub struct Hex;

/// How many rows of hex the fallback draws. Sixty-four is a few screens: enough
/// to recognise a format, little enough to read.
const HEX_ROWS: usize = 64;

impl Presenter for Hex {
    fn name(&self) -> &'static str {
        "hex"
    }

    // Every kind, and therefore the last word on any kind nobody else claims.

    fn claims(&self, _: Kind) -> bool {
        true
    }

    fn render(&self, what: &Shown, host: &Host, _store: &Store) -> AnyElement {
        let ch = host.font.char_width();
        let head = &what.bytes[..what.bytes.len().min(HEX_ROWS * 16)];
        let dim = host.theme.dim_on(Surface::Context);
        let rows: Vec<AnyElement> = head
            .chunks(16)
            .enumerate()
            .map(|(i, chunk)| {
                let mut hex = String::with_capacity(48);
                let mut text = String::with_capacity(16);
                for byte in chunk {
                    hex.push_str(&format!("{byte:02x} "));
                    text.push(match byte {
                        0x20..=0x7e => *byte as char,
                        _ => '.',
                    });
                }
                div()
                    .flex()
                    .flex_none()
                    .gap(px(14.0))
                    .text_size(px(11.0))
                    .child(
                        div()
                            .flex_none()
                            .w(px(7.0 * ch))
                            .text_color(rgb(dim))
                            .child(SharedString::from(format!("{:04x}", i * 16))),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(49.0 * ch))
                            .text_color(rgb(host.theme.chrome.fg))
                            .child(SharedString::from(hex)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(dim))
                            .child(SharedString::from(text)),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_y(px(2.0))
            .p(px(14.0))
            .font_family(host.font.family.clone())
            .overflow_hidden()
            .children(rows)
            .into_any_element()
    }
}

/// One blob pair on screen: the side the reader asked for, and what can be
/// drawn of it.
///
/// The pane holds the pair rather than a key, because a *refusal* is part of
/// the answer — a side too big to load, a document with no renderer, a file
/// that vanished — and a key cannot carry a sentence. The bytes it holds are
/// the store's own, by refcount: nothing is copied to draw one.
pub struct Blob {
    pair: Option<blob::Pair>,
    /// The rasterized first page of each PDF side, page key → PNG bytes: the
    /// asset the pane draws for that side, held by refcount beside the
    /// store's entry so a store that evicted one is a re-insert rather than a
    /// blank pane.
    pages: std::collections::HashMap<String, blob::Bytes>,
    /// Which side is on screen. The new one by default: what the file *is*.
    side: SideOf,
    /// A sentence for the reader — no renderer, a refusal, a size.
    note: Option<String>,
    store: Store,
    presenters: Presenters,
}

impl Blob {
    /// A pane over the process's store. One place to make a blob pane, so no
    /// caller can make one that draws from somewhere the renderer cannot see.
    pub fn new() -> Self {
        Self::over(store())
    }

    /// A pane over a store of its own — what a test wants, and what a second
    /// renderer in the same process would need.
    pub fn over(store: Store) -> Self {
        Self {
            pair: None,
            pages: std::collections::HashMap::new(),
            side: SideOf::After,
            note: None,
            store,
            presenters: Presenters::builtin(),
        }
    }

    /// The registry, for a client that registers a presentation — and for the
    /// test that proves a second one fits without an edit to the first. A
    /// window does not read it; an extension host does.
    #[allow(dead_code)]
    pub fn presenters(&mut self) -> &mut Presenters {
        &mut self.presenters
    }

    /// Takes a loaded pair: keeps every blob it can in the store, remembers
    /// the pages a rasterizer drew, and opens on the side that is not history.
    ///
    /// A blob over the store's budget is *not* kept and is not an error — the
    /// presentation that needs bytes (the fallback) reads them from the pair
    /// this pane holds, and the one that needs a key (the image) is refused
    /// honestly by the renderer's own loader.
    pub fn show(&mut self, loaded: &gitten_app::blobs::Loaded) {
        let mut store = self.store.lock().expect("blob store");
        self.pages.clear();
        for (key, png) in &loaded.pages {
            // Under the *page's* key, which is what the pane draws: the store
            // is the renderer's table of contents, so an entry under a name
            // nothing asks for is bytes held for nobody.
            let bytes: blob::Bytes = Arc::from(png.as_slice());
            store.insert_at(key, blob::Blob::new(None, bytes.clone()));
            self.pages.insert(key.clone(), bytes);
        }
        for blob in loaded.pair.blobs() {
            // A side whose page is what gets drawn is not kept under its own
            // key too: the pair already holds those bytes for the fallback,
            // and a stored copy beside the page's is the two of them ping-
            // ponging the eviction when they sum past the budget.
            let paged = blob.kind == Kind::Pdf
                && self
                    .pages
                    .contains_key(&gitten_app::blobs::page_key(blob.key(), 0));
            if !paged {
                store.insert(blob.clone());
            }
        }
        drop(store);
        self.note = loaded.note.clone();
        self.side = SideOf::After;
        self.pair = Some(loaded.pair.clone());
    }

    /// Forgets the last answer — a cursor moved off a blob, or a repository
    /// that moved under it.
    pub fn clear(&mut self) {
        self.pair = None;
        self.pages.clear();
        self.note = None;
        self.side = SideOf::After;
    }

    /// Whether there is a side to draw: the shell's test for "the centre is a
    /// blob this frame".
    pub fn is_showing(&self) -> bool {
        self.pair.as_ref().is_some_and(|pair| !pair.is_empty())
    }

    /// Swaps between the two sides when both exist, and reports whether it
    /// moved — a key bound to this is a no-op on a file that was only added.
    pub fn flip(&mut self) -> bool {
        let Some(pair) = &self.pair else {
            return false;
        };
        let other = match self.side {
            SideOf::After => SideOf::Before,
            SideOf::Before => SideOf::After,
        };
        if side_of(pair, other).is_absent() {
            return false;
        }
        self.side = other;
        true
    }

    /// The key the shown side is drawn from: its own page when a rasterizer
    /// made one for *it* — a borrowed `&str`, because a frame is no place to
    /// be cloning strings or re-hashing bytes.
    fn key_for<'a>(&'a self, blob: &'a blob::Blob) -> &'a str {
        if blob.kind == Kind::Pdf {
            if let Some((key, _)) = self
                .pages
                .get_key_value(&gitten_app::blobs::page_key(blob.key(), 0))
            {
                return key;
            }
        }
        blob.key()
    }

    /// The strip over the body: what the file is, which side, and whichever
    /// sentence the load left.
    fn strip(&self, host: &Host, pair: &blob::Pair, cx: &mut Context<Self>) -> Div {
        let c = host.theme.chrome;
        let dim = host.theme.dim_on(Surface::Context);
        let side = side_of(pair, self.side);
        let word = match side {
            blob::Side::Held(blob) => format!(
                "{} · {}",
                blob.kind.name(),
                bytes_word(blob.bytes.len() as u64)
            ),
            blob::Side::TooBig { size, .. } => {
                format!("too big to show · {}", bytes_word(*size))
            }
            blob::Side::Absent => "nothing on this side".to_string(),
        };
        // One button per side that exists: the reader's way between what the
        // file is and what it was, and the mouse half of `blob.flip`.
        let buttons: Vec<AnyElement> = pair
            .sides()
            .map(|(at, s)| {
                let on = at == self.side && !s.is_absent();
                let exists = !s.is_absent();
                div()
                    .id(SharedString::from(format!("blob-side-{}", at.label())))
                    .px(px(9.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .text_color(rgb(match (on, exists) {
                        (true, _) => c.fg,
                        (false, true) => dim,
                        (false, false) => c.faint,
                    }))
                    .bg(rgb(match on {
                        true => c.selection_bg,
                        false => c.title_bg,
                    }))
                    // The mouse half of `blob.flip`, through the same method.
                    // A side that is not there is drawn dim and does nothing:
                    // the button names what exists, it does not create it.
                    .when(exists, |d| {
                        d.cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
                            if this.flip() {
                                cx.notify();
                            }
                        }))
                    })
                    .child(at.label())
                    .into_any_element()
            })
            .collect();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(14.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(rgb(c.border))
            .text_size(px(11.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(div().text_color(rgb(c.fg)).child(word)),
            )
            .child(div().flex().items_center().gap(px(4.0)).children(buttons))
            .children(
                self.note
                    .clone()
                    .map(|note| div().min_w_0().truncate().text_color(rgb(dim)).child(note)),
            )
    }
}

impl Render for Blob {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = crate::config::host(cx);
        let c = host.theme.chrome;
        let Some(pair) = self.pair.clone() else {
            return chrome::empty_line(&host, SharedString::from("")).into_any_element();
        };
        let side = side_of(&pair, self.side);
        let body: AnyElement = match side {
            blob::Side::Held(blob) => {
                let key = self.key_for(blob);
                // The store is the renderer's table of contents, and this is
                // the one thing on the render path that touches it: a lookup,
                // so a decode never misses a path the pane is about to draw.
                // An entry that is gone — evicted, or another window's — is
                // put back from the bytes the pair already holds.
                let missing = self.store.lock().expect("blob store").get(key).is_none();
                if missing {
                    // The bytes are already here either way — the blob from the
                    // pair, the page from the load — so restoring an evicted
                    // entry is a refcount bump and not a second read.
                    let held = self.pages.get(key).map_or_else(
                        || blob.clone(),
                        |bytes| blob::Blob::new(None, bytes.clone()),
                    );
                    self.store.lock().expect("blob store").insert_at(key, held);
                }
                let shown = Shown {
                    bytes: &blob.bytes,
                    key,
                };
                // The kind a presentation is asked for is the kind it can
                // *draw*, which is not always what the bytes are: a PDF whose
                // own page never arrived is bytes like any other, and the
                // strip above says why. Whoever cannot draw it describes it.
                let drawable = match (blob.kind, key == blob.key()) {
                    (Kind::Pdf, true) => Kind::Opaque,
                    _ => blob.kind,
                };
                self.presenters
                    .claim(drawable)
                    .render(&shown, &host, &self.store)
            }
            blob::Side::TooBig { size, .. } => note(
                &host,
                format!(
                    "{} — over the limit this viewer holds in memory, so nothing was read",
                    bytes_word(*size)
                ),
            ),
            blob::Side::Absent => note(&host, "nothing on this side".to_string()),
        };
        div()
            // Identity for the capture harness and for the test that proves
            // the shell actually put this pane in the centre — a picture
            // nobody can name is a picture nobody can assert on.
            .debug_selector(|| "blob-pane".to_string())
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(rgb(c.bg))
            .child(self.strip(&host, &pair, cx))
            .child(div().flex_grow(1.0).min_h_0().child(body))
            .into_any_element()
    }
}

/// The side of a pair by which end it is.
fn side_of(pair: &blob::Pair, at: SideOf) -> &blob::Side {
    match at {
        SideOf::After => &pair.new,
        SideOf::Before => &pair.old,
    }
}

/// A centered sentence where a picture would be — the pane's answer to a
/// refusal that has a reason.
fn note(host: &Host, text: String) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(24.0))
        .text_color(rgb(host.theme.dim_on(Surface::Context)))
        .child(SharedString::from(text))
        .into_any_element()
}

/// A byte count the way a status line says it: whole units, one decimal where
/// the number is small enough for one to mean something.
pub fn bytes_word(n: u64) -> String {
    const KB: f64 = 1024.0;
    let n = n as f64;
    let which = [(KB * KB * KB, "GB"), (KB * KB, "MB"), (KB, "KB")];
    for (scale, unit) in which {
        if n >= scale {
            return format!("{:.1} {unit}", n / scale);
        }
    }
    format!("{n:.0} bytes")
}

#[cfg(test)]
mod tests {
    // Named imports rather than `use super::*`: `gpui`'s own `test` macro is in
    // scope through the parent's glob, and a `#[test]` that resolves to it
    // recurses to the compiler's limit instead of running anything.
    use super::{asset_path, bytes_word, Kind, Presenter, Presenters, Shown, Store, ASSET_PREFIX};
    use gitten_core::host::Host;
    use gpui::{div, AnyElement, IntoElement as _};

    /// A pane keeps both sides of what it was given, keys them by content, and
    /// flips between them — including refusing the flip nobody can make.
    #[test]
    fn a_pane_holds_both_sides_and_flips_between_them() {
        use super::Blob;
        use gitten_core::blob::{self, Blob as Stored, Side};
        use std::sync::{Arc, Mutex};

        fn held(oid: &str, bytes: &[u8]) -> Side {
            Side::Held(Stored::new(Some(oid.to_string()), bytes.to_vec().into()))
        }

        let store: Store = Arc::new(Mutex::new(blob::Store::new(1 << 20)));
        let mut pane = Blob::over(store.clone());
        assert!(!pane.is_showing(), "nothing loaded, nothing to show");

        pane.show(&gitten_app::blobs::Loaded {
            pair: blob::Pair {
                old: held("aaaa", b"\x89PNG\r\n\x1a\nbefore"),
                new: held("bbbb", b"\x89PNG\r\n\x1a\nafter"),
            },
            pages: Vec::new(),
            note: None,
        });
        assert!(pane.is_showing());
        let pair = pane.pair.clone().expect("a pair");
        assert_eq!(
            pane.key_for(pair.new.blob().expect("the new side")),
            "blob-bbbb",
            "it opens on what the file is, not on what it was"
        );
        // Both sides are in the store, under their own identities — which is
        // what makes the renderer's decode a cache hit rather than a second
        // pass over the same picture.
        let held_keys = |store: &Store| {
            let store = store.lock().expect("store");
            (
                store.get("blob-aaaa").is_some(),
                store.get("blob-bbbb").is_some(),
            )
        };
        assert_eq!(held_keys(&store), (true, true));

        assert!(pane.flip(), "a file that changed has an other side");
        assert_eq!(
            pane.key_for(pair.old.blob().expect("the old side")),
            "blob-aaaa"
        );
        assert!(pane.flip(), "and back");

        // An added file: one side, and the flip is refused rather than
        // swapping to an empty pane.
        pane.show(&gitten_app::blobs::Loaded {
            pair: blob::Pair {
                old: Side::Absent,
                new: held("cccc", b"\x89PNG\r\n\x1a\nonly"),
            },
            pages: Vec::new(),
            note: Some("no PDF renderer on PATH".into()),
        });
        assert!(!pane.flip(), "there is no before to show");
        assert_eq!(pane.note.as_deref(), Some("no PDF renderer on PATH"));

        // A refusal is a side too, and its size is the ledger entry.
        pane.show(&gitten_app::blobs::Loaded {
            pair: blob::Pair {
                old: Side::Absent,
                new: Side::TooBig {
                    oid: Some("dddd".into()),
                    size: 2_000_000_000,
                },
            },
            pages: Vec::new(),
            note: None,
        });
        assert!(pane.is_showing(), "a refused side is still a file to draw");

        // The registry is reachable from the pane — the seam, not just the
        // type: registering here is what an extension does, and the new
        // presentation draws the kind it claimed.
        struct Frame;
        impl Presenter for Frame {
            fn name(&self) -> &'static str {
                "frame"
            }
            fn claims(&self, kind: Kind) -> bool {
                kind == Kind::Png
            }
            fn render(&self, _: &Shown, _: &Host, _: &Store) -> AnyElement {
                div().into_any_element()
            }
        }
        pane.presenters().register(Box::new(Frame));
        assert_eq!(pane.presenters().claim(Kind::Png).name(), "frame");
        assert_eq!(
            pane.presenters().claim(Kind::Opaque).name(),
            "hex",
            "and the fallback is still the fallback"
        );
        pane.clear();
        assert!(
            !pane.is_showing(),
            "and cleared, the centre is the rows again"
        );
    }

    /// A flipped PDF draws the old side's own page — each side's raster is
    /// keyed by that side's document, so "before" can never be the after
    /// picture under the before label.
    #[test]
    fn a_pdfs_two_sides_have_their_own_pages() {
        use super::Blob;
        use gitten_core::blob::{self, Blob as Stored, Side};
        use std::sync::{Arc, Mutex};

        let store: Store = Arc::new(Mutex::new(blob::Store::new(1 << 20)));
        let mut pane = Blob::over(store.clone());

        let old_doc = Stored::new(Some("aaaa".into()), b"%PDF-1.4\nold".to_vec().into());
        let new_doc = Stored::new(Some("bbbb".into()), b"%PDF-1.4\nnew".to_vec().into());
        let old_page = gitten_app::blobs::page_key(old_doc.key(), 0);
        let new_page = gitten_app::blobs::page_key(new_doc.key(), 0);
        let pair = blob::Pair {
            old: Side::Held(old_doc),
            new: Side::Held(new_doc),
        };
        pane.show(&gitten_app::blobs::Loaded {
            pair: pair.clone(),
            pages: vec![
                (old_page.clone(), b"png of before".to_vec()),
                (new_page.clone(), b"png of after".to_vec()),
            ],
            note: None,
        });

        assert_eq!(
            pane.key_for(pair.new.blob().expect("the new side")),
            new_page.as_str(),
            "opens on the after side's own page"
        );
        assert!(pane.flip());
        assert_eq!(
            pane.key_for(pair.old.blob().expect("the old side")),
            old_page.as_str(),
            "and a flip is the before side's own page, not the after one's"
        );
    }

    /// A registry takes a second implementation, replaces by name, and lets the
    /// last claim win — the seam's own proof, and the reason the built-ins go
    /// through `register` rather than being hard-coded.
    #[test]
    fn a_second_presentation_fits_without_editing_the_first() {
        struct Loud;
        impl Presenter for Loud {
            fn name(&self) -> &'static str {
                "loud"
            }
            fn claims(&self, kind: Kind) -> bool {
                kind == Kind::Opaque
            }
            fn render(&self, _: &Shown, _: &Host, _: &Store) -> AnyElement {
                div().into_any_element()
            }
        }

        let mut presenters = Presenters::builtin();
        assert_eq!(presenters.claim(Kind::Png).name(), "image");
        assert_eq!(presenters.claim(Kind::Opaque).name(), "hex");

        presenters.register(Box::new(Loud));
        assert_eq!(
            presenters.claim(Kind::Opaque).name(),
            "loud",
            "the latest claim draws"
        );
        assert_eq!(
            presenters.claim(Kind::Png).name(),
            "image",
            "and only for the kinds it claimed"
        );
        assert_eq!(
            presenters.names().collect::<Vec<_>>(),
            vec!["hex", "image", "loud"],
            "registering added rather than replacing"
        );

        // Registering the same name replaces it in place.
        presenters.register(Box::new(Loud));
        assert_eq!(presenters.names().count(), 3, "a name is registered once");
    }

    /// Every kind the classifier can name has a presentation, and the ones that
    /// are pictures get the picture.
    #[test]
    fn every_kind_is_claimed_by_something() {
        let presenters = Presenters::builtin();
        for kind in [
            Kind::Png,
            Kind::Jpeg,
            Kind::Gif,
            Kind::Webp,
            Kind::Bmp,
            Kind::Ico,
            Kind::Tiff,
            Kind::Svg,
            Kind::Pdf,
        ] {
            assert_eq!(presenters.claim(kind).name(), "image", "{kind:?}");
        }
        for kind in [Kind::Text, Kind::Opaque] {
            assert_eq!(presenters.claim(kind).name(), "hex", "{kind:?}");
        }
    }

    /// The asset path is a path. A scheme in it is a network fetch in every
    /// renderer's asset system, which is the one way this feature could turn a
    /// local file into a request.
    /// A colon is what makes a string a URI to an asset system, and a URI is a
    /// *fetch* — so the one way this feature could turn a local picture into a
    /// network request is a scheme in the path, and there is not one.
    #[test]
    fn an_asset_path_carries_no_scheme() {
        for key in [
            "blob-2f2a870ea158f6559ffb5988857b3aeb",
            "content-1f8a2c3d4e5f6071-4096",
            "blob-doc-page0",
        ] {
            let path = asset_path(key);
            assert!(path.starts_with(ASSET_PREFIX), "{path}");
            assert!(!path.contains(':'), "{path} looks like a URL");
            assert!(
                path.split('/').all(|part| !part.is_empty()),
                "{path} has an empty segment"
            );
        }
    }

    #[test]
    fn sizes_read_the_way_a_status_line_says_them() {
        assert_eq!(bytes_word(0), "0 bytes");
        assert_eq!(bytes_word(999), "999 bytes");
        assert_eq!(bytes_word(1024), "1.0 KB");
        assert_eq!(bytes_word(1536), "1.5 KB");
        assert_eq!(bytes_word(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(bytes_word(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
