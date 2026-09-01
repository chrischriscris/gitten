//! Cutting a path where the eye does.
//!
//! The design draws a path as two inks: the directory dim, the filename
//! bright, because the filename is what a row is *about* and the directory
//! is where it lives. The files pane and the diff header both draw one, and
//! the cut has to land in the same place in both — so it is made here, once,
//! on the string, and a client only picks the two colours.

/// Splits `path` into its directory prefix — **including** the trailing
/// `/`, so the two halves concatenate back to the input — and its final
/// component. A bare filename has an empty directory; a path ending in `/`
/// has an empty name, because there is nothing after the last slash to be
/// bright.
pub fn split_dir_name(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(at) => path.split_at(at + 1),
        None => ("", path),
    }
}

#[cfg(test)]
mod tests {
    use super::split_dir_name;

    #[test]
    fn a_nested_path_cuts_after_the_last_slash() {
        assert_eq!(
            split_dir_name("internal/ai/commit.go"),
            ("internal/ai/", "commit.go")
        );
    }

    #[test]
    fn a_bare_name_has_no_directory() {
        assert_eq!(split_dir_name("README"), ("", "README"));
    }

    #[test]
    fn a_trailing_slash_keeps_the_slash_and_empties_the_name() {
        assert_eq!(split_dir_name("docs/"), ("docs/", ""));
        assert_eq!(split_dir_name("/"), ("/", ""));
    }
}
