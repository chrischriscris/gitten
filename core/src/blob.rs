//! Blobs: the bytes a diff has no lines for.
//!
//! A PNG, a PDF and a tarball reach a frontend as the same thing — a pair with
//! `binary: true` and no lines on either side — and a pane that draws a
//! repository has nothing to say about any of them. This module is the part of
//! the answer that is not a UI: what a run of bytes *is*, and the small store
//! that holds the ones a view has already paid for.
//!
//! **The classifier reads the first bytes, not the extension.** A path is a
//! claim by whoever named the file; the header is the file. `logo.png` holding
//! a JPEG is not a rare shape — it is what renaming a `.gif` does — and a
//! viewer that trusts the name draws nothing at all where it could have drawn
//! the picture. The extension is left for the one question magic bytes cannot
//! answer for a text file, and even there it is a *hint* beside the content
//! rather than a substitute for it.
//!
//! **Identity is content, and the store is keyed on it.** A blob never
//! changes, so an object-database side is keyed by its OID — the identity git
//! already gave it, for free. A working-tree side has no OID (it is nowhere but
//! disk), so its key is a hash of the bytes themselves. Two consequences, both
//! wanted: the same picture at two revisions is one entry in whatever decodes
//! it and one decoded image in the GPU's cache, and a file rewritten between
//! two loads cannot be served from the answer to the previous one — the key it
//! was stored under no longer exists.
//!
//! Nothing here decodes, scales, rasterizes or draws. A kind and a byte range
//! are what a presenter needs and all this decides.

use std::collections::VecDeque;
use std::hash::Hasher as _;
use std::sync::Arc;

use crate::{FxHashMap, FxHasher};

/// A blob's bytes, handed around by refcount.
///
/// The same allocation is read by the classifier, kept by the store and read
/// again by whatever decodes it, so every one of those is a bump rather than a
/// copy — and a frontend that hands the bytes to a decoder on another thread
/// does not need the store to stay borrowed while it runs.
pub type Bytes = Arc<[u8]>;

/// How much of a blob is looked at to decide what it is.
///
/// The same 8000 bytes git itself scans, for the same reason: every format
/// worth naming declares itself in the first handful of bytes, and a reader
/// that has to see all of a 2 GB file before it can say "this is not an image"
/// is a reader that reads 2 GB to say nothing.
pub const SNIFF_BYTES: usize = 8000;

/// What a run of bytes is, as far as presenting it goes.
///
/// Deliberately a short list of things a *client* can tell apart. It is not a
/// media-type registry: the long tail — a tarball, an .stl, a font — is
/// [`Kind::Opaque`], and what a viewer does with those is describe them
/// honestly rather than pretend. A container format is not an entry here
/// until something draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Ico,
    /// Tagged Image File Format, both endiannesses. Worth the arm because a
    /// scanned page is the thing people expect a viewer to open.
    Tiff,
    /// SVG. Text on disk and a picture on screen, which is why it is the one
    /// kind a client has to decide *twice*: as a file it has a diff, as an
    /// asset it has an image.
    Svg,
    Pdf,
    /// Bytes with no NUL in the sniffed window that decode as UTF-8. A blob
    /// context is the only place this matters — a text file with a diff is a
    /// diff — but "git called this binary and it is really text" is a real
    /// answer, and the honest one is the text, shown as bytes.
    Text,
    /// Everything else. Not a failure: the fallback presentation is the one
    /// that says the size, the object id and the first bytes, which is more
    /// than an empty pane says.
    Opaque,
}

impl Kind {
    /// What these bytes are, from the bytes themselves.
    ///
    /// Ordered as the formats are common, because the cost of a miss is a
    /// comparison against the next prefix and the cost of the common case is
    /// one. The PDF check is a scan rather than a prefix: a PDF's `%PDF-`
    /// header has to sit within the first kilobyte (the format allows a
    /// prologue before it), and a viewer that only looks at byte zero fails on
    /// every file some tool has already rewritten.
    pub fn of(bytes: &[u8]) -> Self {
        let head = &bytes[..bytes.len().min(SNIFF_BYTES)];
        if head.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Kind::Png;
        }
        if head.starts_with(b"\xff\xd8\xff") {
            return Kind::Jpeg;
        }
        if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
            return Kind::Gif;
        }
        if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
            return Kind::Webp;
        }
        if head.starts_with(b"BM") {
            return Kind::Bmp;
        }
        if head.starts_with(b"\x00\x00\x01\x00") {
            return Kind::Ico;
        }
        if head.starts_with(b"II\x2a\x00") || head.starts_with(b"MM\x00\x2a") {
            return Kind::Tiff;
        }
        if is_pdf(head) {
            return Kind::Pdf;
        }
        if is_svg(head) {
            return Kind::Svg;
        }
        if !head.contains(&0) && std::str::from_utf8(head).is_ok() {
            return Kind::Text;
        }
        Kind::Opaque
    }

    /// The name a client shows. Lowercase, for a status line rather than a
    /// sentence — `PNG`, `PDF`, `opaque`.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Png => "png",
            Kind::Jpeg => "jpeg",
            Kind::Gif => "gif",
            Kind::Webp => "webp",
            Kind::Bmp => "bmp",
            Kind::Ico => "ico",
            Kind::Tiff => "tiff",
            Kind::Svg => "svg",
            Kind::Pdf => "pdf",
            Kind::Text => "text",
            Kind::Opaque => "opaque",
        }
    }
}

/// A PDF's header, which the format allows to be preceded by up to a kilobyte
/// of anything — a `%PDF-` at byte zero is the common case and not the
/// guaranteed one.
fn is_pdf(head: &[u8]) -> bool {
    let window = &head[..head.len().min(1024)];
    window
        .windows(5)
        .any(|w| w == b"%PDF-" || w.eq_ignore_ascii_case(b"%pdf-"))
}

/// An SVG, which is XML: what makes it one is the *root element*, and the
/// prolog, comments and a doctype may precede it. A `<svg` inside a comment or
/// under an `<html>` is not this document's root — an HTML page with an inline
/// icon is a page, and drawing it as a picture would draw nothing.
///
/// A truncated prolog — the window ending inside `<?xml` — is read as not an
/// SVG rather than guessed at: an 8 KB prolog is not a document, and every
/// real one fits in a line.
fn is_svg(head: &[u8]) -> bool {
    let text = match std::str::from_utf8(head) {
        Ok(text) => text,
        // A multi-byte character cut in half by the window is the ordinary
        // case for non-ASCII prose, so the valid prefix is the window.
        Err(e) => match std::str::from_utf8(&head[..e.valid_up_to()]) {
            Ok(text) => text,
            Err(_) => return false,
        },
    };
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("<?") {
            let Some(end) = after.find("?>") else {
                return false;
            };
            rest = &after[end + 2..];
        } else if let Some(after) = rest.strip_prefix("<!--") {
            let Some(end) = after.find("-->") else {
                return false;
            };
            rest = &after[end + 3..];
        } else if rest.starts_with("<!") {
            let Some(end) = rest.find('>') else {
                return false;
            };
            rest = &rest[end + 1..];
        } else {
            // The root element, and it has to be one: `<svg` followed by the
            // separator a tag name cannot run on into.
            return rest.strip_prefix("<svg").is_some_and(|tail| {
                matches!(
                    tail.as_bytes().first(),
                    Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') | Some(b'>') | Some(b'/')
                )
            });
        }
    }
}

/// A blob's bytes with the identity they are cached under and the kind they
/// were classified as.
#[derive(Debug, Clone)]
pub struct Blob {
    /// The object id, when the bytes came out of the object database. `None`
    /// for a working-tree side, whose content is nowhere but disk.
    pub oid: Option<String>,
    pub kind: Kind,
    pub bytes: Bytes,
    /// The content identity, computed once at build. A working-tree side's key
    /// is a hash of every byte, and a key asked for per frame would pay that
    /// hash per frame — so it is paid here, once, instead.
    key: String,
}

impl Blob {
    /// Builds a blob, classifying its bytes and naming its identity once here
    /// rather than once per reader.
    pub fn new(oid: Option<String>, bytes: Bytes) -> Self {
        let kind = Kind::of(&bytes);
        let key = match &oid {
            Some(oid) => format!("blob-{oid}"),
            None => {
                let mut hasher = FxHasher::default();
                hasher.write(&bytes);
                hasher.write_usize(bytes.len());
                format!("content-{:016x}-{}", hasher.finish(), bytes.len())
            }
        };
        Self {
            oid,
            kind,
            bytes,
            key,
        }
    }

    /// The key this blob is stored and decoded under.
    ///
    /// The OID when there is one — git's own identity, and free. Otherwise a
    /// hash of the bytes, which is the same answer for the same content and a
    /// different one for anything else. FxHash and not a cryptographic hash:
    /// the key is never compared against an input an attacker chooses, and a
    /// clash costs one wrong picture, so the extra cycles of SHA-256 would buy
    /// nothing a byte comparison does not.
    ///
    /// **No colon anywhere in it.** A frontend hands this key to a renderer's
    /// asset system as a path, and a string with a scheme-shaped prefix is a
    /// URL to every such system — `blob:abc` is not a blob, it is a request to
    /// fetch `abc` from the `blob` scheme at the first frame. Dashes carry the
    /// same information and mean nothing to a URI parser.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// One side of a blob pair, which is three different answers and not two.
///
/// **Too big is not missing.** A viewer that conflates them says "the file was
/// added" about a 2 GB video, which is wrong in the direction that wastes
/// somebody's afternoon. The size git reported travels with the refusal so the
/// pane can say the number it refused.
#[derive(Debug, Clone)]
pub enum Side {
    /// The bytes are here, classified.
    Held(Blob),
    /// The side exists and is over whatever limit the reader was given.
    TooBig { oid: Option<String>, size: u64 },
    /// There is nothing on this side: an added file's old side, a deleted
    /// file's new one, a source with no repository behind it.
    Absent,
}

impl Side {
    /// Nothing on this side, which is the honest starting point for a pair
    /// nothing has answered for yet.
    pub fn absent() -> Self {
        Side::Absent
    }

    /// The object id, whichever of the three answers this is.
    pub fn oid(&self) -> Option<&str> {
        match self {
            Side::Held(blob) => blob.oid.as_deref(),
            Side::TooBig { oid, .. } => oid.as_deref(),
            Side::Absent => None,
        }
    }

    pub fn blob(&self) -> Option<&Blob> {
        match self {
            Side::Held(blob) => Some(blob),
            _ => None,
        }
    }

    pub fn is_absent(&self) -> bool {
        matches!(self, Side::Absent)
    }
}

/// A side that was never read is a side with nothing on it, so the default is
/// the empty answer rather than a fourth state.
impl Default for Side {
    fn default() -> Self {
        Side::Absent
    }
}

/// Both sides of one path's blob — what a reader answers when the diff has no
/// lines to show.
#[derive(Debug, Clone, Default)]
pub struct Pair {
    pub old: Side,
    pub new: Side,
}

impl Pair {
    /// Every side a viewer shows, newest first, each with the word a label can
    /// use.
    ///
    /// `before` and `after` rather than `HEAD` and `working tree`: a pair is
    /// also what a committed range or a stash answers, and a label naming the
    /// wrong tree is worse than one naming none.
    pub fn sides(&self) -> impl Iterator<Item = (SideOf, &Side)> {
        [(SideOf::After, &self.new), (SideOf::Before, &self.old)].into_iter()
    }

    /// The blobs worth keeping, for a caller filling a store — refcount bumps
    /// and nothing else.
    pub fn blobs(&self) -> impl Iterator<Item = &Blob> {
        self.sides().filter_map(|(_, side)| side.blob())
    }

    /// Whether there is nothing at all to show.
    pub fn is_empty(&self) -> bool {
        self.old.is_absent() && self.new.is_absent()
    }
}

/// Which end of a pair a side is: what the file is now, or what it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideOf {
    Before,
    After,
}

impl SideOf {
    pub fn label(self) -> &'static str {
        match self {
            SideOf::Before => "before",
            SideOf::After => "after",
        }
    }
}

/// Blobs a view has already paid for, by [`Side::key`], bounded by bytes.
///
/// Bytes and not a count, because one video and one favicon are not the same
/// memory and a count that treats them alike is a budget that means nothing.
/// Eviction is insertion order — the working set of a diff viewer is the file
/// under the cursor and the one it came from, so the interesting question is
/// whether the *last* few are still here, and an LRU's bookkeeping would cost
/// more than it saves on a list this short.
pub struct Store {
    budget: usize,
    spent: usize,
    order: VecDeque<String>,
    entries: FxHashMap<String, Blob>,
}

impl Store {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            spent: 0,
            order: VecDeque::new(),
            entries: FxHashMap::default(),
        }
    }

    /// Keeps a blob, and reports whether it fit.
    ///
    /// Nothing that does not fit is stored — a blob over the budget is not
    /// worth evicting everything else for — so a viewer asking after one gets
    /// the same answer as for one it has never seen, which is the honest pair
    /// of words: load it again, or say it is too big.
    pub fn insert(&mut self, blob: Blob) -> bool {
        let key = blob.key().to_owned();
        self.insert_at(&key, blob)
    }

    /// Keeps a blob under a key of the caller's choosing.
    ///
    /// One caller needs this and it is not a loophole: a *rasterized* page is
    /// not the document it came from — its identity is the document and the
    /// page number, which no hash of the PNG can express — and a store that
    /// could only key by content would hold a page under a name nothing asks
    /// for. Everything stored still fits [`Store`]'s one budget, and a key
    /// still has to carry no scheme; see [`Blob::key`].
    pub fn insert_at(&mut self, key: &str, blob: Blob) -> bool {
        let key = key.to_string();
        let len = blob.bytes.len();
        if len > self.budget {
            return false;
        }
        if let Some(previous) = self.entries.remove(&key) {
            self.spent -= previous.bytes.len();
            self.order.retain(|k| k != &key);
        }
        while self.spent + len > self.budget {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(gone) = self.entries.remove(&oldest) {
                self.spent -= gone.bytes.len();
            }
        }
        self.spent += len;
        self.order.push_back(key.clone());
        self.entries.insert(key, blob);
        true
    }

    /// A side already paid for. `None` is "not here", which for a caller means
    /// the same thing whether it was never loaded or has just been evicted:
    /// ask acquisition again.
    pub fn get(&self, key: &str) -> Option<&Blob> {
        self.entries.get(key)
    }

    /// What is held, in bytes — the number the budget bounds.
    pub fn spent(&self) -> usize {
        self.spent
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(bytes: &[u8]) -> Blob {
        Blob::new(None, Bytes::from(bytes.to_vec()))
    }

    /// Every kind is decided by the bytes, and the ones a viewer can draw are
    /// named. A wrong answer here is a picture that does not appear.
    #[test]
    fn a_kind_comes_from_the_header_and_not_the_name() {
        assert_eq!(Kind::of(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR"), Kind::Png);
        assert_eq!(Kind::of(b"\xff\xd8\xff\xe0\x00\x10JFIF"), Kind::Jpeg);
        assert_eq!(Kind::of(b"GIF89a\x01\x00"), Kind::Gif);
        assert_eq!(
            Kind::of(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            Kind::Webp,
            "the RIFF container is not the format: WEBP at byte 8 is"
        );
        assert_eq!(Kind::of(b"RIFF\x24\x00\x00\x00WAVEfmt "), Kind::Opaque);
        assert_eq!(Kind::of(b"BM\x36\x00\x00\x00"), Kind::Bmp);
        assert_eq!(Kind::of(b"\x00\x00\x01\x00\x01\x00"), Kind::Ico);
        assert_eq!(Kind::of(b"II\x2a\x00\x08\x00"), Kind::Tiff);
        assert_eq!(Kind::of(b"MM\x00\x2a\x00\x00"), Kind::Tiff);
    }

    /// A PDF's header is allowed a prologue, so a scan and not a prefix.
    #[test]
    fn a_pdf_is_found_past_a_prologue() {
        assert_eq!(Kind::of(b"%PDF-1.7\n%\xe2\xe3"), Kind::Pdf);
        let mut late = b"% a prologue some tool wrote\n".to_vec();
        late.extend_from_slice(b"%PDF-1.4\n");
        assert_eq!(Kind::of(&late), Kind::Pdf, "byte zero is not the rule");
        // And past the window the format allows, nothing is claimed: those
        // bytes are ASCII, and the classifier says what they are.
        let mut far = vec![b'x'; 2048];
        far.extend_from_slice(b"%PDF-1.4\n");
        assert_eq!(Kind::of(&far), Kind::Text);
    }

    /// An SVG is XML whose root element is `<svg>`, whatever precedes it.
    #[test]
    fn an_svg_is_the_root_element() {
        assert_eq!(Kind::of(b"<svg xmlns=\"...\">"), Kind::Svg);
        assert_eq!(Kind::of(b"<svg/>"), Kind::Svg);
        assert_eq!(Kind::of(b"\n  <?xml version=\"1.0\"?>\n<svg>"), Kind::Svg);
        assert_eq!(
            Kind::of(b"<!-- drawn --><svg>"),
            Kind::Svg,
            "a comment may precede the root"
        );
        assert_eq!(
            Kind::of(b"<!-- <svg> --><html>"),
            Kind::Text,
            "a tag inside a comment is not a root element"
        );
        assert_eq!(
            Kind::of(b"<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x.dtd\"><svg>"),
            Kind::Svg
        );
        assert_eq!(
            Kind::of(b"<svgx>"),
            Kind::Text,
            "`<svg` running on into another tag name is another tag"
        );
        assert_eq!(
            Kind::of(b"<html><body><svg></svg></body></html>"),
            Kind::Text,
            "an SVG inlined in a page is a page"
        );
    }

    /// Text is the absence of a NUL and the presence of UTF-8, which is git's
    /// own test widened by the one thing it does not check.
    #[test]
    fn text_is_utf8_with_no_nul_in_the_window() {
        assert_eq!(Kind::of("fn main() {}\n".as_bytes()), Kind::Text);
        assert_eq!(Kind::of("héllo — ok\n".as_bytes()), Kind::Text);
        assert_eq!(
            Kind::of(b"b\xe9zier"), // Latin-1, as real history carries it
            Kind::Opaque,
            "not UTF-8 is not text"
        );
        let mut nul = b"a\0b".to_vec();
        nul.extend_from_slice(&[b'x'; 100]);
        assert_eq!(Kind::of(&nul), Kind::Opaque);
        // A NUL past the window is not looked at, which is git's trade too.
        let mut far = vec![b'x'; SNIFF_BYTES + 16];
        far[SNIFF_BYTES + 8] = 0;
        assert_eq!(Kind::of(&far), Kind::Text);
    }

    /// The key is the identity: an OID when git gave one, the content when it
    /// did not, and the same content always keys the same.
    #[test]
    fn a_sides_key_is_its_identity() {
        let named = Blob::new(Some("a1b2c3".into()), Bytes::from(b"picture".to_vec()));
        assert_eq!(named.key(), "blob-a1b2c3");

        let first = side(b"same bytes");
        let second = side(b"same bytes");
        assert_eq!(first.key(), second.key(), "content addresses content");
        assert_ne!(side(b"other bytes").key(), first.key());
        assert!(
            !first.key().contains(':') && !named.key().contains(':'),
            "a key is a path: a colon in it is a URL scheme to every renderer's \
             asset loader, and `blob:abc` is a fetch rather than a blob"
        );
        assert_ne!(
            side(b"short").key(),
            side(b"longer").key(),
            "the length is in the key, so a prefix cannot collide with its extension"
        );
    }

    /// The store keeps what fits, evicts the oldest first, and never lets the
    /// spent total drift from what it holds.
    #[test]
    fn the_store_evicts_the_oldest_and_counts_what_it_keeps() {
        let mut store = Store::new(10);
        let a = side(b"aaaa");
        let (ka, kb) = (a.key().to_owned(), side(b"bbbbbb").key().to_owned());
        assert!(store.insert(a));
        assert!(store.insert(side(b"bbbbbb")));
        assert_eq!(store.spent(), 10);
        assert!(store.get(&ka).is_some() && store.get(&kb).is_some());

        // One more byte than the budget evicts in insertion order, oldest
        // first, until it fits.
        assert!(store.insert(side(b"ccc")));
        assert!(store.get(&ka).is_none(), "the oldest went");
        assert!(store.get(&kb).is_some(), "and only the oldest");
        assert_eq!(store.spent(), 9);
        assert_eq!(store.len(), 2);

        // Anything over the whole budget is refused rather than stored and
        // evicting everything else for.
        assert!(!store.insert(side(b"01234567890")));
        assert_eq!(store.len(), 2, "a refused side changed nothing");
    }

    /// Re-inserting the same side is not a second copy, and it cannot leave the
    /// spent total above what is held.
    #[test]
    fn re_inserting_a_side_moves_it_and_does_not_double_count() {
        let mut store = Store::new(10);
        let a = side(b"aaaa");
        let ka = a.key().to_owned();
        store.insert(a);
        store.insert(side(b"bbbb"));
        store.insert(side(b"aaaa"));
        assert_eq!(store.spent(), 8);
        assert_eq!(store.len(), 2);
        // And the re-inserted one is now the newest, so the *other* goes first.
        store.insert(side(b"cccccc"));
        assert!(store.get(&ka).is_some(), "it was re-inserted, not read");
        assert!(store.get(side(b"bbbb").key()).is_none());
    }

    #[test]
    fn a_blob_can_be_kept_under_a_name_of_its_own() {
        // A rasterized page: the bytes are a PNG nobody can hash into the key
        // that names them, because the identity is the document and the page.
        let mut store = Store::new(64);
        let page = Blob::new(None, Bytes::from(b"a drawn page".to_vec()));
        assert!(store.insert_at("blob-doc-page0", page.clone()));
        assert!(store.get("blob-doc-page0").is_some());
        assert!(
            store.get(page.key()).is_none(),
            "and not under the content key as well — one entry, one name"
        );
        assert_eq!(store.spent(), page.bytes.len());
    }

    #[test]
    fn a_pair_lists_the_new_side_before_the_old() {
        let pair = Pair {
            old: Side::Held(side(b"before")),
            new: Side::Held(side(b"after")),
        };
        let names: Vec<&str> = pair.sides().map(|(at, _)| at.label()).collect();
        assert_eq!(
            names,
            vec!["after", "before"],
            "what it is, then what it was"
        );
        assert_eq!(pair.blobs().count(), 2);
        assert!(Pair::default().is_empty(), "two absent sides are nothing");
    }

    /// A refused side keeps its size and its identity, because "too big" and
    /// "not there" are different sentences and only one of them is a bug.
    #[test]
    fn a_side_over_the_limit_is_not_a_missing_one() {
        let pair = Pair {
            old: Side::Absent,
            new: Side::TooBig {
                oid: Some("a1b2c3".into()),
                size: 2_000_000_000,
            },
        };
        assert!(!pair.is_empty(), "a refused side is a side");
        assert_eq!(pair.new.oid(), Some("a1b2c3"));
        assert!(pair.new.blob().is_none());
        assert_eq!(
            pair.blobs().count(),
            0,
            "nothing to store, and nothing lost"
        );
        assert!(pair.old.is_absent());
    }
}
