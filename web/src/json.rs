//! Just enough JSON to write, never to read.
//!
//! Serde would be the obvious reach and it buys nothing here: every type that
//! crosses this wire is one we own, the shapes are flat, and `core` cannot take
//! a derive anyway — it has no dependencies and that is the rule. So the
//! encoders live beside the routes, hand-written, and the one thing worth
//! getting right is escaping.

use plait_core::theme::Rgb;

/// Appends a JSON string literal, quotes included.
///
/// Rust strings are UTF-8 and JSON is defined over UTF-8, so anything printable
/// passes through as itself — no `\u` escaping of non-ASCII, which would triple
/// the size of a diff of anything but English. What must be escaped is the two
/// structural characters and C0: a raw control byte is not legal JSON, and diffs
/// carry them. A tab in a source line is the common case.
pub fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // U+2028/9 are legal JSON and illegal in a JavaScript string
            // literal. Nothing here is `eval`ed today, but a diff containing one
            // is not the place to find out that something downstream is.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A `"key":` prefix, comma-separated from whatever came before it.
///
/// `first` is a `&mut bool` the caller flips once per object rather than a
/// trailing-comma trim, because a trim has to know where the object started and
/// this does not.
pub fn key(out: &mut String, first: &mut bool, k: &str) {
    if !*first {
        out.push(',');
    }
    *first = false;
    string(out, k);
    out.push(':');
}

pub fn field_str(out: &mut String, first: &mut bool, k: &str, v: &str) {
    key(out, first, k);
    string(out, v);
}

pub fn field_num(out: &mut String, first: &mut bool, k: &str, v: impl std::fmt::Display) {
    key(out, first, k);
    out.push_str(&v.to_string());
}

pub fn field_bool(out: &mut String, first: &mut bool, k: &str, v: bool) {
    key(out, first, k);
    out.push_str(if v { "true" } else { "false" });
}

/// A colour, as the `#rrggbb` a stylesheet wants.
///
/// Hex and not the `u32` [`Rgb`] actually is: the client puts these straight
/// into `style.color`, and converting 84 resolved styles per theme load in
/// JavaScript is work for no reason.
pub fn field_rgb(out: &mut String, first: &mut bool, k: &str, v: Rgb) {
    field_str(out, first, k, &format!("#{v:06x}"));
}

pub fn rgb_list(out: &mut String, cs: &[Rgb]) {
    list(out, cs, |o, c| string(o, &format!("#{c:06x}")));
}

/// `[a,b,c]` from anything that can write itself into the buffer.
pub fn list<T>(
    out: &mut String,
    items: impl IntoIterator<Item = T>,
    mut f: impl FnMut(&mut String, T),
) {
    out.push('[');
    let mut first = true;
    for it in items {
        if !first {
            out.push(',');
        }
        first = false;
        f(out, it);
    }
    out.push(']');
}

/// `{...}` from a closure that writes the fields.
pub fn object(out: &mut String, f: impl FnOnce(&mut String, &mut bool)) {
    out.push('{');
    let mut first = true;
    f(out, &mut first);
    out.push('}');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_and_a_quote_survive_a_line_shaped_like_a_diff_line() {
        let mut out = String::new();
        string(&mut out, "\tlet s = \"a\\b\";");
        assert_eq!(out, r#""\tlet s = \"a\\b\";""#);
    }

    #[test]
    fn a_control_byte_becomes_an_escape_and_not_a_raw_byte() {
        let mut out = String::new();
        string(&mut out, "a\u{1}b\u{7}c");
        assert_eq!(out, "\"a\\u0001b\\u0007c\"");
    }

    #[test]
    fn non_ascii_passes_through_unescaped() {
        let mut out = String::new();
        string(&mut out, "café 日本語");
        assert_eq!(out, "\"café 日本語\"");
    }

    #[test]
    fn the_line_separators_javascript_cannot_hold_are_escaped() {
        let mut out = String::new();
        string(&mut out, "a\u{2028}b\u{2029}c");
        assert_eq!(out, "\"a\\u2028b\\u2029c\"");
    }

    #[test]
    fn a_colour_is_six_digits_even_when_it_is_mostly_zeroes() {
        let mut out = String::new();
        let mut first = true;
        field_rgb(&mut out, &mut first, "bg", 0x00ff08);
        assert_eq!(out, r#""bg":"#.to_string() + "\"#00ff08\"");
    }
}
