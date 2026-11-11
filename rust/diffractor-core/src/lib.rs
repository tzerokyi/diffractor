//! Safe Rust access to the existing Diffractor media scanner.
//!
//! Probing reads files and belongs on a worker, never the UI thread.

use std::{
    ffi::{CStr, CString},
    io::Read,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[repr(C)]
#[derive(Default)]
struct RawImageInfo {
    width: u32,
    height: u32,
    orientation: u16,
    container_transform: u8,
}

unsafe extern "C" {
    fn diffractor_probe_image(path: *const std::ffi::c_char, out: *mut RawImageInfo) -> i32;
    fn diffractor_decode_image(
        path: *const std::ffi::c_char,
        width: u32,
        height: u32,
    ) -> *mut std::ffi::c_void;
    fn diffractor_image_pixels(image: *const std::ffi::c_void) -> *const u8;
    fn diffractor_image_width(image: *const std::ffi::c_void) -> u32;
    fn diffractor_image_height(image: *const std::ffi::c_void) -> u32;
    fn diffractor_image_orientation(image: *const std::ffi::c_void) -> u16;
    fn diffractor_image_free(image: *mut std::ffi::c_void);
    fn diffractor_metadata_read(path: *const std::ffi::c_char) -> *mut std::ffi::c_void;
    fn diffractor_metadata_text(
        metadata: *const std::ffi::c_void,
        field: i32,
    ) -> *const std::ffi::c_char;
    fn diffractor_metadata_rating(metadata: *const std::ffi::c_void, out: *mut i32) -> i32;
    fn diffractor_metadata_free(metadata: *mut std::ffi::c_void);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub orientation: u16,
    pub container_transform: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeError {
    InvalidPath,
    Unreadable,
    UnsupportedOrMalformed,
    UnexpectedCode(i32),
}

#[derive(Debug)]
pub enum SearchError {
    Start(std::io::Error),
    Io(std::io::Error),
    Worker { code: Option<i32>, message: String },
    InvalidResponse,
    OutputTooLarge,
    Cancelled,
}

/// Search a recursive folder using Diffractor's existing query parser and in-memory index.
/// This starts a worker process and blocks; it must not run on the UI thread.
pub fn search_folder(
    worker: &Path,
    folder: &Path,
    query: &str,
) -> Result<Vec<PathBuf>, SearchError> {
    search_folder_cancellable(worker, folder, query, || false)
}

/// Like `search_folder`, but terminates and reaps the worker when `cancelled` becomes true.
pub fn search_folder_cancellable(
    worker: &Path,
    folder: &Path,
    query: &str,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<PathBuf>, SearchError> {
    search_folder_with_cache(worker, folder, query, cancelled, None)
}

fn search_folder_with_cache(
    worker: &Path,
    folder: &Path,
    query: &str,
    cancelled: impl Fn() -> bool,
    cache: Option<&Path>,
) -> Result<Vec<PathBuf>, SearchError> {
    if cancelled() {
        return Err(SearchError::Cancelled);
    }
    let mut command = Command::new(worker);
    command
        .arg("--index-query")
        .arg(folder)
        .arg(query)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cache) = cache {
        command.env("DIFFRACTOR_INDEX_CACHE_DIR", cache);
    }
    let mut child = command.spawn().map_err(SearchError::Start)?;

    // Drain both pipes concurrently while waiting, including bytes beyond the storage limit, so
    // a large result or diagnostic cannot block the child on a full pipe.
    fn drain(mut pipe: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
        let mut result = Vec::new();
        let mut overflow = false;
        let mut buffer = [0u8; 8192];
        loop {
            let count = pipe.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            let keep = count.min(limit.saturating_sub(result.len()));
            result.extend_from_slice(&buffer[..keep]);
            overflow |= keep != count;
        }
        Ok((result, overflow))
    }
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out_reader = std::thread::spawn(move || drain(stdout, 64 * 1024 * 1024));
    let err_reader = std::thread::spawn(move || drain(stderr, 64 * 1024));
    let status = loop {
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_reader.join();
            let _ = err_reader.join();
            return Err(SearchError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_reader.join();
                let _ = err_reader.join();
                return Err(SearchError::Io(error));
            }
        }
    };
    let (stdout, too_large) = out_reader
        .join()
        .map_err(|_| SearchError::InvalidResponse)?
        .map_err(SearchError::Io)?;
    let (stderr, _) = err_reader
        .join()
        .map_err(|_| SearchError::InvalidResponse)?
        .map_err(SearchError::Io)?;
    if too_large {
        return Err(SearchError::OutputTooLarge);
    }
    if !status.success() {
        return Err(SearchError::Worker {
            code: status.code(),
            message: String::from_utf8_lossy(&stderr).trim().to_owned(),
        });
    }
    let body = stdout
        .strip_prefix(b"DIFIDX1\0")
        .ok_or(SearchError::InvalidResponse)?;
    if !body.is_empty() && !body.ends_with(b"\0") {
        return Err(SearchError::InvalidResponse);
    }
    Ok(body
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| PathBuf::from(std::ffi::OsString::from_vec(part.to_vec())))
        .collect())
}

#[derive(Debug)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// EXIF orientation, 1 through 8. Pixels have not been reoriented.
    pub orientation: u16,
    /// Straight-alpha RGBA, in row-major order.
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    InvalidPath,
    Failed,
}

/// Decode one bounded video preview in the existing media worker. This blocks and must run off the UI thread.
pub fn video_preview(worker: &Path, path: &Path) -> Result<DecodedImage, DecodeError> {
    let output = Command::new(worker)
        .arg("--video-frame")
        .arg(path)
        .output()
        .map_err(|_| DecodeError::Failed)?;
    if !output.status.success() || output.stdout.len() < 20 || &output.stdout[..8] != b"DIFVID1\0" {
        return Err(DecodeError::Failed);
    }
    let integer =
        |offset| u32::from_le_bytes(output.stdout[offset..offset + 4].try_into().unwrap());
    let width = integer(8);
    let height = integer(12);
    let orientation = integer(16);
    let len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .filter(|&n| n > 0 && n <= 1280 * 1280 * 4)
        .ok_or(DecodeError::Failed)?;
    if output.stdout.len() != 20 + len || orientation > 8 {
        return Err(DecodeError::Failed);
    }
    let mut rgba = output.stdout[20..].to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    Ok(DecodedImage {
        width,
        height,
        orientation: orientation as u16,
        rgba,
    })
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub title: String,
    pub camera_make: String,
    pub camera_model: String,
    pub description: String,
    pub keywords: String,
    pub lens: String,
    pub rating: Option<i32>,
}

/// Read metadata on a worker, including conventional RAW XMP sidecars. No file is modified.
pub fn read_metadata(path: &Path) -> Result<Metadata, ProbeError> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| ProbeError::InvalidPath)?;
    let ptr = std::ptr::NonNull::new(unsafe { diffractor_metadata_read(path.as_ptr()) })
        .ok_or(ProbeError::Unreadable)?;
    struct Guard(std::ptr::NonNull<std::ffi::c_void>);
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { diffractor_metadata_free(self.0.as_ptr()) }
        }
    }
    let guard = Guard(ptr);
    let text = |field| {
        let value = unsafe { diffractor_metadata_text(guard.0.as_ptr(), field) };
        if value.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(value) }
                .to_string_lossy()
                .into_owned()
        }
    };
    let mut rating = 0;
    let has_rating = unsafe { diffractor_metadata_rating(guard.0.as_ptr(), &mut rating) } != 0;
    Ok(Metadata {
        title: text(0),
        camera_make: text(1),
        camera_model: text(2),
        description: text(3),
        keywords: text(4),
        lens: text(5),
        rating: has_rating.then_some(rating),
    })
}

/// Decode on a worker. Target dimensions guide codecs that support reduced decoding.
pub fn decode_image(path: &Path, target: (u32, u32)) -> Result<DecodedImage, DecodeError> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| DecodeError::InvalidPath)?;
    // The native object is owned exclusively here and freed on all paths by the guard.
    let ptr = std::ptr::NonNull::new(unsafe {
        diffractor_decode_image(path.as_ptr(), target.0, target.1)
    })
    .ok_or(DecodeError::Failed)?;
    struct Guard(std::ptr::NonNull<std::ffi::c_void>);
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { diffractor_image_free(self.0.as_ptr()) }
        }
    }
    let image = Guard(ptr);
    let (width, height, orientation, pixels) = unsafe {
        (
            diffractor_image_width(image.0.as_ptr()),
            diffractor_image_height(image.0.as_ptr()),
            diffractor_image_orientation(image.0.as_ptr()),
            diffractor_image_pixels(image.0.as_ptr()),
        )
    };
    let len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .filter(|&n| n > 0 && n <= 256 * 1024 * 1024)
        .ok_or(DecodeError::Failed)?;
    if pixels.is_null() {
        return Err(DecodeError::Failed);
    }
    // The C++ guard owns at least width * height * 4 bytes until it is dropped below.
    let mut rgba = unsafe { std::slice::from_raw_parts(pixels, len) }.to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(DecodedImage {
        width,
        height,
        orientation,
        rgba,
    })
}

/// Scan image headers using Diffractor's libfiles. This performs blocking file I/O.
pub fn probe_image(path: &Path) -> Result<ImageInfo, ProbeError> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| ProbeError::InvalidPath)?;
    let mut info = RawImageInfo::default();
    // Both pointers stay valid for the call; the bridge does not retain either one.
    let code = unsafe { diffractor_probe_image(path.as_ptr(), &mut info) };
    match code {
        0 => Ok(ImageInfo {
            width: info.width,
            height: info.height,
            orientation: info.orientation,
            container_transform: info.container_transform != 0,
        }),
        1 => Err(ProbeError::InvalidPath),
        2 => Err(ProbeError::Unreadable),
        3 => Err(ProbeError::UnsupportedOrMalformed),
        other => Err(ProbeError::UnexpectedCode(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../exe/test")
            .join(name)
    }

    #[test]
    fn scans_real_jpeg_and_gif() {
        let jpeg = probe_image(&fixture("Test.jpg")).unwrap();
        assert_eq!((jpeg.width, jpeg.height, jpeg.orientation), (1024, 683, 1));
        let gif = probe_image(&fixture("tuesday.gif")).unwrap();
        assert_eq!((gif.width, gif.height), (300, 300));
    }

    #[test]
    fn rejects_unreadable_invalid_and_non_image_paths() {
        assert_eq!(
            probe_image(&fixture("missing-image")),
            Err(ProbeError::Unreadable)
        );
        assert_eq!(
            probe_image(&fixture("Gherkin.xmp")),
            Err(ProbeError::UnsupportedOrMalformed)
        );
        let nul = std::ffi::OsString::from_vec(b"invalid\0path".to_vec());
        assert_eq!(probe_image(Path::new(&nul)), Err(ProbeError::InvalidPath));
    }

    #[test]
    fn decodes_pixels_and_rejects_non_images() {
        let image = decode_image(&fixture("Test.jpg"), (256, 256)).unwrap();
        assert_eq!(
            (image.width, image.height, image.orientation),
            (512, 342, 1)
        );
        assert_eq!(
            image.rgba.len(),
            image.width as usize * image.height as usize * 4
        );
        assert!(image.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        let full = decode_image(&fixture("Test.jpg"), (0, 0)).unwrap();
        let info = probe_image(&fixture("Test.jpg")).unwrap();
        assert_eq!((full.width, full.height), (info.width, info.height));
        assert!(decode_image(&fixture("Gherkin.xmp"), (256, 256)).is_err());
    }

    #[test]
    fn decodes_a_rotated_video_preview_and_rejects_stills() {
        let worker = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp/linux-build/diffractor");
        let frame = video_preview(&worker, &fixture("excluded1/rotated90.mp4")).unwrap();
        assert_eq!(frame.orientation, 6);
        assert!(frame.width > 0 && frame.height > 0);
        assert_eq!(
            frame.rgba.len(),
            frame.width as usize * frame.height as usize * 4
        );
        assert!(frame
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255));
        assert!(matches!(
            video_preview(&worker, &fixture("Test.jpg")),
            Err(DecodeError::Failed)
        ));
    }

    #[test]
    fn reads_embedded_photo_metadata_without_changing_file() {
        let metadata = read_metadata(&fixture("Test.jpg")).unwrap();
        assert_eq!(metadata.title, "Title");
        assert_eq!(metadata.camera_model, "Canon EOS 7D");
        assert_eq!(metadata.description, "Caption");
        assert_eq!(metadata.keywords, "key1, key2, key3");
        assert_eq!(metadata.rating, Some(4));
        assert_eq!(
            read_metadata(&fixture("missing-image")),
            Err(ProbeError::Unreadable)
        );
    }

    #[test]
    fn reads_raw_sidecar_and_treats_empty_sidecar_as_authoritative() {
        let dir = std::env::temp_dir().join(format!("diffractor-raw-xmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("photo.CR2");
        std::fs::copy(fixture("Gherkin.CR2"), &raw).unwrap();
        let sidecar = dir.join("photo.xmp");
        std::fs::write(&sidecar, r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmp:Rating="3"><dc:title><rdf:Alt><rdf:li xml:lang="x-default">Sidecar title</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta>"#).unwrap();
        let before = std::fs::metadata(&raw).unwrap().len();
        let metadata = read_metadata(&raw).unwrap();
        assert_eq!(metadata.title, "Sidecar title");
        assert_eq!(metadata.rating, Some(3));
        assert!(!metadata.camera_model.is_empty());
        assert_eq!(std::fs::metadata(&raw).unwrap().len(), before);
        std::fs::write(&sidecar, "").unwrap();
        assert_eq!(read_metadata(&raw).unwrap().rating, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn searches_existing_index_and_rejects_outside_scope() {
        let root = std::env::temp_dir().join(format!(
            "diffractor-search-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let media = root.join("media");
        let cache = root.join("cache");
        std::fs::create_dir(&media).unwrap();
        std::fs::copy(fixture("Test.jpg"), media.join("photo.jpg")).unwrap();
        let worker = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp/linux-build/diffractor");
        let search =
            |query: &str| search_folder_with_cache(&worker, &media, query, || false, Some(&cache));
        let matches = search("key1").unwrap();
        assert!(matches
            .iter()
            .any(|p| p.file_name() == Some(std::ffi::OsStr::new("photo.jpg"))));
        assert!(search("not-a-real-tag-9348").unwrap().is_empty());
        assert!(matches!(search("/tmp"), Err(SearchError::Worker { .. })));
        assert!(matches!(
            search_folder_with_cache(
                &worker,
                &root.join("missing"),
                "key1",
                || false,
                Some(&cache)
            ),
            Err(SearchError::Worker { .. })
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_index_reuses_and_refreshes_metadata() {
        let root = std::env::temp_dir().join(format!(
            "diffractor-cache-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let media = root.join("media");
        let cache = root.join("cache");
        std::fs::create_dir(&media).unwrap();
        std::fs::copy(fixture("Test.jpg"), media.join("photo.jpg")).unwrap();
        let worker = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp/linux-build/diffractor");
        let run = || {
            Command::new(&worker)
                .arg("--index-query")
                .arg(&media)
                .arg("key1")
                .env("DIFFRACTOR_INDEX_CACHE_DIR", &cache)
                .env("DIFFRACTOR_INDEX_REPORT_STATS", "1")
                .output()
                .unwrap()
        };
        let first = run();
        assert!(first.status.success());
        assert!(String::from_utf8_lossy(&first.stderr).contains("scanned=1"));
        assert!(first
            .stdout
            .windows(b"photo.jpg".len())
            .any(|w| w == b"photo.jpg"));
        let second = run();
        assert!(second.status.success());
        assert!(String::from_utf8_lossy(&second.stderr).contains("scanned=0"));
        assert_eq!(first.stdout, second.stdout);
        std::fs::copy(fixture("engine.png"), media.join("photo.jpg")).unwrap();
        let changed = run();
        assert!(changed.status.success());
        assert!(String::from_utf8_lossy(&changed.stderr).contains("scanned=1"));
        assert_eq!(changed.stdout, b"DIFIDX1\0");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_workers_serialize_access_to_the_same_cache() {
        use std::io::{Read, Write};
        let root = std::env::temp_dir().join(format!(
            "diffractor-lock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let media = root.join("media");
        let cache = root.join("cache");
        std::fs::create_dir(&media).unwrap();
        std::fs::copy(fixture("Test.jpg"), media.join("photo.jpg")).unwrap();
        let worker = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp/linux-build/diffractor");
        search_folder_with_cache(&worker, &media, "key1", || false, Some(&cache)).unwrap();
        let lock = std::fs::read_dir(&cache)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|ext| ext == "lock"))
            .unwrap();
        let mut holder = Command::new("flock")
            .arg("-x")
            .arg(lock)
            .arg("sh")
            .arg("-c")
            .arg("printf ready; read line")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = [0u8; 5];
        holder
            .stdout
            .take()
            .unwrap()
            .read_exact(&mut ready)
            .unwrap();
        assert_eq!(&ready, b"ready");
        let (tx, rx) = std::sync::mpsc::channel();
        let worker2 = worker.clone();
        let media2 = media.clone();
        let cache2 = cache.clone();
        let query = std::thread::spawn(move || {
            tx.send(search_folder_with_cache(
                &worker2,
                &media2,
                "key1",
                || false,
                Some(&cache2),
            ))
            .unwrap();
        });
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(200)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        holder.stdin.take().unwrap().write_all(b"\n").unwrap();
        assert!(holder.wait().unwrap().success());
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .len(),
            1
        );
        query.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancels_and_reaps_a_running_search_worker() {
        use std::{
            os::unix::fs::PermissionsExt,
            sync::{
                atomic::{AtomicBool, Ordering},
                Arc,
            },
        };
        let script = std::env::temp_dir().join(format!(
            "diffractor-cancel-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let worker = script.clone();
        let task = std::thread::spawn(move || {
            search_folder_cancellable(&worker, Path::new("/"), "x", || {
                flag.load(Ordering::Acquire)
            })
        });
        std::thread::sleep(Duration::from_millis(100));
        let started = std::time::Instant::now();
        cancelled.store(true, Ordering::Release);
        assert!(matches!(task.join().unwrap(), Err(SearchError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(3));
        std::fs::remove_file(script).unwrap();
    }

    #[test]
    fn drains_worker_output_larger_than_a_pipe_buffer() {
        use std::os::unix::fs::PermissionsExt;
        let script = std::env::temp_dir().join(format!(
            "diffractor-pipe-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&script, "#!/bin/sh\nyes x | head -c 131072\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = search_folder(&script, Path::new("/"), "x");
        assert!(
            matches!(result, Err(SearchError::InvalidResponse)),
            "{result:?}"
        );
        std::fs::remove_file(script).unwrap();
    }
}
