//! Native media playback. LibVLC runs on the media worker; its callbacks publish bounded RGBA frames.
use std::{
    alloc::{alloc_zeroed, dealloc, handle_alloc_error, Layout},
    ffi::{c_char, c_int, c_uint, c_void, CString},
    os::unix::ffi::OsStrExt,
    path::Path,
    ptr::{self, NonNull},
    sync::{Arc, Condvar, Mutex},
};

#[link(name = "vlc")]
unsafe extern "C" {
    fn libvlc_new(argc: c_int, argv: *const *const c_char) -> *mut c_void;
    fn libvlc_release(instance: *mut c_void);
    fn libvlc_media_new_path(instance: *mut c_void, path: *const c_char) -> *mut c_void;
    fn libvlc_media_release(media: *mut c_void);
    fn libvlc_media_player_new_from_media(media: *mut c_void) -> *mut c_void;
    fn libvlc_media_player_release(player: *mut c_void);
    fn libvlc_media_player_play(player: *mut c_void) -> c_int;
    fn libvlc_media_player_stop(player: *mut c_void);
    fn libvlc_media_player_set_pause(player: *mut c_void, pause: c_int);
    fn libvlc_media_player_get_state(player: *mut c_void) -> c_int;
    fn libvlc_media_player_get_time(player: *mut c_void) -> i64;
    fn libvlc_media_player_get_length(player: *mut c_void) -> i64;
    fn libvlc_media_player_set_time(player: *mut c_void, time: i64);
    fn libvlc_video_set_callbacks(
        player: *mut c_void,
        lock: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> *mut c_void,
        unlock: unsafe extern "C" fn(*mut c_void, *mut c_void, *const *mut c_void),
        display: unsafe extern "C" fn(*mut c_void, *mut c_void),
        opaque: *mut c_void,
    );
    fn libvlc_video_set_format_callbacks(
        player: *mut c_void,
        setup: unsafe extern "C" fn(
            *mut *mut c_void,
            *mut c_char,
            *mut c_uint,
            *mut c_uint,
            *mut c_uint,
            *mut c_uint,
        ) -> c_uint,
        cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
    );
}

#[derive(Clone, Copy, Default)]
struct Format {
    width: usize,
    height: usize,
    pitch: usize,
    lines: usize,
}

pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

pub struct Shared {
    format: Mutex<Format>,
    in_flight: Mutex<usize>,
    available: Condvar,
    pending: Mutex<Option<Frame>>,
    displayed: Mutex<Option<Frame>>,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            format: Mutex::new(Format::default()),
            in_flight: Mutex::new(0),
            available: Condvar::new(),
            pending: Mutex::new(None),
            displayed: Mutex::new(None),
            notify: Box::new(notify),
        })
    }
    pub fn take_frame(&self) -> Option<Frame> {
        self.displayed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

// Each VLC picture owns an aligned allocation until unlock; the number of live pictures is capped.
struct Picture {
    data: NonNull<u8>,
    layout: Layout,
    format: Format,
}
impl Drop for Picture {
    fn drop(&mut self) {
        unsafe { dealloc(self.data.as_ptr(), self.layout) }
    }
}

unsafe extern "C" fn setup(
    opaque: *mut *mut c_void,
    chroma: *mut c_char,
    width: *mut c_uint,
    height: *mut c_uint,
    pitches: *mut c_uint,
    lines: *mut c_uint,
) -> c_uint {
    let shared = unsafe { &*((*opaque).cast::<Shared>()) };
    let (w, h) = unsafe { (*width as usize, *height as usize) };
    if w == 0 || h == 0 {
        return 0;
    }
    let scale = (1280.0 / w as f64).min(720.0 / h as f64).min(1.0);
    let w = ((w as f64 * scale).round() as usize).max(1);
    let h = ((h as f64 * scale).round() as usize).max(1);
    let pitch = (w * 4).div_ceil(32) * 32;
    let rows = h.div_ceil(32) * 32;
    *shared.format.lock().unwrap_or_else(|e| e.into_inner()) = Format {
        width: w,
        height: h,
        pitch,
        lines: rows,
    };
    unsafe {
        ptr::copy_nonoverlapping(b"RV32".as_ptr().cast::<c_char>(), chroma, 4);
        *width = w as c_uint;
        *height = h as c_uint;
        *pitches = pitch as c_uint;
        *lines = rows as c_uint;
    }
    1
}

unsafe extern "C" fn lock(opaque: *mut c_void, planes: *mut *mut c_void) -> *mut c_void {
    let shared = unsafe { &*(opaque.cast::<Shared>()) };
    let mut count = shared.in_flight.lock().unwrap_or_else(|e| e.into_inner());
    while *count >= 3 {
        count = shared
            .available
            .wait(count)
            .unwrap_or_else(|e| e.into_inner());
    }
    *count += 1;
    drop(count);
    let format = *shared.format.lock().unwrap_or_else(|e| e.into_inner());
    // setup bounds both dimensions, pitch and line count, so this layout is finite and aligned.
    let layout =
        Layout::from_size_align(format.pitch * format.lines, 32).expect("bounded video buffer");
    let data =
        NonNull::new(unsafe { alloc_zeroed(layout) }).unwrap_or_else(|| handle_alloc_error(layout));
    unsafe {
        *planes = data.as_ptr().cast();
    }
    Box::into_raw(Box::new(Picture {
        data,
        layout,
        format,
    }))
    .cast()
}

unsafe extern "C" fn unlock(
    opaque: *mut c_void,
    picture: *mut c_void,
    _planes: *const *mut c_void,
) {
    let shared = unsafe { &*(opaque.cast::<Shared>()) };
    let picture = unsafe { Box::from_raw(picture.cast::<Picture>()) };
    let f = picture.format;
    let source = unsafe { std::slice::from_raw_parts(picture.data.as_ptr(), f.pitch * f.lines) };
    let mut rgba = vec![0; f.width * f.height * 4];
    for y in 0..f.height {
        for x in 0..f.width {
            let src = y * f.pitch + x * 4;
            let dst = (y * f.width + x) * 4;
            rgba[dst..dst + 4].copy_from_slice(&[
                source[src + 2],
                source[src + 1],
                source[src],
                255,
            ]);
        }
    }
    *shared.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(Frame {
        width: f.width,
        height: f.height,
        rgba,
    });
    drop(picture);
    let mut count = shared.in_flight.lock().unwrap_or_else(|e| e.into_inner());
    *count -= 1;
    shared.available.notify_one();
}

unsafe extern "C" fn display(opaque: *mut c_void, _picture: *mut c_void) {
    let shared = unsafe { &*(opaque.cast::<Shared>()) };
    let frame = shared
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if frame.is_some() {
        *shared.displayed.lock().unwrap_or_else(|e| e.into_inner()) = frame;
        (shared.notify)();
    }
}

pub struct Player {
    instance: NonNull<c_void>,
    player: NonNull<c_void>,
    opaque: *const Shared,
}

impl Player {
    pub fn open(path: &Path, shared: Arc<Shared>, mute: bool) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("Cannot open media: {e}"))?;
        if !file
            .metadata()
            .map_err(|e| format!("Cannot inspect media: {e}"))?
            .is_file()
        {
            return Err("Media path is not a regular file.".into());
        }
        drop(file);
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| "Invalid media path".to_owned())?;
        let quiet = c"--quiet";
        let no_title = c"--no-video-title-show";
        let dummy_audio = c"--aout=adummy";
        let pulse_audio = c"--aout=pulse";
        let mut args = vec![quiet.as_ptr(), no_title.as_ptr()];
        if mute {
            args.push(dummy_audio.as_ptr());
        } else if cfg!(test) && std::env::var_os("DIFFRACTOR_TEST_PULSE").is_some() {
            args.push(pulse_audio.as_ptr());
        }
        let instance = NonNull::new(unsafe { libvlc_new(args.len() as c_int, args.as_ptr()) })
            .ok_or("Cannot initialize libvlc")?;
        let media = unsafe { libvlc_media_new_path(instance.as_ptr(), path.as_ptr()) };
        if media.is_null() {
            unsafe { libvlc_release(instance.as_ptr()) };
            return Err("Cannot open media".into());
        }
        let player = unsafe { libvlc_media_player_new_from_media(media) };
        unsafe { libvlc_media_release(media) };
        let Some(player) = NonNull::new(player) else {
            unsafe { libvlc_release(instance.as_ptr()) };
            return Err("Cannot create media player".into());
        };
        let opaque = Arc::into_raw(shared);
        unsafe {
            libvlc_video_set_callbacks(
                player.as_ptr(),
                lock,
                unlock,
                display,
                opaque.cast_mut().cast(),
            );
            libvlc_video_set_format_callbacks(player.as_ptr(), setup, None);
        }
        let result = Self {
            instance,
            player,
            opaque,
        };
        if unsafe { libvlc_media_player_play(player.as_ptr()) } != 0 {
            return Err("Cannot start playback".into());
        }
        Ok(result)
    }
    pub fn state(&self) -> i32 {
        unsafe { libvlc_media_player_get_state(self.player.as_ptr()) }
    }
    pub fn time(&self) -> i64 {
        unsafe { libvlc_media_player_get_time(self.player.as_ptr()) }
    }
    pub fn duration(&self) -> i64 {
        unsafe { libvlc_media_player_get_length(self.player.as_ptr()) }
    }
    pub fn pause(&self, pause: bool) {
        unsafe { libvlc_media_player_set_pause(self.player.as_ptr(), pause as i32) }
    }
    pub fn seek(&self, time: i64) {
        unsafe { libvlc_media_player_set_time(self.player.as_ptr(), time.max(0)) }
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        // stop/release joins VLC callbacks before their opaque Arc can be released.
        unsafe {
            libvlc_media_player_stop(self.player.as_ptr());
            libvlc_media_player_release(self.player.as_ptr());
            libvlc_release(self.instance.as_ptr());
            drop(Arc::from_raw(self.opaque));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn plays_video_and_delivers_bounded_frames() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../exe/test/excluded1/rotated90.mp4");
        let shared = Shared::new(|| {});
        let player = Player::open(&path, shared.clone(), true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let frame = loop {
            if let Some(frame) = shared.take_frame() {
                break frame;
            }
            assert!(
                Instant::now() < deadline,
                "no video frame, player state {}",
                player.state()
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(frame.width > 0 && frame.width <= 1280 && frame.height > 0 && frame.height <= 720);
        assert_eq!(frame.rgba.len(), frame.width * frame.height * 4);
        assert!(frame.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut changed = false;
        while Instant::now() < deadline {
            if let Some(next) = shared.take_frame() {
                if next.rgba != frame.rgba {
                    changed = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(changed, "playback must deliver changing video frames");
        player.pause(true);
        let pause_deadline = Instant::now() + Duration::from_secs(3);
        while player.state() != 4 && Instant::now() < pause_deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(player.state(), 4, "player should pause");
        player.seek(0);
        player.pause(false);
        let _ = (player.time(), player.duration());
    }

    #[test]
    fn decodes_audio_through_dummy_output() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../exe/test/gen.wav");
        let shared = Shared::new(|| {});
        let player = Player::open(&path, shared, true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while player.time() <= 0
            && player.state() != 6
            && player.state() != 7
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(player.state(), 7, "audio playback failed");
        assert!(
            player.time() > 0 || player.state() == 6,
            "audio did not advance"
        );
    }

    #[test]
    #[ignore = "requires an isolated PulseAudio-compatible sink"]
    fn sends_audio_to_system_output() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../exe/test/gen.wav");
        let player = Player::open(&path, Shared::new(|| {}), false).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while player.state() != 6 && player.state() != 7 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(player.state(), 6, "audio did not complete");
    }

    #[test]
    fn reports_missing_media_and_releases_repeated_players() {
        let shared = Shared::new(|| {});
        assert!(Player::open(Path::new("/no/such/diffractor-media.mp4"), shared, true).is_err());
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../exe/test/excluded1/rotated90.mp4");
        for _ in 0..5 {
            let shared = Shared::new(|| {});
            let player = Player::open(&path, shared, true).unwrap();
            std::thread::sleep(Duration::from_millis(50));
            drop(player);
        }
    }
}
