//! A `git bisect` in progress, in the repository's own words.
//!
//! Bisecting is not an [`Operation`](crate::operation::Operation): no index
//! stages stand unmerged, no sequencer drives, and the tree is simply
//! checked out at whatever commit git is asking about. What a client needs
//! is the fact of it — read through the acquisition layer from the state
//! files git keeps, never modelled twice — so the status line can say the
//! checkout is a question, and the good/bad/skip verbs can gate on
//! something rather than on git's refusal alone.
//!
//! State files, all resolved through `rev-parse --git-path` so linked
//! worktrees answer for themselves: `BISECT_LOG` exists exactly while a
//! bisection stands; `BISECT_EXPECTED_REV` names the commit under test;
//! `BISECT_HEAD` names the revision the bisection started from; each line
//! of `BISECT_ANCESTORS_OK` names a commit already judged good.

/// A bisection standing right now, as the state files report it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BisectState {
    /// The commit under test — what `BISECT_EXPECTED_REV` names, full
    /// object id. The checkout should agree; when it does not, somebody
    /// moved while the question stood, and the verbs say so rather than
    /// marking the wrong commit.
    pub current: String,
    /// The revision the bisection started from — what `BISECT_HEAD` names.
    /// `reset` returns here, which is why the banner names it: the way
    /// back is part of the question.
    pub original: String,
    /// The commits already judged good, oldest judgement first.
    pub goods: Vec<String>,
}

impl BisectState {
    /// The word the status line uses beside the commit under test.
    pub fn word(&self) -> String {
        format!("bisecting {}", short(&self.current))
    }
}

/// Seven hex digits, the way git abbreviates when it has room — display
/// text only, never aimed at anything.
fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// Builds the state from the three files' contents: `None` unless the log
/// stands — a bisection is the log's existence, not the revs' — and empty
/// revs degrade to the words git wrote rather than to a failed read.
pub fn parse_bisect_state(
    log_present: bool,
    expected_rev: Option<&[u8]>,
    head: Option<&[u8]>,
    ancestors_ok: Option<&[u8]>,
) -> Option<BisectState> {
    if !log_present {
        return None;
    }
    let word = |raw: Option<&[u8]>| {
        String::from_utf8_lossy(raw.unwrap_or_default())
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let goods = ancestors_ok
        .map(|raw| {
            String::from_utf8_lossy(raw)
                .split_whitespace()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(BisectState {
        current: word(expected_rev),
        original: word(head),
        goods,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_log_is_no_bisect_whatever_else_lingers() {
        assert_eq!(
            parse_bisect_state(false, Some(b"abc"), Some(b"def"), None),
            None
        );
    }

    #[test]
    fn the_log_standing_names_current_original_and_goods() {
        let state = parse_bisect_state(
            true,
            Some(b"abc123\n"),
            Some(b"def456\n"),
            Some(b"aaa111\nbbb222\n"),
        )
        .expect("the log stands");
        assert_eq!(state.current, "abc123");
        assert_eq!(state.original, "def456");
        assert_eq!(state.goods, vec!["aaa111", "bbb222"]);
        assert_eq!(state.word(), "bisecting abc123");
    }

    #[test]
    fn missing_revs_degrade_to_empty_words_not_a_failed_read() {
        let state = parse_bisect_state(true, None, None, None).expect("the log stands");
        assert_eq!(state.current, "");
        assert_eq!(state.original, "");
        assert!(state.goods.is_empty());
    }
}
