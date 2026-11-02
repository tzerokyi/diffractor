mod history;
mod locale;
mod selection;
mod video;

use history::{History, Travel, Visit};
use selection::{Intent, Selection};
use locale::{tr, Locale, Status, Text};

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
};

use diffractor_core::{
    decode_image, read_metadata, search_folder_cancellable, video_preview, DecodedImage, Metadata,
    SearchError,
};
use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle};

const MAX_ENTRIES: usize = 10_000;
const MAX_THUMBNAILS: usize = 96;
const MAX_THUMBNAIL_PENDING: usize = 48;

fn selection_intent(modifiers: egui::Modifiers) -> Intent {
    match (modifiers.shift, modifiers.command || modifiers.ctrl) {
        (true, true) => Intent::AddRange,
        (true, false) => Intent::Range,
        (false, true) => Intent::Toggle,
        (false, false) => Intent::Replace,
    }
}

fn clamp_pan(pan: egui::Vec2, image: egui::Vec2, viewport: egui::Vec2) -> egui::Vec2 {
    let limit = ((image - viewport) * 0.5).max(egui::Vec2::ZERO);
    egui::vec2(
        pan.x.clamp(-limit.x, limit.x),
        pan.y.clamp(-limit.y, limit.y),
    )
}

fn anchored_pan(pan: egui::Vec2, anchor: egui::Vec2, old_zoom: f32, new_zoom: f32) -> egui::Vec2 {
    anchor - (anchor - pan) * (new_zoom / old_zoom)
}

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    directory: bool,
}

enum Request {
    Folder(u64, PathBuf),
    OpenPath(u64, PathBuf),
    Image(u64, PathBuf),
    Metadata(u64, PathBuf),
    Compare(u64, PathBuf, PathBuf),
    FullImage(u64, PathBuf),
}

enum Response {
    Folder(
        u64,
        PathBuf,
        Result<(Vec<Entry>, bool), String>,
        Option<PathBuf>,
    ),
    Image(
        u64,
        PathBuf,
        Result<(ColorImage, Option<Metadata>, bool), String>,
    ),
    Metadata(u64, PathBuf, Result<Metadata, String>),
    Compare(u64, PathBuf, PathBuf, Result<(ColorImage, ColorImage), String>),
    Search(u64, PathBuf, Result<Vec<PathBuf>, String>),
    FullImage(u64, PathBuf, Result<ColorImage, String>),
    Playback(u64, Result<(i32, i64, i64), String>),
    Thumbnail(u64, PathBuf, Option<ColorImage>),
}

struct ThumbnailRequest {
    generation: u64,
    path: PathBuf,
}

enum PlaybackRequest {
    Open(u64, PathBuf, Arc<video::Shared>),
    Pause(u64, bool),
    Seek(u64, i64),
    Stop,
}

struct SearchRequest {
    id: u64,
    folder: PathBuf,
    query: String,
}

fn index_worker_path() -> PathBuf {
    if let Some(path) = std::env::var_os("DIFFRACTOR_INDEX_WORKER") {
        return path.into();
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            let sibling = parent.join("diffractor");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp/linux-build/diffractor")
}

fn is_image(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
        [
            "jpg", "jpeg", "png", "webp", "heic", "heif", "avif", "jxl", "gif", "tif", "tiff",
            "bmp", "psd", "cr2", "cr3", "nef", "nrw", "arw", "dng", "raf", "orf", "rw2", "pef",
            "srw", "3fr", "iiq", "erf", "kdc", "mos", "mrw", "x3f",
        ]
        .iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
    })
}

fn is_video(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
        [
            "mp4", "m4v", "mov", "mkv", "webm", "avi", "mpg", "mpeg", "m2ts", "mts", "ts", "wmv",
            "flv", "3gp",
        ]
        .iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
    })
}

fn is_audio(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
        [
            "mp3", "m4a", "aac", "flac", "wav", "ogg", "opus", "wma", "aiff", "aif",
        ]
        .iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
    })
}

// The media list is the browsing order. Directories cannot become preview
// targets, and reaching the first/last item does not wrap implicitly.
// Columns are derived from the actual available pane width, not screen width.
// Selected set order is not image presentation order: the source list order
// determines the left/right comparison panes and stays stable across refactors.
fn comparison_paths(visible: &[PathBuf], selection: &Selection) -> Option<(PathBuf, PathBuf)> {
    if selection.len() != 2 {
        return None;
    }
    let mut paths = visible.iter().filter(|path| selection.contains(path));
    let left = paths.next()?;
    let right = paths.next()?;
    if is_image(left) && is_image(right) {
        Some((left.clone(), right.clone()))
    } else {
        None
    }
}

fn grid_columns(width: f32) -> usize {
    if !width.is_finite() {
        return 1;
    }
    (width.max(0.0) / 146.0).floor().clamp(1.0, 4.0) as usize
}

fn next_media_index(entries: &[Entry], selected: Option<&Path>, reverse: bool) -> Option<usize> {
    let current = selected.and_then(|path| {
        entries.iter().position(|item| !item.directory && item.path == path)
    });
    if reverse {
        match current {
            Some(index) => entries[..index]
                .iter()
                .rposition(|item| !item.directory)
                .or(Some(index)),
            None => entries.iter().rposition(|item| !item.directory),
        }
    } else {
        entries.iter().enumerate().skip(current.map_or(0, |index| index + 1))
            .find(|(_, item)| !item.directory)
            .map(|(index, _)| index)
            .or(current)
    }
}

fn list_folder(path: &Path) -> Result<(Vec<Entry>, bool), String> {
    let mut items = Vec::new();
    let mut truncated = false;
    let entries = std::fs::read_dir(path).map_err(|e| format!("Cannot open folder: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Cannot read folder entry: {e}"))?;
        let kind = entry
            .file_type()
            .map_err(|e| format!("Cannot inspect an entry: {e}"))?;
        let path = entry.path();
        // Symlinks may point to folders or files. Resolving them is still worker-side I/O.
        let directory = if kind.is_symlink() {
            path.is_dir()
        } else {
            kind.is_dir()
        };
        if directory || is_image(&path) || is_video(&path) || is_audio(&path) {
            if items.len() == MAX_ENTRIES {
                truncated = true;
                break;
            }
            items.push(Entry { path, directory });
        }
    }
    items.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.path.file_name().cmp(&b.path.file_name()))
    });
    Ok((items, truncated))
}

// Resolve on the worker: file-manager arguments may be relative, missing, or symlinks.
fn open_target(path: &Path) -> Result<(PathBuf, Option<PathBuf>), String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Cannot resolve path: {e}"))?
            .join(path)
    };
    let kind = std::fs::metadata(&path).map_err(|e| format!("Cannot open path: {e}"))?;
    if kind.is_dir() {
        Ok((path, None))
    } else if kind.is_file() && (is_image(&path) || is_video(&path) || is_audio(&path)) {
        let folder = path
            .parent()
            .ok_or("File has no parent folder.")?
            .to_path_buf();
        Ok((folder, Some(path)))
    } else {
        Err("This path is not a supported media file or folder.".into())
    }
}

// EXIF orientation is applied once here; libfiles reports 1 for container-transformed images.
fn orient(image: DecodedImage) -> ColorImage {
    let (w, h) = (image.width as usize, image.height as usize);
    let rotated = matches!(image.orientation, 5..=8);
    let (out_w, out_h) = if rotated { (h, w) } else { (w, h) };
    let mut rgba = vec![0; image.rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = match image.orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                8 => (y, w - 1 - x),
                _ => (x, y),
            };
            let from = (y * w + x) * 4;
            let to = (dy * out_w + dx) * 4;
            rgba[to..to + 4].copy_from_slice(&image.rgba[from..from + 4]);
        }
    }
    ColorImage::from_rgba_unmultiplied([out_w, out_h], &rgba)
}

// Sample the upright image directly into a small texture, without allocating a full-size rotated copy.
fn thumbnail(image: DecodedImage) -> ColorImage {
    let (w, h) = (image.width as usize, image.height as usize);
    let (ow, oh) = if matches!(image.orientation, 5..=8) {
        (h, w)
    } else {
        (w, h)
    };
    let scale = (144.0 / ow as f64).min(96.0 / oh as f64).min(1.0);
    let (tw, th) = (
        ((ow as f64 * scale).round() as usize).max(1),
        ((oh as f64 * scale).round() as usize).max(1),
    );
    let mut rgba = vec![0; tw * th * 4];
    for y in 0..th {
        for x in 0..tw {
            let (dx, dy) = (x * ow / tw, y * oh / th);
            let (sx, sy) = match image.orientation {
                2 => (w - 1 - dx, dy),
                3 => (w - 1 - dx, h - 1 - dy),
                4 => (dx, h - 1 - dy),
                5 => (dy, dx),
                6 => (dy, h - 1 - dx),
                7 => (w - 1 - dy, h - 1 - dx),
                8 => (w - 1 - dy, dx),
                _ => (dx, dy),
            };
            let from = (sy * w + sx) * 4;
            let to = (y * tw + x) * 4;
            rgba[to..to + 4].copy_from_slice(&image.rgba[from..from + 4]);
        }
    }
    ColorImage::from_rgba_unmultiplied([tw, th], &rgba)
}

struct Comparison {
    left: TextureHandle,
    right: TextureHandle,
}

struct Browser {
    sender: Sender<Request>,
    search_sender: Sender<SearchRequest>,
    playback_sender: Sender<PlaybackRequest>,
    thumbnail_sender: Sender<ThumbnailRequest>,
    receiver: Receiver<Response>,
    folder: PathBuf,
    address: String,
    entries: Vec<Entry>,
    // This is the displayed media, NOT the complete selected batch target.
    selected: Option<PathBuf>,
    comparison: Option<Comparison>,
    selection: Selection,
    visible_media: Vec<PathBuf>,
    grid_view: bool,
    fullscreen: bool,
    texture: Option<TextureHandle>,
    zoom: f32,
    pan: egui::Vec2,
    full_image_requested: bool,
    metadata: Option<Metadata>,
    status: Status,
    language: Locale,
    folder_generation: u64,
    folder_open_pending: bool,
    history: History,
    pending_travel: Travel,
    image_generation: u64,
    truncated: bool,
    search_text: String,
    search_generation: u64,
    search_currency: Arc<AtomicU64>,
    search_active: bool,
    search_pending: bool,
    playback_generation: u64,
    playback_shared: Option<Arc<video::Shared>>,
    playback_state: i32,
    playback_time: i64,
    playback_duration: i64,
    video_aspect: Option<f32>,
    thumbnail_generation: u64,
    thumbnail_currency: Arc<AtomicU64>,
    thumbnails: HashMap<PathBuf, TextureHandle>,
    thumbnail_order: VecDeque<PathBuf>,
    thumbnail_pending: HashSet<PathBuf>,
    thumbnail_failed: HashSet<PathBuf>,
    #[cfg(feature = "visual-audit")]
    audit_image: Option<PathBuf>,
    #[cfg(feature = "visual-audit")]
    audit_query: Option<String>,
    #[cfg(feature = "visual-audit")]
    audit_output: Option<PathBuf>,
    #[cfg(feature = "visual-audit")]
    audit_autoplay: bool,
    #[cfg(feature = "visual-audit")]
    audit_frame_seen: bool,
}

impl Browser {
    fn new(cc: &eframe::CreationContext<'_>, initial: PathBuf) -> Self {
        // Native dark chrome: controls, panels and selection share one palette.
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 7.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = Color32::from_rgb(24, 27, 32);
        style.visuals.extreme_bg_color = Color32::from_rgb(17, 20, 24);
        style.visuals.faint_bg_color = Color32::from_rgb(34, 39, 45);
        style.visuals.selection.bg_fill = Color32::from_rgb(44, 87, 116);
        style.visuals.selection.stroke = egui::Stroke::new(
            1.0,
            Color32::from_rgb(173, 215, 233),
        );
        style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(40, 45, 52);
        style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(55, 64, 74);
        cc.egui_ctx.set_style(style);

        let (sender, requests) = mpsc::channel();
        let (responses, receiver) = mpsc::channel();
        let (thumbnail_sender, thumbnail_requests) = mpsc::channel::<ThumbnailRequest>();
        let thumbnail_currency = Arc::new(AtomicU64::new(0));
        let thumbnail_worker_currency = Arc::clone(&thumbnail_currency);
        let thumbnail_responses = responses.clone();
        let thumbnail_context = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            while let Ok(request) = thumbnail_requests.recv() {
                if thumbnail_worker_currency.load(Ordering::Acquire) != request.generation {
                    continue;
                }
                let decoded = if is_video(&request.path) {
                    video_preview(&index_worker_path(), &request.path)
                } else {
                    decode_image(&request.path, (144, 96))
                };
                if thumbnail_worker_currency.load(Ordering::Acquire) != request.generation {
                    continue;
                }
                if thumbnail_responses
                    .send(Response::Thumbnail(
                        request.generation,
                        request.path,
                        decoded.ok().map(thumbnail),
                    ))
                    .is_err()
                {
                    break;
                }
                thumbnail_context.request_repaint();
            }
        });
        let (playback_sender, playback_requests) = mpsc::channel::<PlaybackRequest>();
        let playback_responses = responses.clone();
        let playback_context = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let mut player: Option<(u64, video::Player)> = None;
            loop {
                match playback_requests.recv_timeout(std::time::Duration::from_millis(200)) {
                    Ok(PlaybackRequest::Open(id, path, shared)) => {
                        player = None;
                        let mute = cfg!(feature = "visual-audit")
                            && std::env::var_os("DIFFRACTOR_AUDIT_MUTE").is_some();
                        match video::Player::open(&path, shared, mute) {
                            Ok(opened) => player = Some((id, opened)),
                            Err(error) => {
                                let _ = playback_responses.send(Response::Playback(id, Err(error)));
                            }
                        }
                    }
                    Ok(PlaybackRequest::Pause(id, paused)) => {
                        if let Some((current, player)) = &player {
                            if *current == id {
                                player.pause(paused);
                            }
                        }
                    }
                    Ok(PlaybackRequest::Seek(id, time)) => {
                        if let Some((current, player)) = &player {
                            if *current == id {
                                player.seek(time);
                            }
                        }
                    }
                    Ok(PlaybackRequest::Stop) => player = None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if let Some((id, player)) = &player {
                    if playback_responses
                        .send(Response::Playback(
                            *id,
                            Ok((player.state(), player.time(), player.duration())),
                        ))
                        .is_err()
                    {
                        break;
                    }
                    playback_context.request_repaint();
                }
            }
        });
        let (search_sender, searches) = mpsc::channel::<SearchRequest>();
        let search_currency = Arc::new(AtomicU64::new(0));
        let worker_currency = Arc::clone(&search_currency);
        let search_responses = responses.clone();
        let search_context = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            while let Ok(mut request) = searches.recv() {
                while let Ok(newer) = searches.try_recv() {
                    request = newer;
                }
                let folder = if request.folder.is_absolute() {
                    request.folder.clone()
                } else {
                    std::env::current_dir()
                        .unwrap_or_default()
                        .join(&request.folder)
                };
                let result = search_folder_cancellable(
                    &index_worker_path(),
                    &folder,
                    &request.query,
                    || worker_currency.load(Ordering::Acquire) != request.id,
                )
                .map_err(|error| match error {
                    SearchError::Worker { message, .. } => message,
                    SearchError::Start(error) => format!("Cannot start search: {error}"),
                    SearchError::Io(error) => format!("Search I/O failed: {error}"),
                    SearchError::InvalidResponse => {
                        "Search worker returned an invalid response.".to_owned()
                    }
                    SearchError::OutputTooLarge => {
                        "Search results exceed the memory limit.".to_owned()
                    }
                    SearchError::Cancelled => "Search cancelled.".to_owned(),
                });
                if search_responses
                    .send(Response::Search(request.id, request.folder, result))
                    .is_err()
                {
                    break;
                }
                search_context.request_repaint();
            }
        });
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            while let Ok(mut request) = requests.recv() {
                // Keep only the newest queued action after any in-flight operation.
                while let Ok(newer) = requests.try_recv() {
                    request = newer;
                }
                let response = match request {
                    Request::Folder(id, path) => {
                        let result = list_folder(&path);
                        Response::Folder(id, path, result, None)
                    }
                    Request::OpenPath(id, path) => match open_target(&path) {
                        Ok((folder, selected)) => {
                            let result = list_folder(&folder);
                            Response::Folder(id, folder, result, selected)
                        }
                        Err(error) => Response::Folder(id, path, Err(error), None),
                    },
                    Request::Image(id, path) => {
                        let video = is_video(&path);
                        let decoded = if video {
                            video_preview(&index_worker_path(), &path)
                        } else {
                            decode_image(&path, (1600, 1200))
                        };
                        let result = decoded
                            .map(|image| (orient(image), read_metadata(&path).ok(), video))
                            .map_err(|_| {
                                "Cannot decode this image with the current media backend."
                                    .to_owned()
                            });
                        Response::Image(id, path, result)
                    }
                    Request::Metadata(id, path) => {
                        Response::Metadata(
                            id,
                            path.clone(),
                            read_metadata(&path).map_err(|error| format!("{error:?}")),
                        )
                    }
                    Request::Compare(id, left, right) => {
                        // Both decoded pictures are bounded by their requested
                        // preview extent. Source images remain untouched.
                        let result = decode_image(&left, (1200, 1000))
                            .and_then(|first| decode_image(&right, (1200, 1000))
                                .map(|second| (orient(first), orient(second))))
                            .map_err(|error| format!("{error:?}"));
                        Response::Compare(id, left, right, result)
                    }
                    Request::FullImage(id, path) => {
                        let result = decode_image(&path, (0, 0))
                            .map(orient)
                            .map_err(|_| "Full-resolution image could not be loaded.".to_owned());
                        Response::FullImage(id, path, result)
                    }
                };
                if responses.send(response).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        let mut browser = Self {
            sender,
            search_sender,
            playback_sender,
            thumbnail_sender,
            receiver,
            folder: PathBuf::new(),
            address: String::new(),
            entries: Vec::new(),
            selected: None,
            comparison: None,
            selection: Selection::default(),
            visible_media: Vec::new(),
            grid_view: true,
            fullscreen: false,
            texture: None,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            full_image_requested: false,
            metadata: None,
            status: Status::Empty,
            language: Locale::detect(),
            folder_generation: 0,
            folder_open_pending: false,
            history: History::default(),
            pending_travel: Travel::Push,
            image_generation: 0,
            truncated: false,
            search_text: String::new(),
            search_generation: 0,
            search_currency,
            search_active: false,
            search_pending: false,
            playback_generation: 0,
            playback_shared: None,
            playback_state: 0,
            playback_time: 0,
            playback_duration: 0,
            video_aspect: None,
            thumbnail_generation: 0,
            thumbnail_currency,
            thumbnails: HashMap::new(),
            thumbnail_order: VecDeque::new(),
            thumbnail_pending: HashSet::new(),
            thumbnail_failed: HashSet::new(),
            #[cfg(feature = "visual-audit")]
            audit_image: std::env::var_os("DIFFRACTOR_AUDIT_IMAGE").map(PathBuf::from),
            #[cfg(feature = "visual-audit")]
            audit_query: std::env::var("DIFFRACTOR_AUDIT_QUERY").ok(),
            #[cfg(feature = "visual-audit")]
            audit_output: std::env::var_os("DIFFRACTOR_AUDIT_OUTPUT").map(PathBuf::from),
            #[cfg(feature = "visual-audit")]
            audit_autoplay: std::env::var_os("DIFFRACTOR_AUDIT_AUTOPLAY").is_some(),
            #[cfg(feature = "visual-audit")]
            audit_frame_seen: false,
        };
        browser.open_path(initial);
        browser
    }

    fn open_folder(&mut self, path: PathBuf) {
        self.begin_open(path, false);
    }

    fn open_path(&mut self, path: PathBuf) {
        self.begin_open(path, true);
    }

    fn begin_open(&mut self, path: PathBuf, resolve: bool) {
        self.begin_travel(path, resolve, Travel::Push);
    }

    fn go_back(&mut self) {
        if let Some(target) = self.history.back_target().map(Path::to_path_buf) {
            self.begin_travel(target, false, Travel::Back);
        }
    }

    fn go_forward(&mut self) {
        if let Some(target) = self.history.forward_target().map(Path::to_path_buf) {
            self.begin_travel(target, false, Travel::Forward);
        }
    }

    fn begin_travel(&mut self, path: PathBuf, resolve: bool, travel: Travel) {
        // A failed or refused path must never destroy the current browser state.
        // Only a successful Folder response commits the replacement view.
        self.folder_generation += 1;
        self.folder_open_pending = true;
        self.pending_travel = travel;
        self.search_generation += 1;
        self.search_currency
            .store(self.search_generation, Ordering::Release);
        self.search_pending = false;
        self.status = Status::Text(Text::OpeningFolder);
        let request = if resolve {
            Request::OpenPath(self.folder_generation, path)
        } else {
            Request::Folder(self.folder_generation, path)
        };
        if self.sender.send(request).is_err() {
            self.folder_open_pending = false;
            self.status = Status::Error("Folder worker is unavailable.".into());
        }
    }

    fn clear_thumbnails(&mut self) {
        self.thumbnail_generation += 1;
        self.thumbnail_currency
            .store(self.thumbnail_generation, Ordering::Release);
        self.thumbnails.clear();
        self.thumbnail_order.clear();
        self.thumbnail_pending.clear();
        self.thumbnail_failed.clear();
    }

    fn request_thumbnail(&mut self, path: PathBuf) {
        if self.thumbnail_pending.len() >= MAX_THUMBNAIL_PENDING
            || self.thumbnails.contains_key(&path)
            || self.thumbnail_pending.contains(&path)
            || self.thumbnail_failed.contains(&path)
        {
            return;
        }
        self.thumbnail_pending.insert(path.clone());
        if self
            .thumbnail_sender
            .send(ThumbnailRequest {
                generation: self.thumbnail_generation,
                path: path.clone(),
            })
            .is_err()
        {
            self.thumbnail_pending.remove(&path);
        }
    }

    fn search(&mut self) {
        if self.folder_open_pending
            || self.folder.as_os_str().is_empty()
            || self.search_text.trim().is_empty() {
            return;
        }
        self.search_generation += 1;
        self.search_currency
            .store(self.search_generation, Ordering::Release);
        self.search_pending = true;
        self.status = Status::Text(Text::Indexing);
        let _ = self.search_sender.send(SearchRequest {
            id: self.search_generation,
            folder: self.folder.clone(),
            query: self.search_text.clone(),
        });
    }

    // Focus/selection are source-owned state, not inferred from rendered
    // thumbnails. Keyboard arrows reuse the same grammar as mouse selection.
    fn step_media(&mut self, direction: isize, extend: bool) {
        self.selection
            .move_focus(&self.visible_media, direction < 0, extend);
        self.present_selection();
    }

    fn select_media(&mut self, path: PathBuf, intent: Intent) {
        if self.folder_open_pending {
            return;
        }
        self.selection.apply(&self.visible_media, &path, intent);
        self.present_selection();
    }

    fn comparison_pair(&self) -> Option<(PathBuf, PathBuf)> {
        comparison_paths(&self.visible_media, &self.selection)
    }

    fn present_selection(&mut self) {
        let single = self.selection.single().map(Path::to_path_buf);
        if let Some(path) = single {
            self.comparison = None;
            if self.selected.as_ref() != Some(&path) {
                self.open_image(path);
            }
            return;
        }
        let pair = self.comparison_pair();
        if self.selected.is_some() || self.texture.is_some() || self.comparison.is_some() || pair.is_some() {
            self.stop_playback();
            self.image_generation += 1;
            self.selected = None;
            self.texture = None;
            self.comparison = None;
            self.metadata = None;
            self.video_aspect = None;
            self.full_image_requested = false;
        }
        if let Some((left, right)) = pair {
            self.status = Status::Text(Text::LoadingCompare);
            let _ = self.sender.send(Request::Compare(self.image_generation, left, right));
        } else if self.selection.len() > 1 {
            self.status = Status::Empty;
        }
    }

    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        // Keep the same folder/selection/media state; fullscreen only changes
        // presentation and native window mode.
        if self.selected.is_none() && !self.fullscreen {
            return;
        }
        self.fullscreen = !self.fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
    }

    fn open_image(&mut self, path: PathBuf) {
        // A pending folder transaction owns the next state transition.
        // Enqueueing an image here would supersede its folder request.
        if self.folder_open_pending {
            return;
        }
        self.stop_playback();
        self.zoom = 1.0;
        self.pan = egui::Vec2::ZERO;
        self.full_image_requested = false;
        self.image_generation += 1;
        self.selected = Some(path.clone());
        self.texture = None;
        self.metadata = None;
        self.video_aspect = None;
        if is_audio(&path) {
            self.status = Status::Text(Text::AudioReady);
            let _ = self.sender.send(Request::Metadata(self.image_generation, path));
            return;
        }
        self.status = Status::Text(Text::LoadingImage);
        let _ = self
            .sender
            .send(Request::Image(self.image_generation, path));
    }

    fn stop_playback(&mut self) {
        self.playback_generation += 1;
        self.playback_shared = None;
        self.playback_state = 0;
        self.playback_time = 0;
        self.playback_duration = 0;
        let _ = self.playback_sender.send(PlaybackRequest::Stop);
    }

    fn start_playback(&mut self, ctx: &egui::Context) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        self.playback_generation += 1;
        self.playback_state = 1;
        self.status = Status::Text(Text::OpeningMedia);
        let repaint = ctx.clone();
        let shared = video::Shared::new(move || repaint.request_repaint());
        self.playback_shared = Some(shared.clone());
        let _ = self.playback_sender.send(PlaybackRequest::Open(
            self.playback_generation,
            path,
            shared,
        ));
    }

    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(response) = self.receiver.try_recv() {
            match response {
                Response::Folder(id, path, result, selected) if id == self.folder_generation => {
                    self.folder_open_pending = false;
                    match result {
                        Ok((entries, truncated)) => {
                            let previous = if self.folder.as_os_str().is_empty() {
                                None
                            } else {
                                Some(Visit {
                                    folder: self.folder.clone(),
                                    selected: self.selected.clone(),
                                })
                            };
                            let remembered_selection = self.history.commit(
                                self.pending_travel,
                                previous,
                                &path,
                            );
                            // Commit all view changes together; failed opening leaves
                            // selection, preview, playback and thumbnails intact.
                            self.stop_playback();
                            self.clear_thumbnails();
                            self.image_generation += 1;
                            self.selected = None;
                            self.selection.clear();
                            self.comparison = None;
                            self.texture = None;
                            self.metadata = None;
                            self.video_aspect = None;
                            self.search_active = false;
                            self.folder = path;
                            self.address = self.folder.to_string_lossy().into_owned();
                            self.entries = entries;
                            self.visible_media = self.entries.iter()
                                .filter(|entry| !entry.directory)
                                .map(|entry| entry.path.clone())
                                .collect();
                            self.truncated = truncated;
                            self.status = Status::Text(if self.entries.is_empty() {
                                Text::EmptyFolder
                            } else {
                                Text::SelectImage
                            });
                            // Explicit file-manager targets were already checked by
                            // open_target and can sit beyond the 10k list cap. Only
                            // history-restored selections require list membership.
                            let restore = selected.or_else(|| remembered_selection.filter(|path| {
                                self.entries.iter().any(|entry| !entry.directory && entry.path == *path)
                            }));
                            if let Some(path) = restore {
                                self.selection.set_direct(path.clone());
                                self.open_image(path);
                            }
                            #[cfg(feature = "visual-audit")]
                            if let Some(query) = self.audit_query.take() {
                                self.search_text = query;
                                self.search();
                            } else if let Some(path) = self.audit_image.take() {
                                self.selection.set_direct(path.clone());
                                self.open_image(path);
                            }
                        }
                        Err(error) => self.status = Status::Error(error),
                    }
                }
                Response::Image(id, path, result)
                    if id == self.image_generation && self.selected.as_ref() == Some(&path) =>
                {
                    match result {
                        Ok((image, metadata, video)) => {
                            let size = image.size;
                            if video && size[1] > 0 {
                                self.video_aspect = Some(size[0] as f32 / size[1] as f32);
                            }
                            self.metadata = metadata;
                            self.texture = Some(ctx.load_texture(
                                "preview",
                                image,
                                egui::TextureOptions::LINEAR,
                            ));
                            self.status = Status::Image { width: size[0], height: size[1], video, full: false };
                            #[cfg(feature = "visual-audit")]
                            if video && self.audit_autoplay {
                                self.start_playback(ctx);
                            }
                        }
                        Err(error) => self.status = Status::Error(error),
                    }
                }
                Response::Compare(id, left, right, result)
                    if id == self.image_generation && !self.folder_open_pending =>
                {
                    if self.comparison_pair().as_ref() != Some(&(left, right)) {
                        continue;
                    }
                    match result {
                        Ok((first, second)) => {
                            self.comparison = Some(Comparison {
                                left: ctx.load_texture("compare-left", first, egui::TextureOptions::LINEAR),
                                right: ctx.load_texture("compare-right", second, egui::TextureOptions::LINEAR),
                            });
                            self.status = Status::Text(Text::Comparing);
                        }
                        Err(error) => self.status = Status::Error(error),
                    }
                }
                Response::Metadata(id, path, result)
                    if id == self.image_generation && self.selected.as_ref() == Some(&path) =>
                {
                    self.metadata = result.ok();
                }
                Response::FullImage(id, path, result)
                    if id == self.image_generation && self.selected.as_ref() == Some(&path) =>
                {
                    match result {
                        Ok(image) => {
                            let size = image.size;
                            let max_side = ctx.input(|input| input.max_texture_side);
                            if size[0] > max_side || size[1] > max_side {
                                self.status = Status::Text(Text::FullImageTooLarge);
                                continue;
                            }
                            self.texture = Some(ctx.load_texture(
                                "full image",
                                image,
                                egui::TextureOptions::LINEAR,
                            ));
                            self.status = Status::Image { width: size[0], height: size[1], video: false, full: true };
                        }
                        Err(error) => self.status = Status::Error(error),
                    }
                }
                Response::Search(id, folder, result)
                    if id == self.search_generation && folder == self.folder =>
                {
                    self.search_pending = false;
                    match result {
                        Ok(paths) => {
                            self.stop_playback();
                            self.clear_thumbnails();
                            self.image_generation += 1;
                            self.selected = None;
                            self.comparison = None;
                            self.texture = None;
                            self.metadata = None;
                            self.entries = paths
                                .into_iter()
                                .map(|path| Entry {
                                    path,
                                    directory: false,
                                })
                                .collect();
                            self.visible_media = self.entries.iter()
                                .map(|entry| entry.path.clone()).collect();
                            self.selection.retain_visible(&self.visible_media);
                            self.search_active = true;
                            self.truncated = false;
                            self.status = Status::Matches(self.entries.len());
                            if self.selection.len() != 0 {
                                self.present_selection();
                            }
                            #[cfg(feature = "visual-audit")]
                            if let Some(path) = self.audit_image.take() {
                                if self.entries.iter().any(|entry| entry.path == path) {
                                    self.selection.set_direct(path.clone());
                                    self.open_image(path);
                                }
                            }
                        }
                        Err(error) => self.status = Status::SearchError(error),
                    }
                }
                Response::Playback(id, result) if id == self.playback_generation => match result {
                    Ok((state, time, duration)) => {
                        self.playback_state = state;
                        self.playback_time = time.max(0);
                        self.playback_duration = duration.max(0);
                        if state == 7 {
                            self.status = Status::Text(Text::PlaybackFailed);
                        } else if state == 6 {
                            self.status = Status::Text(Text::PlaybackFinished);
                        } else if state == 3 {
                            self.status = Status::Text(Text::Playing);
                        } else if state == 4 {
                            self.status = Status::Text(Text::Paused);
                        }
                    }
                    Err(error) => {
                        self.playback_shared = None;
                        self.playback_state = 0;
                        self.status = Status::Error(error);
                    }
                },
                Response::Thumbnail(generation, path, image)
                    if generation == self.thumbnail_generation =>
                {
                    self.thumbnail_pending.remove(&path);
                    if let Some(image) = image {
                        while self.thumbnails.len() >= MAX_THUMBNAILS {
                            if let Some(old) = self.thumbnail_order.pop_front() {
                                self.thumbnails.remove(&old);
                            } else {
                                break;
                            }
                        }
                        let texture = ctx.load_texture(
                            format!("thumbnail:{}", path.to_string_lossy()),
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.thumbnail_order.push_back(path.clone());
                        self.thumbnails.insert(path, texture);
                    } else {
                        self.thumbnail_failed.insert(path);
                    }
                }
                _ => {}
            }
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.search_currency.store(u64::MAX, Ordering::Release);
        self.thumbnail_currency.store(u64::MAX, Ordering::Release);
        let _ = self.playback_sender.send(PlaybackRequest::Stop);
    }
}

impl eframe::App for Browser {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.receive(&ctx);
        // Escape leaves magnification first, then fullscreen; it never
        // unexpectedly closes the native window or drops file selection.
        if !ctx.wants_keyboard_input() {
            if ctx.input(|input| input.key_pressed(egui::Key::F11)) {
                self.toggle_fullscreen(&ctx);
            } else if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
                if self.zoom > 1.0 {
                    self.zoom = 1.0;
                    self.pan = egui::Vec2::ZERO;
                } else if self.fullscreen {
                    self.toggle_fullscreen(&ctx);
                }
            }
        }
        // Never steal arrow keys from path/search edits or focused media controls.
        if !ctx.wants_keyboard_input() && !self.folder_open_pending {
            let (back, forward, up, all, next, previous, extend) = ctx.input(|input| {
                let mods = input.modifiers;
                (
                    mods.alt && input.key_pressed(egui::Key::ArrowLeft),
                    mods.alt && input.key_pressed(egui::Key::ArrowRight),
                    mods.alt && input.key_pressed(egui::Key::ArrowUp),
                    mods.command && input.key_pressed(egui::Key::A),
                    !mods.alt && !mods.command && input.key_pressed(egui::Key::ArrowRight),
                    !mods.alt && !mods.command && input.key_pressed(egui::Key::ArrowLeft),
                    mods.shift,
                )
            });
            if back {
                self.go_back();
            } else if forward {
                self.go_forward();
            } else if up {
                if let Some(parent) = self.folder.parent().map(Path::to_path_buf) {
                    self.open_folder(parent);
                }
            } else if all {
                self.selection.select_all(&self.visible_media);
                self.present_selection();
            } else if next {
                self.step_media(1, extend);
            } else if previous {
                self.step_media(-1, extend);
            }
        }
        if !self.fullscreen {
        egui::Panel::top("header").show(root, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Diffractor").size(22.0).strong()
                    .color(Color32::from_rgb(220, 237, 244)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(self.language.badge()).clicked() {
                        self.language.toggle();
                    }
                    if ui.add_enabled(self.selected.is_some(),
                        egui::Button::new("⛶"))
                        .on_hover_text(tr(self.language, Text::Fullscreen))
                        .clicked() {
                        self.toggle_fullscreen(&ctx);
                    }
                });
            });
            ui.horizontal(|ui| {
                let can_back = self.history.back_target().is_some();
                if ui.add_enabled(can_back, egui::Button::new("←"))
                    .on_hover_text(tr(self.language, Text::HistoryBack)).clicked() {
                    self.go_back();
                }
                let can_forward = self.history.forward_target().is_some();
                if ui.add_enabled(can_forward, egui::Button::new("→"))
                    .on_hover_text(tr(self.language, Text::HistoryForward)).clicked() {
                    self.go_forward();
                }
                let parent = self.folder.parent().map(Path::to_path_buf);
                if ui.add_enabled(
                    parent.is_some(),
                    egui::Button::new(tr(self.language, Text::Up)),
                ).clicked() {
                    if let Some(parent) = parent {
                        self.open_folder(parent);
                    }
                }
                let edit = ui.add_sized(
                    [(ui.available_width() - 65.0).max(100.0), 28.0],
                    egui::TextEdit::singleline(&mut self.address).hint_text(tr(self.language, Text::AddressHint)),
                );
                if (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    || ui.button(tr(self.language, Text::Open)).clicked()
                {
                    self.open_path(PathBuf::from(self.address.clone()));
                }
            });
            ui.label(RichText::new(tr(self.language, Text::SearchScope)).weak());
            ui.horizontal(|ui| {
                let return_visible = self.search_active || self.search_pending;
                let width_reserved = if return_visible { 185.0 } else { 92.0 };
                let edit = ui.add_sized(
                    [(ui.available_width() - width_reserved).max(100.0), 28.0],
                    egui::TextEdit::singleline(&mut self.search_text)
                        .hint_text(tr(self.language, Text::SearchHint)),
                );
                let enter = edit.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let can_search = !self.folder_open_pending
                    && !self.folder.as_os_str().is_empty()
                    && !self.search_text.trim().is_empty();
                let clicked = ui.add_enabled(
                    can_search,
                    egui::Button::new(tr(self.language, Text::Search)),
                ).clicked();
                if can_search && (enter || clicked) {
                    self.search();
                }
                if return_visible && ui.button(tr(self.language, Text::BackToFolder)).clicked() {
                    self.open_folder(self.folder.clone());
                }
            });
        });
        let mut open = None;
        let mut needed_thumbnails = Vec::new();
        egui::Panel::left("items")
            .resizable(true)
            .default_size(300.0)
            .size_range(180.0..=500.0)
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(if self.search_active {
                            tr(self.language, Text::SearchResults)
                        } else {
                            tr(self.language, Text::FoldersMedia)
                        })
                        .strong(),
                    );
                    ui.label(RichText::new(self.entries.len().to_string()).weak());
                    if ui.selectable_label(self.grid_view, "▦")
                        .on_hover_text(tr(self.language, Text::GridView))
                        .clicked() {
                        self.grid_view = true;
                    }
                    if ui.selectable_label(!self.grid_view, "☷")
                        .on_hover_text(tr(self.language, Text::ListView))
                        .clicked() {
                        self.grid_view = false;
                    }
                    if self.selection.len() > 0 {
                        ui.separator();
                        ui.label(
                            RichText::new(format!("{} {}", self.selection.len(),
                                tr(self.language, Text::SelectedCount)))
                                .color(Color32::from_rgb(180, 215, 237)),
                        );
                    }
                });
                if self.truncated {
                    ui.label(tr(self.language, Text::EntryLimit));
                }
                ui.separator();
                if self.grid_view {
                    let columns = grid_columns(ui.available_width());
                    let gap = ui.spacing().item_spacing.x;
                    let cell_width = ((ui.available_width() - gap * (columns - 1) as f32)
                        / columns as f32).max(96.0);
                    let row_count = self.entries.len().div_ceil(columns);
                    egui::ScrollArea::vertical().show_rows(ui, 152.0, row_count, |ui, rows| {
                        for row in rows {
                            ui.horizontal(|ui| {
                                for index in row * columns..((row + 1) * columns).min(self.entries.len()) {
                                    let entry = &self.entries[index];
                                    let selected = self.selection.contains(&entry.path);
                                    let name = if self.search_active {
                                        entry.path.strip_prefix(&self.folder)
                                            .unwrap_or(&entry.path).to_string_lossy()
                                    } else {
                                        entry.path.file_name().unwrap_or_default().to_string_lossy()
                                    };
                                    let frame = egui::Frame::new()
                                        .fill(if selected { ui.visuals().selection.bg_fill } else {
                                            ui.visuals().faint_bg_color
                                        })
                                        .corner_radius(7.0)
                                        .inner_margin(egui::Margin::same(5))
                                        .show(ui, |ui| {
                                            ui.set_width((cell_width - 12.0).max(84.0));
                                            ui.allocate_ui_with_layout(
                                                egui::vec2((cell_width - 22.0).max(72.0), 96.0),
                                                egui::Layout::centered_and_justified(egui::Direction::TopDown),
                                                |ui| {
                                                    if let Some(texture) = self.thumbnails.get(&entry.path) {
                                                        let extent = texture.size_vec2();
                                                        let scale = ((cell_width - 22.0) / extent.x)
                                                            .min(96.0 / extent.y).min(1.0);
                                                        if ui.add(egui::Image::new(texture)
                                                            .fit_to_exact_size(extent * scale)
                                                            .sense(egui::Sense::click())).clicked() {
                                                            open = Some((entry.path.clone(), entry.directory,
                                                                selection_intent(ui.input(|i| i.modifiers))));
                                                        }
                                                    } else {
                                                        if !entry.directory
                                                            && (is_image(&entry.path) || is_video(&entry.path)) {
                                                            needed_thumbnails.push(entry.path.clone());
                                                        }
                                                        let symbol = if entry.directory { "▰" }
                                                            else if is_audio(&entry.path) { "♫" }
                                                            else { "▧" };
                                                        ui.label(RichText::new(symbol).size(30.0).weak());
                                                    }
                                                },
                                            );
                                            if ui.add(egui::Label::new(name.to_string()).truncate()
                                                .sense(egui::Sense::click()))
                                                .on_hover_text(name.as_ref()).clicked() {
                                                open = Some((entry.path.clone(), entry.directory,
                                                    selection_intent(ui.input(|i| i.modifiers))));
                                            }
                                        });
                                    if frame.response.interact(egui::Sense::click()).clicked() {
                                        open = Some((entry.path.clone(), entry.directory,
                                            selection_intent(ui.input(|i| i.modifiers))));
                                    }
                                }
                            });
                        }
                    });
                } else {
                egui::ScrollArea::vertical().show_rows(ui, 56.0, self.entries.len(), |ui, rows| {
                    for entry in &self.entries[rows] {
                        let name = if self.search_active {
                            entry
                                .path
                                .strip_prefix(&self.folder)
                                .unwrap_or(&entry.path)
                                .to_string_lossy()
                        } else {
                            entry.path.file_name().unwrap_or_default().to_string_lossy()
                        };
                        let selected = self.selection.contains(&entry.path);
                        let row = egui::Frame::new()
                            .fill(if selected { ui.visuals().selection.bg_fill } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::symmetric(6, 4))
                            .show(ui, |ui| {
                            ui.set_min_height(48.0);
                            ui.horizontal(|ui| {
                            let media = !entry.directory
                                && (is_image(&entry.path) || is_video(&entry.path));
                            ui.allocate_ui_with_layout(
                                egui::vec2(64.0, 48.0),
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                                |ui| {
                                    if let Some(texture) = self.thumbnails.get(&entry.path) {
                                        let extent = texture.size_vec2();
                                        let scale = (64.0 / extent.x).min(48.0 / extent.y).min(1.0);
                                        if ui
                                            .add(
                                                egui::Image::new(texture)
                                                    .fit_to_exact_size(extent * scale)
                                                    .sense(egui::Sense::click()),
                                            )
                                            .clicked()
                                        {
                                            open = Some((entry.path.clone(), entry.directory, selection_intent(ui.input(|i| i.modifiers))));
                                        }
                                    } else {
                                        if media {
                                            needed_thumbnails.push(entry.path.clone());
                                        }
                                        // Distinct, low-cost placeholders; no decorative
                                        // loading animation is needed for every visible row.
                                        let (symbol, tint, kind) = if entry.directory {
                                            ("▰", Color32::from_rgb(187, 159, 112),
                                                tr(self.language, Text::Folder))
                                        } else if is_audio(&entry.path) {
                                            ("♫", Color32::from_rgb(127, 184, 193),
                                                tr(self.language, Text::Audio))
                                        } else {
                                            ("▧", Color32::from_rgb(114, 124, 135), "")
                                        };
                                        ui.label(RichText::new(symbol).size(25.0).color(tint))
                                            .on_hover_text(kind);
                                    }
                                },
                            );
                            let label = name.to_string();
                            if ui
                                .add(
                                    egui::Label::new(label)
                                        .halign(egui::Align::Min)
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                )
                                .on_hover_text(name.as_ref())
                                .clicked()
                            {
                                open = Some((entry.path.clone(), entry.directory, selection_intent(ui.input(|i| i.modifiers))));
                            }
                            });
                        });
                        if row.response.interact(egui::Sense::click()).clicked() {
                            open = Some((entry.path.clone(), entry.directory, selection_intent(ui.input(|i| i.modifiers))));
                        }
                    }
                });
                }
            });
        for path in needed_thumbnails {
            self.request_thumbnail(path);
        }
        if let Some((path, directory, intent)) = open {
            if directory {
                self.open_folder(path);
            } else {
                self.select_media(path, intent);
            }
        }
        if self.selection.len() > 1 {
            let images = self.selection.iter().filter(|path| is_image(path)).count();
            let videos = self.selection.iter().filter(|path| is_video(path)).count();
            let audio = self.selection.iter().filter(|path| is_audio(path)).count();
            egui::Panel::right("selection-summary")
                .resizable(true)
                .default_size(230.0)
                .size_range(180.0..=400.0)
                .show(root, |ui| {
                    ui.heading(tr(self.language, Text::SelectionSummary));
                    ui.label(RichText::new(format!("{} {}",
                        self.selection.len(),
                        tr(self.language, Text::SelectedCount))).strong());
                    ui.separator();
                    if images > 0 {
                        ui.horizontal(|ui| {
                            ui.label(tr(self.language, Text::Images));
                            ui.label(images.to_string());
                        });
                    }
                    if videos > 0 {
                        ui.horizontal(|ui| {
                            ui.label(tr(self.language, Text::Videos));
                            ui.label(videos.to_string());
                        });
                    }
                    if audio > 0 {
                        ui.horizontal(|ui| {
                            ui.label(tr(self.language, Text::Audio));
                            ui.label(audio.to_string());
                        });
                    }
                    ui.add_space(12.0);
                    ui.label(RichText::new(tr(self.language, Text::BatchUnavailable)).weak());
                });
        } else if let Some(metadata) = &self.metadata {
            egui::Panel::right("metadata")
                .resizable(true)
                .default_size(230.0)
                .size_range(180.0..=400.0)
                .show(root, |ui| {
                    ui.label(RichText::new(tr(self.language, Text::MediaDetails)).strong());
                    ui.label(RichText::new(tr(self.language, Text::EmbeddedDetails)).weak());
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if *metadata == Metadata::default() {
                            ui.label(tr(self.language, Text::NoDetails));
                        }
                        let camera = if metadata.camera_model.is_empty() {
                            &metadata.camera_make
                        } else {
                            &metadata.camera_model
                        };
                        for (label, value) in [
                            (tr(self.language, Text::Title), &metadata.title),
                            (tr(self.language, Text::Camera), camera),
                            (tr(self.language, Text::Lens), &metadata.lens),
                            (tr(self.language, Text::Description), &metadata.description),
                            (tr(self.language, Text::Keywords), &metadata.keywords),
                        ] {
                            if !value.is_empty() {
                                ui.label(RichText::new(label).weak());
                                ui.label(value);
                                ui.add_space(8.0);
                            }
                        }
                        if let Some(rating) = metadata.rating {
                            ui.label(RichText::new(tr(self.language, Text::Rating)).weak());
                            ui.label(if rating == -1 {
                                tr(self.language, Text::Rejected).to_owned()
                            } else {
                                format!("{rating} / 5")
                            });
                        }
                    });
                });
        } else if self.texture.is_some() {
            egui::Panel::right("metadata-error")
                .default_size(230.0)
                .show(root, |ui| {
                    ui.label(RichText::new(tr(self.language, Text::MediaDetails)).strong());
                    ui.label(tr(self.language, Text::ReadDetailsFailed));
                });
        }
        } // Keep native Items panels hidden while displaying the same media fullscreen.
        if let Some(frame) = self
            .playback_shared
            .as_ref()
            .and_then(|shared| shared.take_frame())
        {
            #[cfg(feature = "visual-audit")]
            {
                self.audit_frame_seen = true;
            }
            if frame.height > 0 && self.selected.as_ref().is_some_and(|path| is_video(path)) {
                self.video_aspect = Some(frame.width as f32 / frame.height as f32);
            }
            let image =
                ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
            if let Some(texture) = &mut self.texture {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.texture =
                    Some(ctx.load_texture("playback", image, egui::TextureOptions::LINEAR));
            }
        }
        egui::CentralPanel::default().show(root, |ui| {
            if self.fullscreen && ui.button(tr(self.language, Text::ExitFullscreen)).clicked() {
                self.toggle_fullscreen(&ctx);
            }
            if let Some(file_name) = self.selected.as_ref().map(|path| {
                path.file_name().unwrap_or_default().to_string_lossy().into_owned()
            }) {
                ui.horizontal(|ui| {
                    if ui.button("‹").on_hover_text(tr(self.language, Text::Previous)).clicked() {
                        self.step_media(-1, false);
                    }
                    if ui.button("›").on_hover_text(tr(self.language, Text::Next)).clicked() {
                        self.step_media(1, false);
                    }
                    ui.label(RichText::new(file_name).strong().size(17.0));
                });
            } else if self.comparison.is_some() {
                ui.heading(tr(self.language, Text::Compare));
            } else if self.selection.len() > 1 {
                ui.heading(tr(self.language, Text::SelectionSummary));
            } else {
                ui.heading(tr(self.language, Text::PhotoPreview));
            }
            ui.label(RichText::new(self.status.display(self.language)).color(Color32::LIGHT_GRAY));
            if self
                .selected
                .as_ref()
                .is_some_and(|path| is_video(path) || is_audio(path))
            {
                ui.horizontal(|ui| {
                    let label = if self.playback_state == 3 {
                        tr(self.language, Text::Pause)
                    } else if self.playback_state == 4 {
                        tr(self.language, Text::Resume)
                    } else if self.playback_state == 6 {
                        tr(self.language, Text::Replay)
                    } else {
                        tr(self.language, Text::Play)
                    };
                    if ui
                        .add_enabled(
                            !matches!(self.playback_state, 1 | 2),
                            egui::Button::new(label),
                        )
                        .clicked()
                    {
                        if self.playback_state == 3 || self.playback_state == 4 {
                            let _ = self.playback_sender.send(PlaybackRequest::Pause(
                                self.playback_generation,
                                self.playback_state == 3,
                            ));
                        } else {
                            self.start_playback(&ctx);
                        }
                    }
                    if self.playback_shared.is_some() && ui.button(tr(self.language, Text::Stop)).clicked() {
                        self.stop_playback();
                        self.status = Status::Text(Text::Stopped);
                    }
                    if self.playback_duration > 0 {
                        let mut seconds = self.playback_time as f64 / 1000.0;
                        let duration = self.playback_duration as f64 / 1000.0;
                        if ui
                            .add(egui::Slider::new(&mut seconds, 0.0..=duration).show_value(false))
                            .changed()
                        {
                            let _ = self.playback_sender.send(PlaybackRequest::Seek(
                                self.playback_generation,
                                (seconds * 1000.0) as i64,
                            ));
                        }
                        ui.label(format!(
                            "{}:{:02} / {}:{:02}",
                            (seconds as i64) / 60,
                            (seconds as i64) % 60,
                            (duration as i64) / 60,
                            (duration as i64) % 60
                        ));
                    }
                });
            }
            let still_image = self.selected.as_ref().is_some_and(|path| is_image(path));
            let mut zoom_factor = 1.0;
            if still_image && self.texture.is_some() {
                ui.horizontal(|ui| {
                    if ui.button("−").on_hover_text(tr(self.language, Text::ZoomOut)).clicked() {
                        zoom_factor = 1.0 / 1.25;
                    }
                    if ui.button("+").on_hover_text(tr(self.language, Text::ZoomIn)).clicked() {
                        zoom_factor = 1.25;
                    }
                    if ui.button(tr(self.language, Text::Fit)).clicked() {
                        self.zoom = 1.0;
                        self.pan = egui::Vec2::ZERO;
                    }
                    ui.label(format!("{:.0}% {}", self.zoom * 100.0, tr(self.language, Text::OfFit)));
                    ui.label(
                        RichText::new(tr(self.language, Text::ZoomHelp))
                            .small()
                            .color(Color32::GRAY),
                    );
                });
            }
            ui.add_space(12.0);
            if let Some(compare) = &self.comparison {
                let available = ui.available_size().max(egui::Vec2::ZERO);
                let (viewport, _) = ui.allocate_exact_size(available, egui::Sense::hover());
                let gap = 12.0;
                let pane_width = ((viewport.width() - gap) * 0.5).max(0.0);
                let left_rect = egui::Rect::from_min_size(
                    viewport.min, egui::vec2(pane_width, viewport.height()));
                let right_rect = egui::Rect::from_min_size(
                    egui::pos2(viewport.min.x + pane_width + gap, viewport.min.y),
                    egui::vec2(pane_width, viewport.height()));
                for (texture, bounds) in [(&compare.left, left_rect), (&compare.right, right_rect)] {
                    let size = texture.size_vec2();
                    if size.x <= 0.0 || size.y <= 0.0 {
                        continue;
                    }
                    let scale = (bounds.width() / size.x).min(bounds.height() / size.y).max(0.0);
                    let rect = egui::Rect::from_center_size(bounds.center(), size * scale);
                    ui.painter_at(viewport).image(
                        texture.id(),
                        rect,
                        egui::Rect::from_min_max(
                            egui::Pos2::ZERO, egui::pos2(1.0, 1.0),
                        ),
                        Color32::WHITE,
                    );
                }
            } else if let Some(texture) = self.texture.clone() {
                let available = ui.available_size();
                // Keep a stopped video centered too; the preview and player share
                // the same aspect-driven media stage.
                if self.video_aspect.is_some() {
                    let aspect = self.video_aspect.unwrap_or(1.0);
                    let width = available.x.min(available.y * aspect).max(0.0);
                    let (viewport, _) = ui.allocate_exact_size(
                        available.max(egui::Vec2::ZERO),
                        egui::Sense::hover(),
                    );
                    let display = egui::vec2(width, width / aspect);
                    let rect = egui::Rect::from_center_size(viewport.center(), display);
                    ui.painter_at(viewport).image(
                        texture.id(),
                        rect,
                        egui::Rect::from_min_max(
                            egui::Pos2::ZERO,
                            egui::pos2(1.0, 1.0),
                        ),
                        Color32::WHITE,
                    );
                } else if still_image {
                    let (viewport, response) = ui.allocate_exact_size(
                        available.max(egui::Vec2::ZERO),
                        egui::Sense::click_and_drag(),
                    );
                    let mut anchor = egui::Vec2::ZERO;
                    if response.hovered() {
                        let (scroll, control, pointer) = ui.input(|i| {
                            (
                                i.smooth_scroll_delta.y,
                                i.modifiers.ctrl,
                                i.pointer.hover_pos(),
                            )
                        });
                        if control && scroll != 0.0 {
                            zoom_factor *= (scroll * 0.002).exp();
                            anchor = pointer.unwrap_or(viewport.center()) - viewport.center();
                        }
                    }
                    let next_zoom = (self.zoom * zoom_factor).clamp(1.0, 16.0);
                    self.pan = anchored_pan(self.pan, anchor, self.zoom, next_zoom);
                    self.zoom = next_zoom;
                    if response.dragged() {
                        self.pan += response.drag_delta();
                    }
                    let size = texture.size_vec2();
                    let fit = (viewport.width() / size.x)
                        .min(viewport.height() / size.y)
                        .max(0.0);
                    let display = size * fit * self.zoom;
                    self.pan = clamp_pan(self.pan, display, viewport.size());
                    let rect = egui::Rect::from_center_size(viewport.center() + self.pan, display);
                    ui.painter_at(viewport).image(
                        texture.id(),
                        rect,
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    if self.zoom > 1.0 && !self.full_image_requested
                        && !self.folder_open_pending {
                        self.full_image_requested = true;
                        if let Some(path) = &self.selected {
                            let _ = self
                                .sender
                                .send(Request::FullImage(self.image_generation, path.clone()));
                        }
                    }
                } else {
                    ui.add(
                        egui::Image::new(&texture)
                            .max_size(available)
                            .maintain_aspect_ratio(true),
                    );
                }
            } else if self.selected.as_ref().is_some_and(|path| is_audio(path)) {
                // Audio has no raster frame; a compact centered stage gives the
                // player an intentional resting surface instead of empty space.
                let available_height = ui.available_height();
                ui.vertical_centered(|ui| {
                    ui.add_space(((available_height - 165.0) * 0.4).max(16.0));
                    ui.label(RichText::new("♫").size(80.0)
                        .color(Color32::from_rgb(127, 184, 193)));
                    ui.add_space(8.0);
                    ui.label(RichText::new(tr(self.language, Text::Audio))
                        .size(18.0).strong());
                });
            }
        });
        #[cfg(feature = "visual-audit")]
        if self.texture.is_some()
            && self.thumbnail_pending.is_empty()
            && (!self.audit_autoplay || (self.audit_frame_seen && self.playback_state == 3))
        {
            if let Some(path) = self.audit_output.take() {
                let callback_ctx = ctx.clone();
                ctx.request_screenshot(move |image| {
                    use std::io::Write;
                    if let Ok(mut file) = std::fs::File::create(path) {
                        let _ = write!(file, "P6\n{} {}\n255\n", image.size[0], image.size[1]);
                        for pixel in &image.pixels {
                            let _ = file.write_all(&[pixel.r(), pixel.g(), pixel.b()]);
                        }
                    }
                    callback_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                });
            }
        }
    }
}

fn main() -> eframe::Result {
    let initial = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([540.0, 360.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Diffractor",
        options,
        Box::new(move |cc| Ok(Box::new(Browser::new(cc, initial)))),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn image_comparison_preserves_browse_order_not_set_order() {
        let visible: Vec<PathBuf> = ["z.jpg", "folder/ignore.mp4", "a.png"]
            .into_iter().map(PathBuf::from).collect();
        let mut selection = Selection::default();
        selection.apply(&visible, &visible[0], Intent::Replace);
        selection.apply(&visible, &visible[2], Intent::Toggle);
        assert_eq!(
            comparison_paths(&visible, &selection),
            Some((visible[0].clone(), visible[2].clone())),
        );
        selection.apply(&visible, &visible[1], Intent::Replace);
        selection.apply(&visible, &visible[2], Intent::Toggle);
        assert_eq!(comparison_paths(&visible, &selection), None);
    }

    #[test]
    fn grid_columns_follow_actual_pane_width() {
        assert_eq!(grid_columns(0.0), 1);
        assert_eq!(grid_columns(200.0), 1);
        assert_eq!(grid_columns(300.0), 2);
        assert_eq!(grid_columns(480.0), 3);
        assert_eq!(grid_columns(f32::NAN), 1);
    }

    #[test]
    fn opens_files_and_folders_and_rejects_invalid_paths() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../exe/test");
        let file = root.join("Test.jpg");
        assert_eq!(open_target(&root).unwrap(), (root.clone(), None));
        assert_eq!(open_target(&file).unwrap(), (root.clone(), Some(file)));
        assert!(open_target(&root.join("Gherkin.xmp")).is_err());
        assert!(open_target(&root.join("does-not-exist.jpg")).is_err());
    }

    #[test]
    fn media_navigation_skips_folders_and_stops_at_edges() {
        let entries = [
            Entry { path: PathBuf::from("/gallery/folder"), directory: true },
            Entry { path: PathBuf::from("/gallery/one.jpg"), directory: false },
            Entry { path: PathBuf::from("/gallery/more"), directory: true },
            Entry { path: PathBuf::from("/gallery/two.mp4"), directory: false },
        ];
        assert_eq!(next_media_index(&entries, None, false), Some(1));
        assert_eq!(next_media_index(&entries, None, true), Some(3));
        assert_eq!(next_media_index(&entries, Some(&entries[1].path), false), Some(3));
        assert_eq!(next_media_index(&entries, Some(&entries[3].path), true), Some(1));
        assert_eq!(next_media_index(&entries, Some(&entries[1].path), true), Some(1));
        assert_eq!(next_media_index(&entries, Some(&entries[3].path), false), Some(3));
        assert_eq!(next_media_index(&entries[..1], None, false), None);
        assert_eq!(next_media_index(&[], None, true), None);
    }

    #[test]
    fn zoom_anchor_and_pan_bounds() {
        let anchor = egui::vec2(40.0, -20.0);
        let pan = egui::vec2(10.0, 5.0);
        let next = super::anchored_pan(pan, anchor, 1.0, 2.0);
        assert_eq!((anchor - pan) / 1.0, (anchor - next) / 2.0);
        assert_eq!(
            super::clamp_pan(
                egui::vec2(500.0, -500.0),
                egui::vec2(300.0, 80.0),
                egui::vec2(100.0, 100.0)
            ),
            egui::vec2(100.0, 0.0)
        );
    }
    use super::*;

    #[test]
    fn orientation_maps_all_eight_exif_transforms() {
        let expected: [&[u8]; 8] = [
            &[1, 2, 3, 4, 5, 6],
            &[2, 1, 4, 3, 6, 5],
            &[6, 5, 4, 3, 2, 1],
            &[5, 6, 3, 4, 1, 2],
            &[1, 3, 5, 2, 4, 6],
            &[5, 3, 1, 6, 4, 2],
            &[6, 4, 2, 5, 3, 1],
            &[2, 4, 6, 1, 3, 5],
        ];
        for (i, expected) in expected.iter().enumerate() {
            let mut rgba = Vec::new();
            for n in 1..=6 {
                rgba.extend_from_slice(&[n, 0, 0, 255]);
            }
            let out = orient(DecodedImage {
                width: 2,
                height: 3,
                orientation: i as u16 + 1,
                rgba,
            });
            assert_eq!(out.size, if i >= 4 { [3, 2] } else { [2, 3] });
            assert_eq!(
                out.pixels.iter().map(|p| p.r()).collect::<Vec<_>>(),
                *expected
            );
            let mut rgba = Vec::new();
            for n in 1..=6 {
                rgba.extend_from_slice(&[n, 0, 0, 255]);
            }
            let small = thumbnail(DecodedImage {
                width: 2,
                height: 3,
                orientation: i as u16 + 1,
                rgba,
            });
            assert_eq!(small.size, out.size);
            assert_eq!(small.pixels, out.pixels);
        }
    }

    #[test]
    fn lists_fixture_folder_and_reports_missing_folder() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../exe/test");
        let (entries, truncated) = list_folder(&root).unwrap();
        assert!(!truncated);
        assert!(entries
            .iter()
            .any(|e| e.path.file_name() == Some(OsStr::new("Test.jpg"))));
        assert!(entries
            .iter()
            .any(|e| e.path.file_name() == Some(OsStr::new("indy.mp4"))));
        assert!(entries.iter().any(|e| e.directory));
        assert!(list_folder(&root.join("missing-folder")).is_err());
    }

    #[test]
    fn thumbnail_downscales_with_rotated_aspect() {
        let frame = DecodedImage {
            width: 1024,
            height: 256,
            orientation: 6,
            rgba: vec![255; 1024 * 256 * 4],
        };
        let result = thumbnail(frame);
        assert_eq!(result.size, [24, 96]);
        assert_eq!(result.pixels.len(), 24 * 96);
    }
}
