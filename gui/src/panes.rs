//! Registered pane tenants and logical focus.
//!
//! This is deliberately generic and knows no GPUI. A pane is a stable name and
//! a value supplied by the shell; registering the same name replaces that
//! tenant in place, so adding a files or branches panel does not add another
//! branch to layout or dispatch code.

struct Entry<T> {
    name: String,
    value: T,
}

pub struct Panes<T> {
    entries: Vec<Entry<T>>,
    focused: usize,
}

impl<T> Panes<T> {
    pub fn new(name: impl Into<String>, value: T) -> Self {
        Self {
            entries: vec![Entry {
                name: name.into(),
                value,
            }],
            focused: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The focused tenant's index.
    pub fn focused_index(&self) -> usize {
        self.focused
    }

    pub fn focused(&self) -> &T {
        &self.entries[self.focused].value
    }

    /// The focused tenant's stable registration name — what a prompt holds so
    /// its result can be routed back to the pane it was opened over, however
    /// focus moves while it is open.
    pub fn focused_name(&self) -> &str {
        &self.entries[self.focused].name
    }

    /// A tenant by its stable registration name. The Changes rail reads
    /// `files`, the History timeline `commits`, and the panel whatever pane
    /// the keyboard is on — drawing goes by name, never by assuming an index.
    pub fn get(&self, name: &str) -> Option<&T> {
        self.position(name).map(|at| &self.entries[at].value)
    }

    /// Every registered name, in registration order. The cycle order is
    /// *derived* from this rather than being it: the design's number keys
    /// name the sidebar first, so the shell walks `files, branches, stashes,
    /// commits` and appends whatever an extension added after.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.name.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().map(|entry| &entry.value)
    }

    /// The same registry, mutably — what a window-wide operation that keeps
    /// every tenant's entity (a repository switch re-aims screens rather
    /// than rebuilding them) needs to reach each screen in place.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.entries.iter_mut().map(|entry| &mut entry.value)
    }

    /// Where a tenant lives, by its stable registration name — what a
    /// focus-by-name command (`files.focus`) needs to find it.
    pub fn position(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|entry| entry.name == name)
    }

    /// Adds a tenant, or replaces one already registered under `name`, and
    /// focuses it. Returns the replaced tenant when there was one.
    pub fn register(&mut self, name: impl Into<String>, value: T) -> Option<T> {
        let name = name.into();
        if let Some(at) = self.entries.iter().position(|entry| entry.name == name) {
            self.focused = at;
            return Some(std::mem::replace(&mut self.entries[at].value, value));
        }
        self.entries.push(Entry { name, value });
        self.focused = self.entries.len() - 1;
        None
    }

    pub fn focus(&mut self, at: usize) -> bool {
        if at >= self.entries.len() || at == self.focused {
            return false;
        }
        self.focused = at;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::Panes;

    #[test]
    fn registration_adds_replaces_and_focuses_by_stable_name() {
        let mut panes = Panes::new("commits", 1);
        assert_eq!(panes.register("diff", 2), None);
        assert_eq!(
            (panes.len(), panes.focused_index(), *panes.focused()),
            (2, 1, 2)
        );

        assert_eq!(panes.register("diff", 3), Some(2));
        assert_eq!(panes.len(), 2, "replacement appended a duplicate pane");
        assert_eq!(*panes.focused(), 3);
        assert_eq!(panes.iter().copied().collect::<Vec<_>>(), [1, 3]);
    }

    #[test]
    fn focus_moves_to_a_registered_index_and_refuses_indices_that_do_not_exist() {
        let mut panes = Panes::new("one", 1);
        panes.register("two", 2);
        panes.register("three", 3);
        assert!(panes.focus(0));
        assert_eq!(*panes.focused(), 1);
        assert!(!panes.focus(0), "focusing the focused pane is not a move");
        assert!(!panes.focus(99));
        assert_eq!(panes.focused_index(), 0);
    }

    #[test]
    fn a_tenant_is_found_by_its_stable_name() {
        let mut panes = Panes::new("commits", 1);
        panes.register("files", 2);
        assert_eq!(panes.position("files"), Some(1));
        assert_eq!(panes.position("commits"), Some(0));
        // Replacing keeps the name where it was, so a focus command does not
        // have to care whether the tenant is new.
        panes.register("diff", 3);
        panes.register("files", 4);
        assert_eq!(panes.position("files"), Some(1));
        assert_eq!(panes.position("branches"), None);
    }
}
