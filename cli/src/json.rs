//! Minimal JSON output: escaping and small object builders.
//!
//! A `String`, the way gitten-web's writer is — this crate adds no dependencies,
//! and neither `inspect` nor `dispatch` needs more than flat objects and arrays.

/// Escapes a string for JSON double quotes.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// `"key": "value"` pair.
pub fn str_field(key: &str, value: &str) -> String {
    format!("\"{}\": \"{}\"", key, esc(value))
}

/// `"key": 123` pair.
pub fn num_field(key: &str, value: usize) -> String {
    format!("\"{key}\": {value}")
}

/// `"key": true` pair.
pub fn bool_field(key: &str, value: bool) -> String {
    format!("\"{key}\": {value}")
}

/// `["a", "b"]` array.
pub fn str_list(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| format!("\"{}\"", esc(s))).collect();
    format!("[{}]", inner.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_and_quotes_escape() {
        assert_eq!(esc("a\"b\\c"), "a\\\"b\\\\c");
        assert_eq!(esc("a\nb\rc\td"), "a\\nb\\rc\\td");
        assert_eq!(esc("a\x01b"), "a\\u0001b");
        assert_eq!(
            esc("héllo — ✓"),
            "héllo — ✓",
            "printable unicode passes through"
        );
    }

    #[test]
    fn fields_and_lists_shape() {
        assert_eq!(str_field("k", "v"), "\"k\": \"v\"");
        assert_eq!(num_field("n", 3), "\"n\": 3");
        assert_eq!(bool_field("b", true), "\"b\": true");
        assert_eq!(
            str_list(&["a".to_string(), "b c".to_string()]),
            "[\"a\", \"b c\"]"
        );
        assert_eq!(str_list(&[]), "[]");
    }
}
