//! What a verb *decides*, once, for every client.
//!
//! `verbs.rs` holds what a verb *does* — a `Write` over the repository handle.
//! This holds the part above it that was being written twice: which guards run
//! in which order, the words a refusal uses, and when a destructive question is
//! asked rather than answered. A client supplies the things it genuinely owns —
//! what is selected, how a sentence reaches the reader, and how a job is queued —
//! and nothing else about it is visible here.

use crate::jobs::Job;
use crate::verbs::Write;
use gitten_core::refs::Target;
use gitten_core::status::PathBytes;
use gitten_core::Hunk;
use gitten_git::Handle;

/// The client services every shared action needs. Drawing and input stay in
/// the client; this is the narrow window shared policy reaches through.
pub trait Client {
    /// A refusal or a result, in the client's own furniture.
    fn say(&mut self, message: String);
    /// A destructive question standing until it is answered or dropped.
    fn ask(&mut self, question: String);
    /// The repository, absent when the client is showing a fixture.
    fn repo(&self) -> Option<Handle>;
    /// Queues the job. `false` means the queue is shutting down.
    fn submit(&mut self, job: Box<dyn Job>) -> bool;
}

/// The client-owned selection and confirmation state needed by branch actions.
pub trait BranchClient: Client {
    /// The branch row the keyboard is on. Clients refuse a command aimed at the
    /// wrong pane before entering the shared action; `None` means no selected row.
    fn branch_target(&self) -> Option<Target>;
    /// Arms this target, or spends an arm already standing on it.
    fn confirm_or_arm_branch(&mut self, target: &Target) -> bool;
}

/// Which presentation section a working-tree path occupies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileSection {
    Staged,
    Unstaged,
    Untracked,
    Conflicts,
}

/// The selected working-tree row, with its path preserved for git and already
/// prepared for user-facing questions by the client presentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedFile {
    pub section: FileSection,
    pub path: PathBytes,
    pub shown: String,
}

/// The client-owned reads and confirmation state needed by file actions.
pub trait FileClient: Client {
    fn selected_file(&self) -> Option<SelectedFile>;
    fn cursor_section(&self) -> Option<FileSection>;
    fn paths_in(&self, section: FileSection) -> Vec<PathBytes>;
    /// Arms this file target, or spends an identical arm already standing.
    fn confirm_or_arm_file(&mut self, target: &SelectedFile) -> bool;
}

/// `branches.delete`, for every client.
///
/// The guard order is load-bearing and is why this is shared rather than
/// described: a detached HEAD is refused before a remote is, because "not a
/// branch" is a truer thing to say than "its remote's to delete"; and the arm
/// is spent only after the repository is known to exist, so a fixture cannot
/// consume a question it can never answer.
pub fn delete_branch(client: &mut impl BranchClient) {
    let Some(target) = client.branch_target() else {
        client.say("nothing selected to delete".into());
        return;
    };
    let shown = match &target {
        Target::Local(name) => name.to_string_lossy().into_owned(),
        Target::Remote { remote, branch } => {
            format!("{}/{}", remote.to_string_lossy(), branch.to_string_lossy())
        }
        Target::Detached => {
            client.say("a detached HEAD is not a branch".into());
            return;
        }
    };
    if matches!(target, Target::Remote { .. }) {
        client.say("a remote branch is its remote's to delete — fetch prunes it here".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to delete branches from".into());
        return;
    };
    if !client.confirm_or_arm_branch(&target) {
        client.ask(format!("delete branch {shown}? press again to confirm"));
        return;
    }
    let Target::Local(name) = target else {
        unreachable!("remotes and detached refuse above");
    };
    let job = Write::delete_branch(&repo, name.as_bytes().to_vec(), false);
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}
/// `files.stage`: stage the selected path, or unstage it when it is staged.
pub fn stage_or_unstage(client: &mut impl FileClient) {
    let Some(file) = client.selected_file() else {
        client.say("nothing selected to stage".into());
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no working tree to stage in".into());
        return;
    };
    let bytes = file.path.as_bytes().to_vec();
    let job = match file.section {
        FileSection::Staged => Write::unstage(&repo, bytes),
        _ => Write::stage(&repo, bytes),
    };
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `files.stage-all`: act on every path on the cursor's side of the index.
pub fn stage_all(client: &mut impl FileClient) {
    let staging = client.cursor_section() != Some(FileSection::Staged);
    let mut targets = if staging {
        client.paths_in(FileSection::Unstaged)
    } else {
        client.paths_in(FileSection::Staged)
    };
    if staging {
        targets.extend(client.paths_in(FileSection::Untracked));
    }
    if targets.is_empty() {
        client.say(
            if staging {
                "nothing unstaged or untracked to stage"
            } else {
                "nothing staged to unstage"
            }
            .into(),
        );
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no working tree to act on".into());
        return;
    };
    let bytes = targets
        .into_iter()
        .map(|path| path.as_bytes().to_vec())
        .collect();
    let job = if staging {
        Write::stage_many(&repo, bytes)
    } else {
        Write::unstage_many(&repo, bytes)
    };
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `files.discard`: refuse unsafe sections and ask twice for one file target.
pub fn discard_file(client: &mut impl FileClient) {
    let Some(file) = client.selected_file() else {
        client.say("nothing selected to discard".into());
        return;
    };
    match file.section {
        FileSection::Staged => {
            client.say("that change is staged — unstage it before discarding".into());
            return;
        }
        FileSection::Conflicts => {
            client.say("a conflicted file needs its merge resolved, not discarded".into());
            return;
        }
        FileSection::Untracked | FileSection::Unstaged => {}
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no working tree to discard from".into());
        return;
    };
    if !client.confirm_or_arm_file(&file) {
        let verb = if file.section == FileSection::Untracked {
            "delete"
        } else {
            "discard"
        };
        client.ask(format!("{verb} {}? press again to confirm", file.shown));
        return;
    }
    let bytes = file.path.as_bytes().to_vec();
    let job = if file.section == FileSection::Untracked {
        Write::remove_untracked(&repo, bytes)
    } else {
        Write::discard(&repo, bytes)
    };
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `files.ignore`: append one selected untracked path to `.gitignore`.
pub fn ignore_file(client: &mut impl FileClient) {
    let Some(file) = client.selected_file() else {
        client.say("only an untracked file can be ignored".into());
        return;
    };
    if file.section != FileSection::Untracked {
        client.say("only an untracked file can be ignored".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to ignore in".into());
        return;
    };
    let job = Write::ignore(&repo, file.path.as_bytes().to_vec());
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `files.stash`: park the tracked working tree with git's default message.
pub fn stash_working_tree(client: &mut impl Client) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no working tree to park".into());
        return;
    };
    if !client.submit(Box::new(Write::stash_push(&repo, None))) {
        client.say("the job queue is shutting down".into());
    }
}

/// Turn accepted commit text into its write job.
pub fn commit_message(client: &mut impl Client, message: String) {
    if message.trim().is_empty() {
        client.say("a commit needs a message".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to commit in".into());
        return;
    };
    if !client.submit(Box::new(Write::commit(&repo, message))) {
        client.say("the job queue is shutting down".into());
    }
}

/// Turn accepted amend text into its write job.
pub fn amend_message(client: &mut impl Client, message: String) {
    if message.trim().is_empty() {
        client.say("a commit needs a message".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to amend in".into());
        return;
    };
    if !client.submit(Box::new(Write::amend(&repo, message))) {
        client.say("the job queue is shutting down".into());
    }
}

// ----------------------------------------------------------------- hunks

/// Which side of the index a hunk verb is aimed at — the diff source's own
/// word for it, narrowed to the sides a patch can address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkSide {
    /// `HEAD`'s tree → the index: the staged side of one path.
    Staged,
    /// The index → the working tree: the unstaged side of one path.
    Unstaged,
    /// The working tree against nothing: a file known to no part of git.
    Untracked,
    /// The combined `HEAD`→worktree aggregate. It cannot aim a stage or an
    /// unstage — its hunks fold both sides of the index into one text —
    /// and only a discard, which addresses the working tree alone, is
    /// answerable here.
    Combined,
}

/// What the keyboard selected on a diff, in the diff's own terms: one
/// whole hunk, or line ranges within the hunks it spans. The hunks are
/// clones of what the screen drew; [`hunk_job`] re-reads the repository
/// and matches them against what git holds now before anything applies.
#[derive(Clone, Debug)]
pub enum HunkSelection {
    Whole(Hunk),
    /// Per touched hunk: the hunk as drawn and the inclusive line range
    /// marked within it.
    Lines(Vec<(Hunk, usize, usize)>),
}

/// A hunk verb's aim: the path it acts on, the side of the index it means,
/// and what was selected.
#[derive(Clone, Debug)]
pub struct HunkAsk {
    pub path: String,
    pub side: HunkSide,
    pub selection: HunkSelection,
}

/// The one hunk verb, for every client.
///
/// A partial stage is a read, then a patch, then a checked write, in that
/// order, and this function is the whole of it. The read is fresh — the
/// one path the verb addresses, re-read now, however long ago the preview
/// was drawn — because a patch built from a stale picture aims at content
/// nobody is looking at. The patch is [`gitten_core::patch`]'s, built from
/// the hunks the fresh read still shows; the write carries the blob
/// identities the patch was built against and re-checks them when it runs
/// (see [`CheckedPatch`]), so the gap between the keypress and the queue's
/// turn is covered too. Every refusal is worded here, once, where an
/// extension calling the same verb through the same name reads the same
/// sentence.
/// What a hunk verb on one side of the index means, and where it means
/// nothing — the verb table, pure, so a client can read it *before* it
/// spends a destructive arm and the job build reads it again before it
/// aims. Each row is a fact about the side, not a mood: what the verb
/// would mean there, and why that is nothing. Where a door exists one
/// keypress away, the refusal names it.
pub fn verb_refusal(command: &str, side: HunkSide) -> Option<String> {
    #[derive(Clone, Copy)]
    enum Verb {
        Stage,
        Unstage,
        Discard,
    }
    let verb = match command {
        "diff.stage-hunk" => Verb::Stage,
        "diff.unstage-hunk" => Verb::Unstage,
        "diff.discard-hunk" => Verb::Discard,
        other => return Some(format!("{other} is not a hunk verb")),
    };
    let refusal = match (verb, side) {
        (Verb::Stage, HunkSide::Combined) => {
            Some("the combined view folds both sides of the index — open the file's own side from the files pane (enter)")
        }
        (Verb::Unstage, HunkSide::Combined) => {
            Some("the combined view folds both sides of the index — open the file's staged side from the files pane (enter, then tab)")
        }
        (Verb::Stage, HunkSide::Staged) => {
            Some("the index is this diff's new side — it is already staged")
        }
        (Verb::Unstage, HunkSide::Unstaged) => Some(
            "this side is the index→worktree change — nothing here is staged; tab opens the staged side",
        ),
        (Verb::Unstage, HunkSide::Untracked) => {
            Some("an untracked file has nothing in the index to take out")
        }
        (Verb::Stage, HunkSide::Untracked) => Some(
            "an untracked file stages whole — a creation patch needs the mode git only knows from `git add`; stage it from the files pane",
        ),
        (Verb::Discard, HunkSide::Staged) => Some(
            "the staged side has no working tree to discard — unstage it (u), then discard the unstaged side",
        ),
        (Verb::Discard, HunkSide::Untracked) => {
            Some("an untracked file is all or nothing — remove it whole from the files pane")
        }
        _ => None,
    };
    refusal.map(|s| s.to_string())
}

pub fn hunk_job(
    command: &str,
    ask: HunkAsk,
    repo: &Handle,
    differs: &gitten_core::differ::Differs,
    over: &gitten_core::differ::Overrides,
) -> Result<Box<dyn Job>, String> {
    if let Some(refusal) = verb_refusal(command, ask.side) {
        return Err(refusal);
    }
    // The verb again, for the parts only the job build needs: which
    // unchosen changes a line window keeps, and what the write applies.
    #[derive(Clone, Copy)]
    enum Verb {
        Stage,
        Unstage,
        Discard,
    }
    let verb = match command {
        "diff.stage-hunk" => Verb::Stage,
        "diff.unstage-hunk" => Verb::Unstage,
        _ => Verb::Discard,
    };

    // The combined view is the one aim that cannot re-read a single path:
    // there is no per-path read of HEAD→worktree, only the whole-repo one,
    // and re-reading that per keypress is not a price a verb pays. Its
    // discard keeps the patch the screen drew, with HEAD's blob — read now
    // — as the revalidation anchor; content that drifted under it fails
    // `git apply`'s context check in git's own words.
    if ask.side == HunkSide::Combined {
        let HunkSelection::Whole(hunk) = ask.selection else {
            return Err(
                "the combined view discards whole hunks — open the file's own side for line-level work".into(),
            );
        };
        let expected = repo.head_blob_oid(ask.path.as_bytes())?;
        return Ok(Box::new(CheckedPatch {
            name: format!("discard patch: {}", ask.path),
            repo: Handle::clone(repo),
            verb: PatchVerb::Discard,
            patch: gitten_core::patch::emit(&ask.path, &[&hunk]),
            expect: vec![ExpectOid {
                side: OidSide::Head,
                path: ask.path.clone().into_bytes(),
                oid: expected,
            }],
        }));
    }

    // One path, re-read now. An empty answer is a side that stopped
    // existing between the preview and this keypress — nothing to aim at,
    // said the way the preview itself says it.
    let pair = match ask.side {
        HunkSide::Unstaged => repo.pairs_unstaged(Some(ask.path.as_bytes()))?.pop(),
        HunkSide::Staged => repo.pairs_staged(Some(ask.path.as_bytes()))?.pop(),
        HunkSide::Untracked => repo.pair_untracked(ask.path.as_bytes())?,
        HunkSide::Combined => unreachable!("the combined aim returned above"),
    }
    .ok_or_else(|| match ask.side {
        HunkSide::Unstaged => format!("nothing unstaged for {} — refresh (R)", ask.path),
        HunkSide::Staged => format!("nothing staged for {} — refresh (R)", ask.path),
        HunkSide::Untracked => format!("{} is not readable — deleted, or not a file", ask.path),
        HunkSide::Combined => unreachable!("the combined aim returned above"),
    })?;
    if pair.binary {
        return Err(format!(
            "{} is binary — there are no lines to select; stage or discard it whole from the files pane",
            ask.path
        ));
    }
    if pair.old_path.as_ref().is_some_and(|old| old != &pair.path) {
        return Err(format!(
            "{} is a rename from {} — partial staging of a rename is not supported; stage it whole",
            ask.path,
            pair.old_path.as_deref().unwrap_or_default()
        ));
    }

    // The drawn selection, matched against what git holds now. Matching is
    // by the hunks' full line content — kinds, texts and numbers — which is
    // exact whenever the file is as the preview drew it and fails loudly
    // whenever it is not: the diff cache keys on the pair's OIDs, so a
    // fresh re-diff of unchanged content is the preview's own hunks back.
    let fresh = gitten_git::diff_pairs(std::slice::from_ref(&pair), differs, over);
    let hunks = fresh.first().map(|f| f.hunks.as_slice()).unwrap_or(&[]);
    let stale = || {
        format!(
            "{} changed since this diff was drawn — refresh (R) and try again",
            ask.path
        )
    };
    let mut chosen: Vec<Hunk> = Vec::new();
    match &ask.selection {
        HunkSelection::Whole(drawn) => {
            let hunk = hunks
                .iter()
                .find(|h| h.lines == drawn.lines)
                .ok_or_else(stale)?;
            chosen.push(hunk.clone());
        }
        HunkSelection::Lines(parts) => {
            // The verb decides which unchosen changes a window keeps. The
            // forward verb rewrites the index and matches the patch's
            // preimage against it; the two reverse verbs — unstage and
            // discard — are matched by `git apply --reverse` against the
            // tree they rewrite, which is the patch's *post*image. See
            // [`gitten_core::patch::Unselected`] for the rule and why.
            let keep = match verb {
                Verb::Stage => gitten_core::patch::Unselected::KeepRemovals,
                Verb::Unstage | Verb::Discard => gitten_core::patch::Unselected::KeepAdditions,
            };
            for (drawn, lo, hi) in parts {
                let hunk = hunks
                    .iter()
                    .find(|h| h.lines == drawn.lines)
                    .ok_or_else(stale)?;
                // A window with no changed line in it — the keyboard sat on
                // context — stages nothing, and is skipped rather than
                // refused: the rest of the selection still means what it
                // says. One that emptied the whole selection is refused
                // below, where "nothing selected" is one sentence.
                if let Some(window) = gitten_core::patch::line_window(hunk, *lo, *hi, keep) {
                    chosen.push(window);
                }
            }
            if chosen.is_empty() {
                return Err("no changed lines in the selection".into());
            }
        }
    }
    let sides = gitten_core::patch::Sides {
        old_lines: pair.old.len(),
        old_final_newline: pair.old_final_newline,
        new_lines: pair.new.len(),
        new_final_newline: pair.new_final_newline,
    };
    // A hunk that carries no context and addresses an old side cannot be
    // aimed: `git apply` locates a change by the context around it, and a
    // zero-context read — `[diff] context = 0` — leaves nothing to locate
    // with. Refused here, in words that name the setting, rather than in
    // git's own "patch does not apply" further down the pipe. An insertion
    // (no old side to the hunk) and a creation need no context and are
    // allowed through.
    if chosen.iter().any(|h| {
        h.lines
            .iter()
            .all(|l| l.kind != gitten_core::LineKind::Context)
            && h.lines.iter().any(|l| l.old_no.is_some())
    }) {
        return Err(
            "this diff was read with no context — a partial patch cannot be aimed; raise [diff] context"
                .into(),
        );
    }
    let refs: Vec<&Hunk> = chosen.iter().collect();
    let patch = gitten_core::patch::emit_with(&ask.path, &refs, &sides)?;
    if patch.is_empty() {
        return Err("nothing selected".into());
    }

    // What the write re-checks: the blob the patch was built against must
    // still be the blob the receiving side holds. Stage and discard aim at
    // the index or the worktree over the unstaged pair's old side — the
    // index; an unstage rewrites the index from HEAD's side, so both ends
    // are checked; staging an untracked file requires the index to still
    // have *no* entry under the name. An OID a pair does not carry — a
    // gitlink, a null side — is one that cannot be re-read, so it expects
    // nothing and `git apply`'s own refusal stands guard instead.
    let mut expect = Vec::new();
    match (verb, ask.side) {
        (Verb::Stage, HunkSide::Unstaged) | (Verb::Discard, HunkSide::Unstaged) => {
            expect.push((OidSide::Index, pair.old_oid.clone()))
        }
        (Verb::Unstage, HunkSide::Staged) => {
            expect.push((OidSide::Head, pair.old_oid.clone()));
            expect.push((OidSide::Index, pair.new_oid.clone()));
        }
        _ => unreachable!("every eligible shape is matched above"),
    }
    let verb_name = match verb {
        Verb::Stage => "stage",
        Verb::Unstage => "unstage",
        Verb::Discard => "discard",
    };
    let patch_verb = match verb {
        Verb::Stage => PatchVerb::Stage,
        Verb::Unstage => PatchVerb::Unstage,
        Verb::Discard => PatchVerb::Discard,
    };
    Ok(Box::new(CheckedPatch {
        name: format!("{verb_name} patch: {}", ask.path),
        repo: Handle::clone(repo),
        verb: patch_verb,
        patch,
        expect: expect
            .into_iter()
            .map(|(side, oid)| ExpectOid {
                side,
                path: ask.path.clone().into_bytes(),
                oid,
            })
            .collect(),
    }))
}

/// What [`hunk_job`]'s write applies, once its expectations hold.
enum PatchVerb {
    Stage,
    Unstage,
    Discard,
}

/// Which side of the index an expectation reads.
#[derive(Clone, Copy)]
enum OidSide {
    Index,
    Head,
}

/// One identity the write re-checks: the side, the path, and the blob the
/// patch was built against.
struct ExpectOid {
    side: OidSide,
    path: Vec<u8>,
    oid: Option<String>,
}

/// A patch whose assumptions are checked at write time, not build time.
///
/// The gap between the keypress that built a patch and the queue's turn at
/// it is real: another job, another process, another minute. Before one
/// byte applies, every blob the patch was built against is re-read and
/// compared; a mismatch refuses with the refresh spelled out rather than
/// letting `git apply` aim at whatever the side holds now.
struct CheckedPatch {
    name: String,
    repo: Handle,
    verb: PatchVerb,
    patch: Vec<u8>,
    expect: Vec<ExpectOid>,
}

impl Job for CheckedPatch {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(self: Box<Self>) -> Result<(), String> {
        let repo = self.repo.as_ref();
        for e in &self.expect {
            let now = match e.side {
                OidSide::Index => repo.index_blob_oid(&e.path)?,
                OidSide::Head => repo.head_blob_oid(&e.path)?,
            };
            if now != e.oid {
                return Err(format!(
                    "{} changed since the patch was read — refresh (R) and try again",
                    String::from_utf8_lossy(&e.path)
                ));
            }
        }
        match self.verb {
            PatchVerb::Stage => repo.stage_patch(&self.patch),
            PatchVerb::Unstage => repo.unstage_patch(&self.patch),
            PatchVerb::Discard => repo.discard_patch(&self.patch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::refs::RefName;
    use gitten_core::status::Status;
    use gitten_core::Commit;
    use gitten_git::{Pair, Repo};
    use std::collections::VecDeque;
    use std::sync::{mpsc, Arc};

    struct EmptyRepo(mpsc::Sender<String>);

    impl Repo for EmptyRepo {
        fn log(&self, _: usize) -> gitten_git::Result<Vec<Commit>> {
            Ok(Vec::new())
        }

        fn pairs(&self, _: &str) -> gitten_git::Result<Vec<Pair>> {
            Ok(Vec::new())
        }

        fn status(&self) -> gitten_git::Result<Status> {
            Ok(Status::default())
        }

        fn commit(&self, message: &str) -> gitten_git::Result<String> {
            self.0.send(format!("commit:{message}")).unwrap();
            Ok("commit".into())
        }
        fn amend(&self, message: &str) -> gitten_git::Result<String> {
            self.0.send(format!("amend:{message}")).unwrap();
            Ok("amend".into())
        }

        fn describe(&self) -> String {
            "empty".into()
        }
    }

    struct Fake {
        target: Option<Target>,
        selected: Option<SelectedFile>,
        cursor: Option<FileSection>,
        paths: [Vec<PathBytes>; 4],
        repo: Option<Handle>,
        records: mpsc::Receiver<String>,
        confirmations: VecDeque<bool>,
        confirm_calls: usize,
        said: Vec<String>,
        asked: Vec<String>,
        events: Vec<String>,
        jobs: Vec<String>,
        submit_ok: bool,
        run_jobs: bool,
    }

    impl Fake {
        fn with(target: Option<Target>) -> Self {
            let (record, records) = mpsc::channel();
            Self {
                target,
                selected: None,
                cursor: None,
                paths: Default::default(),
                repo: Some(Arc::new(EmptyRepo(record))),
                records,
                confirmations: VecDeque::new(),
                confirm_calls: 0,
                said: Vec::new(),
                asked: Vec::new(),
                events: Vec::new(),
                jobs: Vec::new(),
                submit_ok: true,
                run_jobs: false,
            }
        }

        fn file(section: FileSection, path: &[u8], shown: &str) -> SelectedFile {
            SelectedFile {
                section,
                path: PathBytes::from_bytes(path),
                shown: shown.into(),
            }
        }

        fn path_index(section: FileSection) -> usize {
            match section {
                FileSection::Staged => 0,
                FileSection::Unstaged => 1,
                FileSection::Untracked => 2,
                FileSection::Conflicts => 3,
            }
        }

        fn set_paths(&mut self, section: FileSection, paths: &[&[u8]]) {
            self.paths[Self::path_index(section)] = paths
                .iter()
                .map(|path| PathBytes::from_bytes(path))
                .collect();
        }
    }

    impl Client for Fake {
        fn say(&mut self, message: String) {
            self.events.push(format!("say:{message}"));
            self.said.push(message);
        }

        fn ask(&mut self, question: String) {
            self.events.push(format!("ask:{question}"));
            self.asked.push(question);
        }

        fn repo(&self) -> Option<Handle> {
            self.repo.clone()
        }

        fn submit(&mut self, job: Box<dyn Job>) -> bool {
            self.events.push(format!("submit:{}", job.name()));
            self.jobs.push(job.name().to_string());
            if self.run_jobs {
                job.run().unwrap();
            }
            self.submit_ok
        }
    }

    impl BranchClient for Fake {
        fn branch_target(&self) -> Option<Target> {
            self.target.clone()
        }

        fn confirm_or_arm_branch(&mut self, _: &Target) -> bool {
            self.confirm_calls += 1;
            self.confirmations.pop_front().unwrap_or(false)
        }
    }

    impl FileClient for Fake {
        fn selected_file(&self) -> Option<SelectedFile> {
            self.selected.clone()
        }

        fn cursor_section(&self) -> Option<FileSection> {
            self.cursor
        }

        fn paths_in(&self, section: FileSection) -> Vec<PathBytes> {
            self.paths[Self::path_index(section)].clone()
        }

        fn confirm_or_arm_file(&mut self, _: &SelectedFile) -> bool {
            self.confirm_calls += 1;
            self.confirmations.pop_front().unwrap_or(false)
        }
    }

    #[test]
    fn deleting_with_nothing_selected_refuses_in_words() {
        let mut client = Fake::with(None);
        delete_branch(&mut client);
        assert_eq!(client.said, ["nothing selected to delete"]);
        assert!(client.jobs.is_empty());
    }

    #[test]
    fn a_detached_head_is_refused_before_a_remote_is() {
        let mut client = Fake::with(Some(Target::Detached));
        delete_branch(&mut client);
        assert_eq!(client.said, ["a detached HEAD is not a branch"]);
        assert!(client.jobs.is_empty());
    }

    #[test]
    fn a_remote_branch_is_refused() {
        let mut client = Fake::with(Some(Target::Remote {
            remote: RefName::from("origin"),
            branch: RefName::from("main"),
        }));
        delete_branch(&mut client);
        assert_eq!(
            client.said,
            ["a remote branch is its remote's to delete — fetch prunes it here"]
        );
        assert!(client.jobs.is_empty());
    }

    #[test]
    fn the_first_press_asks_and_the_second_deletes() {
        let mut client = Fake::with(Some(Target::Local(RefName::from("feature"))));
        client.confirmations = [false, true].into();
        delete_branch(&mut client);
        assert_eq!(
            client.asked,
            ["delete branch feature? press again to confirm"]
        );
        assert!(client.jobs.is_empty());
        delete_branch(&mut client);
        assert_eq!(client.asked.len(), 1);
        assert_eq!(client.jobs, ["delete branch feature"]);
    }

    #[test]
    fn a_fixture_refuses_before_it_spends_the_arm() {
        let mut client = Fake::with(Some(Target::Local(RefName::from("feature"))));
        client.repo = None;
        client.confirmations.push_back(true);
        delete_branch(&mut client);
        assert_eq!(
            client.said,
            ["a fixture has no repository to delete branches from"]
        );
        assert_eq!(client.confirm_calls, 0);
        assert!(client.jobs.is_empty());
    }

    #[test]
    fn stage_selected_chooses_the_job_from_its_section() {
        let mut none = Fake::with(None);
        stage_or_unstage(&mut none);
        assert_eq!(none.said, ["nothing selected to stage"]);
        assert!(none.jobs.is_empty());

        for (section, expected) in [
            (FileSection::Staged, "unstage file"),
            (FileSection::Unstaged, "stage file"),
            (FileSection::Untracked, "stage file"),
            (FileSection::Conflicts, "stage file"),
        ] {
            let mut client = Fake::with(None);
            client.selected = Some(Fake::file(section, b"file", "file"));
            stage_or_unstage(&mut client);
            assert_eq!(client.jobs, [expected]);
            assert!(client.said.is_empty());
        }
    }

    #[test]
    fn stage_all_chooses_cursor_side_targets_and_refuses_empty_sets() {
        let mut staging = Fake::with(None);
        staging.cursor = Some(FileSection::Unstaged);
        staging.set_paths(FileSection::Unstaged, &[b"modified"]);
        staging.set_paths(FileSection::Untracked, &[b"new"]);
        staging.set_paths(FileSection::Conflicts, &[b"conflict"]);
        stage_all(&mut staging);
        assert_eq!(staging.jobs, ["stage 2 paths"]);

        let mut unstaging = Fake::with(None);
        unstaging.cursor = Some(FileSection::Staged);
        unstaging.set_paths(FileSection::Staged, &[b"one", b"two"]);
        unstaging.set_paths(FileSection::Unstaged, &[b"ignored"]);
        stage_all(&mut unstaging);
        assert_eq!(unstaging.jobs, ["unstage 2 paths"]);

        let mut no_stage = Fake::with(None);
        stage_all(&mut no_stage);
        assert_eq!(no_stage.said, ["nothing unstaged or untracked to stage"]);
        assert!(no_stage.jobs.is_empty());

        let mut no_unstage = Fake::with(None);
        no_unstage.cursor = Some(FileSection::Staged);
        stage_all(&mut no_unstage);
        assert_eq!(no_unstage.said, ["nothing staged to unstage"]);
        assert!(no_unstage.jobs.is_empty());
    }

    #[test]
    fn discard_refuses_before_confirmation_in_the_existing_order() {
        let mut none = Fake::with(None);
        discard_file(&mut none);
        assert_eq!(none.events, ["say:nothing selected to discard"]);
        assert_eq!(none.confirm_calls, 0);
        assert!(none.jobs.is_empty());

        for (section, refusal) in [
            (
                FileSection::Staged,
                "that change is staged — unstage it before discarding",
            ),
            (
                FileSection::Conflicts,
                "a conflicted file needs its merge resolved, not discarded",
            ),
        ] {
            let mut client = Fake::with(None);
            client.selected = Some(Fake::file(section, b"file", "file"));
            discard_file(&mut client);
            assert_eq!(client.said, [refusal]);
            assert_eq!(client.confirm_calls, 0);
            assert!(client.jobs.is_empty());
        }
    }

    #[test]
    fn discard_asks_then_uses_discard_or_delete_for_the_same_target() {
        for (section, question, job) in [
            (
                FileSection::Unstaged,
                "discard shown path? press again to confirm",
                "discard raw-path",
            ),
            (
                FileSection::Untracked,
                "delete shown path? press again to confirm",
                "delete raw-path",
            ),
        ] {
            let mut client = Fake::with(None);
            client.selected = Some(Fake::file(section, b"raw-path", "shown path"));
            client.confirmations = [false, true].into();
            discard_file(&mut client);
            assert_eq!(client.asked, [question]);
            assert!(client.jobs.is_empty());
            discard_file(&mut client);
            assert_eq!(
                client.events,
                [format!("ask:{question}"), format!("submit:{job}")]
            );
            assert_eq!(client.jobs, [job]);
        }
    }

    #[test]
    fn ignore_accepts_only_an_untracked_selection() {
        for selected in [
            None,
            Some(Fake::file(FileSection::Staged, b"tracked", "tracked")),
        ] {
            let mut client = Fake::with(None);
            client.selected = selected;
            ignore_file(&mut client);
            assert_eq!(client.said, ["only an untracked file can be ignored"]);
            assert!(client.jobs.is_empty());
        }

        let mut client = Fake::with(None);
        client.selected = Some(Fake::file(FileSection::Untracked, b"new", "new"));
        ignore_file(&mut client);
        assert_eq!(client.jobs, ["ignore new"]);
    }

    #[test]
    fn stash_commit_and_amend_build_the_named_jobs() {
        let mut stash = Fake::with(None);
        stash_working_tree(&mut stash);
        assert_eq!(stash.jobs, ["stash push"]);

        let mut commit = Fake::with(None);
        commit_message(&mut commit, "message".into());
        assert_eq!(commit.jobs, ["commit"]);

        let mut amend = Fake::with(None);
        amend_message(&mut amend, "message".into());
        assert_eq!(amend.jobs, ["amend"]);
    }

    #[test]
    fn commit_and_amend_reject_empty_text_but_preserve_nonempty_text() {
        for message in ["", " \t\n "] {
            let mut commit = Fake::with(None);
            commit_message(&mut commit, message.into());
            assert_eq!(commit.said, ["a commit needs a message"]);
            assert!(commit.jobs.is_empty());

            let mut amend = Fake::with(None);
            amend_message(&mut amend, message.into());
            assert_eq!(amend.said, ["a commit needs a message"]);
            assert!(amend.jobs.is_empty());
        }

        let mut client = Fake::with(None);
        client.run_jobs = true;
        commit_message(&mut client, "  keep commit space  ".into());
        amend_message(&mut client, "  keep amend space  ".into());
        assert_eq!(
            client.records.try_iter().collect::<Vec<_>>(),
            ["commit:  keep commit space  ", "amend:  keep amend space  "]
        );
    }

    #[test]
    fn every_repository_action_preserves_its_fixture_refusal() {
        let mut stage = Fake::with(None);
        stage.selected = Some(Fake::file(FileSection::Unstaged, b"file", "file"));
        stage.repo = None;
        stage_or_unstage(&mut stage);
        assert_eq!(stage.said, ["a fixture has no working tree to stage in"]);

        let mut all = Fake::with(None);
        all.set_paths(FileSection::Unstaged, &[b"file"]);
        all.repo = None;
        stage_all(&mut all);
        assert_eq!(all.said, ["a fixture has no working tree to act on"]);

        let mut discard = Fake::with(None);
        discard.selected = Some(Fake::file(FileSection::Unstaged, b"file", "file"));
        discard.confirmations.push_back(true);
        discard.repo = None;
        discard_file(&mut discard);
        assert_eq!(
            discard.said,
            ["a fixture has no working tree to discard from"]
        );
        assert_eq!(discard.confirm_calls, 0);

        let mut ignore = Fake::with(None);
        ignore.selected = Some(Fake::file(FileSection::Untracked, b"file", "file"));
        ignore.repo = None;
        ignore_file(&mut ignore);
        assert_eq!(ignore.said, ["a fixture has no repository to ignore in"]);

        let mut stash = Fake::with(None);
        stash.repo = None;
        stash_working_tree(&mut stash);
        assert_eq!(stash.said, ["a fixture has no working tree to park"]);

        let mut commit = Fake::with(None);
        commit.repo = None;
        commit_message(&mut commit, "message".into());
        assert_eq!(commit.said, ["a fixture has no repository to commit in"]);

        let mut amend = Fake::with(None);
        amend.repo = None;
        amend_message(&mut amend, "message".into());
        assert_eq!(amend.said, ["a fixture has no repository to amend in"]);
    }

    #[test]
    fn every_working_tree_action_reports_queue_rejection() {
        fn assert_shutdown(client: &Fake) {
            assert_eq!(client.said, ["the job queue is shutting down"]);
            assert_eq!(
                client.events.last().unwrap(),
                "say:the job queue is shutting down"
            );
            assert_eq!(client.jobs.len(), 1);
        }

        let mut stage = Fake::with(None);
        stage.selected = Some(Fake::file(FileSection::Unstaged, b"file", "file"));
        stage.submit_ok = false;
        stage_or_unstage(&mut stage);
        assert_shutdown(&stage);

        let mut all = Fake::with(None);
        all.set_paths(FileSection::Unstaged, &[b"file"]);
        all.submit_ok = false;
        stage_all(&mut all);
        assert_shutdown(&all);

        let mut discard = Fake::with(None);
        discard.selected = Some(Fake::file(FileSection::Unstaged, b"file", "file"));
        discard.confirmations.push_back(true);
        discard.submit_ok = false;
        discard_file(&mut discard);
        assert_shutdown(&discard);

        let mut ignore = Fake::with(None);
        ignore.selected = Some(Fake::file(FileSection::Untracked, b"file", "file"));
        ignore.submit_ok = false;
        ignore_file(&mut ignore);
        assert_shutdown(&ignore);

        let mut stash = Fake::with(None);
        stash.submit_ok = false;
        stash_working_tree(&mut stash);
        assert_shutdown(&stash);

        let mut commit = Fake::with(None);
        commit.submit_ok = false;
        commit_message(&mut commit, "message".into());
        assert_shutdown(&commit);

        let mut amend = Fake::with(None);
        amend.submit_ok = false;
        amend_message(&mut amend, "message".into());
        assert_shutdown(&amend);
    }

    // ----------------------------------------------------------- hunk jobs

    use std::sync::Mutex;

    /// A repository that exists only as this struct, for the hunk policy:
    /// the side reads answer what the test handed in, the OID reads answer
    /// the identity the test set, and the three patch verbs record the
    /// bytes they were asked to apply. Nothing here runs git — the same
    /// shape `tui`'s and `verbs`' fakes take.
    #[derive(Default)]
    struct HunkState {
        staged: Vec<gitten_git::Pair>,
        unstaged: Vec<gitten_git::Pair>,
        untracked: Option<gitten_git::Pair>,
        index_oids: Vec<(String, String)>,
        head_oids: Vec<(String, String)>,
        writes: Vec<String>,
    }

    struct HunkFake(Arc<Mutex<HunkState>>);

    impl gitten_git::Repo for HunkFake {
        fn log(&self, _limit: usize) -> gitten_git::Result<Vec<gitten_core::Commit>> {
            Ok(Vec::new())
        }

        fn pairs(&self, _revspec: &str) -> gitten_git::Result<Vec<gitten_git::Pair>> {
            Ok(Vec::new())
        }

        fn status(&self) -> gitten_git::Result<gitten_core::status::Status> {
            Ok(Default::default())
        }

        fn describe(&self) -> String {
            "hunk fake".into()
        }

        fn pairs_staged(&self, path: Option<&[u8]>) -> gitten_git::Result<Vec<gitten_git::Pair>> {
            let s = self.0.lock().unwrap();
            Ok(match path {
                Some(p) => s
                    .staged
                    .iter()
                    .filter(|c| c.path.as_bytes() == p)
                    .cloned()
                    .collect(),
                None => s.staged.clone(),
            })
        }

        fn pairs_unstaged(&self, path: Option<&[u8]>) -> gitten_git::Result<Vec<gitten_git::Pair>> {
            let s = self.0.lock().unwrap();
            Ok(match path {
                Some(p) => s
                    .unstaged
                    .iter()
                    .filter(|c| c.path.as_bytes() == p)
                    .cloned()
                    .collect(),
                None => s.unstaged.clone(),
            })
        }

        fn pair_untracked(&self, path: &[u8]) -> gitten_git::Result<Option<gitten_git::Pair>> {
            let s = self.0.lock().unwrap();
            Ok(s.untracked
                .as_ref()
                .filter(|p| p.path.as_bytes() == path)
                .cloned())
        }

        fn index_blob_oid(&self, path: &[u8]) -> gitten_git::Result<Option<String>> {
            let shown = String::from_utf8_lossy(path).into_owned();
            Ok(self
                .0
                .lock()
                .unwrap()
                .index_oids
                .iter()
                .find(|(p, _)| p == &shown)
                .map(|(_, o)| o.clone()))
        }

        fn head_blob_oid(&self, path: &[u8]) -> gitten_git::Result<Option<String>> {
            let shown = String::from_utf8_lossy(path).into_owned();
            Ok(self
                .0
                .lock()
                .unwrap()
                .head_oids
                .iter()
                .find(|(p, _)| p == &shown)
                .map(|(_, o)| o.clone()))
        }

        fn stage_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .writes
                .push(format!("stage {}", String::from_utf8_lossy(patch)));
            Ok(())
        }

        fn unstage_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .writes
                .push(format!("unstage {}", String::from_utf8_lossy(patch)));
            Ok(())
        }

        fn discard_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .writes
                .push(format!("discard {}", String::from_utf8_lossy(patch)));
            Ok(())
        }
    }

    /// One unstaged pair with an identity — the index holds `i0`, the
    /// worktree replaces the third line — and the hunk the pipeline draws
    /// from it, via the same `diff_pairs` the verb re-reads with, so the
    /// selection matches by construction.
    fn unstaged_fixture() -> (Arc<HunkFake>, Hunk) {
        let pair = gitten_git::Pair {
            path: "f.txt".into(),
            old_path: None,
            status: 'M',
            old: vec![
                Arc::<str>::from("alpha"),
                Arc::<str>::from("keep one"),
                Arc::<str>::from("keep two"),
            ],
            new: vec![
                Arc::<str>::from("alpha"),
                Arc::<str>::from("keep one"),
                Arc::<str>::from("WORKTREE TWO"),
            ],
            old_oid: Some("i0".into()),
            new_oid: None,
            old_final_newline: true,
            new_final_newline: true,
            binary: false,
        };
        let fake = Arc::new(HunkFake(Arc::new(Mutex::new(HunkState {
            unstaged: vec![pair],
            index_oids: vec![("f.txt".into(), "i0".into())],
            ..Default::default()
        }))));
        let differs = gitten_core::differ::Differs::builtin();
        let staged = fake.pairs_unstaged(Some(b"f.txt")).expect("the side");
        let files = gitten_git::diff_pairs(
            &staged,
            &differs,
            &gitten_core::differ::Overrides::default(),
        );
        let hunk = files[0].hunks[0].clone();
        (fake, hunk)
    }

    fn run_hunk(
        repo: &gitten_git::Handle,
        command: &str,
        side: HunkSide,
        selection: HunkSelection,
        path: &str,
    ) -> Result<(), String> {
        let differs = gitten_core::differ::Differs::builtin();
        let job = hunk_job(
            command,
            HunkAsk {
                path: path.into(),
                side,
                selection,
            },
            repo,
            &differs,
            &Default::default(),
        )?;
        job.run()
    }

    #[test]
    fn a_hunk_verb_is_refused_where_it_means_nothing() {
        let (fake, hunk) = unstaged_fixture();
        let whole = HunkSelection::Whole(hunk.clone());
        for (command, side, fragment) in [
            (
                "diff.stage-hunk",
                HunkSide::Combined,
                "combined view folds both sides",
            ),
            (
                "diff.unstage-hunk",
                HunkSide::Combined,
                "combined view folds both sides",
            ),
            ("diff.stage-hunk", HunkSide::Staged, "already staged"),
            (
                "diff.unstage-hunk",
                HunkSide::Unstaged,
                "tab opens the staged side",
            ),
            (
                "diff.unstage-hunk",
                HunkSide::Untracked,
                "nothing in the index",
            ),
            ("diff.stage-hunk", HunkSide::Untracked, "stages whole"),
            (
                "diff.discard-hunk",
                HunkSide::Staged,
                "no working tree to discard",
            ),
            ("diff.discard-hunk", HunkSide::Untracked, "all or nothing"),
            ("diff.nope-hunk", HunkSide::Unstaged, "is not a hunk verb"),
        ] {
            let repo: gitten_git::Handle = fake.clone();
            let err = run_hunk(&repo, command, side, whole.clone(), "f.txt").expect_err("refused");
            assert!(err.contains(fragment), "{command} on {side:?}: {err}");
            assert!(
                fake.0.lock().unwrap().writes.is_empty(),
                "a refusal queued a write"
            );
        }
    }

    #[test]
    fn a_stage_is_built_from_the_unstaged_side_and_checked_at_write() {
        let (fake, hunk) = unstaged_fixture();
        let repo: gitten_git::Handle = fake.clone();
        run_hunk(
            &repo,
            "diff.stage-hunk",
            HunkSide::Unstaged,
            HunkSelection::Whole(hunk.clone()),
            "f.txt",
        )
        .expect("the hunk stages");
        let writes = fake.0.lock().unwrap().writes.clone();
        assert_eq!(writes.len(), 1, "{writes:?}");
        assert!(writes[0].starts_with("stage "), "{writes:?}");
        // The patch says exactly the hunk: the change, its context, no
        // neighbour.
        let applied = gitten_core::parse_unified_diff(&writes[0]["stage ".len()..]);
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].path, "f.txt");
        assert_eq!(applied[0].hunks.len(), 1);
        assert_eq!(applied[0].hunks[0].lines, hunk.lines);
    }

    #[test]
    fn a_drifted_index_refuses_the_write() {
        let (fake, hunk) = unstaged_fixture();
        // The patch was built against i0; the index now holds something
        // else. The write checks before it aims.
        fake.0.lock().unwrap().index_oids = vec![("f.txt".into(), "moved-on".into())];
        let repo: gitten_git::Handle = fake.clone();
        let err = run_hunk(
            &repo,
            "diff.stage-hunk",
            HunkSide::Unstaged,
            HunkSelection::Whole(hunk),
            "f.txt",
        )
        .expect_err("the drifted write refuses");
        assert!(err.contains("changed since the patch was read"), "{err}");
        assert!(
            fake.0.lock().unwrap().writes.is_empty(),
            "a refused write applied nothing"
        );
    }

    #[test]
    fn a_stale_selection_refuses_before_anything_is_read_for_the_write() {
        let (fake, hunk) = unstaged_fixture();
        // The drawn hunk names a change the fresh pair no longer shows:
        // the fake's content moved between the preview and the keypress.
        fake.0.lock().unwrap().unstaged[0].new[2] = Arc::from("MOVED ON");
        let repo: gitten_git::Handle = fake.clone();
        let err = run_hunk(
            &repo,
            "diff.stage-hunk",
            HunkSide::Unstaged,
            HunkSelection::Whole(hunk),
            "f.txt",
        )
        .expect_err("the stale selection refuses");
        assert!(err.contains("changed since this diff was drawn"), "{err}");
        assert!(
            fake.0.lock().unwrap().writes.is_empty(),
            "a stale selection queued nothing"
        );
    }

    #[test]
    fn a_line_selection_stages_only_the_marked_lines() {
        // One added line chosen out of a hunk that also removes a line:
        // the removal is unchosen, so it stays in the index as context and
        // the patch inserts the addition alone.
        let (fake, _) = unstaged_fixture();
        // Widen the fixture to a replacement: the hunk now removes and adds.
        {
            let mut s = fake.0.lock().unwrap();
            let pair = &mut s.unstaged[0];
            pair.new[2] = Arc::from("WORKTREE TWO");
            pair.old[2] = Arc::from("keep two");
        }
        let differs = gitten_core::differ::Differs::builtin();
        let staged = fake.pairs_unstaged(Some(b"f.txt")).expect("the side");
        let files = gitten_git::diff_pairs(
            &staged,
            &differs,
            &gitten_core::differ::Overrides::default(),
        );
        let hunk = files[0].hunks[0].clone();
        let plus = hunk
            .lines
            .iter()
            .position(|l| l.kind == gitten_core::LineKind::Added)
            .expect("the addition");
        let repo: gitten_git::Handle = fake.clone();
        run_hunk(
            &repo,
            "diff.stage-hunk",
            HunkSide::Unstaged,
            HunkSelection::Lines(vec![(hunk, plus, plus)]),
            "f.txt",
        )
        .expect("the line stages");
        let writes = fake.0.lock().unwrap().writes.clone();
        let applied = gitten_core::parse_unified_diff(&writes[0]["stage ".len()..]);
        let lines = &applied[0].hunks[0].lines;
        let changed: Vec<&str> = lines
            .iter()
            .filter(|l| l.kind != gitten_core::LineKind::Context)
            .map(|l| l.text.as_ref())
            .collect();
        assert_eq!(changed, ["WORKTREE TWO"], "only the marked line travels");
        let kinds: Vec<gitten_core::LineKind> = lines.iter().map(|l| l.kind).collect();
        assert!(
            kinds.contains(&gitten_core::LineKind::Context),
            "the unchosen removal rides as context, keeping the preimage whole: {kinds:?}"
        );
    }
}
