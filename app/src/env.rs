//! Typed environment for headless tools, examples and scripts.
//!
//! Every knob a machine (or a human without a window) turns on the headless
//! tools lives here, parsed once and typed, so `tui/examples/dump.rs` and the
//! `core` examples do not each re-decide what `COLS=abc` means. A client that
//! needs one of these reads it from here rather than from `std::env`
//! directly; the doc comment on each accessor is the definition of the
//! variable.
//!
//! # Variables
//!
//! | variable | type | default | used by |
//! |---|---|---|
//! | `COLS` | `usize` | 100 | `dump`: screen width in columns |
//! | `ROWS` | `usize` | 40 | `dump`: screen height in rows, status bar included |
//! | `LAYOUT` | name | host default (`unified`) | `dump`: which presentation to paint |
//! | `WRAP` | name | host default | `dump`: which wrap registry entry breaks lines |
//! | `THEME` | name | host default (`dark`) | `dump`, `paint`: which registered palette to draw with |
//! | `AT` | `usize` | 0 | `dump`: how many rows down the view starts scrolled |
//! | `FRAMES` | `usize` | 50 | `dump`: repaints to average the frame timing over |
//! | `WRAP_COLS` | `usize` | 150 (`bench`), 100 (`paint`) | wrap budget in columns for the measurement |
//! | `WORST` | presence | unset (off) | `diffcheck`: also report the files each algorithm did worst on |
//! | `GITTEN_STATS` | `0` off, anything else on | off | overlays: the frame/row/heap readout |
//! | `GITTEN_START_LOG` | `0` off, anything else on | off | `Startup`: per-stage startup timings on stderr |
//! | `GITTEN_FORMAT` | `json` or unset | unset (human) | every `--json` tool: machine-readable output on stdout |
//! | `GITTEN_CONFIG` | path | see `config::path` | which `gitten.toml` to read; honoured by [`config::path`](crate::config::path) |
//!
//! Numbers that do not parse fall back to the default rather than failing:
//! these tools measure and draw, and refusing to run over `COLS=wide` would
//! be a worse failure than drawing at 100 columns. Names (`LAYOUT`, `WRAP`,
//! `THEME`) are returned as-is; the caller reports an unknown one against
//! its registry, which is the only place that knows the alternatives.

use std::path::PathBuf;

/// The screen width `dump` draws when `COLS` says nothing.
pub const DEFAULT_COLS: usize = 100;
/// The screen height `dump` draws when `ROWS` says nothing.
pub const DEFAULT_ROWS: usize = 40;
/// Repaints `dump` averages its frame timing over when `FRAMES` says nothing.
pub const DEFAULT_FRAMES: usize = 50;

/// The raw string of a variable, if set.
pub fn get(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// A variable as a number, or the default when unset or unparseable.
///
/// Unparseable falls back rather than failing — see the module docs for why.
pub fn number(name: &str, default: usize) -> usize {
    get(name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Whether a variable is set at all, whatever its value.
///
/// For presence flags like `WORST`: `WORST=0` still reports.
pub fn present(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

/// Whether a variable enables something: set to anything but `"0"`.
///
/// The rule `GITTEN_STATS` and `GITTEN_START_LOG` both follow — unset is off,
/// `0` is off, anything else is on.
pub fn enabled(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v != "0")
}

/// Whether the environment alone asks for machine-readable output:
/// `GITTEN_FORMAT=json`, case-insensitive, surrounding whitespace ignored.
pub fn format_is_json() -> bool {
    get("GITTEN_FORMAT").is_some_and(|v| v.trim().eq_ignore_ascii_case("json"))
}

/// Whether `args` hold a `--json` flag, exactly. A value-taking `--format`
/// is not recognised: the flag is `--json`, anywhere in the argument list.
pub fn has_json_arg(args: &[String]) -> bool {
    args.iter().any(|a| a == "--json")
}

/// Whether this invocation wants machine-readable output: `--json` on the
/// command line, or [`format_is_json`] from the environment.
pub fn wants_json(args: &[String]) -> bool {
    has_json_arg(args) || format_is_json()
}

/// `args` without every `--json` in it, so the positional parsing underneath
/// never sees the flag whatever position it was given in.
pub fn strip_json_arg(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|a| a.as_str() != "--json")
        .cloned()
        .collect()
}

/// Screen width in columns for `dump`. `COLS`, default [`DEFAULT_COLS`].
pub fn cols() -> usize {
    number("COLS", DEFAULT_COLS)
}

/// Screen height in rows, status bar included, for `dump`. `ROWS`, default
/// [`DEFAULT_ROWS`].
pub fn rows() -> usize {
    number("ROWS", DEFAULT_ROWS)
}

/// How many rows down the view starts scrolled. `AT`, default 0.
pub fn at() -> usize {
    number("AT", 0)
}

/// Repaints to average the frame timing over. `FRAMES`, default
/// [`DEFAULT_FRAMES`].
pub fn frames() -> usize {
    number("FRAMES", DEFAULT_FRAMES)
}

/// Which presentation to paint, if the caller named one. `LAYOUT`.
pub fn layout() -> Option<String> {
    get("LAYOUT")
}

/// Which wrap registry entry breaks lines, if the caller named one. `WRAP`.
pub fn wrap() -> Option<String> {
    get("WRAP")
}

/// Which registered palette to draw with, if the caller named one. `THEME`.
pub fn theme() -> Option<String> {
    get("THEME")
}

/// Wrap budget in columns for a measurement. `WRAP_COLS`, default `default`.
/// The default differs per tool — 150 for `bench` (a real window), 100 for
/// `paint` — so it stays a parameter rather than a constant.
pub fn wrap_cols(default: usize) -> usize {
    number("WRAP_COLS", default)
}

/// Whether to also report the worst files. `WORST`, set at all means on.
pub fn worst() -> bool {
    present("WORST")
}

/// Whether the frame/row/heap readout is on. `GITTEN_STATS`, anything but
/// `"0"`.
pub fn stats() -> bool {
    enabled("GITTEN_STATS")
}

/// Whether startup-stage timings go to stderr. `GITTEN_START_LOG`, anything
/// but `"0"`.
pub fn start_log() -> bool {
    enabled("GITTEN_START_LOG")
}

/// An explicit config path, if the caller gave one. `GITTEN_CONFIG`.
/// Honoured by [`config::path`](crate::config::path); this accessor is for
/// tools that want to report which file was chosen.
pub fn config_path() -> Option<PathBuf> {
    std::env::var_os("GITTEN_CONFIG").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_is_found_wherever_it_sits() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(has_json_arg(&args(&["diff", "--json", "."])));
        assert!(has_json_arg(&args(&["--json"])));
        assert!(!has_json_arg(&args(&["diff", "."])));
        // A value-taking spelling is not the flag.
        assert!(!has_json_arg(&args(&["--format=json"])));
    }

    #[test]
    fn stripping_leaves_the_positionals_where_they_were() {
        let args = ["--json", "diff", ".", "HEAD~1..HEAD", "--json"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        assert_eq!(strip_json_arg(&args), ["diff", ".", "HEAD~1..HEAD"]);
    }
}
