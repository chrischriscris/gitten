//! The command line every client shares.
//!
//! `gitten-shell diff . HEAD~2..HEAD` and `gitten-web diff . HEAD~2..HEAD` and
//! `gitten-tui diff . HEAD~2..HEAD` are the same words in the same order, and
//! that is a promise rather than a coincidence: a client is a way of *looking*
//! at a repository, not a different tool, so the thing you type to reach one
//! should reach any of them.
//!
//! It was written twice before it was written once. `gitten-shell` and
//! `gitten-web` each had their own `USAGE`, their own `Source`, their own
//! `--fixtures` arm; the two drifted in their error messages within a week of
//! each other.
//!
//! # A client's own flags
//!
//! Taken out first, then the rest reads positionally — which is what `--port`
//! already did by hand. [`take_value`] and [`take_switch`] are that loop, so a
//! flag can appear anywhere on the line rather than only where a positional
//! parser happens to look for it.

use std::path::PathBuf;

/// Which view was asked for.
///
/// Two, because there are two. When there is a third this grows a variant and
/// every client's `match` stops compiling, which is the point of it being an
/// enum rather than the string it is parsed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Commits,
    Diff,
}

impl View {
    pub fn name(self) -> &'static str {
        match self {
            View::Commits => "commits",
            View::Diff => "diff",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "commits" => Some(View::Commits),
            "diff" => Some(View::Diff),
            _ => None,
        }
    }
}

/// Where the data comes from.
///
/// `Fixtures` is not a debugging convenience bolted on the side — it is how
/// every client is exercised at pathological scale without a repository that
/// has those shapes in it, and `./check.sh` runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Repo {
        path: PathBuf,
        /// A revspec for `diff`, a commit limit for `commits`. One field
        /// because it is one positional argument, and what it means is the
        /// view's business.
        arg: String,
    },
    /// A patch, not a repository: `-` reads standard input, `--patch FILE`
    /// names a file. The patch *is* the whole changeset — no revspec, no
    /// second positional, which is why a word left over beside it is
    /// [`Request::Help`] rather than an argument nobody noticed.
    Patch {
        /// `None` for `-`.
        file: Option<PathBuf>,
    },
    Fixtures,
}

impl Source {
    /// A stable string naming this exact view of this exact source.
    ///
    /// What a saved reading position is keyed on, so a position taken in one
    /// diff is never restored into another — see the shell's `session.rs`.
    pub fn key(&self, view: View) -> String {
        match self {
            Source::Repo { path, arg } => {
                format!("{}:{}:{arg}", view.name(), path.to_string_lossy())
            }
            Source::Patch { file: Some(path) } => {
                format!("{}:{}:", view.name(), path.to_string_lossy())
            }
            Source::Patch { file: None } => format!("{}:-:", view.name()),
            Source::Fixtures => format!("{}:--fixtures:", view.name()),
        }
    }
}

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// `-h`, `--help`, or nothing a client understood.
    Help,
    /// `gitten config` — print the current host as TOML. A complete, correct
    /// starting file rather than a page of documentation to copy out of.
    Config,
    Open {
        view: View,
        source: Source,
    },
}

/// Reads the shared positional shape, after a client has taken its own flags.
///
/// `default` is the view a client opens with nothing typed — the window opens on
/// the graph, and something whose reason to exist is one diff may sensibly open
/// on a diff.
///
/// **A word that is not a view is [`Request::Help`]**, not a repository. The
/// tempting alternative is to let the view word be optional so `gitten .` means
/// `gitten diff .`, and it costs more than it gives: `gitten dfif .` then shows
/// the default view of a repository called `dfif` and looks like it worked. A
/// typo and a request for help want the same answer.
pub fn parse(args: &[String], default: View) -> Request {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        return Request::Help;
    }
    // `--patch` names a source rather than adjusting one, so it belongs to
    // this parse and not to any client: the same words name a patch in a
    // window, a browser tab and a terminal. Taken out first, so it sits where
    // the line wants it — beside every other flag.
    let mut rest = args.to_vec();
    let patch = match take_value(&mut rest, "--patch") {
        Ok(Some(v)) => Some(v),
        Ok(None) => None,
        Err(_) => return Request::Help,
    };
    let (view, tail): (View, &[String]) = match rest.first().map(String::as_str) {
        None => (default, &[]),
        Some("config") => return Request::Config,
        Some(word) => match View::parse(word) {
            Some(v) => (v, &rest[1..]),
            None => return Request::Help,
        },
    };
    let positional: Option<&str> = tail.first().map(String::as_str);
    let source = match (patch.as_deref(), positional) {
        // A patch takes no revspec — anything left over beside it is a word
        // nobody could have meant. `--patch -` and a bare `-` are both stdin.
        (Some("-"), None | Some("-")) => Source::Patch { file: None },
        (Some(file), None | Some("-")) => Source::Patch {
            file: Some(PathBuf::from(file)),
        },
        (Some(_), Some(_)) => return Request::Help,
        (None, Some("-")) => Source::Patch { file: None },
        (None, Some("--fixtures")) => Source::Fixtures,
        (None, Some(path)) => {
            // `-` past the repository slot cannot be meant: stdin has no
            // revspec to pair with, so `diff main -` is help and not a
            // revision called `-` that fails somewhere inside git.
            if tail.get(1).map(String::as_str) == Some("-") {
                return Request::Help;
            }
            Source::Repo {
                path: PathBuf::from(path),
                arg: tail.get(1).cloned().unwrap_or_default(),
            }
        }
        (None, None) => Source::Repo {
            path: PathBuf::from("."),
            arg: tail.get(1).cloned().unwrap_or_default(),
        },
    };
    Request::Open { view, source }
}

/// Removes `--name VALUE` from `args` and returns the value.
///
/// `Err` when the flag is there and the value is not, which is a different
/// mistake from the flag being absent and deserves a different message.
pub fn take_value(args: &mut Vec<String>, name: &str) -> Result<Option<String>, String> {
    let Some(at) = args.iter().position(|a| a == name) else {
        return Ok(None);
    };
    if at + 1 >= args.len() {
        return Err(format!("{name} wants a value"));
    }
    args.remove(at);
    Ok(Some(args.remove(at)))
}

/// Removes `--name` from `args` and says whether it was there.
pub fn take_switch(args: &mut Vec<String>, name: &str) -> bool {
    match args.iter().position(|a| a == name) {
        Some(at) => {
            args.remove(at);
            true
        }
        None => false,
    }
}

/// The usage text, with the client's own lines folded in.
///
/// One shape for every client, because the arguments are one shape. `binary` is
/// what to call it, `blurb` is the one line under the name, and `extra` is
/// whatever only this client has — a `--port`, a title bar, an overlay.
pub fn usage(binary: &str, blurb: &str, extra: &str) -> String {
    let common = format!(
        "\
{binary} — {blurb}

  {binary} commits [REPO] [LIMIT]     history graph      (default: . , 5000)
  {binary} diff    [REPO] [REVSPEC]   a diff             (default: . , working tree)
  {binary} diff    --patch FILE       review a patch     (--patch - or diff - = stdin)
  {binary} config                     print the current theme and font as TOML

  REVSPEC is anything git takes:  HEAD~50..HEAD   main..feature   <sha>
  Pass --fixtures instead of REPO to read fixtures/ instead of a repository.
  A patch needs no checkout at all:  git diff | {binary} diff -

  gitten.toml next to the binary (or $GITTEN_CONFIG) picks the theme — dark, light
  or slate, or one it defines itself — and sets the font and the [diff] table:
  the algorithm, how much whitespace has to match, how much context, and what
  the presentation and the wrap open on. Every client reads the same file.
  Start one with:  {binary} config > gitten.toml
"
    );
    match extra.is_empty() {
        true => common,
        false => format!("{common}\n{}\n", extra.trim_end()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    fn parsed(line: &str) -> Request {
        parse(&args(line), View::Commits)
    }

    fn repo(path: &str, arg: &str) -> Source {
        Source::Repo {
            path: PathBuf::from(path),
            arg: arg.into(),
        }
    }

    #[test]
    fn the_shape_every_client_promises() {
        assert_eq!(
            parsed("diff . HEAD~2..HEAD"),
            Request::Open {
                view: View::Diff,
                source: repo(".", "HEAD~2..HEAD")
            }
        );
        assert_eq!(
            parsed("commits ~/src 500"),
            Request::Open {
                view: View::Commits,
                source: repo("~/src", "500")
            }
        );
    }

    #[test]
    fn everything_defaults_to_this_repository() {
        assert_eq!(
            parsed("diff"),
            Request::Open {
                view: View::Diff,
                source: repo(".", "")
            }
        );
        assert_eq!(
            parsed(""),
            Request::Open {
                view: View::Commits,
                source: repo(".", "")
            }
        );
        // A client whose reason to exist is one diff opens on one.
        assert_eq!(
            parse(&args(""), View::Diff),
            Request::Open {
                view: View::Diff,
                source: repo(".", "")
            }
        );
    }

    #[test]
    fn a_word_that_is_not_a_view_is_help_and_not_a_repository() {
        // The cost of letting the view word be optional: `gitten dfif .` would
        // show the default view of a repository called `dfif` and look right.
        assert_eq!(parse(&args(". HEAD~1..HEAD"), View::Diff), Request::Help);
        assert_eq!(parsed("dfif ."), Request::Help);
    }

    #[test]
    fn fixtures_is_a_source_and_not_a_repository_called_fixtures() {
        assert_eq!(
            parsed("diff --fixtures"),
            Request::Open {
                view: View::Diff,
                source: Source::Fixtures
            }
        );
    }

    #[test]
    fn a_patch_is_named_explicitly_and_never_guessed() {
        // A `.diff` suffix is not a rule: the word after `diff` means a
        // repository until `--patch` or `-` says otherwise, because a typo
        // must not look like success.
        assert_eq!(
            parsed("diff --patch pr30683.diff"),
            Request::Open {
                view: View::Diff,
                source: Source::Patch {
                    file: Some(PathBuf::from("pr30683.diff"))
                }
            }
        );
        assert_eq!(
            parsed("diff -"),
            Request::Open {
                view: View::Diff,
                source: Source::Patch { file: None }
            }
        );
        assert_eq!(
            parsed("diff --patch -"),
            Request::Open {
                view: View::Diff,
                source: Source::Patch { file: None }
            }
        );
    }

    #[test]
    fn a_patch_takes_no_revspec() {
        // Whatever is left over beside a patch is a word nobody could have
        // meant, and help is what a word nobody meant gets.
        assert_eq!(parsed("diff . --patch pr.diff"), Request::Help);
        assert_eq!(parsed("diff HEAD~1..HEAD -"), Request::Help);
        // And `--patch` with no value is its own mistake.
        assert_eq!(parsed("diff --patch"), Request::Help);
        // A patch is not history either; the parse stays syntactic and
        // acquisition says so — see acquire.
        assert_eq!(
            parsed("commits -"),
            Request::Open {
                view: View::Commits,
                source: Source::Patch { file: None }
            }
        );
    }

    #[test]
    fn a_patch_key_names_one_patch() {
        let file = Source::Patch {
            file: Some(PathBuf::from("a.diff")),
        };
        let stdin = Source::Patch { file: None };
        assert_eq!(file.key(View::Diff), file.key(View::Diff));
        assert_ne!(file.key(View::Diff), stdin.key(View::Diff));
        assert_ne!(
            file.key(View::Diff),
            repo(".", "HEAD~1..HEAD").key(View::Diff)
        );
        assert_ne!(stdin.key(View::Diff), Source::Fixtures.key(View::Diff));
    }

    #[test]
    fn help_and_a_typo_get_the_same_answer() {
        assert_eq!(parsed("--help"), Request::Help);
        assert_eq!(parsed("-h"), Request::Help);
        assert_eq!(parsed("diff . -h"), Request::Help);
    }

    #[test]
    fn config_is_answered_before_anything_is_acquired() {
        assert_eq!(parsed("config"), Request::Config);
        assert_eq!(parsed("config extra words"), Request::Config);
    }

    #[test]
    fn a_client_flag_can_appear_anywhere_on_the_line() {
        let mut a = args("diff --port 9000 . HEAD~1..HEAD");
        assert_eq!(
            take_value(&mut a, "--port").unwrap().as_deref(),
            Some("9000")
        );
        assert!(!take_switch(&mut a, "--stats"));
        assert_eq!(
            parse(&a, View::Diff),
            Request::Open {
                view: View::Diff,
                source: repo(".", "HEAD~1..HEAD")
            }
        );

        let mut b = args("diff . --stats");
        assert!(take_switch(&mut b, "--stats"));
        assert_eq!(
            parse(&b, View::Diff),
            Request::Open {
                view: View::Diff,
                source: repo(".", "")
            }
        );
    }

    #[test]
    fn a_flag_with_no_value_is_its_own_mistake() {
        let mut a = args("diff . --port");
        assert!(take_value(&mut a, "--port").is_err());
        assert!(take_value(&mut args("diff ."), "--port").unwrap().is_none());
    }

    #[test]
    fn a_session_key_names_one_view_of_one_source() {
        let d = repo(".", "HEAD~1..HEAD");
        assert_ne!(d.key(View::Diff), d.key(View::Commits));
        assert_ne!(d.key(View::Diff), repo(".", "HEAD~2..HEAD").key(View::Diff));
        assert_eq!(d.key(View::Diff), repo(".", "HEAD~1..HEAD").key(View::Diff));
        assert_ne!(Source::Fixtures.key(View::Diff), d.key(View::Diff));
    }

    #[test]
    fn the_usage_names_the_binary_it_was_asked_about() {
        let text = usage(
            "gitten-tui",
            "gitten in a terminal",
            "  --ascii   no box drawing",
        );
        assert!(text.starts_with("gitten-tui — gitten in a terminal"));
        assert!(text.contains("gitten-tui config > gitten.toml"));
        assert!(text.contains("--ascii"));
        // Every client documents the same two views and the same file.
        assert!(text.contains("commits [REPO] [LIMIT]"));
        assert!(text.contains("diff    [REPO] [REVSPEC]"));
        assert!(!usage("gitten-web", "b", "").ends_with("\n\n"));
    }
}
