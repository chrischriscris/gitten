//! Syntax highlighting, as a scanner plus a table.
//!
//! The scanner knows nothing about any language: comments, strings, numbers and
//! words are all it understands. Everything language-specific — which byte
//! sequence opens a comment, whether block comments nest, which words are
//! keywords — is data in a [`Syntax`], and a [`Syntax`] can be built at runtime.
//! That is the point: registering a language is not a code change, so an
//! extension adds one exactly the way the built-ins do.
//!
//! Why not a real parser: a diff hands you fragments, and a parser's fast path
//! assumes a whole parseable file. Measured over 85 files of Zed's `gpui`,
//! tree-sitter runs at 7.1 MB/s on whole files but 2.6 MB/s on hunk-shaped
//! fragments, and loses a fifth of its spans to error recovery on the way. This
//! scanner runs at 104–262 MB/s depending on the language and does not care
//! that its input has holes in it, because it never had parse context to lose.
//! It colours 40–67% of bytes where tree-sitter colours 66–89%.
//!
//! What it costs: no semantic classes. A call is a name followed by `(`, a type
//! is a capitalised word. And markup defeats it — HTML with inline `<script>`,
//! PHP's `<?php` islands and Markdown all need injections, which means a real
//! parser. Those languages get no table rather than a wrong one; a second
//! [`Highlighter`] implementation can cover them without this one changing.

use crate::LineKind;
use std::ops::Range;

/// What a token is, coarsely. Deliberately small: these are the classes a
/// scanner can identify without a parse, and a dense diff should not be
/// wearing more colours than this anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Comment,
    Str,
    Number,
    Keyword,
    Type,
    Constant,
    Func,
    Property,
    /// The classes below exist for prose. A scanner over code never emits them;
    /// the Markdown highlighter emits little else. They are here rather than in
    /// that highlighter because a theme has to be able to style them, and a
    /// theme cannot depend on which highlighter happened to run.
    Heading,
    Strong,
    Emphasis,
    Link,
}

impl Kind {
    pub const ALL: [Kind; 12] = [
        Kind::Comment,
        Kind::Str,
        Kind::Number,
        Kind::Keyword,
        Kind::Type,
        Kind::Constant,
        Kind::Func,
        Kind::Property,
        Kind::Heading,
        Kind::Strong,
        Kind::Emphasis,
        Kind::Link,
    ];
    pub const COUNT: usize = Self::ALL.len();

    /// Position in [`Kind::ALL`], so a theme can be an array rather than a match.
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// A classified byte range within one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

impl Token {
    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// One kind of string literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrRule {
    pub open: String,
    pub close: String,
    /// `\` escapes the closing delimiter.
    pub escape: bool,
    /// May contain newlines. When false, an unterminated string ends at the end
    /// of its line — which is what stops one stray quote from painting the rest
    /// of the file, and matters far more in a diff than in an editor because a
    /// hunk boundary cuts literals in half all the time.
    pub multiline: bool,
}

/// Everything the scanner needs to know about one language.
///
/// Build with [`Syntax::new`] and the setters; they sort the keyword list and
/// precompute the opener lookup, so nothing derived is recomputed per file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Syntax {
    line: Vec<String>,
    block: Vec<(String, String)>,
    nested_block: bool,
    strings: Vec<StrRule>,
    keywords: Vec<String>,
    capitalized_types: bool,
    call_heuristic: bool,
    line_needs_boundary: bool,
    quote_after_eq: bool,
    /// Bytes that could open a comment or a string here. Without it every byte
    /// of every file walks every rule; with it only the few bytes that could
    /// start something pay. Worth ~2x on every language measured.
    opens: [bool; 256],
}

impl Default for Syntax {
    fn default() -> Self {
        Self::new()
    }
}

impl Syntax {
    pub fn new() -> Self {
        Self {
            line: Vec::new(),
            block: Vec::new(),
            nested_block: false,
            strings: Vec::new(),
            keywords: Vec::new(),
            capitalized_types: false,
            call_heuristic: false,
            line_needs_boundary: false,
            quote_after_eq: false,
            opens: [false; 256],
        }
    }

    fn mark(&mut self, pat: &str) {
        if let Some(b) = pat.as_bytes().first() {
            self.opens[*b as usize] = true;
        }
    }

    /// Comment markers that run to the end of the line: `//`, `#`, `--`.
    pub fn line(mut self, pats: &[&str]) -> Self {
        for p in pats {
            self.mark(p);
            self.line.push((*p).to_string());
        }
        self
    }

    /// Delimited comments: `("/*", "*/")`, `("<!--", "-->")`.
    pub fn block(mut self, pairs: &[(&str, &str)]) -> Self {
        for (o, c) in pairs {
            self.mark(o);
            self.block.push(((*o).to_string(), (*c).to_string()));
        }
        self
    }

    /// Block comments nest, the way they do in Rust, Swift and Kotlin but not C.
    pub fn nested_block(mut self) -> Self {
        self.nested_block = true;
        self
    }

    /// String rules, longest opener first: `"""` must be tried before `"`.
    pub fn strings(mut self, rules: &[(&str, &str, bool, bool)]) -> Self {
        for (open, close, escape, multiline) in rules {
            self.mark(open);
            self.strings.push(StrRule {
                open: (*open).to_string(),
                close: (*close).to_string(),
                escape: *escape,
                multiline: *multiline,
            });
        }
        self
    }

    /// Keywords, in any order — they are sorted here for binary search.
    pub fn keywords(mut self, words: &[&str]) -> Self {
        self.keywords.extend(words.iter().map(|w| (*w).to_string()));
        self.keywords.sort_unstable();
        self.keywords.dedup();
        self
    }

    /// A capitalised word is a type, unless it has no lowercase letter at all,
    /// which makes it a constant — `MaybeUninit` against `LIBUS_RECV_BUFFER`.
    /// Two characters or fewer stay types so `T`, `E` and `IO` read correctly.
    ///
    /// Off for C and Lua, where the convention collides: nearly every
    /// capitalised word there is a macro and the file would light up.
    pub fn capitalized_types(mut self) -> Self {
        self.capitalized_types = true;
        self
    }

    /// A word followed by `(` or `!` is a call; a word after `.` is a field.
    /// Two heuristics, and between them most of what a parser would add here:
    /// they lift coverage on Rust from 29% to 45% of bytes for 12 MB/s.
    pub fn call_heuristic(mut self) -> Self {
        self.call_heuristic = true;
        self
    }

    /// A line-comment marker only counts at line start or after whitespace.
    /// Without this every `$#` and `${x#y}` paints the rest of a shell line —
    /// measured at ~1% of all bytes in a shell corpus before the rule, and it
    /// is the single worst failure this scanner had.
    pub fn line_needs_boundary(mut self) -> Self {
        self.line_needs_boundary = true;
        self
    }

    /// A quote only opens a string directly after `=`, i.e. inside a tag.
    /// Markup is prose, prose is full of apostrophes, and none of them are
    /// strings. Cuts stray colouring in an HTML corpus from 6.2% to 2.5%.
    pub fn quote_after_eq(mut self) -> Self {
        self.quote_after_eq = true;
        self
    }
}

/// Which [`Syntax`] a path gets. Extensions register here the same way the
/// built-ins do, and a later registration wins, so a table can be replaced.
#[derive(Debug, Clone, Default)]
pub struct Languages {
    by_ext: Vec<(String, Syntax)>,
}

impl Languages {
    pub fn empty() -> Self {
        Self { by_ext: Vec::new() }
    }

    pub fn register(&mut self, exts: &[&str], syntax: Syntax) {
        for ext in exts {
            let ext = ext.to_ascii_lowercase();
            match self.by_ext.iter_mut().find(|(e, _)| *e == ext) {
                Some(slot) => slot.1 = syntax.clone(),
                None => self.by_ext.push((ext, syntax.clone())),
            }
        }
    }

    /// `None` means no table, which means no highlighting — the honest answer
    /// for a language nobody has described yet, and for the ones a scanner
    /// cannot do at all.
    ///
    /// The whole filename is tried before the extension, which is how a name
    /// with no useful extension — `Cargo.lock` — reaches a table at all.
    pub fn for_path(&self, path: &str) -> Option<&Syntax> {
        let name = path.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
        let find = |key: &str| self.by_ext.iter().find(|(e, _)| e == key).map(|(_, s)| s);
        find(&name).or_else(|| find(name.rsplit_once('.')?.1))
    }
}

// ------------------------------------------------------------------ the trait

/// One side of a hunk in, one token list per line out.
///
/// Lines, not a file, because that is what a diff has. Implementations that
/// want a whole file (a tree-sitter one, say) can still stitch these together
/// or fetch the blob themselves; the frontend never learns which happened.
pub trait Highlighter {
    fn highlight(&self, path: &str, lines: &[&str]) -> Vec<Vec<Token>>;
}

/// The built-in [`Highlighter`]: the scanner plus a table registry.
#[derive(Debug, Clone, Default)]
pub struct Lexer {
    pub languages: Languages,
}

impl Lexer {
    /// Every language described below.
    pub fn builtin() -> Self {
        Self { languages: builtin_languages() }
    }
}

impl Highlighter for Lexer {
    fn highlight(&self, path: &str, lines: &[&str]) -> Vec<Vec<Token>> {
        match self.languages.for_path(path) {
            Some(syn) => lex_lines(lines, syn),
            None => vec![Vec::new(); lines.len()],
        }
    }
}

/// Routes each path to a [`Highlighter`], so implementations are chosen per
/// language rather than for the whole app.
///
/// This is the seam that matters: the scanner cannot do Markdown, HTML or PHP,
/// and rather than teach it to guess, those paths go somewhere else. A
/// tree-sitter highlighter — its own crate, its own dependencies, none of them
/// reaching `core` — registers here exactly the way [`Markdown`] does below.
pub struct Highlighters {
    routes: Vec<(Vec<String>, Box<dyn Highlighter>)>,
    fallback: Fallback,
}

/// The fallback is kept concrete while it is still the scanner, so that
/// registering a language — much the most common extension there is — does not
/// mean rebuilding it.
enum Fallback {
    Scanner(Lexer),
    Custom(Box<dyn Highlighter>),
}

impl Fallback {
    fn as_highlighter(&self) -> &dyn Highlighter {
        match self {
            Fallback::Scanner(l) => l,
            Fallback::Custom(h) => h.as_ref(),
        }
    }
}

impl Default for Highlighters {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Highlighters {
    /// The scanner for everything, with Markdown routed away from it.
    pub fn builtin() -> Self {
        let mut h = Self { routes: Vec::new(), fallback: Fallback::Scanner(Lexer::builtin()) };
        h.route(&["md", "markdown", "mdx"], Markdown);
        h
    }

    pub fn with_fallback(fallback: impl Highlighter + 'static) -> Self {
        Self { routes: Vec::new(), fallback: Fallback::Custom(Box::new(fallback)) }
    }

    /// The scanner's language tables, for adding or replacing one:
    ///
    /// ```ignore
    /// host.syntax.languages().unwrap().register(&["nim"], syntax);
    /// ```
    ///
    /// `None` once the fallback has been replaced by something that is not the
    /// scanner — there are no tables to register with then.
    pub fn languages(&mut self) -> Option<&mut Languages> {
        match &mut self.fallback {
            Fallback::Scanner(lexer) => Some(&mut lexer.languages),
            Fallback::Custom(_) => None,
        }
    }

    /// Keys are extensions or whole filenames, matched the way
    /// [`Languages::for_path`] matches them. A later route wins, so a built-in
    /// can be replaced rather than only added to.
    pub fn route(&mut self, keys: &[&str], hl: impl Highlighter + 'static) {
        let keys: Vec<String> = keys.iter().map(|k| k.to_ascii_lowercase()).collect();
        self.routes.push((keys, Box::new(hl)));
    }

    pub fn set_fallback(&mut self, hl: impl Highlighter + 'static) {
        self.fallback = Fallback::Custom(Box::new(hl));
    }

    pub fn for_path(&self, path: &str) -> &dyn Highlighter {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_ascii_lowercase();
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_string());
        for (keys, hl) in self.routes.iter().rev() {
            let hit = keys.iter().any(|k| *k == name || Some(k) == ext.as_ref());
            if hit {
                return hl.as_ref();
            }
        }
        self.fallback.as_highlighter()
    }
}

impl Highlighter for Highlighters {
    fn highlight(&self, path: &str, lines: &[&str]) -> Vec<Vec<Token>> {
        self.for_path(path).highlight(path, lines)
    }
}

// ---------------------------------------------------------------- the scanner

#[inline]
fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

#[inline]
fn at(b: &[u8], i: usize, pat: &str) -> bool {
    let p = pat.as_bytes();
    i + p.len() <= b.len() && &b[i..i + p.len()] == p
}

/// Scans `src` once, appending tokens in order. Ranges never overlap.
pub fn lex(src: &str, syn: &Syntax, out: &mut Vec<Token>) {
    let b = src.as_bytes();
    let mut i = 0;
    let mut push = |start: usize, end: usize, kind: Kind| out.push(Token { start, end, kind });

    'scan: while i < b.len() {
        let c = b[i];

        // The common case by a wide margin: a byte that cannot open anything.
        if !syn.opens[c as usize] && !is_word(c) {
            i += 1;
            while i < b.len() && (b[i] & 0xC0) == 0x80 {
                i += 1;
            }
            continue;
        }

        for pat in &syn.line {
            let boundary_ok = !syn.line_needs_boundary || i == 0 || b[i - 1].is_ascii_whitespace();
            if boundary_ok && at(b, i, pat) {
                let start = i;
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                push(start, i, Kind::Comment);
                continue 'scan;
            }
        }

        for (open, close) in &syn.block {
            if at(b, i, open) {
                let start = i;
                i += open.len();
                let mut depth = 1usize;
                while i < b.len() && depth > 0 {
                    if at(b, i, close) {
                        depth -= 1;
                        i += close.len();
                    } else if syn.nested_block && at(b, i, open) {
                        depth += 1;
                        i += open.len();
                    } else {
                        i += 1;
                    }
                }
                push(start, i, Kind::Comment);
                continue 'scan;
            }
        }

        for r in &syn.strings {
            if !at(b, i, &r.open) {
                continue;
            }
            if syn.quote_after_eq && matches!(r.open.as_str(), "\"" | "'") {
                let prev = b[..i].iter().rfind(|c| !c.is_ascii_whitespace()).copied();
                if prev != Some(b'=') {
                    continue;
                }
            }
            let start = i;
            i += r.open.len();
            while i < b.len() {
                if r.escape && b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if !r.multiline && b[i] == b'\n' {
                    break;
                }
                if at(b, i, &r.close) {
                    i += r.close.len();
                    break;
                }
                i += 1;
            }
            push(start, i.min(b.len()), Kind::Str);
            continue 'scan;
        }

        if c.is_ascii_digit() {
            let start = i;
            // Suffixes and separators stay inside the literal: 0xFF, 1_000, 3.5,
            // 10ms. A `.` only continues a number if a digit follows, so `x.0.1`
            // and a range `0..n` do not swallow the operator.
            while i < b.len()
                && (is_word(b[i]) || (b[i] == b'.' && matches!(b.get(i + 1), Some(d) if d.is_ascii_digit())))
            {
                i += 1;
            }
            push(start, i, Kind::Number);
            continue;
        }

        if is_word(c) {
            let start = i;
            while i < b.len() && is_word(b[i]) {
                i += 1;
            }
            let word = &src[start..i];
            if syn.keywords.binary_search_by(|k| k.as_str().cmp(word)).is_ok() {
                push(start, i, Kind::Keyword);
            } else if syn.capitalized_types && c.is_ascii_uppercase() {
                let shouty = word.len() > 2 && !word.bytes().any(|b| b.is_ascii_lowercase());
                push(start, i, if shouty { Kind::Constant } else { Kind::Type });
            } else if syn.call_heuristic {
                let next = b[i..].iter().find(|c| !c.is_ascii_whitespace()).copied();
                let prev = b[..start].iter().rfind(|c| !c.is_ascii_whitespace()).copied();
                if matches!(next, Some(b'(') | Some(b'!')) {
                    push(start, i, Kind::Func);
                } else if prev == Some(b'.') && !src[..start].trim_end().ends_with("..") {
                    push(start, i, Kind::Property);
                }
            }
            continue;
        }

        // A byte that could have opened something but did not: an apostrophe in
        // HTML prose, a `#` inside `${x#y}`, the `/` that closed a comment.
        // Stepping over it here is what keeps the scan finite.
        i += 1;
        while i < b.len() && (b[i] & 0xC0) == 0x80 {
            i += 1;
        }
    }
}

/// Scans `lines` as one text and returns tokens per line, with ranges relative
/// to their own line.
///
/// Joining first is the whole point: a doc comment or a multi-line string is
/// several rows in a diff, and lexing each row alone would lose it. Pass one
/// side of a hunk — old lines or new lines, never both — because interleaving
/// them produces text that was never valid in any language.
pub fn lex_lines(lines: &[&str], syn: &Syntax) -> Vec<Vec<Token>> {
    let mut joined = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    let mut starts = Vec::with_capacity(lines.len());
    for l in lines {
        starts.push(joined.len());
        joined.push_str(l);
        joined.push('\n');
    }

    let mut flat = Vec::new();
    lex(&joined, syn, &mut flat);

    let mut out = vec![Vec::new(); lines.len()];
    let mut row = 0;
    for t in flat {
        // Tokens come out in order, so the row only ever moves forward.
        while row + 1 < lines.len() && t.start >= starts[row + 1] {
            row += 1;
        }
        // A block comment or a multi-line string covers several rows; clip it
        // into each one so a row's spans always index its own text.
        let mut r = row;
        loop {
            let base = starts[r];
            let len = lines[r].len();
            let start = t.start.saturating_sub(base).min(len);
            let end = (t.end - base).min(len);
            if start < end {
                out[r].push(Token { start, end, kind: t.kind });
            }
            if r + 1 >= lines.len() || t.end <= starts[r + 1] {
                break;
            }
            r += 1;
        }
    }
    out
}

/// Tokens for every line of a hunk, one side at a time.
///
/// The old and new lines of a hunk are two different texts that happen to be
/// printed interleaved. Lexing them as one would splice a removed line into an
/// added one and produce something that was never valid in any language, so each
/// side is scanned separately and context lines — which belong to both — are
/// scanned twice. That is the only redundancy here and it buys correctness.
///
/// `texts` must already be clipped the way the view will render them, so a token
/// range can never point past what is on screen.
pub fn highlight_hunk(
    hl: &dyn Highlighter,
    path: &str,
    texts: &[&str],
    kinds: &[LineKind],
) -> Vec<Vec<Token>> {
    let mut out = vec![Vec::new(); texts.len()];
    for_each_side(kinds, |rows| {
        let lines: Vec<&str> = rows.iter().map(|&i| texts[i]).collect();
        for (row, tokens) in rows.iter().zip(hl.highlight(path, &lines)) {
            out[*row] = tokens;
        }
    });
    out
}

/// Calls `f` once per side of a hunk with the row indices that side is made of.
///
/// Anything carrying state from one line to the next — a lexer's open string, a
/// Markdown fence, a block classifier — has to run this way rather than over the
/// interleaved rows, or a removed line splices into an added one and the state is
/// nonsense. Context rows belong to both sides and so appear in both calls; the
/// later call wins wherever the caller writes per row, which makes the *added*
/// side authoritative for them.
///
/// One implementation because there is one rule. [`highlight_hunk`] and
/// `markdown::lay_out` are both callers and neither may drift from the other:
/// if the block pass split a hunk differently from the token pass, a fence would
/// open on one and not the other and the two would disagree about the same line.
pub fn for_each_side(kinds: &[LineKind], mut f: impl FnMut(&[usize])) {
    // A hunk with no added lines has nothing for the added pass to see that the
    // removed pass has not already covered, and pure-deletion diffs are common
    // enough to be worth the check: it halves the work on them.
    let has = |k: LineKind| kinds.iter().any(|c| *c == k);
    let sides: &[LineKind] = match (has(LineKind::Removed), has(LineKind::Added)) {
        (true, true) => &[LineKind::Removed, LineKind::Added],
        (true, false) => &[LineKind::Removed],
        // All context selects every line whichever side is asked for.
        _ => &[LineKind::Added],
    };
    let mut rows: Vec<usize> = Vec::with_capacity(kinds.len());
    // Removed first, then added, so a context line ends up carrying the new
    // side's tokens. The text is identical either way; this is just definite.
    for &side in sides {
        rows.clear();
        rows.extend((0..kinds.len()).filter(|&i| kinds[i] == side || kinds[i] == LineKind::Context));
        if !rows.is_empty() {
            f(&rows);
        }
    }
}

// ----------------------------------------------------------------- markdown

/// Markdown, as a second [`Highlighter`] rather than another table.
///
/// The scanner's model does not fit here at all: prose has no keywords, an
/// apostrophe is not a string, and the thing worth colouring is structure —
/// headings, code fences, emphasis, links. Measured against tree-sitter the
/// table-driven attempt mis-coloured a fifth of every file, which is what a
/// wrong model looks like from the outside.
///
/// So it is not a table. It is 100 lines that walk lines instead of bytes,
/// registered in [`Highlighters`] for `md`. Nothing about the scanner changed to
/// make room for it, and nothing about it would have to change if a tree-sitter
/// implementation took the same route later.
pub struct Markdown;

impl Highlighter for Markdown {
    fn highlight(&self, _path: &str, lines: &[&str]) -> Vec<Vec<Token>> {
        let mut out = Vec::with_capacity(lines.len());
        // A fence is the one piece of state that crosses lines. It is also why
        // this cannot be done per row: a diff shows the middle of a code block
        // constantly, and only the hunk knows the block opened.
        let mut fence: Option<&'static str> = None;

        for line in lines {
            let mut toks = Vec::new();
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            let whole = |toks: &mut Vec<Token>, kind| {
                if !line.is_empty() {
                    toks.push(Token { start: 0, end: line.len(), kind });
                }
            };

            match (fence, fence_marker(trimmed)) {
                // Closing fence: the same marker, nothing else required.
                (Some(open), Some(found)) if open == found => {
                    fence = None;
                    whole(&mut toks, Kind::Str);
                }
                // Inside a block, everything is code — including a stray ``` of
                // the other flavour.
                (Some(_), _) => whole(&mut toks, Kind::Str),
                (None, Some(found)) => {
                    fence = Some(found);
                    whole(&mut toks, Kind::Str);
                }
                (None, None) => {
                    let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
                    let heading = (1..=6).contains(&hashes)
                        && matches!(trimmed.as_bytes().get(hashes), None | Some(b' '));
                    if heading || is_setext(trimmed) {
                        whole(&mut toks, Kind::Heading);
                    } else if trimmed.starts_with('>') {
                        whole(&mut toks, Kind::Comment);
                    } else if is_break(trimmed) {
                        whole(&mut toks, Kind::Keyword);
                    } else {
                        // A list marker is not emphasis, and `* item` would
                        // otherwise open one that never closes.
                        let marker = list_marker(trimmed);
                        if marker > 0 {
                            toks.push(Token {
                                start: indent,
                                end: indent + marker,
                                kind: Kind::Keyword,
                            });
                        }
                        inline(line, indent + marker, &mut toks);
                    }
                }
            }
            out.push(toks);
        }
        out
    }
}

pub(crate) fn fence_marker(trimmed: &str) -> Option<&'static str> {
    ["```", "~~~"].into_iter().find(|m| trimmed.starts_with(m))
}

/// `====` or `----` under a line of text. Cheap to spot, and without it every
/// underlined heading in an old README reads as body text.
pub(crate) fn is_setext(trimmed: &str) -> bool {
    let t = trimmed.trim_end();
    t.len() >= 2 && (t.bytes().all(|b| b == b'=') || t.bytes().all(|b| b == b'-'))
}

pub(crate) fn is_break(trimmed: &str) -> bool {
    let t = trimmed.trim_end();
    t.len() >= 3 && (t.bytes().all(|b| b == b'*') || t.bytes().all(|b| b == b'_'))
}

/// Length of a leading `- `, `* `, `+ ` or `12. `, or 0 if there is none.
pub(crate) fn list_marker(trimmed: &str) -> usize {
    let b = trimmed.as_bytes();
    if matches!(b.first(), Some(b'-' | b'*' | b'+')) && b.get(1) == Some(&b' ') {
        return 2;
    }
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && matches!(b.get(digits), Some(b'.' | b')')) && b.get(digits + 1) == Some(&b' ') {
        return digits + 2;
    }
    0
}

/// Code spans, emphasis and links, left to right. Unclosed delimiters are left
/// as text rather than run to the end of the line — in a diff a line often *is*
/// half a construct, and an unmatched `*` is far more likely to be a bullet or a
/// footnote than the start of emphasis.
fn inline(line: &str, from: usize, out: &mut Vec<Token>) {
    let b = line.as_bytes();
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'`' => {
                let run = b[i..].iter().take_while(|c| **c == b'`').count();
                match close_run(b, i + run, b'`', run) {
                    Some(end) => {
                        out.push(Token { start: i, end: end + run, kind: Kind::Str });
                        i = end + run;
                    }
                    None => i += run,
                }
            }
            c @ (b'*' | b'_') => {
                let run = if b.get(i + 1) == Some(&c) { 2 } else { 1 };
                match close_run(b, i + run, c, run) {
                    Some(end) => {
                        let kind = if run == 2 { Kind::Strong } else { Kind::Emphasis };
                        out.push(Token { start: i, end: end + run, kind });
                        i = end + run;
                    }
                    None => i += run,
                }
            }
            b'[' => match link_end(b, i) {
                Some(end) => {
                    out.push(Token { start: i, end, kind: Kind::Link });
                    i = end;
                }
                None => i += 1,
            },
            _ => {
                i += 1;
                while i < b.len() && (b[i] & 0xC0) == 0x80 {
                    i += 1;
                }
            }
        }
    }
}

/// Start of the next run of exactly `len` `delim` bytes at or after `from`.
fn close_run(b: &[u8], from: usize, delim: u8, len: usize) -> Option<usize> {
    let mut i = from;
    while i < b.len() {
        if b[i] != delim {
            i += 1;
            continue;
        }
        let run = b[i..].iter().take_while(|c| **c == delim).count();
        if run >= len {
            return Some(i);
        }
        i += run;
    }
    None
}

/// End of a `[text](url)`, or `None` if this bracket does not open one.
fn link_end(b: &[u8], open: usize) -> Option<usize> {
    let close = b[open..].iter().position(|c| *c == b']')? + open;
    if b.get(close + 1) != Some(&b'(') {
        return None;
    }
    let paren = b[close + 2..].iter().position(|c| *c == b')')? + close + 2;
    Some(paren + 1)
}

// -------------------------------------------------------------- the languages
//
// Data, not code. Every entry here is something an extension could register
// itself, which is the only way to know the seam is real.

fn builtin_languages() -> Languages {
    let mut l = Languages::empty();

    const C_BLOCK: &[(&str, &str)] = &[("/*", "*/")];
    const DQ: (&str, &str, bool, bool) = ("\"", "\"", true, false);
    const SQ: (&str, &str, bool, bool) = ("'", "'", true, false);

    l.register(
        &["rs"],
        Syntax::new()
            .line(&["//"])
            .block(C_BLOCK)
            .nested_block()
            // A raw string can hold a bare `"`, so it has to be tried first.
            .strings(&[("r#\"", "\"#", false, true), ("\"", "\"", true, true)])
            .keywords(&[
                "Self", "as", "async", "await", "break", "const", "continue", "crate", "dyn",
                "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop",
                "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static", "struct",
                "super", "trait", "true", "type", "union", "unsafe", "use", "where", "while",
            ])
            .capitalized_types()
            .call_heuristic(),
    );

    l.register(
        &["go"],
        Syntax::new()
            .line(&["//"])
            .block(C_BLOCK)
            .strings(&[("`", "`", false, true), DQ, SQ])
            .keywords(&[
                "break", "case", "chan", "const", "continue", "default", "defer", "else",
                "fallthrough", "for", "func", "go", "goto", "if", "import", "interface", "map",
                "package", "range", "return", "select", "struct", "switch", "type", "var",
            ])
            .capitalized_types()
            .call_heuristic(),
    );

    l.register(
        &["py", "pyi"],
        Syntax::new()
            .line(&["#"])
            .line_needs_boundary()
            .strings(&[
                ("\"\"\"", "\"\"\"", false, true),
                ("'''", "'''", false, true),
                DQ,
                SQ,
            ])
            .keywords(&[
                "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
                "continue", "def", "del", "elif", "else", "except", "finally", "for", "from",
                "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass",
                "raise", "return", "try", "while", "with", "yield",
            ])
            .capitalized_types()
            .call_heuristic(),
    );

    l.register(
        &["java"],
        Syntax::new()
            .line(&["//"])
            .block(C_BLOCK)
            .strings(&[("\"\"\"", "\"\"\"", true, true), DQ, SQ])
            .keywords(&[
                "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class",
                "const", "continue", "default", "do", "double", "else", "enum", "extends", "final",
                "finally", "float", "for", "if", "implements", "import", "instanceof", "int",
                "interface", "long", "native", "new", "package", "private", "protected", "public",
                "record", "return", "short", "static", "super", "switch", "synchronized", "this",
                "throw", "throws", "transient", "try", "var", "void", "volatile", "while",
            ])
            .capitalized_types()
            .call_heuristic(),
    );

    l.register(
        &["kt", "kts"],
        Syntax::new()
            .line(&["//"])
            .block(C_BLOCK)
            .nested_block()
            .strings(&[("\"\"\"", "\"\"\"", false, true), DQ, SQ])
            .keywords(&[
                "as", "break", "by", "class", "companion", "const", "continue", "data", "do",
                "else", "enum", "false", "for", "fun", "if", "import", "in", "interface",
                "internal", "is", "null", "object", "open", "override", "package", "private",
                "protected", "public", "return", "sealed", "super", "suspend", "this", "throw",
                "true", "try", "typealias", "val", "var", "when", "while",
            ])
            .capitalized_types()
            .call_heuristic(),
    );

    l.register(
        &["swift"],
        Syntax::new()
            .line(&["//"])
            .block(C_BLOCK)
            .nested_block()
            .strings(&[("\"\"\"", "\"\"\"", true, true), DQ])
            .keywords(&[
                "as", "associatedtype", "break", "case", "catch", "class", "continue", "default",
                "defer", "deinit", "do", "else", "enum", "extension", "fallthrough", "false",
                "fileprivate", "for", "func", "guard", "if", "import", "in", "init", "inout",
                "internal", "is", "let", "nil", "open", "operator", "private", "protocol",
                "public", "repeat", "return", "self", "static", "struct", "subscript", "super",
                "switch", "throw", "throws", "true", "try", "typealias", "var", "where", "while",
            ])
            .capitalized_types()
            .call_heuristic(),
    );

    l.register(
        &["ts", "tsx", "js", "jsx", "mjs", "cjs"],
        Syntax::new()
            .line(&["//"])
            .block(C_BLOCK)
            .strings(&[("`", "`", true, true), DQ, SQ])
            .keywords(&[
                "abstract", "any", "as", "async", "await", "boolean", "break", "case", "catch",
                "class", "const", "constructor", "continue", "declare", "default", "delete", "do",
                "else", "enum", "export", "extends", "false", "finally", "for", "from", "function",
                "get", "if", "implements", "import", "in", "instanceof", "interface", "let",
                "namespace", "new", "null", "number", "of", "private", "protected", "public",
                "readonly", "return", "set", "static", "string", "super", "switch", "this",
                "throw", "true", "try", "type", "typeof", "undefined", "var", "void", "while",
                "yield",
            ])
            .capitalized_types()
            .call_heuristic(),
    );

    // No capitalised types: C spells its constants in capitals and would light
    // up like a christmas tree.
    l.register(
        &["c", "h"],
        Syntax::new()
            .line(&["//"])
            .block(C_BLOCK)
            .strings(&[DQ, SQ])
            .keywords(&[
                "auto", "break", "case", "char", "const", "continue", "default", "do", "double",
                "else", "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long",
                "register", "restrict", "return", "short", "signed", "sizeof", "static", "struct",
                "switch", "typedef", "union", "unsigned", "void", "volatile", "while",
            ])
            .call_heuristic(),
    );

    l.register(
        &["cpp", "cc", "cxx", "hpp", "hh"],
        Syntax::new()
            .line(&["//"])
            .block(C_BLOCK)
            .strings(&[DQ, SQ])
            .keywords(&[
                "auto", "bool", "break", "case", "catch", "char", "class", "const", "constexpr",
                "continue", "default", "delete", "do", "double", "else", "enum", "explicit",
                "export", "extern", "false", "float", "for", "friend", "goto", "if", "inline",
                "int", "long", "mutable", "namespace", "new", "noexcept", "nullptr", "operator",
                "private", "protected", "public", "return", "short", "signed", "sizeof", "static",
                "struct", "switch", "template", "this", "throw", "true", "try", "typedef",
                "typename", "union", "unsigned", "using", "virtual", "void", "volatile", "while",
            ])
            .capitalized_types()
            .call_heuristic(),
    );

    l.register(
        &["zig"],
        Syntax::new()
            .line(&["//"])
            .strings(&[DQ, SQ])
            .keywords(&[
                "align", "allowzero", "and", "anyframe", "anytype", "asm", "async", "await",
                "break", "catch", "comptime", "const", "continue", "defer", "else", "enum",
                "errdefer", "error", "export", "extern", "fn", "for", "if", "inline", "noalias",
                "nosuspend", "opaque", "or", "orelse", "packed", "pub", "resume", "return",
                "struct", "suspend", "switch", "test", "threadlocal", "try", "union",
                "unreachable", "usingnamespace", "var", "volatile", "while",
            ])
            .capitalized_types()
            .call_heuristic(),
    );

    l.register(
        &["lua"],
        Syntax::new()
            .line(&["--"])
            .line_needs_boundary()
            .block(&[("--[[", "]]")])
            .strings(&[("[[", "]]", false, true), DQ, SQ])
            .keywords(&[
                "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto",
                "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true",
                "until", "while",
            ])
            .call_heuristic(),
    );

    l.register(
        &["sh", "bash", "zsh", "fish"],
        Syntax::new()
            .line(&["#"])
            .line_needs_boundary()
            .strings(&[("\"", "\"", true, true), ("'", "'", false, true)])
            .keywords(&[
                "case", "do", "done", "elif", "else", "esac", "exit", "export", "fi", "for",
                "function", "if", "in", "local", "readonly", "return", "select", "then", "until",
                "while",
            ]),
    );

    l.register(
        &["yaml", "yml"],
        Syntax::new()
            .line(&["#"])
            .line_needs_boundary()
            .strings(&[DQ, SQ])
            .keywords(&["false", "no", "null", "true", "yes"]),
    );

    l.register(
        &["toml", "Cargo.lock"],
        Syntax::new()
            .line(&["#"])
            .line_needs_boundary()
            .strings(&[
                ("\"\"\"", "\"\"\"", false, true),
                ("'''", "'''", false, true),
                DQ,
                ("'", "'", false, false),
            ])
            .keywords(&["false", "true"]),
    );

    l.register(
        &["json", "jsonc"],
        Syntax::new()
            .line(&["//"])
            .block(C_BLOCK)
            .strings(&[DQ])
            .keywords(&["false", "null", "true"]),
    );

    l.register(
        &["css", "scss", "less"],
        Syntax::new()
            .line(&["//"])
            .block(C_BLOCK)
            .strings(&[DQ, SQ])
            .keywords(&["and", "from", "important", "not", "to"])
            .call_heuristic(),
    );

    l.register(
        &["sql"],
        Syntax::new()
            .line(&["--"])
            .line_needs_boundary()
            .block(C_BLOCK)
            .strings(&[("'", "'", false, false), ("\"", "\"", false, false)])
            .keywords(&[
                "and", "as", "asc", "by", "case", "create", "delete", "desc", "distinct", "drop",
                "else", "end", "from", "group", "having", "in", "inner", "insert", "into", "join",
                "left", "limit", "not", "null", "on", "or", "order", "outer", "select", "set",
                "table", "then", "union", "update", "values", "when", "where",
            ]),
    );

    // Markup gets comments and attribute values only. Tags, entities and
    // anything injected — a `<script>` body, a `<?php` island, a fenced code
    // block — need a parser, and guessing produced the worst mis-colouring of
    // any language measured. Better dim than wrong.
    l.register(
        &["html", "htm", "xml", "svg", "vue", "svelte"],
        Syntax::new()
            .block(&[("<!--", "-->")])
            .strings(&[DQ, SQ])
            .quote_after_eq(),
    );

    l
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(path: &str, src: &str) -> Vec<(Kind, String)> {
        let lexer = Lexer::builtin();
        let syn = lexer.languages.for_path(path).expect("no table for path");
        let mut out = Vec::new();
        lex(src, syn, &mut out);
        out.iter().map(|t| (t.kind, src[t.range()].to_string())).collect()
    }

    #[test]
    fn finds_the_obvious_things() {
        let got = tokens("a.rs", "let n = 42; // why");
        assert_eq!(
            got,
            vec![
                (Kind::Keyword, "let".into()),
                (Kind::Number, "42".into()),
                (Kind::Comment, "// why".into()),
            ]
        );
    }

    #[test]
    fn ranges_never_overlap_and_stay_in_order() {
        let src = "/* c */ \"s\" 1 if Foo bar() x.y";
        let lexer = Lexer::builtin();
        let syn = lexer.languages.for_path("a.rs").unwrap();
        let mut out = Vec::new();
        lex(src, syn, &mut out);
        assert!(out.windows(2).all(|w| w[0].end <= w[1].start), "{out:?}");
        assert!(out.iter().all(|t| t.start < t.end && t.end <= src.len()));
    }

    #[test]
    fn a_stray_quote_stops_at_the_end_of_its_line() {
        // A hunk boundary cuts literals in half constantly. Bleeding past the
        // line is the failure that makes a diff unreadable, so a language whose
        // strings are single-line must recover on the next row.
        let got = tokens("a.c", "printf(\"unterminated\nint x = 1;");
        assert_eq!(got[1], (Kind::Str, "\"unterminated".into()));
        assert!(got.iter().any(|(k, s)| *k == Kind::Keyword && s == "int"));
    }

    #[test]
    fn an_unterminated_multiline_string_cannot_escape_its_hunk() {
        // Rust strings legitimately span lines, so an opening quote cut off by a
        // hunk boundary does run on — but only to the end of the text it was
        // given, which is one side of one hunk. The next hunk starts clean.
        let lexer = Lexer::builtin();
        let got = lexer.highlight("a.rs", &["let s = \"open", "still inside", "\";"]);
        assert_eq!(got[1], vec![Token { start: 0, end: 12, kind: Kind::Str }]);
        let next = lexer.highlight("a.rs", &["fn clean() {}"]);
        assert!(next[0].iter().all(|t| t.kind != Kind::Str));
    }

    #[test]
    fn rust_raw_strings_may_contain_quotes() {
        let got = tokens("a.rs", "let s = r#\"say \"hi\"\"#;");
        assert!(got.contains(&(Kind::Str, "r#\"say \"hi\"\"#".into())), "{got:?}");
    }

    #[test]
    fn block_comments_nest_in_rust_but_not_in_c() {
        let rust = tokens("a.rs", "/* a /* b */ c */ let");
        assert_eq!(rust[0], (Kind::Comment, "/* a /* b */ c */".into()));
        let c = tokens("a.c", "/* a /* b */ c */ int");
        assert_eq!(c[0], (Kind::Comment, "/* a /* b */".into()));
    }

    #[test]
    fn shell_hash_needs_a_word_boundary() {
        // `$#` and `${x#y}` are not comments, and treating them as ones paints
        // the rest of the line in nearly every real script.
        let got = tokens("x.sh", "echo $# ${x#y} # real");
        assert_eq!(got.iter().filter(|(k, _)| *k == Kind::Comment).count(), 1);
        assert_eq!(got.last().unwrap(), &(Kind::Comment, "# real".into()));
    }

    #[test]
    fn markup_quotes_only_count_inside_a_tag() {
        let got = tokens("i.html", "<p class=\"x\">don't</p>");
        assert_eq!(got, vec![(Kind::Str, "\"x\"".into())]);
    }

    #[test]
    fn numbers_do_not_swallow_operators() {
        let got = tokens("a.rs", "for i in 0..10 { x = 1.5 }");
        let nums: Vec<_> = got.iter().filter(|(k, _)| *k == Kind::Number).map(|(_, s)| s.as_str()).collect();
        assert_eq!(nums, vec!["0", "10", "1.5"]);
    }

    #[test]
    fn calls_and_fields_come_from_the_two_heuristics() {
        let got = tokens("a.rs", "self.pool.submit(ev)");
        assert!(got.contains(&(Kind::Property, "pool".into())), "{got:?}");
        assert!(got.contains(&(Kind::Func, "submit".into())), "{got:?}");
    }

    #[test]
    fn a_doc_comment_spanning_rows_colours_every_row() {
        let lexer = Lexer::builtin();
        let lines = ["/* one", " * two", " */ let x = 1;"];
        let got = lexer.highlight("a.rs", &lines);
        assert_eq!(got[0], vec![Token { start: 0, end: 6, kind: Kind::Comment }]);
        assert_eq!(got[1], vec![Token { start: 0, end: 6, kind: Kind::Comment }]);
        assert_eq!(got[2][0], Token { start: 0, end: 3, kind: Kind::Comment });
        assert!(got[2].iter().any(|t| t.kind == Kind::Keyword));
    }

    #[test]
    fn per_line_ranges_index_their_own_line() {
        let lexer = Lexer::builtin();
        let lines = ["fn a() {}", "let s = \"hello\";"];
        let got = lexer.highlight("a.rs", &lines);
        for (line, toks) in lines.iter().zip(&got) {
            for t in toks {
                assert!(t.end <= line.len(), "{t:?} outside {line:?}");
            }
        }
        assert_eq!(&lines[1][got[1].last().unwrap().range()], "\"hello\"");
    }

    #[test]
    fn an_unknown_language_gets_no_tokens_rather_than_wrong_ones() {
        let lexer = Lexer::builtin();
        assert!(lexer.languages.for_path("a.wat").is_none());
        assert_eq!(lexer.highlight("a.wat", &["(module)"]), vec![Vec::new()]);
        assert_eq!(lexer.highlight("Makefile", &["all:"]), vec![Vec::new()]);
    }

    #[test]
    fn a_registered_table_replaces_a_built_in_one() {
        // The extension seam: if this test can do it, so can an extension.
        let mut lexer = Lexer::builtin();
        lexer.languages.register(
            &["rs"],
            Syntax::new().line(&[";;"]).keywords(&["fn"]),
        );
        let got = lexer.highlight("a.rs", &["fn x() {} ;; note"]);
        assert_eq!(got[0].first().unwrap().kind, Kind::Keyword);
        assert_eq!(got[0].last().unwrap().kind, Kind::Comment);
    }

    #[test]
    fn each_side_of_a_hunk_is_scanned_on_its_own() {
        // The removed line opens a string that the added line closes. Lexed
        // together they would cancel out; lexed apart each one runs to its own
        // end of line, which is what the reader sees.
        let lexer = Lexer::builtin();
        let texts = ["let a = 1;", "let s = \"x;", "let s = \"y\";", "done();"];
        let kinds = [LineKind::Context, LineKind::Removed, LineKind::Added, LineKind::Context];
        let got = highlight_hunk(&lexer, "a.rs", &texts, &kinds);
        assert_eq!(got.len(), 4);
        assert_eq!(got[1].last().unwrap().kind, Kind::Str);
        assert_eq!(&texts[2][got[2].last().unwrap().range()], "\"y\"");
        // The trailing context line is still code on both sides.
        assert_eq!(got[3].first().unwrap().kind, Kind::Func);
    }

    #[test]
    fn shouty_names_are_constants_and_short_ones_are_types() {
        let got = tokens("a.rs", "const MAX: usize = 1; let x: Vec<T> = IO::new();");
        assert!(got.contains(&(Kind::Constant, "MAX".into())), "{got:?}");
        assert!(got.contains(&(Kind::Type, "Vec".into())), "{got:?}");
        assert!(got.contains(&(Kind::Type, "T".into())), "{got:?}");
        assert!(got.contains(&(Kind::Type, "IO".into())), "{got:?}");
    }

    #[test]
    fn a_name_with_no_useful_extension_can_still_have_a_table() {
        let lexer = Lexer::builtin();
        assert!(lexer.languages.for_path("Cargo.lock").is_some());
        assert!(lexer.languages.for_path("deps/Cargo.lock").is_some());
        assert!(lexer.languages.for_path("Gemfile.lock").is_none());
    }

    #[test]
    fn token_edges_always_land_on_char_boundaries() {
        // The renderer indexes the line by these numbers. Landing mid-character
        // is a panic in a debug build and mojibake in a release one, and escape
        // handling is the place it would happen.
        let lexer = Lexer::builtin();
        let syn = lexer.languages.for_path("a.rs").unwrap();
        for src in [
            "let s = \"caf\u{e9} \u{1f600}\";",
            "// \u{e9}migr\u{e9}",
            "let s = \"\\\u{e9}",
            "let \u{e9}t\u{e9} = 1;",
            "/* \u{1f600} */ x",
        ] {
            let mut out = Vec::new();
            lex(src, syn, &mut out);
            for t in &out {
                assert!(src.is_char_boundary(t.start), "{t:?} in {src:?}");
                assert!(src.is_char_boundary(t.end), "{t:?} in {src:?}");
            }
        }
    }

    // ---------------------------------------------------------- routing

    /// The smallest possible foreign highlighter: what an extension ships.
    struct EverythingIsAComment;
    impl Highlighter for EverythingIsAComment {
        fn highlight(&self, _path: &str, lines: &[&str]) -> Vec<Vec<Token>> {
            lines
                .iter()
                .map(|l| vec![Token { start: 0, end: l.len(), kind: Kind::Comment }])
                .collect()
        }
    }

    #[test]
    fn a_route_takes_precedence_over_the_fallback() {
        let mut hl = Highlighters::builtin();
        hl.route(&["rs"], EverythingIsAComment);
        assert_eq!(hl.highlight("a.rs", &["let x = 1;"])[0].len(), 1);
        assert_eq!(hl.highlight("a.rs", &["let x = 1;"])[0][0].kind, Kind::Comment);
        // Everything else still goes to the scanner.
        assert!(hl.highlight("a.go", &["func main() {}"])[0].len() > 1);
    }

    #[test]
    fn the_last_route_registered_wins() {
        let mut hl = Highlighters::builtin();
        hl.route(&["rs"], EverythingIsAComment);
        hl.route(&["rs"], Markdown);
        let got = hl.highlight("a.rs", &["# not rust at all"]);
        assert_eq!(got[0][0].kind, Kind::Heading);
    }

    #[test]
    fn routes_match_whole_filenames_too() {
        let mut hl = Highlighters::builtin();
        hl.route(&["Makefile"], EverythingIsAComment);
        assert_eq!(hl.highlight("deps/Makefile", &["all:"])[0][0].kind, Kind::Comment);
        assert!(hl.highlight("makefile.rs", &["let x = 1;"])[0][0].kind != Kind::Comment);
    }

    #[test]
    fn a_language_can_be_added_without_rebuilding_the_fallback() {
        // The most common extension there is, and it should not cost more than
        // this: one call, no reconstruction of the scanner.
        let mut hl = Highlighters::builtin();
        hl.languages()
            .expect("the scanner is still the fallback")
            .register(&["nim"], Syntax::new().line(&["#"]).keywords(&["proc"]));
        let got = hl.highlight("x.nim", &["proc main() # note"]);
        assert_eq!(got[0][0].kind, Kind::Keyword);
        assert_eq!(got[0][1].kind, Kind::Comment);
    }

    #[test]
    fn there_are_no_tables_to_register_once_the_scanner_is_gone() {
        let mut hl = Highlighters::with_fallback(EverythingIsAComment);
        assert!(hl.languages().is_none());
    }

    #[test]
    fn the_fallback_itself_is_replaceable() {
        // An extension that wants to own every language it was not asked about.
        let mut hl = Highlighters::with_fallback(EverythingIsAComment);
        hl.route(&["md"], Markdown);
        assert_eq!(hl.highlight("whatever.xyz", &["hello"])[0][0].kind, Kind::Comment);
        assert_eq!(hl.highlight("r.md", &["# h"])[0][0].kind, Kind::Heading);
    }

    #[test]
    fn markdown_is_routed_away_from_the_scanner_by_default() {
        let hl = Highlighters::builtin();
        let got = hl.highlight("README.md", &["# Title"]);
        assert_eq!(got[0], vec![Token { start: 0, end: 7, kind: Kind::Heading }]);
    }

    // --------------------------------------------------------- markdown

    fn md(lines: &[&str]) -> Vec<Vec<(Kind, String)>> {
        Markdown
            .highlight("r.md", lines)
            .iter()
            .zip(lines)
            .map(|(toks, line)| {
                toks.iter().map(|t| (t.kind, line[t.range()].to_string())).collect()
            })
            .collect()
    }

    #[test]
    fn markdown_colours_structure_not_keywords() {
        let got = md(&[
            "## Building",
            "",
            "Run `check.sh` before **every** commit, see [docs](x.md).",
            "- a bullet",
        ]);
        assert_eq!(got[0], vec![(Kind::Heading, "## Building".into())]);
        assert!(got[1].is_empty());
        assert_eq!(
            got[2],
            vec![
                (Kind::Str, "`check.sh`".into()),
                (Kind::Strong, "**every**".into()),
                (Kind::Link, "[docs](x.md)".into()),
            ]
        );
        assert_eq!(got[3], vec![(Kind::Keyword, "- ".into())]);
    }

    #[test]
    fn a_fenced_block_survives_starting_mid_hunk() {
        // The common diff shape: the fence opened in a line the hunk shows, and
        // the language inside it is not markdown at all.
        let got = md(&["```rust", "let x = **not emphasis**;", "```", "*after*"]);
        assert_eq!(got[0][0].0, Kind::Str);
        assert_eq!(got[1], vec![(Kind::Str, "let x = **not emphasis**;".into())]);
        assert_eq!(got[2][0].0, Kind::Str);
        assert_eq!(got[3], vec![(Kind::Emphasis, "*after*".into())]);
    }

    #[test]
    fn an_unclosed_delimiter_is_left_as_text() {
        // A hunk boundary cuts constructs in half; guessing is worse than not.
        let got = md(&["a * b * c", "an unclosed `code", "2 * 3 = 6"]);
        assert_eq!(got[0], vec![(Kind::Emphasis, "* b *".into())]);
        assert!(got[1].is_empty(), "{:?}", got[1]);
        assert!(got[2].is_empty(), "{:?}", got[2]);
    }

    #[test]
    fn markdown_tokens_are_ordered_and_disjoint() {
        let lines = [
            "# `code` in a heading",
            "> quoted **bold** text",
            "1. numbered *item* with `code` and [link](url)",
            "---",
            "Setext",
            "======",
        ];
        for (toks, line) in Markdown.highlight("r.md", &lines).iter().zip(lines) {
            assert!(toks.windows(2).all(|w| w[0].end <= w[1].start), "{toks:?}");
            for t in toks {
                assert!(t.start < t.end && t.end <= line.len(), "{t:?} in {line:?}");
                assert!(line.is_char_boundary(t.start) && line.is_char_boundary(t.end));
            }
        }
    }

    #[test]
    fn markdown_handles_multi_byte_prose() {
        let got = md(&["caf\u{e9} **cr\u{e8}me** \u{1f600} `caf\u{e9}`"]);
        assert_eq!(got[0][0], (Kind::Strong, "**cr\u{e8}me**".into()));
        assert_eq!(got[0][1], (Kind::Str, "`caf\u{e9}`".into()));
    }

    #[test]
    fn tables_cover_the_extensions_we_claim() {
        let lexer = Lexer::builtin();
        for ext in ["rs", "go", "py", "java", "kt", "swift", "ts", "tsx", "js", "c", "h", "cpp",
                    "zig", "lua", "sh", "yaml", "yml", "toml", "json", "css", "sql", "html"] {
            assert!(lexer.languages.for_path(&format!("f.{ext}")).is_some(), "{ext}");
        }
    }
}
