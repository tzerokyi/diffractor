//! Native Linux folder scope and type recognition.
//! Only this module enumerates the local directory. Never browse from egui's UI thread.
use std::{
    cmp::Ordering,
    collections::BinaryHeap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Eq, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub directory: bool,
}

// Directories first, then filename, then full path for stable ties. The heap
// holds the worst admitted entry at its root and never stores more than 10k.
impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        other.directory.cmp(&self.directory)
            .then_with(|| self.path.file_name().cmp(&other.path.file_name()))
            .then_with(|| self.path.cmp(&other.path))
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn is_image(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
        [
            "jpg", "jpeg", "png", "webp", "heic", "heif", "avif", "jxl",
            "gif", "tif", "tiff", "bmp", "psd", "cr2", "cr3", "nef", "nrw",
            "arw", "dng", "raf", "orf", "rw2", "pef", "srw", "3fr",
            "iiq", "erf", "kdc", "mos", "mrw", "x3f",
        ]
        .iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
    })
}

pub fn is_video(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
        [
            "mp4", "m4v", "mov", "mkv", "webm", "avi", "mpg", "mpeg",
            "m2ts", "mts", "ts", "wmv", "flv", "3gp",
        ]
        .iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
    })
}

pub fn is_audio(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
        [
            "mp3", "m4a", "aac", "flac", "wav", "ogg", "opus", "wma", "aiff", "aif",
        ]
        .iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
    })
}

/// Select the first MAX_ENTRIES *after sorting*, irrespective of filesystem
/// enumeration order. Limits memory even when the folder contains millions of
/// entries, rather than silently choosing an arbitrary filesystem prefix.
pub fn list_folder(path: &Path) -> Result<(Vec<Entry>, bool), String> {
    let mut first = BinaryHeap::new();
    let mut truncated = false;
    let entries = std::fs::read_dir(path)
        .map_err(|e| format!("Cannot open folder: {e}"))?;
    for candidate in entries {
        let candidate = candidate.map_err(|e| format!("Cannot read folder entry: {e}"))?;
        let kind = candidate.file_type()
            .map_err(|e| format!("Cannot inspect an entry: {e}"))?;
        let path = candidate.path();
        let directory = if kind.is_symlink() { path.is_dir() } else { kind.is_dir() };
        if !(directory || is_image(&path) || is_video(&path) || is_audio(&path)) {
            continue;
        }
        let item = Entry { path, directory };
        if first.len() < MAX_ENTRIES {
            first.push(item);
        } else {
            truncated = true;
            if first.peek().is_some_and(|worst| item < *worst) {
                first.pop();
                first.push(item);
            }
        }
    }
    Ok((first.into_sorted_vec(), truncated))
}

/// Resolve on the worker, never while painting the UI.
pub fn open_target(path: &Path) -> Result<(PathBuf, Option<PathBuf>), String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Cannot resolve path: {e}"))?
            .join(path)
    };
    let kind = std::fs::metadata(&path)
        .map_err(|e| format!("Cannot open path: {e}"))?;
    if kind.is_dir() {
        Ok((path, None))
    } else if kind.is_file() && (is_image(&path) || is_video(&path) || is_audio(&path)) {
        let folder = path.parent().ok_or("File has no parent folder.")?.to_path_buf();
        Ok((folder, Some(path)))
    } else {
        Err("This path is not a supported media file or folder.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_recognition_is_case_insensitive_but_paths_are_not() {
        assert!(is_image(Path::new("Photo.NEF")));
        assert!(is_video(Path::new("Clip.MP4")));
        assert!(is_audio(Path::new("Song.FLAC")));
        assert!(!is_image(Path::new("Photo.NEF.exe")));
        assert_ne!(Path::new("/a/Pic.jpg"), Path::new("/a/pic.jpg"));
    }

    #[test]
    fn bounded_sort_keeps_directories_and_lexically_first_files() {
        let mut heap = BinaryHeap::new();
        let mut add = |path: String, directory| {
            let item = Entry { path: path.into(), directory };
            if heap.len() < 3 {
                heap.push(item);
            } else if heap.peek().is_some_and(|worst| item < *worst) {
                heap.pop();
                heap.push(item);
            }
        };
        for name in ["z.jpg", "x.jpg", "b.jpg", "a.jpg"] {
            add(name.into(), false);
        }
        add("directory".into(), true);
        let sorted = heap.into_sorted_vec();
        assert_eq!(sorted.iter().map(|item| item.path.as_os_str())
            .collect::<Vec<_>>(),
            ["directory", "a.jpg", "b.jpg"].map(OsStr::new).to_vec());
    }
}
