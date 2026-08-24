//! Registered pane tenants and logical focus.
//!
//! This is deliberately generic and knows no GPUI. A pane is a stable name and
//! a value supplied by the shell; registering the same name replaces that
//! tenant in place, so adding a files or branches panel does not add another
//! branch to layout or dispatch code.

pub const MODE: &str = "panes";

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

    pub fn focused_index(&self) -> usize {
        self.focused
    }

    pub fn focused(&self) -> &T {
        &self.entries[self.focused].value
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().map(|entry| &entry.value)
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

    pub fn cycle(&mut self, by: isize) -> bool {
        if self.entries.len() < 2 {
            return false;
        }
        let len = self.entries.len() as isize;
        let next = (self.focused as isize + by).rem_euclid(len) as usize;
        self.focus(next)
    }

    /// Closes the focused secondary tenant. The first pane is the workspace's
    /// root and, like the old screen stack's first entry, is never removed.
    pub fn close_focused(&mut self) -> Option<T> {
        if self.focused == 0 || self.entries.len() == 1 {
            return None;
        }
        let removed = self.entries.remove(self.focused).value;
        self.focused = self.focused.min(self.entries.len() - 1);
        Some(removed)
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
    fn focus_cycles_both_ways_and_refuses_indices_that_do_not_exist() {
        let mut panes = Panes::new("one", 1);
        panes.register("two", 2);
        panes.register("three", 3);
        assert!(panes.cycle(1));
        assert_eq!(*panes.focused(), 1, "next did not wrap");
        assert!(panes.cycle(-1));
        assert_eq!(*panes.focused(), 3, "previous did not wrap");
        assert!(!panes.focus(99));
    }

    #[test]
    fn only_secondary_panes_close_and_focus_stays_valid() {
        let mut panes = Panes::new("root", 1);
        assert_eq!(panes.close_focused(), None);
        panes.register("middle", 2);
        panes.register("last", 3);
        assert_eq!(panes.close_focused(), Some(3));
        assert_eq!(
            (panes.len(), panes.focused_index(), *panes.focused()),
            (2, 1, 2)
        );
        assert!(panes.focus(0));
        assert_eq!(panes.close_focused(), None);
    }
}
