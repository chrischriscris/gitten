//! Commit-list search: fold once at load, substring per keystroke.
//!
//! Filtering is a pure function of a query and already-loaded commits — no
//! I/O, no dependencies, no UI — because every client that shows a commit list
//! wants the same answer for `/`, and a filter decided in a renderer could not
//! be tested without opening one.
//!
//! The shape is an index rather than a free function for rule 3's sake:
//! lowercasing three fields of 82k commits costs real time, and doing it per
//! keystroke would put that cost on the render path where a cache holds it.
//! So [`Index::new`] folds once, beside the rest of what `prepare` derives at
//! load (it runs on the background thread during refresh), and a keystroke
//! folds only its own needle and scans.

use crate::Commit;

/// One commit's search text, folded.
#[derive(Debug, Clone)]
struct Row {
    sha: String,
    author: String,
    subject: String,
}

/// A commit list prepared for substring search.
#[derive(Debug, Clone)]
pub struct Index {
    rows: Vec<Row>,
}

impl Index {
    /// Folds the sha, author and subject of every commit. Once per data load,
    /// never per keystroke.
    pub fn new(commits: &[Commit]) -> Self {
        Self {
            rows: commits
                .iter()
                .map(|c| Row {
                    sha: c.sha.to_lowercase(),
                    author: c.author.to_lowercase(),
                    subject: c.subject.to_lowercase(),
                })
                .collect(),
        }
    }

    /// Indices into the indexed list whose sha prefix, author or subject
    /// contains `query` — ascending and complete, so the caller keeps the full
    /// vector and these name its visible rows in order.
    ///
    /// An empty (or whitespace-only) query is every row, so a cleared prompt
    /// restores instantly and the caller never special-cases it. The fields are
    /// matched separately: a needle cannot straddle an author's tail and the
    /// subject's head, which would otherwise match things git never wrote on
    /// one line.
    pub fn indices(&self, query: &str) -> Vec<usize> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::from_iter(0..self.rows.len());
        }
        Vec::from_iter(
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    r.sha.contains(&needle)
                        || r.author.contains(&needle)
                        || r.subject.contains(&needle)
                })
                .map(|(i, _)| i),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Index;
    use crate::Commit;
    use std::sync::Arc;

    fn commit(sha: &str, author: &str, subject: &str) -> Commit {
        Commit {
            sha: sha.into(),
            short: sha.chars().take(7).collect(),
            parents: Box::from(&[][..]),
            author: Arc::from(author),
            timestamp: 0,
            subject: subject.into(),
        }
    }

    fn commits() -> Vec<Commit> {
        vec![
            commit("f00d0001", "Ada Lovelace", "Fix the Analytical Engine"),
            commit("beef0002", "grace hopper", "compiler: initial pass"),
            commit("cafe0003", "Émile Zola", "Le Vocabulaire du naturalisme"),
            commit("12340004", "ada lovelace", "notes on the engine, revisited"),
        ]
    }

    #[test]
    fn folding_runs_both_ways_across_the_three_fields() {
        let index = Index::new(&commits());
        // Author, typed in either case.
        assert_eq!(index.indices("ADA"), vec![0, 3]);
        assert_eq!(index.indices("hopper"), vec![1]);
        // Subject, mixed case against folded hay.
        assert_eq!(index.indices("analytical"), vec![0]);
        assert_eq!(index.indices("COMPILER"), vec![1]);
        // Sha prefix and sha interior alike: a substring of the hash names it.
        assert_eq!(index.indices("BEEF"), vec![1]);
        assert_eq!(index.indices("00d"), vec![0]);
    }

    #[test]
    fn unicode_folds_and_matches_whatever_the_case_typed() {
        let index = Index::new(&commits());
        assert_eq!(index.indices("émile"), vec![2]);
        assert_eq!(index.indices("ÉMILE"), vec![2]);
        assert_eq!(index.indices("vocabulaire"), vec![2]);
    }

    #[test]
    fn a_needle_cannot_straddle_two_fields() {
        // "lovelace fix" matches nothing: the author ends where the subject
        // begins, and no commit line ever read as one run of both.
        let index = Index::new(&commits());
        assert!(index.indices("lovelace fix").is_empty());
        // Either field alone still answers.
        assert_eq!(index.indices("lovelace").len(), 2);
    }

    #[test]
    fn an_empty_query_is_every_row_in_order() {
        let index = Index::new(&commits());
        assert_eq!(index.indices(""), Vec::from_iter(0..4));
        assert_eq!(index.indices("   "), Vec::from_iter(0..4), "trimmed first");
    }

    #[test]
    fn misses_are_empty_and_hits_keep_the_original_order() {
        let index = Index::new(&commits());
        assert!(index.indices("nothing matches this").is_empty());
        assert_eq!(index.indices("e"), vec![0, 1, 2, 3], "ascending, complete");
    }
}
