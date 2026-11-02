//! Navigation history is committed only after a successful folder opening.
//! The prior selected media is restored when it still exists in the destination.

use std::path::{Path, PathBuf};

const MAX_VISITS: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Visit {
    pub folder: PathBuf,
    pub selected: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Travel {
    #[default]
    Push,
    Back,
    Forward,
}

#[derive(Default)]
pub struct History {
    back: Vec<Visit>,
    forward: Vec<Visit>,
}

impl History {
    pub fn back_target(&self) -> Option<&Path> {
        self.back.last().map(|visit| visit.folder.as_path())
    }

    pub fn forward_target(&self) -> Option<&Path> {
        self.forward.last().map(|visit| visit.folder.as_path())
    }

    // Call only after successfully enumerating the destination folder.
    // Failed navigations preserve the previous history, selection and scope.
    pub fn commit(
        &mut self,
        direction: Travel,
        previous: Option<Visit>,
        destination: &Path,
    ) -> Option<PathBuf> {
        match direction {
            Travel::Push => {
                if let Some(previous) = previous {
                    if previous.folder != destination {
                        push_bounded(&mut self.back, previous);
                        self.forward.clear();
                    }
                }
                None
            }
            Travel::Back => {
                let visit = self.back.pop()?;
                if let Some(previous) = previous {
                    push_bounded(&mut self.forward, previous);
                }
                visit.selected
            }
            Travel::Forward => {
                let visit = self.forward.pop()?;
                if let Some(previous) = previous {
                    push_bounded(&mut self.back, previous);
                }
                visit.selected
            }
        }
    }
}

fn push_bounded(stack: &mut Vec<Visit>, visit: Visit) {
    if stack.len() == MAX_VISITS {
        stack.remove(0);
    }
    stack.push(visit);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visit(folder: &str, selected: Option<&str>) -> Visit {
        Visit {
            folder: folder.into(),
            selected: selected.map(PathBuf::from),
        }
    }

    #[test]
    fn back_forward_restore_selection_without_stale_branch() {
        let mut history = History::default();
        history.commit(Travel::Push, Some(visit("/one", Some("/one/a.jpg"))), Path::new("/two"));
        history.commit(Travel::Push, Some(visit("/two", Some("/two/b.jpg"))), Path::new("/three"));
        assert_eq!(history.back_target(), Some(Path::new("/two")));

        let selected = history.commit(
            Travel::Back,
            Some(visit("/three", None)),
            Path::new("/two"),
        );
        assert_eq!(selected, Some(PathBuf::from("/two/b.jpg")));
        assert_eq!(history.forward_target(), Some(Path::new("/three")));

        let selected = history.commit(
            Travel::Back,
            Some(visit("/two", selected.as_deref().and_then(Path::to_str))),
            Path::new("/one"),
        );
        assert_eq!(selected, Some(PathBuf::from("/one/a.jpg")));
        assert_eq!(history.forward_target(), Some(Path::new("/two")));

        let selected = history.commit(
            Travel::Forward,
            Some(visit("/one", selected.as_deref().and_then(Path::to_str))),
            Path::new("/two"),
        );
        assert_eq!(selected, Some(PathBuf::from("/two/b.jpg")));

        history.commit(Travel::Push, Some(visit("/two", selected.as_deref().and_then(Path::to_str))), Path::new("/four"));
        assert!(history.forward_target().is_none());
    }

    #[test]
    fn refreshing_same_folder_keeps_history_and_bound_is_respected() {
        let mut history = History::default();
        history.commit(Travel::Push, Some(visit("/one", None)), Path::new("/two"));
        history.commit(Travel::Back, Some(visit("/two", None)), Path::new("/one"));
        assert!(history.forward_target().is_some());

        history.commit(Travel::Push, Some(visit("/one", None)), Path::new("/one"));
        assert!(history.forward_target().is_some());
        for number in 0..150 {
            let from = PathBuf::from(format!("/folder/{number}"));
            let to = PathBuf::from(format!("/folder/{}", number + 1));
            history.commit(Travel::Push, Some(Visit { folder: from, selected: None }), &to);
        }
        assert_eq!(history.back.len(), MAX_VISITS);
    }
}
