//! Analyze and execute a reviewed single-file rename through the original index and rename code.
use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::{Path, PathBuf}, process::Command};
use crate::rating::{version, Version};

#[derive(Clone)]
pub struct Plan {
    pub source: PathBuf,
    pub name: String,
    pub moves: Vec<(PathBuf, PathBuf)>,
    versions: Vec<Version>,
}

pub enum Job { Analyze(PathBuf, String), Run(Plan) }
pub enum ResultMessage {
    Analyzed(PathBuf, String, Result<Plan, String>),
    Finished(PathBuf, PathBuf, Result<(), String>),
}

fn output(command: &mut Command) -> Result<Vec<u8>, String> {
    let result = command.output().map_err(|e| format!("Cannot start rename worker: {e}"))?;
    if !result.status.success() {
        let error = String::from_utf8_lossy(&result.stderr).trim().to_owned();
        return Err(if error.is_empty() { format!("Rename worker failed: {}", result.status) } else { error });
    }
    Ok(result.stdout)
}

fn analyze(worker: &Path, source: &Path, name: &str) -> Result<Plan, String> {
    let data = output(Command::new(worker).arg("--rename-plan").arg(source).arg(name))?;
    let body = data.strip_prefix(b"DIFREN1\0").ok_or("Invalid rename plan.")?;
    if body.is_empty() || !body.ends_with(b"\0") || body.len() > 1024 * 1024 {
        return Err("Invalid or oversized rename plan.".into());
    }
    let paths: Vec<PathBuf> = body.split(|b| *b == 0).filter(|part| !part.is_empty())
        .map(|part| PathBuf::from(OsString::from_vec(part.to_vec()))).collect();
    if paths.len() < 2 || paths.len() % 2 != 0 || paths[0] != source {
        return Err("Invalid rename plan paths.".into());
    }
    let moves: Vec<_> = paths.chunks_exact(2).map(|pair| (pair[0].clone(), pair[1].clone())).collect();
    let mut versions = Vec::with_capacity(moves.len());
    for (from, to) in &moves {
        versions.push(version(from)?.ok_or_else(|| format!("Source disappeared: {}", from.display()))?);
        // symlink_metadata treats dangling destination symlinks as collisions too.
        match std::fs::symlink_metadata(to) {
            Ok(_) => return Err(format!("Destination already exists: {}", to.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(format!("Cannot inspect {}: {e}", to.display())),
        }
    }
    Ok(Plan { source: source.to_path_buf(), name: name.to_owned(), moves, versions })
}

pub fn run(job: Job, worker: &Path) -> ResultMessage {
    match job {
        Job::Analyze(source, name) => {
            let result = analyze(worker, &source, &name);
            ResultMessage::Analyzed(source, name, result)
        }
        Job::Run(plan) => {
            let destination = plan.moves[0].1.clone();
            let result = (|| {
                for ((from, to), expected) in plan.moves.iter().zip(&plan.versions) {
                    if version(from)?.as_ref() != Some(expected) { return Err(format!("Source changed: {}", from.display())); }
                    match std::fs::symlink_metadata(to) {
                        Ok(_) => return Err(format!("Destination appeared: {}", to.display())),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                        Err(e) => return Err(format!("Cannot inspect {}: {e}", to.display())),
                    }
                }
                let mut command = Command::new(worker);
                command.arg("--rename-run").arg(&plan.source).arg(&plan.name);
                for (from, _) in plan.moves.iter().skip(1) {
                    command.arg(from.file_name().ok_or("Invalid sidecar name.")?);
                }
                output(&mut command)?;
                Ok(())
            })();
            ResultMessage::Finished(plan.source, destination, result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reviewed_rename_moves_sidecar_and_refuses_new_collisions() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let worker = root.join("tmp/linux-build/diffractor");
        let dir = std::env::temp_dir().join(format!("diffractor-rename-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("Gherkin.CR2");
        std::fs::copy(root.join("exe/test/Gherkin.CR2"), &source).unwrap();
        std::fs::copy(root.join("exe/test/Gherkin.xmp"), dir.join("Gherkin.xmp")).unwrap();
        let ResultMessage::Analyzed(_, _, Ok(plan)) = run(Job::Analyze(source.clone(), "renamed".into()), &worker) else { panic!("analysis failed") };
        assert_eq!(plan.moves.len(), 2);
        std::fs::write(dir.join("renamed.xmp"), "collision").unwrap();
        assert!(matches!(run(Job::Run(plan), &worker), ResultMessage::Finished(_, _, Err(_))));
        assert!(source.exists() && dir.join("Gherkin.xmp").exists());
        std::fs::remove_file(dir.join("renamed.xmp")).unwrap();
        let ResultMessage::Analyzed(_, _, Ok(plan)) = run(Job::Analyze(source.clone(), "renamed".into()), &worker) else { panic!("analysis failed") };
        assert!(matches!(run(Job::Run(plan), &worker), ResultMessage::Finished(_, _, Ok(()))));
        assert!(dir.join("renamed.CR2").exists() && dir.join("renamed.xmp").exists());
        assert!(!source.exists() && !dir.join("Gherkin.xmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
