//! What the window runs, as one strict [`Availability`].
//!
//! The registry is shared and lists everything any client can be asked to
//! do; this is the window's word on which of those names reach a handler
//! here. The palette lists only what this says runs, a disabled row says why
//! where its doc line was, and [`DevShell::run_command`](crate::DevShell)
//! refuses through the same map before its `match` is reached. One word per
//! name, read twice, so the list and the dispatch cannot disagree; the test
//! at the bottom reads the `match` itself to hold them to that.
//!
//! Strict, because the alternative is the palette the window shipped with:
//! every registered name, more than half of them answering "not supported
//! here" when chosen.

use gitten_core::command::{Availability, Usable};

/// Names the shell's dispatch answers whatever the launch had behind it:
/// the window's own chrome, navigation, the panes' focus and the view
/// movement every screen shares.
const ANSWERED: &[&str] = &[
    "message.show",
    "quit",
    "help",
    "commands.palette",
    "settings",
    "back",
    "theme.picker",
    "theme.cycle",
    "input.accept",
    "input.cancel",
    "pane.next",
    "pane.prev",
    "pane.left",
    "pane.right",
    "project.switch",
    "project.open",
    "project.browse",
    "project.next",
    "project.prev",
    "diff.focus",
    "commits.focus",
    "files.focus",
    "stashes.focus",
    "branches.focus",
    "commits.open-diff",
    "commits.search",
    "files.search",
    "branches.search",
    "stashes.search",
    "workspace.changes",
    "workspace.history",
    "workspace.toggle-sidebar",
    "workspace.toggle-inspector",
    "workspace.toggle-timeline",
    "workspace.preview",
    "workspace.commit-cancel",
    "blob.flip",
    "document.flip",
    "copy.selection",
    "select.all",
    "select.none",
    "view.down",
    "view.up",
    "view.page-down",
    "view.page-up",
    "view.scroll-down",
    "view.scroll-up",
    "view.top",
    "view.bottom",
    "view.left",
    "view.right",
    "diff.next-file",
    "diff.prev-file",
    "diff.cycle-layout",
    "diff.cycle-wrap",
];

const NO_COMMIT: &str = "a fixture has no repository to commit in";
const NO_HUNK: &str = "a fixture has no repository behind it";
const NO_RESET: &str = "a fixture has no repository to reset in";
const NO_REWRITE: &str = "a fixture has no repository to rewrite";
const NO_BRANCH: &str = "a fixture has no repository to create branches in";
const NO_TAG: &str = "a fixture has no repository to tag in";
const NO_CHECKOUT: &str = "a fixture has no repository to check out in";
const NO_ABORT: &str = "a fixture has no repository to abort in";
const NO_CONTINUE: &str = "a fixture has no repository to continue in";
const NO_STASH: &str = "a fixture has no stash stack to act on";
const NO_SYNC: &str = "a fixture has no repository to sync";

/// Names the dispatch answers only with a repository behind the window,
/// each with the sentence a fixture launch says instead — the same one the
/// verb itself would have reached, so the palette's row and the band agree.
const NEEDS_REPO: &[(&str, &str)] = &[
    ("repo.refresh", "a fixture has nothing to refresh"),
    ("repo.push", NO_SYNC),
    ("repo.pull", NO_SYNC),
    ("repo.fetch", NO_SYNC),
    // The shared acts' own sentences, for the verbs that reach one.
    ("files.stage", "a fixture has no working tree to stage in"),
    ("files.stage-all", "a fixture has no working tree to act on"),
    (
        "files.discard",
        "a fixture has no working tree to discard from",
    ),
    ("files.ignore", "a fixture has no repository to ignore in"),
    ("files.stash", "a fixture has no working tree to park"),
    ("files.commit", NO_COMMIT),
    ("files.amend", "a fixture has no repository to amend in"),
    ("workspace.commit", NO_COMMIT),
    ("workspace.commit-confirm", NO_COMMIT),
    ("diff.stage-hunk", NO_HUNK),
    ("diff.unstage-hunk", NO_HUNK),
    ("diff.discard-hunk", NO_HUNK),
    ("commits.reset-menu", NO_RESET),
    ("commits.reset-soft", NO_RESET),
    ("commits.reset-mixed", NO_RESET),
    ("commits.reset-hard", NO_RESET),
    ("commits.revert", "a fixture has no repository to revert in"),
    (
        "commits.cherry-pick",
        "a fixture has no repository to cherry-pick in",
    ),
    ("commits.new-tag", NO_TAG),
    ("commits.new-branch", NO_BRANCH),
    ("commits.checkout", NO_CHECKOUT),
    ("commits.squash-up", NO_REWRITE),
    ("commits.fixup-up", NO_REWRITE),
    ("commits.drop-commit", NO_REWRITE),
    (
        "commits.rebase-onto",
        "a fixture has no repository to rebase in",
    ),
    ("rebase.abort", NO_ABORT),
    ("rebase.continue", NO_CONTINUE),
    ("commits.cherry-pick-abort", NO_ABORT),
    ("commits.cherry-pick-continue", NO_CONTINUE),
    ("stashes.apply", NO_STASH),
    ("stashes.pop", NO_STASH),
    ("stashes.drop", NO_STASH),
    ("branches.checkout", NO_CHECKOUT),
    ("branches.new", NO_BRANCH),
    ("branches.rename", "a fixture has no branches to rename"),
    ("branches.new-tag", NO_TAG),
    ("branches.delete", "a fixture has no branches to delete"),
];

/// The window's availability: `repo` is whether a repository stands behind
/// it, and `extensions` the names its registered extension panes answer —
/// a compiled-in pane is as runnable as a built-in, which is rule 1.
pub(crate) fn window<'a>(
    repo: bool,
    extensions: impl IntoIterator<Item = &'a str>,
) -> Availability {
    let mut a = Availability::strict();
    a.available(ANSWERED.iter().copied());
    for &(name, reason) in NEEDS_REPO {
        match repo {
            true => a.available([name]),
            false => a.disabled(name, reason),
        };
    }
    a.available(extensions);
    a
}

/// The one spelling of "nothing here answers that".
pub(crate) fn unsupported(command: &str) -> String {
    format!("{command} is not supported here")
}

/// What the band says for a name the window turns away, or `None` when it
/// runs.
pub(crate) fn refusal(command: &str, state: &Usable) -> Option<String> {
    match state {
        Usable::Available => None,
        Usable::Disabled(reason) => Some(reason.clone()),
        Usable::Unsupported => Some(unsupported(command)),
    }
}

#[cfg(test)]
mod tests {
    use super::{window, ANSWERED, NEEDS_REPO};
    use gitten_core::command::{Commands, Usable};
    use std::collections::BTreeSet;

    /// Every quoted name in the patterns of `DevShell::run_command_from`'s
    /// `match`: the lines that start an arm or continue one, up to the `=>`
    /// or the guard. Read off the source because the `match` is the thing
    /// being held to account and nothing else spells it.
    fn dispatched() -> BTreeSet<String> {
        let src = include_str!("main.rs");
        let start = src
            .find("    fn run_command_from(")
            .expect("main.rs still has run_command_from");
        let body = &src[start..];
        let end = body
            .find("\n            _ => {")
            .expect("run_command_from still ends in a fallback arm");
        let mut names = BTreeSet::new();
        for line in body[..end].lines() {
            let t = line.trim_start();
            let t = t.strip_prefix("| ").unwrap_or(t);
            if !t.starts_with('"') {
                continue;
            }
            let pattern = t.split("=>").next().unwrap_or("");
            let pattern = pattern.split(" if ").next().unwrap_or("");
            for piece in pattern.split('|') {
                let piece = piece.trim();
                if let Some(name) = piece.strip_prefix('"').and_then(|p| p.strip_suffix('"')) {
                    names.insert(name.to_string());
                }
            }
        }
        names
    }

    #[test]
    fn the_parse_finds_the_dispatch() {
        let names = dispatched();
        // A few from different shapes of arm: a lone one, an alternation,
        // a multi-line guarded alternation.
        for name in ["quit", "commands.palette", "view.right", "diff.cycle-wrap"] {
            assert!(names.contains(name), "{name} not parsed from {names:?}");
        }
        assert!(names.len() > 80, "suspiciously few arms: {}", names.len());
    }

    /// Both directions: what the `match` answers is runnable with a
    /// repository, and what this says runs is a name the `match` answers.
    /// Adding an arm without listing it, or listing a name nothing answers,
    /// fails here rather than in the palette.
    #[test]
    fn the_list_and_the_match_cannot_drift() {
        let a = window(true, []);
        let matched = dispatched();
        for name in &matched {
            assert_eq!(
                a.state(name),
                &Usable::Available,
                "{name} is dispatched but the palette would not list it"
            );
        }
        let listed: BTreeSet<String> = ANSWERED
            .iter()
            .copied()
            .chain(NEEDS_REPO.iter().map(|(n, _)| *n))
            .map(String::from)
            .collect();
        let unmatched: Vec<_> = listed.difference(&matched).collect();
        assert!(
            unmatched.is_empty(),
            "listed as runnable, answered by no arm: {unmatched:?}"
        );
    }

    #[test]
    fn a_fixture_turns_the_writes_away_with_the_verbs_own_reason() {
        let a = window(false, []);
        assert_eq!(
            a.state("commits.revert"),
            &Usable::Disabled("a fixture has no repository to revert in".into())
        );
        assert_eq!(
            a.state("repo.push"),
            &Usable::Disabled("a fixture has no repository to sync".into())
        );
        assert!(a.runnable("view.down"));
        assert!(a.runnable("commands.palette"));
        // Strict: a registered name nothing here answers is unsupported in
        // both kinds of launch.
        assert_eq!(a.state("remotes.fetch"), &Usable::Unsupported);
        assert_eq!(
            window(true, []).state("remotes.fetch"),
            &Usable::Unsupported
        );
    }

    #[test]
    fn an_extension_panes_names_run_like_a_builtins() {
        let a = window(false, ["extension.toggle"]);
        assert!(a.runnable("extension.toggle"));
    }

    /// The registry and the list agree on spelling: a name listed here that
    /// the registry has never heard of is a typo, unless it is one of the
    /// window's own names that no other client has.
    #[test]
    fn every_listed_name_is_registered_or_the_windows_own() {
        let registry = Commands::builtin();
        let own = [
            "message.show",
            "workspace.commit",
            "workspace.commit-confirm",
            "workspace.commit-cancel",
        ];
        for name in ANSWERED
            .iter()
            .copied()
            .chain(NEEDS_REPO.iter().map(|(n, _)| *n))
        {
            assert!(
                registry.get(name).is_some() || own.contains(&name),
                "{name} is listed but not registered"
            );
        }
    }
}
