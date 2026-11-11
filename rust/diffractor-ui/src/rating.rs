//! Reviewed metadata edits, executed away from the UI thread.
use std::{os::unix::fs::MetadataExt, path::{Path, PathBuf}, process::Command,
    sync::{Arc, atomic::{AtomicBool, Ordering}}};
use diffractor_core::{read_metadata, Metadata};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Version { device: u64, inode: u64, size: u64, modified: i64, nanos: i64 }

pub(crate) fn version(path: &Path) -> Result<Option<Version>, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Cannot inspect {}: {error}", path.display())),
    };
    if !meta.is_file() { return Err(format!("Not a regular file: {}", path.display())); }
    Ok(Some(Version { device: meta.dev(), inode: meta.ino(), size: meta.size(),
        modified: meta.mtime(), nanos: meta.mtime_nsec() }))
}

#[derive(Clone, PartialEq, Eq)]
pub struct Snapshot { file: Version, stem: Option<Version>, full: Option<Version> }

fn inspect(path: &Path) -> Result<Snapshot, String> {
    let file = version(path)?.ok_or_else(|| "File no longer exists.".to_owned())?;
    let stem = version(&path.with_extension("xmp"))?;
    let mut full_name = path.as_os_str().to_os_string();
    full_name.push(".xmp");
    let full = version(Path::new(&full_name))?;
    if stem.is_some() && full.is_some() {
        return Err("Two XMP sidecars exist; cannot choose which to edit.".into());
    }
    Ok(Snapshot { file, stem, full })
}

#[derive(Clone, Default)]
pub struct Edits {
    pub rating: Option<i32>, pub title: Option<String>, pub description: Option<String>,
    pub add_tags: Vec<String>, pub remove_tags: Vec<String>,
}

pub enum Job {
    Prepare(PathBuf), Save(PathBuf, Edits, Snapshot),
    PrepareBatch(Vec<PathBuf>), SaveBatch(Vec<(PathBuf, Snapshot)>, Edits, Arc<AtomicBool>),
}
pub enum ResultMessage {
    Prepared(PathBuf, Result<(Metadata, Snapshot), String>),
    Saved(PathBuf, Result<Option<Metadata>, String>),
    BatchPrepared(Result<Vec<(PathBuf, Snapshot)>, String>),
    BatchSaved { updated: Vec<(PathBuf, Option<Metadata>)>, failures: Vec<(PathBuf, String)>, cancelled: Vec<PathBuf> },
}

pub fn run(job: Job, worker: &Path) -> ResultMessage {
    run_with_progress(job, worker, |_, _| {})
}

pub fn run_with_progress(job: Job, worker: &Path, mut progress: impl FnMut(usize, usize)) -> ResultMessage {
    match job {
        Job::Prepare(path) => {
            let result = (|| {
                let before = inspect(&path)?;
                let metadata = read_metadata(&path).map_err(|error| format!("Cannot read metadata: {error:?}"))?;
                if before != inspect(&path)? { return Err("File or sidecar changed while preparing the edit.".into()); }
                Ok((metadata, before))
            })();
            ResultMessage::Prepared(path, result)
        }
        Job::Save(path, edits, expected) => {
            let result = (|| {
                if inspect(&path)? != expected { return Err("File or sidecar changed; reopen the metadata editor.".into()); }
                let mut command = Command::new(worker);
                command.arg("--set-metadata").arg(&path);
                if let Some(rating) = edits.rating { command.arg("--rating").arg(rating.to_string()); }
                if let Some(title) = edits.title { command.arg("--title").arg(title); }
                if let Some(description) = edits.description { command.arg("--description").arg(description); }
                for tag in edits.add_tags { command.arg("--add-tag").arg(tag); }
                for tag in edits.remove_tags { command.arg("--remove-tag").arg(tag); }
                let output = command.output().map_err(|error| format!("Cannot start metadata worker: {error}"))?;
                if !output.status.success() {
                    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                    return Err(if message.is_empty() { format!("Metadata worker failed: {}", output.status) } else { message });
                }
                Ok(read_metadata(&path).ok())
            })();
            ResultMessage::Saved(path, result)
        }
        Job::PrepareBatch(paths) => {
            let result = (|| {
                let mut items = Vec::with_capacity(paths.len());
                let mut identities = std::collections::HashSet::new();
                let mut sidecars = std::collections::HashSet::new();
                for path in paths {
                    let snapshot = inspect(&path).map_err(|error| format!("{}: {error}", path.display()))?;
                    if !identities.insert((snapshot.file.device, snapshot.file.inode)) {
                        return Err("Selected paths refer to the same file.".into());
                    }
                    let mut full = path.as_os_str().to_os_string();
                    full.push(".xmp");
                    if !sidecars.insert(path.with_extension("xmp")) || !sidecars.insert(PathBuf::from(full)) {
                        return Err("Selected files may share an XMP sidecar; edit them individually.".into());
                    }
                    items.push((path, snapshot));
                }
                Ok(items)
            })();
            ResultMessage::BatchPrepared(result)
        }
        Job::SaveBatch(items, edits, cancel) => {
            let mut updated = Vec::new();
            let mut failures = Vec::new();
            let mut cancelled = Vec::new();
            let total = items.len();
            let mut completed = 0;
            for (path, snapshot) in items {
                if cancel.load(Ordering::Acquire) {
                    cancelled.push(path);
                    continue;
                }
                match run(Job::Save(path, edits.clone(), snapshot), worker) {
                    ResultMessage::Saved(path, Ok(metadata)) => updated.push((path, metadata)),
                    ResultMessage::Saved(path, Err(error)) => failures.push((path, error)),
                    _ => unreachable!(),
                }
                completed += 1;
                progress(completed, total);
            }
            ResultMessage::BatchSaved { updated, failures, cancelled }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reviewed_rating_saves_and_refuses_a_changed_file() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let worker = root.join("tmp/linux-build/diffractor");
        let dir = std::env::temp_dir().join(format!("diffractor-rating-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.jpg");
        std::fs::copy(root.join("exe/test/Test.jpg"), &path).unwrap();
        let ResultMessage::Prepared(_, Ok((_, snapshot))) = run(Job::Prepare(path.clone()), &worker) else { panic!("prepare failed") };
        let changes = Edits { rating: Some(2), title: Some("Changed title".into()),
            add_tags: vec!["새 태그".into()], remove_tags: vec!["key2".into()], ..Default::default() };
        let ResultMessage::Saved(_, Ok(Some(metadata))) = run(Job::Save(path.clone(), changes, snapshot), &worker) else { panic!("save failed") };
        assert_eq!(metadata.rating, Some(2));
        assert_eq!(metadata.title, "Changed title");
        assert_eq!(metadata.description, "Caption");
        assert!(metadata.keywords.contains("key1"));
        assert!(!metadata.keywords.contains("key2"));
        assert!(metadata.keywords.contains("새 태그"));
        let ResultMessage::Prepared(_, Ok((_, snapshot))) = run(Job::Prepare(path.clone()), &worker) else { panic!("prepare failed") };
        std::fs::write(path.with_extension("xmp"), "changed").unwrap();
        assert!(matches!(run(Job::Save(path.clone(), Edits { rating: Some(5), ..Default::default() }, snapshot), &worker), ResultMessage::Saved(_, Err(_))));
        assert_eq!(read_metadata(&path).unwrap().rating, Some(2));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn batch_reports_partial_success_and_refuses_shared_sidecars() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let worker = root.join("tmp/linux-build/diffractor");
        let dir = std::env::temp_dir().join(format!("diffractor-batch-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("one.jpg");
        let second = dir.join("two.jpg");
        for path in [&first, &second] { std::fs::copy(root.join("exe/test/Test.jpg"), path).unwrap(); }
        let ResultMessage::BatchPrepared(Ok(items)) = run(Job::PrepareBatch(vec![first.clone(), second.clone()]), &worker) else { panic!("prepare failed") };
        std::fs::write(second.with_extension("xmp"), "changed").unwrap();
        let edits = Edits { rating: Some(3), title: Some("공통 제목".into()),
            description: Some(String::new()), ..Default::default() };
        let ResultMessage::BatchSaved { updated, failures, cancelled } = run(Job::SaveBatch(items, edits, Arc::new(AtomicBool::new(false))), &worker) else { panic!("save failed") };
        assert!(cancelled.is_empty());
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].0, first);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, second);
        let changed = read_metadata(&first).unwrap();
        assert_eq!(changed.rating, Some(3));
        assert_eq!(changed.title, "공통 제목");
        assert_eq!(changed.description, "");
        assert_eq!(read_metadata(&second).unwrap().description, "Caption");
        std::fs::remove_file(second.with_extension("xmp")).unwrap();
        let ResultMessage::BatchPrepared(Ok(items)) = run(Job::PrepareBatch(vec![first.clone(), second.clone()]), &worker) else { panic!("prepare failed") };
        let cancel = Arc::new(AtomicBool::new(false));
        let request = Job::SaveBatch(items, Edits { rating: Some(5), ..Default::default() }, Arc::clone(&cancel));
        let ResultMessage::BatchSaved { updated, failures, cancelled } = run_with_progress(request, &worker, |done, _| {
            if done == 1 { cancel.store(true, Ordering::Release); }
        }) else { panic!("batch failed") };
        assert_eq!(updated.len(), 1);
        assert!(failures.is_empty());
        assert_eq!(cancelled, vec![second.clone()]);
        assert_eq!(read_metadata(&second).unwrap().rating, Some(4));
        let raw = dir.join("one.CR2");
        std::fs::copy(root.join("exe/test/Gherkin.CR2"), &raw).unwrap();
        assert!(matches!(run(Job::PrepareBatch(vec![first, raw]), &worker), ResultMessage::BatchPrepared(Err(_))));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
