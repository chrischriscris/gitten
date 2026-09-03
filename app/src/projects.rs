//! Recently opened repositories, most-recent first.
//!
//! A scratch file under `target/`, because that is already git-ignored and
//! already what you delete for a clean slate. One repo path per line, in the
//! order to show them — the first line is the most recently opened.
//!
//! Hand-rolled lines rather than TOML for the reason `shell::session` encodes
//! by hand: a format nobody hand-edits does not need a parser, and this file
//! is read once at startup and rewritten on every open.
//!
//! Best effort throughout: a missing or corrupt file loads as empty, a failed
//! write is silently dropped. Losing the recent list is never worth an error,
//! and never a panic.

use std::path::{Path, PathBuf};

/// How many repositories are remembered. Past this the oldest entry falls off.
pub const MAX: usize = 15;

/// Under `target/`, for the reason above. Overridable so a test never writes
/// to a real one — the same convention as `shell::session::path`.
pub fn path() -> PathBuf {
    std::env::var_os("GITTEN_PROJECTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/gitten-projects"))
}

/// The stored list, most-recent first. Blank lines are skipped; a missing file
/// — or one that is not UTF-8 — loads as empty, never an error.
pub fn load() -> Vec<PathBuf> {
    load_from(&path())
}

fn load_from(file: &Path) -> Vec<PathBuf> {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            (!line.is_empty()).then(|| PathBuf::from(line))
        })
        .collect()
}

/// The path as the filesystem spells it, so `/tmp/x` and `/tmp/x/.` compare
/// equal. Falls back to as-given when the path does not exist (yet) and there
/// is nothing to canonicalize against.
fn norm(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Two entries name the same repository when their normalised forms agree —
/// each side normalised on its own, so an entry stored raw still matches once
/// the directory exists.
fn same(a: &Path, b: &Path) -> bool {
    norm(a) == norm(b)
}

fn save(list: &[PathBuf]) {
    let file = path();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut text = String::new();
    for entry in list {
        text.push_str(&entry.to_string_lossy());
        text.push('\n');
    }
    let _ = std::fs::write(file, text);
}

/// Notes `path` as opened: canonicalized when possible, moved to the front,
/// deduped, truncated to [`MAX`]. Writes the list back and returns it.
/// Best-effort I/O throughout — never panics.
pub fn record(path: &Path) -> Vec<PathBuf> {
    let mut list = load();
    let key = norm(path);
    list.retain(|entry| !same(entry, &key));
    list.insert(0, key);
    list.truncate(MAX);
    save(&list);
    list
}

/// Drops `path` from the list — matching canonicalized when possible, raw
/// otherwise — writes the list back and returns it. Best-effort, never panics.
pub fn remove(path: &Path) -> Vec<PathBuf> {
    let mut list = load();
    list.retain(|entry| !same(entry, path));
    save(&list);
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// The store resolves its file through a process-global env var, so every
    /// test that points it at a scratch file holds this while it runs.
    static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("gitten-projects-tests");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join(name);
        let _ = std::fs::remove_file(&file);
        file
    }

    /// Points `path()` at a fresh scratch file, runs `body`, then unsets the
    /// override and removes the file. Serialized: the env var is global.
    fn with_scratch(name: &str, body: impl FnOnce(&Path)) {
        let _guard = serial();
        let file = scratch(name);
        std::env::set_var("GITTEN_PROJECTS", &file);
        body(&file);
        std::env::remove_var("GITTEN_PROJECTS");
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn the_env_override_names_the_file() {
        let _guard = serial();
        let file = scratch("override");
        std::env::set_var("GITTEN_PROJECTS", &file);
        assert_eq!(path(), file);
        std::env::remove_var("GITTEN_PROJECTS");
    }

    #[test]
    fn without_an_override_it_lives_under_target() {
        let _guard = serial();
        std::env::remove_var("GITTEN_PROJECTS");
        assert_eq!(path(), PathBuf::from("target/gitten-projects"));
    }

    #[test]
    fn a_missing_file_loads_as_empty() {
        with_scratch("missing", |_| {
            assert!(load().is_empty());
        });
    }

    #[test]
    fn records_come_back_most_recent_first() {
        with_scratch("round-trip", |_| {
            record(Path::new("/tmp/gitten-test-a"));
            record(Path::new("/tmp/gitten-test-b"));
            record(Path::new("/tmp/gitten-test-c"));
            assert_eq!(
                load(),
                vec![
                    PathBuf::from("/tmp/gitten-test-c"),
                    PathBuf::from("/tmp/gitten-test-b"),
                    PathBuf::from("/tmp/gitten-test-a"),
                ]
            );
        });
    }

    #[test]
    fn re_recording_moves_to_front_without_dupes() {
        with_scratch("re-record", |_| {
            record(Path::new("/tmp/gitten-test-a"));
            record(Path::new("/tmp/gitten-test-b"));
            let list = record(Path::new("/tmp/gitten-test-a"));
            assert_eq!(
                list,
                vec![
                    PathBuf::from("/tmp/gitten-test-a"),
                    PathBuf::from("/tmp/gitten-test-b"),
                ]
            );
            assert_eq!(load(), list, "the move survived the write");
        });
    }

    #[test]
    fn the_list_is_capped_at_max() {
        with_scratch("truncate", |_| {
            let mut list = Vec::new();
            for i in 0..MAX + 5 {
                list = record(Path::new(&format!("/tmp/gitten-test-{i}")));
            }
            assert_eq!(list.len(), MAX);
            assert_eq!(
                list[0],
                PathBuf::from(format!("/tmp/gitten-test-{}", MAX + 4)),
                "the newest entry is first"
            );
            assert!(
                !list.contains(&PathBuf::from("/tmp/gitten-test-0")),
                "the oldest entries fell off"
            );
            assert_eq!(load(), list, "the cap survived the write");
        });
    }

    #[test]
    fn a_corrupt_file_loads_as_empty() {
        with_scratch("corrupt", |file| {
            std::fs::write(file, [0xff, 0xfe, 0x00, 0x80]).unwrap();
            assert!(
                load().is_empty(),
                "non-UTF-8 bytes are ignored, never a panic"
            );
        });
    }

    #[test]
    fn blank_lines_are_skipped() {
        with_scratch("blanks", |file| {
            std::fs::write(file, "/tmp/a\n\n   \n/tmp/b\n").unwrap();
            assert_eq!(
                load(),
                vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]
            );
        });
    }

    #[test]
    fn remove_drops_the_entry_and_keeps_the_order() {
        with_scratch("remove", |_| {
            record(Path::new("/tmp/gitten-test-a"));
            record(Path::new("/tmp/gitten-test-b"));
            record(Path::new("/tmp/gitten-test-c"));
            let list = remove(Path::new("/tmp/gitten-test-b"));
            assert_eq!(
                list,
                vec![
                    PathBuf::from("/tmp/gitten-test-c"),
                    PathBuf::from("/tmp/gitten-test-a"),
                ]
            );
            assert_eq!(load(), list, "the removal survived the write");
        });
    }

    #[test]
    fn remove_matches_through_canonicalization() {
        with_scratch("remove-canon", |_| {
            let dir = std::env::temp_dir().join("gitten-projects-canon");
            let _ = std::fs::create_dir_all(&dir);
            record(&dir);
            assert_eq!(load().len(), 1);
            // `dir/.` is a different string for the same directory; removing
            // through it must still drop the entry.
            let list = remove(&dir.join("."));
            assert!(list.is_empty(), "still there: {list:?}");
            assert!(load().is_empty());
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn removing_what_is_not_there_keeps_the_list() {
        with_scratch("remove-missing", |_| {
            record(Path::new("/tmp/gitten-test-a"));
            let list = remove(Path::new("/tmp/gitten-test-nowhere"));
            assert_eq!(list, vec![PathBuf::from("/tmp/gitten-test-a")]);
        });
    }
}
