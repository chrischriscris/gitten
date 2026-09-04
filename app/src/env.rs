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
//! | `GITTEN_START_QUIT` | `0` off, anything else on | off | shell: quit once the first rows are drawn — a headless end to a time-to-interactive run |
//! | `ROUNDS` | `usize` | 7 (`tti`) | `tti`: runs per side, ABBA-interleaved, median reported |
//! | `SETTLE` | seconds (`f64`) | 1 | `tti`: the gap between two timed runs |
//! | `GITTEN_BASELINE` | path | unset (single side) | `tti`: an older `gitten-tui` to ABBA against |
//! | `GITTEN_BASELINE_SHELL` | path | unset (no baseline) | `tti`: an older `gitten-shell` to ABBA against |
//! | `GITTEN_TTI_SHELL` | `0` off, anything else on | on | `tti`: whether the desktop side runs at all (check.sh turns it off to stay windowless) |
//! | `GITTEN_TTI_MAX_FIRST_FRAME_MS` | `f64` ms | unset (advisory) | `tti`: exit non-zero when the median exceeds it |
//! | `GITTEN_TTI_MAX_FILLED_MS` | `f64` ms | unset (advisory) | `tti`: as above, for the filled frame |
//! | `GITTEN_TTI_MAX_SHELL_MS` | `f64` ms | unset (advisory) | `tti`: as above, for the shell wall clock |
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

/// Whether the desktop client should quit the moment its first rows are
/// drawn. `GITTEN_START_QUIT`, anything but `"0"`.
///
/// The terminal measures its own road to the first frame on a private pty;
/// the window has no such exit and a run otherwise sits there until someone
/// quits it, so a scriptable time-to-interactive number needs a client that
/// ends itself at the exact moment the measurement is over. This is that
/// end: the same "first rows drawn" poll that marks the stage also quits,
/// and the wall clock around the process is the number. A window still
/// appears — GPUI draws nothing without one — for however long the road
/// takes.
pub fn start_quit() -> bool {
    enabled("GITTEN_START_QUIT")
}

/// How many runs per side a measurement interleaves. `ROUNDS`, default
/// `default` — `tti` asks for 7, the seven alternating runs a side
/// `docs/measurements.md` reports a startup comparison with.
pub fn rounds(default: usize) -> usize {
    number("ROUNDS", default)
}

/// Seconds between two timed runs. `SETTLE`, default `default` — 1 for
/// `tti`, the same settle gap `bench.sh` leaves between rounds.
pub fn settle(default: f64) -> f64 {
    get("SETTLE")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// An older binary to ABBA against, if the caller named one. `GITTEN_BASELINE`
/// — the terminal side; the path is a `gitten-tui` of another vintage.
pub fn baseline() -> Option<PathBuf> {
    get("GITTEN_BASELINE").map(PathBuf::from)
}

/// The same for the window: `GITTEN_BASELINE_SHELL`, a `gitten-shell` of
/// another vintage.
pub fn baseline_shell() -> Option<PathBuf> {
    get("GITTEN_BASELINE_SHELL").map(PathBuf::from)
}

/// Whether the desktop side of a TTI measurement runs. `GITTEN_TTI_SHELL`,
/// and the default is *on*: the flag is a veto, `0` being the only off —
/// which is the opposite of the [`enabled`] flags, because the side a caller
/// wants removed is the desktop (check.sh turns it off to stay windowless),
/// never the terminal.
pub fn tti_shell() -> bool {
    !std::env::var("GITTEN_TTI_SHELL").is_ok_and(|v| v == "0")
}

/// A ceiling a caller may pin, if it set one. `name` is the whole variable
/// (`GITTEN_TTI_MAX_FIRST_FRAME_MS` and friends), parsed as milliseconds.
/// Unset or unparseable is no ceiling — the advisory default, where the suite
/// only advises — because a threshold that fails to parse must not silently
/// become a gate.
pub fn ceiling(name: &str) -> Option<f64> {
    get(name).and_then(|v| v.trim().parse().ok())
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
