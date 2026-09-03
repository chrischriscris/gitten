//! The headless dispatch harness: named commands through a real viewport.
//!
//! The same `core::command` resolution every client runs — a command name from
//! the registry, or a key spelling resolved through the keymap — applied to a
//! [`Viewport`](gitten_core::view::Viewport) over already-loaded data. No window,
//! no terminal, no writes: anything that would mutate the repository is refused
//! (see [`WRITE_PREFIXES`]), because a harness an agent drives must not stage,
//! discard or push by spelling a command name.
//!
//! `selection` is what the cursor sits on: `short subject` for a commit, the row
//! text for a diff row. `status` is one of:
//!
//! - `ok` — the command ran and may have moved the cursor or the view.
//! - `noop` — acknowledged and intentionally state-free (`quit`, `view.left` on
//!   a commit list, pane focus in a single view).
//! - `wrong-view` — a real command aimed at the other view (`diff.next-file` on
//!   commits).
//! - `needs-client` — a real command only a live client can run (search prompts,
//!   `commits.open-diff`, refresh).
//! - `refused` — a write verb. Never runs here.
//! - `unknown-command` — neither a command name nor a key spelling.
//! - `pending` — the start of a longer chord; single-key maps never produce it,
//!   a custom `gitten.toml` with multi-key chords can.

use gitten_core::command::{Key, Keymap, Modes, Resolve};
use gitten_core::host::Host;
use gitten_core::rows::{Flat, Row};
use gitten_core::view::Viewport;
use gitten_core::{Commit, LineKind};

/// Viewport height when `--height` says nothing. A terminal default, and the
/// only thing page commands measure against.
pub const DEFAULT_HEIGHT: usize = 24;

/// Columns the wrap reflow uses after `diff.cycle-wrap`. The harness walks
/// logical rows, so the width changes nothing it reports; reflowing anyway keeps
/// `Flat::report` honest about the wrap that is selected.
const REFLOW_COLS: usize = 80;

/// Layout names every client agrees on. The registry itself is frontend-owned —
/// `Host::layout` is only the choice — so this is what `diff.cycle-layout`
/// steps through here rather than a registry no headless client owns.
const LAYOUTS: [&str; 2] = ["unified", "split"];

/// Command prefixes that only ever mutate repository state. A harness refuses
/// the whole prefix rather than enumerating verbs, so a verb added next month is
/// refused by default rather than staged by accident.
const WRITE_PREFIXES: [&str; 4] = ["files.", "branches.", "stashes.", "rebase."];

/// Write verbs outside the refused prefixes, spelled out.
const WRITE_COMMANDS: [&str; 15] = [
    "diff.stage-hunk",
    "diff.unstage-hunk",
    "diff.discard-hunk",
    "commits.reset-soft",
    "commits.reset-mixed",
    "commits.reset-hard",
    "commits.revert",
    "commits.squash-up",
    "commits.fixup-up",
    "commits.drop-commit",
    "commits.rebase-onto",
    "commits.cherry-pick",
    "commits.cherry-pick-abort",
    "commits.cherry-pick-continue",
    "repo.push",
];

/// `diff.*` commands, for the wrong-view check.
fn is_diff_command(cmd: &str) -> bool {
    cmd == "diff.focus" || cmd.starts_with("diff.")
}

/// `commits.*` commands, for the wrong-view check.
fn is_commits_command(cmd: &str) -> bool {
    cmd == "commits.focus" || cmd.starts_with("commits.")
}

fn is_write(cmd: &str) -> bool {
    WRITE_COMMANDS.contains(&cmd)
        || WRITE_PREFIXES.iter().any(|p| cmd.starts_with(p))
        || matches!(cmd, "repo.pull" | "repo.fetch")
}

/// Which view the harness walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispView {
    Diff,
    Commits,
}

impl DispView {
    pub fn name(self) -> &'static str {
        match self {
            DispView::Diff => "diff",
            DispView::Commits => "commits",
        }
    }

    /// The mode stack a client resolves keys against for this view.
    pub fn modes(self) -> Modes {
        let mut modes = Modes::new();
        modes.push(self.name());
        modes
    }
}

/// What `dispatch` was asked to do, parsed from the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchReq {
    pub view: DispView,
    pub repo: std::path::PathBuf,
    pub arg: String,
    pub cmds: Vec<String>,
    pub height: usize,
    pub json: bool,
}

/// Parses `dispatch [VIEW] [REPO] [ARG] --run a,b [--height N] [--json]`.
///
/// `VIEW` is `diff` or `commits` and defaults to commits — history always loads,
/// while a working-tree diff is empty on a clean checkout. `--run` is required.
pub fn parse_dispatch(args: &[String]) -> Result<DispatchReq, String> {
    let mut rest = args.to_vec();
    let json = gitten_app::cli::take_switch(&mut rest, "--json");
    let run = gitten_app::cli::take_value(&mut rest, "--run")
        .map_err(|e| format!("dispatch: {e}"))?
        .ok_or_else(|| "dispatch wants --run cmd,cmd,... — nothing to step".to_string())?;
    let height = match gitten_app::cli::take_value(&mut rest, "--height")
        .map_err(|e| format!("dispatch: {e}"))?
    {
        Some(h) => h
            .parse::<usize>()
            .map_err(|_| format!("dispatch: --height {h:?} is not a row count"))?,
        None => DEFAULT_HEIGHT,
    };
    if height == 0 {
        return Err("dispatch: --height 0 shows nothing".to_string());
    }
    let cmds: Vec<String> = run
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(String::from)
        .collect();
    if cmds.is_empty() {
        return Err("dispatch: --run held no commands".to_string());
    }
    let (view, mut positional) = match rest.first().map(String::as_str) {
        Some("diff") => (DispView::Diff, rest[1..].to_vec()),
        Some("commits") => (DispView::Commits, rest[1..].to_vec()),
        _ => (DispView::Commits, rest),
    };
    if positional.len() > 2 {
        return Err(format!(
            "dispatch: too many arguments: {} — want [VIEW] [REPO] [ARG]",
            positional.join(" ")
        ));
    }
    let arg = match positional.len() {
        2 => positional.pop().unwrap_or_default(),
        _ => String::new(),
    };
    let repo = positional
        .pop()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    Ok(DispatchReq {
        view,
        repo,
        arg,
        cmds,
        height,
        json,
    })
}

/// One stepped command, as `dispatch` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOut {
    pub step: usize,
    pub input: String,
    pub command: String,
    pub cursor: usize,
    pub top: usize,
    pub selection: String,
    pub status: String,
}

/// The harness: a viewport over loaded rows plus the small state the stepped
/// commands may touch (the host's wrap/layout/theme choice, a horizontal offset
/// for diff text).
pub struct Harness {
    view: Viewport,
    kind: Kind,
    host: Host,
    layout_idx: usize,
    x: isize,
}

enum Kind {
    Diff { flat: Flat },
    Commits { items: Vec<String> },
}

impl Harness {
    /// A diff harness over an assembled flat table.
    pub fn for_diff(host: Host, flat: Flat, height: usize) -> Self {
        let layout_idx = LAYOUTS.iter().position(|l| *l == host.layout).unwrap_or(0);
        let mut view = Viewport::new();
        view.set_len(flat.len());
        view.set_height(height);
        view.set_scrolloff(host.view.scrolloff);
        Self {
            view,
            kind: Kind::Diff { flat },
            host,
            layout_idx,
            x: 0,
        }
    }

    /// A commits harness over an acquired commit list.
    pub fn for_commits(host: Host, commits: &[Commit], height: usize) -> Self {
        let items = commits
            .iter()
            .map(|c| format!("{} {}", c.short, c.subject))
            .collect();
        let mut view = Viewport::new();
        view.set_len(commits.len());
        view.set_height(height);
        view.set_scrolloff(host.view.scrolloff);
        Self {
            view,
            kind: Kind::Commits { items },
            host,
            layout_idx: 0,
            x: 0,
        }
    }

    pub fn is_diff(&self) -> bool {
        matches!(self.kind, Kind::Diff { .. })
    }

    pub fn len(&self) -> usize {
        self.view.len()
    }

    /// What the cursor sits on, or `(empty)` when there is nothing to sit on.
    pub fn selection(&self) -> String {
        match &self.kind {
            Kind::Commits { items } => items
                .get(self.view.cursor())
                .cloned()
                .unwrap_or_else(|| "(empty)".to_string()),
            Kind::Diff { flat } => match flat.get(self.view.cursor()) {
                None => "(empty)".to_string(),
                Some(Row::File { path, adds, dels }) => {
                    format!("{path} +{adds} -{dels}")
                }
                Some(Row::Hunk(h)) => truncate(h, 100),
                Some(Row::Line(l)) => {
                    let mark = match l.kind {
                        LineKind::Added => '+',
                        LineKind::Removed => '-',
                        LineKind::Context => ' ',
                    };
                    let moved = match l.moved {
                        true => " (moved)",
                        false => "",
                    };
                    format!("{mark} {}{moved}", truncate(l.text.trim_end(), 100))
                }
            },
        }
    }

    /// Steps one token — a command name or a key spelling — and reports the row.
    pub fn step(&mut self, n: usize, input: &str) -> StepOut {
        let modes = match self.kind {
            Kind::Diff { .. } => DispView::Diff.modes(),
            Kind::Commits { .. } => DispView::Commits.modes(),
        };
        let resolved = resolve(&self.host.keys, &modes, input);
        let (command, status) = match resolved {
            Resolved::Run(name) => {
                let status = self.apply(&name);
                (name, status)
            }
            Resolved::Pending => (String::new(), "pending".to_string()),
            Resolved::Unknown => (String::new(), "unknown-command".to_string()),
        };
        StepOut {
            step: n,
            input: input.to_string(),
            command,
            cursor: self.view.cursor(),
            top: self.view.top(),
            selection: self.selection(),
            status,
        }
    }

    /// Runs a resolved command name against the viewport and the host copy.
    fn apply(&mut self, command: &str) -> String {
        let diff = self.is_diff();
        if is_write(command) {
            return "refused: write verbs never run in the harness".to_string();
        }
        if diff && is_commits_command(command) {
            return "wrong-view: a commits command on a diff".to_string();
        }
        if !diff && is_diff_command(command) {
            return "wrong-view: a diff command on commits".to_string();
        }
        let page = self.view.height().saturating_sub(1).max(1) as isize;
        let scroll = (self.host.view.rows as isize).max(1);
        match command {
            "view.down" => {
                self.view.down();
                "ok".to_string()
            }
            "view.up" => {
                self.view.up();
                "ok".to_string()
            }
            "view.page-down" => {
                self.view.move_by(page);
                "ok".to_string()
            }
            "view.page-up" => {
                self.view.move_by(-page);
                "ok".to_string()
            }
            "view.scroll-down" => {
                self.view.scroll_by(scroll);
                "ok".to_string()
            }
            "view.scroll-up" => {
                self.view.scroll_by(-scroll);
                "ok".to_string()
            }
            "view.top" => {
                self.view.to_top();
                "ok".to_string()
            }
            "view.bottom" => {
                self.view.to_bottom();
                "ok".to_string()
            }
            "view.left" | "view.right" => match diff {
                true => {
                    self.x += match command {
                        "view.right" => 8,
                        _ => -8,
                    };
                    format!("ok x={}", self.x)
                }
                false => "noop: a commit list has nothing off the edge".to_string(),
            },
            "diff.next-file" => match self.jump_file(1) {
                true => "ok".to_string(),
                false => "noop: no further file".to_string(),
            },
            "diff.prev-file" => match self.jump_file(-1) {
                true => "ok".to_string(),
                false => "noop: no earlier file".to_string(),
            },
            "diff.cycle-layout" => {
                self.layout_idx = (self.layout_idx + 1) % LAYOUTS.len();
                self.host.layout = LAYOUTS[self.layout_idx].to_string();
                format!("ok layout={}", self.host.layout)
            }
            "diff.cycle-wrap" => {
                let names = self.host.wrap.names();
                let at = self.host.wrap.selected_index();
                let next = names[(at + 1) % names.len()].to_string();
                self.host.wrap.select(&next);
                if let Kind::Diff { flat } = &mut self.kind {
                    flat.reflow(REFLOW_COLS, self.host.wrap.current());
                }
                format!("ok wrap={next}")
            }
            "theme.cycle" => {
                self.host.cycle_theme();
                format!("ok theme={}", self.host.theme.name)
            }
            "quit" | "back" | "help" | "message.show" | "select.all" | "select.none"
            | "copy.selection" => "noop: no screen to act on".to_string(),
            c if c.ends_with(".focus") || c.starts_with("pane.") => {
                "noop: one view, no panes".to_string()
            }
            _ => "needs-client: prompts, panes and history verbs need a live client".to_string(),
        }
    }

    /// Moves the cursor to the next (or previous) file header row.
    fn jump_file(&mut self, dir: isize) -> bool {
        let Kind::Diff { flat } = &self.kind else {
            return false;
        };
        let mut rows: Vec<usize> = flat.files().iter().map(|e| e.row).collect();
        rows.sort_unstable();
        let at = self.view.cursor();
        let target = match dir.is_negative() {
            false => rows.into_iter().find(|&r| r > at),
            true => rows.into_iter().rev().find(|&r| r < at),
        };
        match target {
            Some(row) => {
                self.view.go_to(row);
                true
            }
            None => false,
        }
    }
}

/// What one input token resolved to.
enum Resolved {
    Run(String),
    Pending,
    Unknown,
}

/// A command name straight from the registry, or a key spelling through the
/// keymap — the same two spellings every client accepts.
fn resolve(keys: &Keymap, modes: &Modes, input: &str) -> Resolved {
    if gitten_core::command::Commands::builtin().known(input) || keys_known(keys, input) {
        return Resolved::Run(input.to_string());
    }
    let Some(key) = Key::parse(input) else {
        return Resolved::Unknown;
    };
    match keys.resolve(modes, std::slice::from_ref(&key)) {
        Resolve::Run(name) => Resolved::Run(name.to_string()),
        Resolve::Pending => Resolved::Pending,
        Resolve::None => Resolved::Unknown,
    }
}

/// A command is known when the registry or any binding names it — an extension
/// command has no other proof of existence.
fn keys_known(keys: &Keymap, name: &str) -> bool {
    keys.bindings().iter().any(|b| b.command == name)
}

fn truncate(s: &str, max: usize) -> String {
    match s.len() > max {
        true => format!("{}…", &s[..max]),
        false => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::parse_unified_diff;
    use gitten_core::prepared::prepare;
    use gitten_core::rows::Present;

    const PATCH: &str = "\
diff --git a/one.rs b/one.rs
index 1111111..2222222 100644
--- a/one.rs
+++ b/one.rs
@@ -1,4 +1,4 @@
 fn one() {}
-let x = 1;
+let x = 2;
 fn two() {}
diff --git a/two.rs b/two.rs
index 1111111..2222222 100644
--- a/two.rs
+++ b/two.rs
@@ -1,3 +1,3 @@
 fn three() {}
-let y = 1;
+let y = 2;
";

    /// The half of a presentation that does not draw — the same seam the real
    /// views build on, holding only a [`Flat`].
    #[derive(Default)]
    struct FlatPresent {
        flat: Flat,
    }

    impl Present for FlatPresent {
        fn claims(&self, _path: &str) -> bool {
            true
        }

        fn len(&self) -> usize {
            self.flat.len()
        }

        fn build(&mut self, file: gitten_core::prepared::File) {
            self.flat.push(file);
        }
    }

    fn diff_harness() -> Harness {
        let host = Host::new();
        let files = parse_unified_diff(PATCH);
        let prepared = prepare(&files, &host.syntax, gitten_app::MAX_LINE_CHARS);
        let mut present = FlatPresent::default();
        for f in prepared.files {
            present.build(f);
        }
        Harness::for_diff(host, present.flat, DEFAULT_HEIGHT)
    }

    fn commits_harness() -> Harness {
        let host = Host::new();
        let commits = vec![
            Commit {
                sha: "aaa".into(),
                short: "aaa".into(),
                parents: Box::from(&[][..]),
                author: "t".into(),
                timestamp: 0,
                subject: "first".into(),
            },
            Commit {
                sha: "bbb".into(),
                short: "bbb".into(),
                parents: Box::from(&[][..]),
                author: "t".into(),
                timestamp: 0,
                subject: "second".into(),
            },
            Commit {
                sha: "ccc".into(),
                short: "ccc".into(),
                parents: Box::from(&[][..]),
                author: "t".into(),
                timestamp: 0,
                subject: "third".into(),
            },
        ];
        Harness::for_commits(host, &commits, DEFAULT_HEIGHT)
    }

    #[test]
    fn a_dispatch_sequence_walks_the_viewport() {
        let mut h = diff_harness();
        assert!(h.len() > 3, "the patch has rows to walk");
        let cursors: Vec<usize> = ["view.down", "view.down", "view.up", "down"]
            .iter()
            .enumerate()
            .map(|(i, cmd)| h.step(i + 1, cmd))
            .map(|s| {
                assert_eq!(s.status, "ok", "{}: {:?}", s.input, s);
                s.cursor
            })
            .collect();
        assert_eq!(cursors, vec![1, 2, 1, 2], "down, down, up, down-as-a-key");
    }

    #[test]
    fn keys_resolve_and_writes_refuse() {
        let mut h = commits_harness();
        let j = h.step(1, "j");
        assert_eq!(j.command, "view.down");
        assert_eq!(j.cursor, 1);
        assert_eq!(j.status, "ok");
        assert_eq!(j.selection, "bbb second");
        let file = h.step(2, "diff.next-file");
        assert_eq!(file.status, "wrong-view: a diff command on commits");
        assert_eq!(file.cursor, 1, "a refused step moves nothing");
        let unknown = h.step(3, "not-a-command-or-key-spelling!!");
        assert_eq!(unknown.status, "unknown-command");
        assert!(unknown.command.is_empty());
    }

    #[test]
    fn file_jumps_land_on_headers() {
        let mut h = diff_harness();
        let next = h.step(1, "diff.next-file");
        assert_eq!(next.status, "ok");
        assert!(
            next.selection.starts_with("two.rs"),
            "jumped to the second file: {}",
            next.selection
        );
        let prev = h.step(2, "diff.prev-file");
        assert_eq!(prev.status, "ok");
        assert!(
            prev.selection.starts_with("one.rs"),
            "jumped back: {}",
            prev.selection
        );
    }

    #[test]
    fn dispatch_wants_a_run_and_a_sane_height() {
        let args = |line: &str| {
            line.split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>()
        };
        assert!(
            parse_dispatch(&args("diff .")).is_err(),
            "--run is required"
        );
        assert!(parse_dispatch(&args("--run ,")).is_err(), "no commands");
        assert!(parse_dispatch(&args("--run down --height 0")).is_err());
        assert!(parse_dispatch(&args("--run down --height many")).is_err());
        let req = parse_dispatch(&args("diff . HEAD~1..HEAD --run down,j --height 40")).unwrap();
        assert_eq!(req.view, DispView::Diff);
        assert_eq!(req.repo.to_string_lossy(), ".");
        assert_eq!(req.arg, "HEAD~1..HEAD");
        assert_eq!(req.cmds, vec!["down", "j"]);
        assert_eq!(req.height, 40);
        // Bare `--run` defaults to the commits view of this repository.
        let bare = parse_dispatch(&args("--run down,down --json")).unwrap();
        assert_eq!(bare.view, DispView::Commits);
        assert!(bare.json);
    }
}
