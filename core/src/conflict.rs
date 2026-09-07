//! A conflicted file, as its own markers spell it.
//!
//! A merge git could not finish leaves the working-tree file carrying the
//! three sides as text: `<<<<<<<` ours, an optional `|||||||` base (diff3
//! style), `=======`, theirs, `>>>>>>>`. Those bytes *are* the conflict —
//! every reader, git's own `checkout --conflict` included, agrees on them —
//! so a presentation built from them shows what a resolution is choosing
//! between, and one built from anything else would show something else.
//!
//! # The marker rule
//!
//! A marker line is a run of one marker character (`<`, `|`, `=`, `>`) of at
//! least [`MARKER`] characters, followed by end-of-line or a space and a
//! label. The separator is held stricter — a whole line of exactly
//! [`MARKER`] `=` characters — because `=======` is also the shape of a
//! Markdown rule, and a rule inside a conflicted region's text must never
//! end the ours half.
//!
//! Nesting (a merge inside a rebase inside a merge) is paired by run
//! *length*, the way git pairs it: an opener pushes its run length, and a
//! separator, base marker or closer matches the innermost open region by
//! sharing that length. A close with no open region, or a run that matches
//! nothing open, is just text.
//!
//! Everything here is bytes: lines are split inclusively so a no-final-newline
//! file survives byte-exactly, and no stage of parsing or recombining demands
//! UTF-8. The answer [`apply`] writes back is the original bytes with chosen
//! regions replaced — an unresolved region keeps its markers, which is what
//! keeps the file unresolved in git's own eyes.

use crate::status::PathBytes;

/// The marker width git writes: `<<<<<<<`, seven characters, plus an
/// optional space and label.
pub const MARKER: usize = 7;

/// One conflict region, addressed by line indices into the file's lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// The `<<<<<<<` line.
    pub start: usize,
    /// The `=======` line separating ours from theirs.
    pub sep: usize,
    /// The `|||||||` line, when diff3 style wrote the base in.
    pub base: Option<usize>,
    /// The `>>>>>>>` line, inclusive.
    pub end: usize,
}

impl Region {
    /// The ours lines: everything between the opener and the first of the
    /// base marker and the separator.
    pub fn ours(&self) -> std::ops::Range<usize> {
        self.start + 1..self.base.unwrap_or(self.sep)
    }

    /// The base lines, when there are any: between `|||||||` and `=======`.
    pub fn base_lines(&self) -> Option<std::ops::Range<usize>> {
        Some(self.base? + 1..self.sep)
    }

    /// The theirs lines: between the separator and the closer.
    pub fn theirs(&self) -> std::ops::Range<usize> {
        self.sep + 1..self.end
    }
}

/// What one region's resolution keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// The ours half.
    Ours,
    /// The theirs half.
    Theirs,
    /// Ours followed by theirs, the same order and boundary
    /// [`crate::operation::Side::Both`] spells at file level.
    Both,
}

/// One conflicted file: the bytes on disk, and where its regions are.
///
/// The snapshot a merging view opens on. The verbs re-read the file at
/// write time and re-parse it; the region *count* is the freshness check —
/// a file whose regions moved under the keyboard refuses rather than
/// applying a choice to the wrong hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictFile {
    /// The path, raw.
    pub path: PathBytes,
    /// The working-tree bytes the regions were parsed from.
    pub bytes: Vec<u8>,
    /// The conflict regions, in file order.
    pub regions: Vec<Region>,
}

impl ConflictFile {
    /// Reads the working-tree bytes of `path` and parses the conflict
    /// regions out of them. A file with no regions is *not* an error here —
    /// the caller decides what an unmarked file means (resolved by hand,
    /// binary, delete/modify), because only it knows what the stages say.
    pub fn parse(path: PathBytes, bytes: Vec<u8>) -> Self {
        let regions = parse(&bytes);
        Self {
            path,
            bytes,
            regions,
        }
    }

    /// Whether the file carries any conflict regions at all.
    pub fn is_conflicted(&self) -> bool {
        !self.regions.is_empty()
    }
}

/// Is this line a marker run of `c`, at least `run` wide, ending the line or
/// introducing a label?
fn is_marker(line: &[u8], c: u8, run: usize) -> bool {
    line.len() >= run
        && line[..run].iter().all(|&b| b == c)
        && line[run..].first().is_none_or(|&b| b == b' ')
}

/// The exact run length of `c` this line opens with.
fn run_of(line: &[u8], c: u8) -> usize {
    line.iter().take_while(|&&b| b == c).count()
}

/// Parses the conflict regions out of a file's bytes.
///
/// See the [marker rule](self) for what counts. Lines are split inclusively,
/// so the last line's missing newline is not anybody's invention.
pub fn parse(bytes: &[u8]) -> Vec<Region> {
    struct Open {
        /// The opener's run length, which its separator and closer share.
        level: usize,
        start: usize,
        base: Option<usize>,
        sep: Option<usize>,
    }
    let lines: Vec<&[u8]> = split_lines(bytes);
    let mut regions = Vec::new();
    // The open regions, innermost last. Only the innermost region can take
    // a separator, a base marker or a close, and each must share that
    // region's opener run length; anything else is the file's own text.
    let mut stack: Vec<Open> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        // Trim the line's trailing newline for the shape tests below.
        let body = strip_eol(line);
        let inner = stack.last_mut();
        match inner {
            None => {
                if is_marker(body, b'<', MARKER) {
                    stack.push(Open {
                        level: run_of(body, b'<'),
                        start: i,
                        base: None,
                        sep: None,
                    });
                }
            }
            Some(top) => {
                if is_marker(body, b'<', MARKER) {
                    // A region inside a region: git grows the outer
                    // markers when it nests, and pairing by run length
                    // tells the inner close from the outer one either way.
                    stack.push(Open {
                        level: run_of(body, b'<'),
                        start: i,
                        base: None,
                        sep: None,
                    });
                } else if top.sep.is_none()
                    && top.base.is_none()
                    && is_marker(body, b'|', top.level)
                {
                    top.base = Some(i);
                } else if top.sep.is_none()
                    && body.len() == top.level
                    && body.iter().all(|&b| b == b'=')
                {
                    top.sep = Some(i);
                } else if run_of(body, b'>') == top.level
                    && body[top.level.min(body.len())..]
                        .first()
                        .is_none_or(|&b| b == b' ')
                {
                    let top = stack.pop().unwrap();
                    // A region with no separator never became one: git
                    // always writes it, so its absence is a mangled file,
                    // and a mangled region is text, not a choice.
                    if let Some(sep) = top.sep {
                        regions.push(Region {
                            start: top.start,
                            sep,
                            base: top.base,
                            end: i,
                        });
                    }
                }
            }
        }
    }
    regions
}

/// Splits into lines, keeping each line's own newline.
fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        match rest.iter().position(|&b| b == b'\n') {
            Some(i) => {
                let (line, tail) = rest.split_at(i + 1);
                lines.push(line);
                rest = tail;
            }
            None => {
                lines.push(rest);
                break;
            }
        }
    }
    lines
}

fn strip_eol(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

/// The number of lines `bytes` holds — what a flattening walk and a
/// region's line indices are counted against.
pub fn line_count(bytes: &[u8]) -> usize {
    split_lines(bytes).len()
}

/// Applies answers to regions, returning the combined file.
///
/// `choices` names region indices in the *current* parse — the caller
/// re-parses the bytes it read and validates the indices against that parse
/// before calling, so a stale index is refused before this is ever reached.
/// A region with no choice keeps its markers: an unresolved region is what
/// keeps the file unresolved.
pub fn apply(
    bytes: &[u8],
    regions: &[Region],
    choices: &[(usize, Answer)],
) -> Result<Vec<u8>, String> {
    let lines = split_lines(bytes);
    for (index, _) in choices {
        if *index >= regions.len() {
            return Err(format!(
                "conflict {index} is not in this file — it has {} region{}",
                regions.len(),
                if regions.len() == 1 { "" } else { "s" }
            ));
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut chosen: Vec<Option<Answer>> = vec![None; regions.len()];
    for (index, answer) in choices {
        chosen[*index] = Some(*answer);
    }
    let mut at = 0;
    for (ri, region) in regions.iter().enumerate() {
        // Everything before the region, byte-exactly.
        for line in &lines[at..region.start] {
            out.extend_from_slice(line);
        }
        match chosen[ri] {
            None => {
                for line in &lines[region.start..=region.end] {
                    out.extend_from_slice(line);
                }
            }
            Some(answer) => {
                let (ours, theirs) = (region.ours(), region.theirs());
                let emit = |out: &mut Vec<u8>, range: std::ops::Range<usize>| {
                    for line in &lines[range] {
                        out.extend_from_slice(line);
                    }
                };
                if matches!(answer, Answer::Ours | Answer::Both) {
                    emit(&mut out, ours);
                }
                if matches!(answer, Answer::Theirs | Answer::Both) {
                    emit(&mut out, theirs);
                }
            }
        }
        at = region.end + 1;
    }
    for line in &lines[at..] {
        out.extend_from_slice(line);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> Vec<u8> {
        text.as_bytes().to_vec()
    }

    const TWO_REGIONS: &str = "\
top
<<<<<<< ours
ours 1
=======
theirs 1
>>>>>>> other
middle
<<<<<<< ours
ours 2
=======
theirs 2
>>>>>>> other
tail
";

    #[test]
    fn regions_land_on_their_markers() {
        let bytes = file(TWO_REGIONS);
        let regions = parse(&bytes);
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].start, 1);
        assert_eq!(regions[0].sep, 3);
        assert_eq!(regions[0].end, 5);
        assert_eq!(regions[0].base, None);
        assert_eq!(regions[1].start, 7);
        // No base anywhere: every `base_lines` answer is None.
        assert!(regions.iter().all(|r| r.base.is_none()));
    }

    #[test]
    fn a_markdown_rule_is_not_a_separator() {
        // `=======` as a setext underline or rule inside the ours text must
        // not end the ours half — the separator is matched only at the
        // region's own level, and an 8-wide run is not one.
        let bytes = file(
            "\
<<<<<<< ours
heading
========
still ours
=======
theirs
>>>>>>> other
",
        );
        let regions = parse(&bytes);
        assert_eq!(regions.len(), 1);
        let ours: Vec<_> = regions[0].ours().collect();
        assert_eq!(ours.len(), 3, "the rule stayed ours text");
    }

    #[test]
    fn a_stray_marker_outside_a_region_is_text() {
        // A `>>>>>>>` with no open region, or an `=======` line alone, is
        // the file's content — quoting a conflict in a README, say.
        let bytes = file(
            "\
quoted:
>>>>>>> not a conflict
=======
still not
",
        );
        assert!(parse(&bytes).is_empty());
    }

    #[test]
    fn diff3_style_carries_the_base() {
        let bytes = file(
            "\
<<<<<<< ours
ours
||||||| base label
base
=======
theirs
>>>>>>> other
",
        );
        let regions = parse(&bytes);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].base, Some(2));
        assert_eq!(
            regions[0].base_lines().unwrap().collect::<Vec<_>>(),
            vec![3]
        );
        assert_eq!(regions[0].ours().count(), 1);
        assert_eq!(regions[0].theirs().count(), 1);
    }

    #[test]
    fn answers_combine_byte_exactly() {
        let bytes = file(TWO_REGIONS);
        let regions = parse(&bytes);
        let out = apply(&bytes, &regions, &[(0, Answer::Ours), (1, Answer::Theirs)]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "top\nours 1\nmiddle\ntheirs 2\ntail\n"
        );
    }

    #[test]
    fn both_is_ours_then_theirs_and_unresolved_keeps_markers() {
        let bytes = file(TWO_REGIONS);
        let regions = parse(&bytes);
        let out = apply(&bytes, &regions, &[(0, Answer::Both)]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "top\nours 1\ntheirs 1\nmiddle\n<<<<<<< ours\nours 2\n=======\ntheirs 2\n>>>>>>> other\ntail\n"
        );
    }

    #[test]
    fn an_index_out_of_range_is_refused_not_panicked() {
        let bytes = file(TWO_REGIONS);
        let regions = parse(&bytes);
        assert!(apply(&bytes, &regions, &[(2, Answer::Ours)]).is_err());
        assert!(apply(&bytes, &regions, &[(99, Answer::Ours)]).is_err());
    }

    #[test]
    fn a_missing_final_newline_survives_the_round_trip() {
        // The last line holds no newline; emitting it unchanged must not
        // invent one, and a resolution that ends on it must not either.
        let bytes = file("<<<<<<< ours\nours\n=======\ntheirs\n>>>>>>> other\nend");
        let regions = parse(&bytes);
        assert_eq!(regions.len(), 1);
        let out = apply(&bytes, &regions, &[(0, Answer::Theirs)]).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "theirs\nend");
    }

    #[test]
    fn nested_regions_pair_by_run_length() {
        // git grows an outer region's markers when one conflict nests
        // inside another; pairing on the run length is what tells the inner
        // region's closer from the outer's.
        let bytes = file(
            "\
<<<<<<< outer
outer ours
<<<<<<< inner
inner ours
=======
inner theirs
>>>>>>> inner
outer theirs
=======
outer theirs side
>>>>>>> outer
",
        );
        let regions = parse(&bytes);
        assert_eq!(regions.len(), 1, "the inner markers are outer text");
        assert_eq!(regions[0].start, 0);
        assert_eq!(regions[0].end, 9);
    }

    #[test]
    fn non_utf8_content_survives_byte_exactly() {
        let mut bytes = file("<<<<<<< ours\n");
        bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);
        bytes.extend_from_slice(file("=======\ntheirs\n>>>>>>> other\n").as_slice());
        let regions = parse(&bytes);
        assert_eq!(regions.len(), 1);
        let out = apply(&bytes, &regions, &[(0, Answer::Ours)]).unwrap();
        assert_eq!(&out[13..16], &[0xff, 0xfe, b'\n']);
    }
}
