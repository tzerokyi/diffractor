mod browser_files;
mod history;
mod locale;
mod rating;
mod rename;
mod selection;
mod video;
mod view;
mod zoom;

use browser_files::{is_audio, is_image, is_video, list_folder, open_target, Entry};
use history::{History, Travel, Visit};
use selection::{Intent, Selection};
use locale::{tr, Locale, Status, Text};
use zoom::Zoom;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    time::{Duration, Instant},
};

use diffractor_core::{
    decode_image, probe_image, read_metadata, search_folder_cancellable, video_preview, DecodedImage, Metadata,
    SearchError,
};
use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle};

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

enum Request {
    Folder(u64, PathBuf),
    OpenPath(u64, PathBuf),
    Image(u64, PathBuf),
    Metadata(u64, PathBuf),
    Compare(u64, PathBuf, PathBuf),
    FullCompare(u64, PathBuf, PathBuf),
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
        Result<(ColorImage, Option<Metadata>, bool, Option<egui::Vec2>), String>,
    ),
    Metadata(u64, PathBuf, Result<Metadata, String>),
    Compare(u64, PathBuf, PathBuf, Result<(ComparisonImage, ComparisonImage), String>),
    FullCompare(u64, PathBuf, PathBuf, Result<(ColorImage, ColorImage), String>),
    Search(u64, PathBuf, Result<Vec<PathBuf>, String>),
    FullImage(u64, PathBuf, Result<ColorImage, String>),
    Playback(u64, Result<(i32, i64, i64), String>),
    Thumbnail(u64, PathBuf, Option<ColorImage>),
    Rating(rating::ResultMessage),
    RatingProgress(usize, usize),
    Picker(u64, u64, u64, Option<PathBuf>),
    Rename(rename::ResultMessage),
}

struct PickerRequest {
    id: u64,
    folder_generation: u64,
    search_generation: u64,
    initial: PathBuf,
    folder: bool,
    title: String,
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
    left_source: egui::Vec2,
    right_source: egui::Vec2,
}

struct ComparisonImage {
    image: ColorImage,
    source: egui::Vec2,
}

#[derive(Default)]
struct BatchEdit {
    items: Vec<(PathBuf, rating::Snapshot)>,
    rating: Option<i32>,
    set_title: bool,
    title: String,
    set_description: bool,
    description: String,
    add_tags: String,
    remove_tags: String,
}

struct RenameDraft {
    source: PathBuf,
    name: String,
    plan: Option<rename::Plan>,
    error: Option<String>,
}

fn decode_comparison_image(path: &Path) -> Result<ComparisonImage, diffractor_core::DecodeError> {
    let decoded = decode_image(path, (1200, 1000))?;
    let fallback = if matches!(decoded.orientation, 5..=8) {
        egui::vec2(decoded.height as f32, decoded.width as f32)
    } else {
        egui::vec2(decoded.width as f32, decoded.height as f32)
    };
    let source = probe_image(path).ok().map(|info| {
        let (width, height) = if matches!(info.orientation, 5..=8) {
            (info.height, info.width)
        } else { (info.width, info.height) };
        egui::vec2(width as f32, height as f32)
    }).unwrap_or(fallback);
    Ok(ComparisonImage { image: orient(decoded), source })
}

struct Browser {
    sender: Sender<Request>,
    rating_sender: Sender<rating::Job>,
    picker_sender: Sender<PickerRequest>,
    rename_sender: Sender<rename::Job>,
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
    comparison_zoom: Zoom,
    comparison_active_right: bool,
    comparison_viewport: egui::Vec2,
    comparison_wheel_remainder: f32,
    comparison_full_requested: bool,
    selection: Selection,
    visible_media: Vec<PathBuf>,
    grid_view: bool,
    fullscreen: bool,
    show_details: bool,
    slideshow: bool,
    slide_deadline: Option<Instant>,
    slide_seconds: u64,
    texture: Option<TextureHandle>,
    zoom: Zoom,
    navigator_enabled: bool,
    navigator_until: Option<Instant>,
    navigator_last_rect: Option<egui::Rect>,
    source_extent: Option<egui::Vec2>,
    last_viewport: egui::Vec2,
    zoom_wheel_remainder: f32,
    full_image_requested: bool,
    metadata: Option<Metadata>,
    rating_busy: bool,
    picker_busy: bool,
    picker_generation: u64,
    rating_edit: Option<(PathBuf, Metadata, Metadata, rating::Snapshot, String, String)>,
    batch_edit: Option<BatchEdit>,
    batch_result: Option<(usize, Vec<(PathBuf, String)>, Vec<PathBuf>, Vec<PathBuf>)>,
    batch_progress: Option<(usize, usize)>,
    batch_cancel: Arc<AtomicBool>,
    operation_error: Option<String>,
    rename_busy: bool,
    rename_draft: Option<RenameDraft>,
    rename_result: Option<String>,
    status: Status,
    language: Locale,
    folder_generation: u64,
    folder_open_pending: bool,
    refreshing_folder: bool,
    history: History,
    pending_travel: Travel,
    image_generation: u64,
    truncated: bool,
    search_text: String,
    pending_search_query: String,
    active_search_query: String,
    search_generation: u64,
    search_currency: Arc<AtomicU64>,
    search_active: bool,
    search_pending: bool,
    search_dirty: bool,
    search_preserve_display: bool,
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
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut style = (*cc.egui_ctx.style_of(egui::Theme::Dark)).clone();
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
        cc.egui_ctx.set_style_of(egui::Theme::Dark, style);

        let (sender, requests) = mpsc::channel();
        let (responses, receiver) = mpsc::channel();
        let (rename_sender, rename_requests) = mpsc::channel::<rename::Job>();
        let rename_responses = responses.clone();
        let rename_context = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            while let Ok(job) = rename_requests.recv() {
                if rename_responses.send(Response::Rename(rename::run(job, &index_worker_path()))).is_err() { break; }
                rename_context.request_repaint();
            }
        });
        let (picker_sender, picker_requests) = mpsc::channel::<PickerRequest>();
        let picker_responses = responses.clone();
        let picker_context = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            while let Ok(request) = picker_requests.recv() {
                let dialog = rfd::FileDialog::new().set_title(&request.title).set_directory(request.initial);
                let selected = if request.folder { dialog.pick_folder() } else { dialog.pick_file() };
                if picker_responses.send(Response::Picker(request.id, request.folder_generation,
                    request.search_generation, selected)).is_err() { break; }
                picker_context.request_repaint();
            }
        });
        let (rating_sender, rating_requests) = mpsc::channel();
        let rating_responses = responses.clone();
        let rating_context = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            while let Ok(job) = rating_requests.recv() {
                let result = rating::run_with_progress(job, &index_worker_path(), |done, total| {
                    let _ = rating_responses.send(Response::RatingProgress(done, total));
                    rating_context.request_repaint();
                });
                if rating_responses.send(Response::Rating(result)).is_err() { break; }
                rating_context.request_repaint();
            }
        });
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
                            .map(|image| {
                                let original = if video {
                                    None
                                } else {
                                    probe_image(&path).ok().map(|info| {
                                        let (width, height) = if matches!(info.orientation, 5..=8) {
                                            (info.height, info.width)
                                        } else {
                                            (info.width, info.height)
                                        };
                                        egui::vec2(width as f32, height as f32)
                                    })
                                };
                                (orient(image), read_metadata(&path).ok(), video, original)
                            })
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
                        let result = decode_comparison_image(&left)
                            .and_then(|first| decode_comparison_image(&right)
                                .map(|second| (first, second)))
                            .map_err(|error| format!("{error:?}"));
                        Response::Compare(id, left, right, result)
                    }
                    Request::FullCompare(id, left, right) => {
                        let result = decode_image(&left, (0, 0))
                            .and_then(|first| decode_image(&right, (0, 0))
                                .map(|second| (orient(first), orient(second))))
                            .map_err(|_| "Full-resolution comparison could not be loaded.".to_owned());
                        Response::FullCompare(id, left, right, result)
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
            rating_sender,
            picker_sender,
            rename_sender,
            search_sender,
            playback_sender,
            thumbnail_sender,
            receiver,
            folder: PathBuf::new(),
            address: String::new(),
            entries: Vec::new(),
            selected: None,
            comparison: None,
            comparison_zoom: Zoom::default(),
            comparison_active_right: false,
            comparison_viewport: egui::Vec2::ZERO,
            comparison_wheel_remainder: 0.0,
            comparison_full_requested: false,
            selection: Selection::default(),
            visible_media: Vec::new(),
            grid_view: true,
            fullscreen: false,
            show_details: true,
            slideshow: false,
            slide_deadline: None,
            slide_seconds: 3,
            texture: None,
            zoom: Zoom::default(),
            navigator_enabled: true,
            navigator_until: None,
            navigator_last_rect: None,
            source_extent: None,
            last_viewport: egui::Vec2::ZERO,
            zoom_wheel_remainder: 0.0,
            full_image_requested: false,
            metadata: None,
            rating_busy: false,
            picker_busy: false,
            picker_generation: 0,
            rating_edit: None,
            batch_edit: None,
            batch_result: None,
            batch_progress: None,
            batch_cancel: Arc::new(AtomicBool::new(false)),
            operation_error: None,
            rename_busy: false,
            rename_draft: None,
            rename_result: None,
            status: Status::Empty,
            language: Locale::detect(),
            folder_generation: 0,
            folder_open_pending: false,
            refreshing_folder: false,
            history: History::default(),
            pending_travel: Travel::Push,
            image_generation: 0,
            truncated: false,
            search_text: String::new(),
            pending_search_query: String::new(),
            active_search_query: String::new(),
            search_generation: 0,
            search_currency,
            search_active: false,
            search_pending: false,
            search_dirty: false,
            search_preserve_display: false,
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

    fn choose_path(&mut self, folder: bool) {
        if self.picker_busy || self.folder_open_pending { return; }
        self.picker_generation += 1;
        self.picker_busy = true;
        let request = PickerRequest {
            id: self.picker_generation,
            folder_generation: self.folder_generation,
            search_generation: self.search_generation,
            initial: self.folder.clone(),
            folder,
            title: tr(self.language, if folder { Text::OpenFolder } else { Text::OpenFile }).into(),
        };
        if self.picker_sender.send(request).is_err() {
            self.picker_busy = false;
            self.status = Status::Error("File chooser is unavailable.".into());
        }
    }

    fn start_rename(&mut self) {
        if self.rename_busy || self.rating_busy || self.rating_edit.is_some() || self.batch_edit.is_some()
            || self.folder_open_pending || self.selection.len() != 1 { return; }
        let Some(source) = self.selection.single().map(Path::to_path_buf) else { return; };
        if self.slideshow { self.stop_slideshow(); }
        let name = source.file_stem().and_then(|part| part.to_str()).unwrap_or("").to_owned();
        self.rename_draft = Some(RenameDraft { source, name, plan: None, error: None });
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
        self.slideshow = false;
        self.slide_deadline = None;
        self.refreshing_folder = false;
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

    fn refresh(&mut self) {
        if self.folder_open_pending || self.folder.as_os_str().is_empty() {
            return;
        }
        if self.search_active {
            self.search_query(self.active_search_query.clone());
        } else {
            self.open_folder(self.folder.clone());
            self.refreshing_folder = self.folder_open_pending;
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
        self.search_query(self.search_text.clone());
    }

    // A completed metadata write can change the membership of the current
    // query. If a query is already running, repeat its exact submitted text
    // after it finishes rather than publishing a potentially stale result.
    fn metadata_changed(&mut self, path: &Path) {
        if path.starts_with(&self.folder) && (self.search_active || self.search_pending) {
            self.search_dirty = true;
            self.refresh_dirty_search();
        }
    }

    fn refresh_dirty_search(&mut self) {
        if !self.search_dirty || self.search_pending || self.folder_open_pending { return; }
        if self.search_active {
            self.search_dirty = false;
            self.search_query(self.active_search_query.clone());
            self.search_preserve_display = true;
        }
    }

    fn search_query(&mut self, query: String) {
        if self.folder_open_pending
            || self.folder.as_os_str().is_empty()
            || query.trim().is_empty() {
            return;
        }
        self.search_generation += 1;
        self.search_preserve_display = false;
        self.slideshow = false;
        self.slide_deadline = None;
        self.search_currency
            .store(self.search_generation, Ordering::Release);
        self.search_pending = true;
        self.pending_search_query = query.clone();
        self.status = Status::Text(Text::Indexing);
        let _ = self.search_sender.send(SearchRequest {
            id: self.search_generation,
            folder: self.folder.clone(),
            query,
        });
    }

    // Focus/selection are source-owned state, not inferred from rendered
    // thumbnails. Keyboard arrows reuse the same grammar as mouse selection.
    fn step_media(&mut self, direction: isize, extend: bool) {
        self.slideshow = false;
        self.slide_deadline = None;
        self.selection
            .move_focus(&self.visible_media, direction < 0, extend);
        self.present_selection();
    }

    fn select_media(&mut self, path: PathBuf, intent: Intent) {
        if self.folder_open_pending {
            return;
        }
        self.slideshow = false;
        self.slide_deadline = None;
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
            self.source_extent = None;
        }
        if let Some((left, right)) = pair {
            self.comparison_zoom.fit();
            self.comparison_active_right = false;
            self.comparison_wheel_remainder = 0.0;
            self.comparison_full_requested = false;
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
        self.slide_deadline = None;
        self.zoom.fit();
        self.navigator_until = None;
        self.navigator_last_rect = None;
        self.zoom_wheel_remainder = 0.0;
        self.source_extent = None;
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

    fn toggle_playback(&mut self, ctx: &egui::Context) {
        if !self.selected.as_ref().is_some_and(|path| is_video(path) || is_audio(path))
            || matches!(self.playback_state, 1 | 2) {
            return;
        }
        self.slideshow = false;
        self.slide_deadline = None;
        if matches!(self.playback_state, 3 | 4) {
            let _ = self.playback_sender.send(PlaybackRequest::Pause(
                self.playback_generation,
                self.playback_state == 3,
            ));
        } else {
            self.start_playback(ctx);
        }
    }

    fn toggle_slideshow(&mut self, ctx: &egui::Context) {
        if self.slideshow {
            self.stop_slideshow();
            return;
        }
        if self.folder_open_pending || self.visible_media.is_empty() {
            return;
        }
        self.slideshow = true;
        self.slide_deadline = None;
        if self.selected.is_none() {
            let start = self.selection.focus()
                .filter(|path| self.visible_media.iter().any(|item| item == *path))
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.visible_media[0].clone());
            self.selection.apply(&self.visible_media, &start, Intent::Replace);
            self.present_selection();
        } else if self.selected.as_ref().is_some_and(|path| is_image(path)) && self.texture.is_some() {
            self.slide_deadline = Some(Instant::now() + Duration::from_secs(self.slide_seconds));
        } else if self.selected.as_ref().is_some_and(|path| is_video(path) || is_audio(path)) {
            if self.playback_state == 4 {
                let _ = self.playback_sender.send(PlaybackRequest::Pause(self.playback_generation, false));
            } else if matches!(self.playback_state, 6 | 7) {
                self.start_playback(ctx);
            }
        }
        self.tick_slideshow(ctx);
    }

    fn stop_slideshow(&mut self) {
        self.slideshow = false;
        self.slide_deadline = None;
        if self.playback_state == 3 {
            let _ = self.playback_sender.send(PlaybackRequest::Pause(self.playback_generation, true));
        }
    }

    fn tick_slideshow(&mut self, ctx: &egui::Context) {
        if !self.slideshow || self.folder_open_pending { return; }
        let Some(path) = self.selected.clone() else { self.stop_slideshow(); return; };
        let advance = if is_image(&path) {
            if let Some(deadline) = self.slide_deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !remaining.is_zero() { ctx.request_repaint_after(remaining); }
                remaining.is_zero()
            } else { false }
        } else if is_audio(&path) || is_video(&path) {
            if self.playback_state == 0 { self.start_playback(ctx); }
            if self.playback_state == 7 { self.stop_slideshow(); return; }
            self.playback_state == 6
        } else { self.stop_slideshow(); return; };
        if advance {
            let next = self.visible_media.iter().position(|item| item == &path)
                .and_then(|index| self.visible_media.get(index + 1)).cloned();
            if let Some(next) = next {
                self.selection.apply(&self.visible_media, &next, Intent::Replace);
                self.present_selection();
                ctx.request_repaint();
            } else {
                self.stop_slideshow();
            }
        }
    }

    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(response) = self.receiver.try_recv() {
            match response {
                Response::Folder(id, path, result, selected) if id == self.folder_generation => {
                    self.folder_open_pending = false;
                    let refreshing = std::mem::take(&mut self.refreshing_folder);
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
                            if !refreshing {
                                self.selection.clear();
                            }
                            self.comparison = None;
                            self.texture = None;
                            self.metadata = None;
                            self.video_aspect = None;
                            self.search_active = false;
                            self.search_dirty = false;
                            self.folder = path;
                            self.address = self.folder.to_string_lossy().into_owned();
                            self.entries = entries;
                            self.visible_media = self.entries.iter()
                                .filter(|entry| !entry.directory)
                                .map(|entry| entry.path.clone())
                                .collect();
                            if refreshing {
                                self.selection.retain_visible(&self.visible_media);
                            }
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
                            } else if refreshing && self.selection.len() > 0 {
                                self.present_selection();
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
                        Err(error) => {
                            self.status = Status::Error(error);
                            self.refresh_dirty_search();
                        },
                    }
                }
                Response::Image(id, path, result)
                    if id == self.image_generation && self.selected.as_ref() == Some(&path) =>
                {
                    match result {
                        Ok((image, metadata, video, original)) => {
                            let size = image.size;
                            if !video {
                                self.source_extent = Some(original.unwrap_or_else(|| {
                                    egui::vec2(size[0] as f32, size[1] as f32)
                                }));
                            }
                            if video && size[1] > 0 {
                                self.video_aspect = Some(size[0] as f32 / size[1] as f32);
                            }
                            self.metadata = metadata;
                            self.texture = Some(ctx.load_texture(
                                "preview",
                                image,
                                egui::TextureOptions::LINEAR,
                            ));
                            #[cfg(feature = "visual-audit")]
                            if !video && std::env::var_os("DIFFRACTOR_AUDIT_ZOOM").is_some() {
                                self.zoom.actual_pixels(self.source_extent.unwrap_or(egui::Vec2::ONE),
                                    self.last_viewport.max(egui::Vec2::ONE));
                            }
                            if self.slideshow && !video {
                                self.slide_deadline = Some(Instant::now() + Duration::from_secs(self.slide_seconds));
                                ctx.request_repaint_after(Duration::from_secs(self.slide_seconds));
                            }
                            self.status = Status::Image { width: size[0], height: size[1], video, full: false };
                            #[cfg(feature = "visual-audit")]
                            if video && self.audit_autoplay {
                                self.start_playback(ctx);
                            }
                        }
                        Err(error) => {
                            if self.slideshow { self.stop_slideshow(); }
                            self.status = Status::Error(error);
                        },
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
                                left: ctx.load_texture("compare-left", first.image, egui::TextureOptions::LINEAR),
                                right: ctx.load_texture("compare-right", second.image, egui::TextureOptions::LINEAR),
                                left_source: first.source,
                                right_source: second.source,
                            });
                            self.status = Status::Text(Text::Comparing);
                        }
                        Err(error) => self.status = Status::Error(error),
                    }
                }
                Response::FullCompare(id, left, right, result)
                    if id == self.image_generation && !self.folder_open_pending =>
                {
                    if self.comparison_pair().as_ref() != Some(&(left, right)) { continue; }
                    match result {
                        Ok((first, second)) => {
                            let max_side = ctx.input(|input| input.max_texture_side);
                            if first.size.iter().chain(second.size.iter()).any(|side| *side > max_side) {
                                self.status = Status::Text(Text::FullImageTooLarge);
                            } else if let Some(compare) = &mut self.comparison {
                                compare.left.set(first, egui::TextureOptions::LINEAR);
                                compare.right.set(second, egui::TextureOptions::LINEAR);
                            }
                        }
                        Err(error) => self.status = Status::Error(error),
                    }
                }
                Response::Metadata(id, path, result)
                    if id == self.image_generation && self.selected.as_ref() == Some(&path) =>
                {
                    self.metadata = result.ok();
                }
                Response::Rating(message) => {
                    self.rating_busy = false;
                    match message {
                        rating::ResultMessage::Prepared(path, Ok((metadata, snapshot))) => {
                            self.rating_edit = Some((path, metadata.clone(), metadata, snapshot, String::new(), String::new()));
                        }
                        rating::ResultMessage::Prepared(_, Err(error)) => self.status = Status::Error(error),
                        rating::ResultMessage::Saved(path, Ok(metadata)) => {
                            let reloaded = metadata.is_some();
                            if self.selected.as_ref() == Some(&path) { self.metadata = metadata; }
                            self.status = if reloaded { Status::RatingSaved(path.to_string_lossy().into_owned()) }
                                else { Status::Error(format!("{}: {}", tr(self.language, Text::ReloadFailed), path.display())) };
                            if !reloaded {
                                self.operation_error = Some(format!("{}: {}", tr(self.language, Text::ReloadFailed), path.display()));
                            }
                            self.metadata_changed(&path);
                        }
                        rating::ResultMessage::Saved(path, Err(error)) => {
                            let message = format!("{}: {error}", path.display());
                            self.status = Status::Error(message.clone());
                            self.operation_error = Some(message);
                            // A failed in-place write can still have changed bytes.
                            self.metadata_changed(&path);
                        }
                        rating::ResultMessage::BatchPrepared(Ok(items)) => {
                            self.batch_edit = Some(BatchEdit { items, ..Default::default() });
                            self.status = Status::Empty;
                        }
                        rating::ResultMessage::BatchPrepared(Err(error)) => self.status = Status::Error(error),
                        rating::ResultMessage::BatchSaved { updated, failures, cancelled } => {
                            self.batch_progress = None;
                            let succeeded = updated.len();
                            let mut reload_failed = Vec::new();
                            let mut search_affected = false;
                            search_affected |= failures.iter().any(|(path, _)| path.starts_with(&self.folder));
                            for (path, metadata) in updated {
                                if metadata.is_none() { reload_failed.push(path.clone()); }
                                search_affected |= path.starts_with(&self.folder);
                                if self.selected.as_ref() == Some(&path) { self.metadata = metadata; }
                            }
                            self.batch_result = Some((succeeded, failures, reload_failed, cancelled));
                            self.status = Status::Text(Text::BatchResult);
                            if search_affected && (self.search_active || self.search_pending) {
                                self.search_dirty = true;
                                self.refresh_dirty_search();
                            }
                        }
                    }
                }
                Response::RatingProgress(done, total) => {
                    if self.batch_progress.is_some() { self.batch_progress = Some((done, total)); }
                }
                Response::Picker(id, folder_generation, search_generation, selected)
                    if id == self.picker_generation => {
                    self.picker_busy = false;
                    if folder_generation == self.folder_generation && search_generation == self.search_generation
                        && !self.folder_open_pending {
                        if let Some(path) = selected { self.open_path(path); }
                    }
                }
                Response::Rename(message) => {
                    self.rename_busy = false;
                    match message {
                        rename::ResultMessage::Analyzed(source, name, result) => {
                            if let Some(draft) = &mut self.rename_draft {
                                if draft.source == source && draft.name == name {
                                    match result {
                                        Ok(plan) => { draft.plan = Some(plan); draft.error = None; }
                                        Err(error) => { draft.plan = None; draft.error = Some(error); }
                                    }
                                }
                            }
                        }
                        rename::ResultMessage::Finished(source, destination, result) => {
                            match result {
                                Ok(()) => {
                                    self.rename_result = Some(format!("{} → {}", source.display(), destination.display()));
                                    if !self.folder_open_pending && self.search_active && source.starts_with(&self.folder) {
                                        if self.selected.as_ref() == Some(&source) {
                                            for entry in &mut self.entries { if entry.path == source { entry.path = destination.clone(); } }
                                            for path in &mut self.visible_media { if *path == source { *path = destination.clone(); } }
                                            self.selection.set_direct(destination.clone());
                                            self.open_image(destination.clone());
                                        }
                                        self.metadata_changed(&source);
                                    } else if !self.folder_open_pending && source.parent() == Some(self.folder.as_path()) {
                                        if self.selected.as_ref() == Some(&source) { self.open_path(destination); }
                                        else { self.refresh(); }
                                    }
                                }
                                Err(error) => {
                                    self.rename_result = Some(format!("{}: {error}", source.display()));
                                    if self.search_active { self.metadata_changed(&source); }
                                    else if !self.folder_open_pending && source.parent() == Some(self.folder.as_path()) {
                                        self.refresh();
                                    }
                                }
                            }
                        }
                    }
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
                            self.source_extent.get_or_insert_with(|| {
                                egui::vec2(size[0] as f32, size[1] as f32)
                            });
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
                    if self.search_dirty {
                        self.search_dirty = false;
                        self.search_query(self.pending_search_query.clone());
                        self.search_preserve_display = true;
                        continue;
                    }
                    match result {
                        Ok(paths) => {
                            let same_query = self.search_active && self.active_search_query == self.pending_search_query;
                            let old_selection_count = self.selection.len();
                            self.active_search_query.clone_from(&self.pending_search_query);
                            self.clear_thumbnails();
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
                            let preserve_display = self.search_preserve_display && same_query && old_selection_count > 0
                                && old_selection_count == self.selection.len()
                                && (self.selected.is_some() || self.comparison.is_some());
                            if !preserve_display {
                                self.stop_playback();
                                self.image_generation += 1;
                                self.selected = None;
                                self.comparison = None;
                                self.texture = None;
                                self.metadata = None;
                            }
                            self.search_active = true;
                            self.truncated = false;
                            self.status = Status::Matches(self.entries.len());
                            if !preserve_display && self.selection.len() != 0 {
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
                        self.slideshow = false;
                        self.slide_deadline = None;
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
        self.batch_cancel.store(true, Ordering::Release);
        self.search_currency.store(u64::MAX, Ordering::Release);
        self.thumbnail_currency.store(u64::MAX, Ordering::Release);
        let _ = self.playback_sender.send(PlaybackRequest::Stop);
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
    use super::*;

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
