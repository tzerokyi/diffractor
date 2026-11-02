//! The original Diffractor selection grammar, isolated from egui and the decoder.
//! Focus (keyboard cursor), anchor (range start) and selected files (batch target)
//! are distinct; only visible media can enter this preview's selection.
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Intent {
    Replace,
    Toggle,
    Range,
    AddRange,
}

#[derive(Default)]
pub struct Selection {
    paths: BTreeSet<PathBuf>,
    focus: Option<PathBuf>,
    anchor: Option<PathBuf>,
}

impl Selection {
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.paths.contains(path)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Path> {
        self.paths.iter().map(PathBuf::as_path)
    }

    pub fn focus(&self) -> Option<&Path> {
        self.focus.as_deref()
    }

    pub fn single(&self) -> Option<&Path> {
        if self.paths.len() == 1 {
            self.paths.iter().next().map(PathBuf::as_path)
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.paths.clear();
        self.focus = None;
        self.anchor = None;
    }

    /// Explicit file-manager launch may target a valid file beyond the bounded
    /// folder listing. Keep it as the only target until the scope changes.
    pub fn set_direct(&mut self, path: PathBuf) {
        self.paths.clear();
        self.paths.insert(path.clone());
        self.focus = Some(path.clone());
        self.anchor = Some(path);
    }

    pub fn select_all(&mut self, visible: &[PathBuf]) {
        self.paths = visible.iter().cloned().collect();
        if self.focus.as_ref().is_none_or(|path| !self.paths.contains(path)) {
            self.focus = visible.first().cloned();
        }
        if self.anchor.as_ref().is_none_or(|path| !self.paths.contains(path)) {
            self.anchor = self.focus.clone();
        }
    }

    /// Remove hidden items from the selected batch target, as Windows does.
    /// Changing a filter cannot implicitly reselect files when it is removed.
    pub fn retain_visible(&mut self, visible: &[PathBuf]) {
        let allowed: BTreeSet<_> = visible.iter().cloned().collect();
        self.paths.retain(|path| allowed.contains(path));
        if self.focus.as_ref().is_some_and(|path| !allowed.contains(path)) {
            self.focus = None;
        }
        if self.anchor.as_ref().is_some_and(|path| !allowed.contains(path)) {
            self.anchor = None;
        }
    }

    pub fn apply(&mut self, visible: &[PathBuf], path: &Path, intent: Intent) {
        let Some(index) = visible.iter().position(|candidate| candidate == path) else {
            return;
        };
        match intent {
            Intent::Replace => {
                self.paths.clear();
                self.paths.insert(path.to_path_buf());
                self.anchor = Some(path.to_path_buf());
            }
            Intent::Toggle => {
                if !self.paths.remove(path) {
                    self.paths.insert(path.to_path_buf());
                }
                self.anchor = Some(path.to_path_buf());
            }
            Intent::Range | Intent::AddRange => {
                let anchor = self.anchor.as_ref()
                    .and_then(|anchor| visible.iter().position(|candidate| candidate == anchor))
                    .or_else(|| self.focus.as_ref().and_then(|focus| {
                        visible.iter().position(|candidate| candidate == focus)
                    }))
                    .unwrap_or(index);
                if intent == Intent::Range {
                    self.paths.clear();
                }
                let (start, end) = if anchor <= index { (anchor, index) } else { (index, anchor) };
                for candidate in &visible[start..=end] {
                    self.paths.insert(candidate.clone());
                }
                self.anchor = Some(visible[anchor].clone());
            }
        }
        self.focus = Some(path.to_path_buf());
    }

    pub fn move_focus(&mut self, visible: &[PathBuf], reverse: bool, extend: bool) {
        if visible.is_empty() {
            return;
        }
        let index = self.focus.as_ref()
            .and_then(|focus| visible.iter().position(|candidate| candidate == focus));
        let next = match (index, reverse) {
            (Some(index), true) => index.saturating_sub(1),
            (Some(index), false) => (index + 1).min(visible.len() - 1),
            (None, true) => visible.len() - 1,
            (None, false) => 0,
        };
        let path = visible[next].clone();
        self.apply(visible, &path, if extend { Intent::Range } else { Intent::Replace });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible() -> Vec<PathBuf> {
        ["a.jpg", "b.jpg", "c.jpg", "d.jpg"]
            .into_iter().map(PathBuf::from).collect()
    }

    #[test]
    fn plain_toggle_and_shift_use_distinct_focus_and_targets() {
        let items = visible();
        let mut s = Selection::default();
        s.apply(&items, &items[0], Intent::Replace);
        s.apply(&items, &items[2], Intent::Toggle);
        assert_eq!(s.len(), 2);
        assert_eq!(s.focus(), Some(items[2].as_path()));
        s.apply(&items, &items[3], Intent::Range);
        assert_eq!(s.len(), 2);
        assert!(s.contains(&items[2]) && s.contains(&items[3]));
        assert!(!s.contains(&items[0]));
        s.apply(&items, &items[0], Intent::AddRange);
        assert_eq!(s.len(), 4);
        s.apply(&items, &items[2], Intent::Toggle);
        assert_eq!(s.len(), 3);
        assert_eq!(s.focus(), Some(items[2].as_path()));
    }

    #[test]
    fn hidden_items_are_removed_and_not_reselected() {
        let items = visible();
        let mut s = Selection::default();
        s.select_all(&items);
        s.retain_visible(&items[1..3]);
        assert_eq!(s.len(), 2);
        s.retain_visible(&items);
        assert_eq!(s.len(), 2);
        assert!(!s.contains(&items[0]));
        s.clear();
        assert_eq!(s.len(), 0);
        assert_eq!(s.focus(), None);
    }

    #[test]
    fn keyboard_boundary_and_range_anchor() {
        let items = visible();
        let mut s = Selection::default();
        s.move_focus(&items, false, false);
        assert_eq!(s.single(), Some(items[0].as_path()));
        s.move_focus(&items, false, true);
        s.move_focus(&items, false, true);
        assert_eq!(s.len(), 3);
        s.move_focus(&items, true, true);
        assert_eq!(s.len(), 2);
        assert!(s.contains(&items[0]) && s.contains(&items[1]));
        s.move_focus(&items, true, false);
        s.move_focus(&items, true, false);
        assert_eq!(s.single(), Some(items[0].as_path()));
        s.move_focus(&[], true, true);
        assert_eq!(s.single(), Some(items[0].as_path()));
    }

    #[test]
    fn invisible_paths_never_become_selection_targets() {
        let items = visible();
        let mut s = Selection::default();
        s.apply(&items, Path::new("missing.jpg"), Intent::Replace);
        assert_eq!(s.len(), 0);
    }
}
