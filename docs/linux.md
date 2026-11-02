# Linux Port

This document owns the current Linux support boundary: what builds, which platform services exist,
and which product surfaces are unavailable. [Implementation](implementation.md) owns architecture;
[Rendering](rendering.md) owns backend parity; [Design](design.md) owns cross-platform user behavior.

## Current Support

The portable core and vendored dependencies build with CMake and GCC on Linux, and the embedded test
suite runs headlessly in Debug and Release. Indexing, search, metadata parsing, image and media decode,
collections, locations, and core file operations use the same model and tests as Windows.

The CMake executable remains a headless test target. An experimental Rust native frontend can
browse folders, preview images, play video and audio, display embedded metadata and search a folder
recursively with the original index. It is not yet the full Diffractor application: managed multi-root
collections, XMP sidecars in the details panel, editing, and desktop integration are not connected.
The Arch package is an experimental preview, not the complete product.

An experimental Rust bridge now exposes libfiles image-header scanning, metadata and bounded image decoding
through a Linux C ABI. A separate headless worker runs the application's index and search parser.
Each folder has its own SQLite cache under `$XDG_CACHE_HOME/diffractor/rust-index-v1` (or
`~/.cache/diffractor/rust-index-v1`), and later searches enumerate the folder again and refresh stale
entries. A per-folder lock serializes workers that share a cache. Folder scopes in the search text
are rejected because the chosen folder is the scope. Starting another query or leaving the folder
terminates the previous worker, and stale results are discarded. The Rust wrapper returns
straight-alpha RGBA pixels and orientation; the frontend applies the orientation on its worker,
which also performs folder reads and decoding outside the UI thread. Video previews use the original
FFmpeg-backed decoder in a separate process. Playback uses libVLC on a separate worker with bounded
software frame buffers, system audio output, play/pause and seeking. This is a CPU-copy rendering path;
hardware video presentation and the original player's full feature set are not yet implemented.
Visible still images and videos receive bounded, in-memory thumbnails on a separate worker; these are
not yet stored in the application's thumbnail database.
Still-image previews now use a Rust source-space zoom model ported from
`src/model_zoom.h`: Fit (never upscale), the original 19-step 5%–1600% ladder,
true original-pixel 1:1, source-anchored zoom and edge-clamped panning.
The worker probes oriented source dimensions separately from the decoded preview,
so switching to better pixels does not change zoom geometry. Full-resolution
decode stays within the bridge's 256 MiB pixel limit and GPU texture-size
cap; unsupported large images retain the preview. The original linked comparison
zoom, navigator and progressive tiles are not yet implemented.
The preview accepts a media file or folder as its command-line argument and advertises supported
media types to desktop file managers. A file argument opens its containing folder and selects the
file; resolving the path and reading the folder happen on the worker.
Common camera RAW extensions are visible in the browser, with previews decoded through libfiles'
RAW support when ordinary image decoding does not apply. RAW sidecar metadata is not yet shown.

## Platform Boundary

Portable application code depends on libdesktop's [platform.h](https://github.com/ZacWalk/libdesktop/blob/main/include/libdesktop/platform.h) -
the OS services of its system module, which also carries their Linux backend and
[documents it](https://github.com/ZacWalk/libdesktop/blob/main/docs/system.md#the-linux-backend) - and on
libdesktop's [platform_render.h](https://github.com/ZacWalk/libdesktop/blob/main/include/libdesktop/platform_render.h) for what the desktop UI layer implements: icon glyphs,
movie writing, audio output and the rendering probes. Operating-system headers, handles, and direct system calls
remain in libdesktop and in the application's own `platform*` files - its side of FFmpeg's hardware
decoding and its entry points. Repository lint enforces the parts
of this boundary visible from source text. It allows OS headers in any `platform*` file, though
only `platform_win_av.cpp` and the Windows platform tests still need them. XMP rationals, property
ratios and search terms are still read with `_snscanf_s` (in `metadata_xmp.cpp`,
`model_property.cpp` and `model_search.cpp`), which libdesktop shims on Linux and which can read
damaged text differently there; dates already parse by one rule on both platforms.

The Linux implementation currently provides:

| Area | Implementation |
| --- | --- |
| Process, synchronization, locale, known folders | libdesktop's `platform_linux.cpp` |
| Files, mapping, enumeration, paths | libdesktop's `platform_linux_files.cpp` |
| Copy, move, delete, and shell stand-ins | libdesktop's `platform_linux_desktop.cpp` |
| INI settings | libdesktop's `platform_linux_settings.cpp` |
| The desktop UI's stand-ins: theme colours, key codes, UI-thread identity, icon glyphs, movie writing and audio output | libdesktop's `platform_linux_ui.cpp` |
| Entry point, the absent hardware video decode, and the empty platform test registration | `platform_linux_app.cpp` |

Path identity follows the filesystem: case-insensitive on Windows and case-sensitive on Linux.
Case-folded comparisons remain appropriate for media types, tags, and other vocabulary, but not for
path-keyed containers.
Filesystem creation time is read from birth-time metadata when available; an absent birth time
stays absent. Directory moves require atomic non-replacing rename support and report failure rather
than emulating it with an existence check followed by a replacing rename.
Startup selects the UTF-8 character locale for library filename conversions, independently of the
shell's locale. A failed file operation always supplies a nonempty diagnostic, including collisions
reported by an atomic move.

## Unsupported Surfaces

The current libdesktop Linux platform has no implementation for:

- native windows, input dispatch, dialogs, menus, clipboard, drag/drop, or monitor/DPI integration;
- font discovery, shaping, fallback, and glyph rasterization;
- a GPU backend or a native present target for the portable software rasterizer;
- audio output and hardware video presentation;
- Windows Shell features such as associations, properties, Explorer thumbnails, device eject, the
  share sheet, and taskbar progress;
- the Windows update, installer and Store workflows.

Portable code must express the intended operation rather than mirror a Win32 API shape. A Linux
stand-in may support headless tests, but it must not report success for a user-visible operation it
did not perform.

## Rendering Path

The platform-independent software rasterizer already produces bounded BGRA tiles. A Linux UI needs a
window-owned buffer and present target plus the text stack described above. Rendering parity remains
defined by [rendering.md](rendering.md); Linux does not get a reduced visual contract merely because
its first backend is software.

## Build

CMake builds the original application and media bridge; Cargo builds the experimental Rust frontend.
Linux builds vendored dependencies by default and
upstream FFmpeg as a required dependency, and fetches libfiles and libdesktop - or builds sibling
`../libfiles` and `../libdesktop` checkouts. Runtime fixtures, languages, dictionaries, and location data are staged beside the test
binary.

Typical commands are:

```bash
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug
cmake --build build
./build/diffractor /test
```

CI definitions remain authoritative for compiler versions and installed system prerequisites.

### Arch Linux

On Arch Linux, `dd setup` uses pacman rather than apt. The package installation
covers the native compiler/toolchain and the optional system codec headers;
CMake continues to fetch the project's pinned libraries.

```bash
git lfs install
git lfs pull
bash ./dd setup --check
bash ./dd setup
bash ./dd build --config Release
bash ./dd test --config Release
```

Run `bash ./dd info` to inspect the detected toolchain and binary path.
The CMake executable is a **headless test binary**. The separate Rust frontend below is an
experimental photo browser and does not yet implement the complete application.

To build and test the experimental Rust media bridge after the Release build:

```bash
sudo pacman -S --needed libvlc vlc-plugins-base vlc-plugin-ffmpeg
cmake --build tmp/linux-build --target diffractor_linux_bridge
LD_LIBRARY_PATH="$PWD/tmp/linux-build${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  cargo test --manifest-path rust/diffractor-core/Cargo.toml
cargo build --manifest-path rust/diffractor-ui/Cargo.toml
LD_LIBRARY_PATH="$PWD/tmp/linux-build${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  ./rust/diffractor-ui/target/debug/diffractor-ui "$HOME/Pictures"
```

The bridge currently uses the checkout's CMake output, not an installed library. Set
`DIFFRACTOR_BRIDGE_DIR` when linking against a different build directory and include that directory
in the runtime library search path as well. Test fixtures require Git LFS content.

To build the experimental Arch package from the current working tree, including local changes:

```bash
bash tools/make_arch_snapshot.sh
```

The script writes a checksummed source snapshot and package to a managed directory under
`tmp/arch-package.*`; its location is printed during the build. It reuses that directory on later
runs, preserving CMake, Cargo and compiler-cache outputs and updating only changed source files.
The package's `check()` runs the headless and Rust tests; pass `--nocheck` to makepkg only when
those checks have already been run and the changes do not warrant repeating them. Set
`DIFFRACTOR_ARCH_CLEAN=1` to start a separate clean package build, or set `DIFFRACTOR_JOBS` to
change the CMake parallelism. First builds fetch pinned native dependencies and Cargo crates and
therefore require network access. `--nodeps` skips Arch dependency checks and is only appropriate
when the required tools and libraries are already available by another route.

Build from a Linux checkout. A Windows checkout with `core.autocrlf` set, built from WSL, gives
FFmpeg's `configure` CRLF line endings, which `/bin/sh` rejects as "No such file or directory".

### Retiring MSBuild

The application solution and project files were removed after CMake reached parity. Repository lint
rejects tracked `.sln` and application `.vcxproj` files so a second build description cannot drift
back in. Vendored forks may retain their own upstream project files; Diffractor's CMake modules decide
how they join the application build.

## Where this lives

- libdesktop's [platform.h](https://github.com/ZacWalk/libdesktop/blob/main/include/libdesktop/platform.h) and
  [platform_render.h](https://github.com/ZacWalk/libdesktop/blob/main/include/libdesktop/platform_render.h): abstraction required from each platform.
- libdesktop's [Linux backend](https://github.com/ZacWalk/libdesktop/tree/main/src/system) and
  [platform_linux_ui.cpp](https://github.com/ZacWalk/libdesktop/blob/main/src/render/platform_linux_ui.cpp) and
  [platform_linux_app.cpp](../src/platform_linux_app.cpp): current Linux implementation.
- libdesktop's [platform_compat.h](https://github.com/ZacWalk/libdesktop/blob/main/include/libdesktop/platform_compat.h): cross-platform
  compatibility helpers.
- libdesktop's [render_software.h](https://github.com/ZacWalk/libdesktop/blob/main/include/libdesktop/render_software.h) and
  [render_software.cpp](https://github.com/ZacWalk/libdesktop/blob/main/src/render/render_software.cpp): portable CPU rasterizer.
- [CMakeLists.txt](../CMakeLists.txt), `cmake/`, [dd.ps1](../dd.ps1), and
  [tools/dd.py](../tools/dd.py): build description and drivers.
- [platform_linux_bridge.cpp](../src/platform_linux_bridge.cpp),
  [platform_linux_bridge.h](../src/platform_linux_bridge.h), and
  [diffractor-core](../rust/diffractor-core/src/lib.rs): the experimental Rust media boundary.
- [diffractor-ui](../rust/diffractor-ui/src/main.rs): the experimental native browser.
- [video.rs](../rust/diffractor-ui/src/video.rs): native audio and video playback integration.
- [platform_linux_index.cpp](../src/platform_linux_index.cpp): the isolated collection search worker.
- [platform_linux_video.cpp](../src/platform_linux_video.cpp): the isolated video preview worker.
- [Arch package template](../packaging/arch/PKGBUILD.in) and
  [snapshot builder](../tools/make_arch_snapshot.sh): the experimental local package workflow.

## Native Rust Migration — Canonical Plan (2026-11-02)

**Decision (revised by the user on 2026-11-02):** Build a **native Rust/egui
Diffractor frontend** preserving the Windows application's recognizable UI,
interaction grammar, and product capabilities. The earlier proposal to finish a
C++/libdesktop Linux window backend as the final product is **superseded**.
That route was useful for reducing immediate implementation risk but conflicts
with the chosen Rust-native frontend goal. Do not revive it as a silent
substitute for the Rust migration.

Original Windows product semantics remain governed by [Design](design.md),
[Selection Controls](selection-controls.md), [Zoom](zoom.md),
[Collections](collections.md), [Metadata](metadata.md), and [File I/O](file-io.md).
This document owns **migration scope, dependency order and evidence**, not a
second, conflicting product behavior specification.

### Runtime ownership and migration boundaries

```text
Native Linux window (eframe/egui, keyboard, pointer, text, accessibility)
    → Rust view state (scope, selection/focus/anchor, media, history, commands)
    → diffractor-core (safe Rust API; path/ownership, errors and generations)
    → bounded C ABI: libfiles/FFmpeg/index/search/metadata (existing C++ code)
    → filesystem / local media / SQLite (authoritative media+sidecars, cache index)
```

This is a staged **Rust-native UI with a C++ media core**, not yet a Rust-only
application. Retain the working C++ core temporarily; replace C++ modules only
when a Rust implementation is justified by measured benefit and preserved
semantics. No browser runtime, Tauri wrapper, or duplicate C++ Linux window UI.
Do not conflate a functioning media bridge with Windows feature parity.

Crossing the bridge must preserve error identity, thread ownership, bounded
decode buffers, cancellation, Linux case-sensitive path identity and metadata
write/recovery semantics. Domain state must not be invented independently in
both the Rust UI and C++ index: explicitly identify the authoritative owner.

### Migration correspondence (source → native)

| Windows source/contract | Rust owner | Current migration evidence |
| --- | --- | --- |
| Items folder/media list, focus and multi-select (Design: Selection) | `rust/diffractor-ui/src/main.rs` and `selection.rs` | SOURCE: row and virtualized grid/list selection, plain/Ctrl/Shift/Ctrl+Shift, Ctrl+A, summary read-only; directories still navigate rather than enter a batch selection |
| Navigation and history (Design: Navigation) | `history.rs`, Rust folder worker | SOURCE: bounded Back/Forward, commit after successful enumeration; query and selection restoration incomplete |
| Image view and zoom (Zoom) | `zoom.rs`, `main.rs`, C++ libfiles decode bridge | SOURCE: original 19-stop ladder, exact source 1:1, source-based anchor/center/pan; navigator, linked comparison zoom and progressive tiles remain |
| Original media index and query parser | `diffractor-core`, `platform_linux_index.cpp` | Existing worker, folder-scoped recursive search, local SQLite cache; managed multi-root collections missing |
| Metadata/sidecars/edit/ratings | `diffractor-core`, future Rust task views and reviewed write plans | Embedded metadata read only; XMP sidecars and writes not connected |
| Audio/video playback | `video.rs` and original FFmpeg preview | CPU-copy libVLC playback + timeline; native video feature parity/hardware acceleration incomplete |
| Fullscreen, comparison, import/sync and batch file operations | Rust `main.rs` native view and future task modules | SOURCE: F11 fullscreen, Esc zoom-before-exit, two-photo source-ordered comparison with decoded frames; import/sync/batch file writes still not ported |
| Localization and original visual identity | `locale.rs` and native style components | Typed Korean/English tokens, graphite theme; original iconography/geometry and broader language coverage incomplete |

### Functional phases

| Phase | Native deliverable and dependency | Acceptance gate |
| --- | --- | --- |
| R1 — Input and selection fidelity | Establish Rust focus/anchor/selected-target model, genuine row selection, keyboard modifiers, history, responsive status, i18n and basic media view | Source tests for Ctrl/Shift/range/filter/failed navigation; local compile and actual pointer/keyboard GUI validation required for VERIFIED |
| R2 — Original Items + Fullscreen | Native thumbnail grid/details, scrolling and focus continuity, original column/panel geometry, source-anchored zoom ladder/navigator, full-window mode, two-media comparison | Side-by-side Windows/Rust reference screenshots and interaction path coverage including awkward sizes and DPI |
| R3 — Search and collections | Managed multi-root collection state, scope/filter/group/sort semantics, original indexed search parser, duplicate/presence/related items through Rust APIs | Reopen index without loss; query/selection retention and stale-worker tests; no UI-thread I/O |
| R4 — Metadata and guided operations | Tags, ratings, sidecars, editing, rename/import/sync/convert with review/skip/collision/recovery semantics | Real files and sidecars roundtrip, cancellation and overwrite collision cases; no unreviewed destructive effect |
| R5 — Desktop parity and distribution | Native dialogs, clipboard/drop, associations, media playback/colour fidelity, Arch package, runtime/visual/performance polish | X11/Wayland launch, Korean typography/IME, GPU failure/fallback, bounded memory, observed Arch QA and license/relinking review |

**Critical path:** R1 → R2 → R3 → R4 → R5. Small adjacent
features may be implemented earlier when they complete an end-to-end slice.
After each phase, consolidate its modules without speculatively mirroring the
Windows class hierarchy. Native UI state and UX decisions are owned in Rust.
Preserve original semantics unless a user-approved difference is documented.

### 2026-11-02 migration feedback and source slices

- **R1** source: native selection owns the complete batch target; the single
  displayed-media handle is not a command target. Ctrl+Shift additive range
  and native folder-history shortcuts are wired. Two selections no longer
  imply a safe editing command.
- **R2 (partial)** source: selectable virtualized thumbnail grid/list, F11
  fullscreen with Esc leaving magnification first, and image/image comparison
  in browse order using a bounded dual decode request. The renderer uses two
  fitted panes, not the original linked zoom/metadata comparison contract yet.
- **Important counterexample:** a lexical selected-file set cannot determine
  the left/right presentation order. The visual order comes from the current
  browsing scope; a `comparison_paths` unit case now asserts this. General
  migration feedback lives in `SKILLS/rustify` rather than duplicating the
  procedure here.
- Linux Release CI was extended to run Rust-core/frontend tests after the C++
  build. **No executed workflow success or Arch GUI evidence is available.**
  The R1 and R2 gates therefore remain unverified.

### Source and evidence pin

- Code checkpoint: `e7d80303dcd58e7fedda0e180b06baa42216986c`
  (`arch-linux-build`, 2026-11-02). Includes pure Rust selection,
  source-ordered paired comparison, virtualized grid/list, native fullscreen,
  history and Korean/English tokens. The original C++ media bridge is retained.
- Evidence collected here: repository source inspection, cross-file token
  parity, source tests, and **observed GitHub Actions `cargo check` success**
  for `9051ab2`. This validates Rust types/borrows, not linker success,
  runtime behavior, Arch GUI or pixel-parity measurements.
- The [Windows reference screenshot](../screenshot.webp) and the original
  [Design](design.md), [Zoom](zoom.md) and [Selection Controls](selection-controls.md)
  govern the next visual alignment pass. Default egui colours, thumbnails and
  panel geometry are not evidence of visual parity.
- Remaining blocker for **verification**, not for source-authoring: execute
  the checked-in Rust CI/local Cargo tests and inspect actual KDE/Wayland
  output. If checks fail, repair root causes before extending dependent UI
  features or promoting the draft PR.

### 2026-11-02 source migration slice — zoom, browser and fast type feedback

- `zoom.rs` now uses the 19 percentages and source-space normalized center
  from Windows `src/model_zoom.h`. `probe_image` on the existing media worker
  supplies source dimensions independent of the preview raster. Fit, pointer
  stepping, 1:1, pan clamps, and texture-swap geometry have checked-in tests.
  GUI integration retains original decoded-frame generation checks.
- Windows input paths added in native egui: Enter fullscreen, F11, K details,
  Space media play/pause, Ctrl+Left/Right extend selected set, Ctrl+Space
  original-size zoom and 0 Fit. Text editor focus suppresses navigation keys.
  Ctrl-wheel at Fit is intentionally not consumed by media zoom.
- `browser_files.rs` now owns Linux case-sensitive file recognition, workers'
  folder enumeration and open-target validation. A bounded max-heap chooses the
  deterministic sorted first 10,000 items rather than the arbitrary first
  10,000 yielded by `read_dir`; folders sort before files.
- A separate `.github/workflows/rust-native-ui.yml` runs `cargo check --locked
  --all-targets` for both Rust crates without waiting for the C++ native
  linker. `.github/workflows/linux.yml` remains the full native test gate.
- **Validation**: GitHub Actions Rust Native UI
  [run 41467435869](https://github.com/tzerokyi/diffractor/actions/runs/41467435869)
  passed both crates' `cargo check --locked --all-targets` on source commit
  `9051ab2`. This proves type/borrow correctness for that revision,
  including the new Rust zoom and bounded browser modules; it does not
  execute their tests or link the C++ media bridge. The first check
  [run 41467144508](https://github.com/tzerokyi/diffractor/actions/runs/41467144508)
  failed on egui 0.37 Context API changes, missing test import and
  `Option<PathBuf>`/borrowed-path test mismatch; all were corrected before
  the passing revision. The dedicated runner now also builds and executes
  isolated `selection`, `history` and `browser_files` unit tests; their
  result is still pending. No Arch binary or Wayland GUI parity is asserted.

### Current source checkpoint / verification integrity

- Development branch: `arch-linux-build`, Draft
  [PR #1](https://github.com/tzerokyi/diffractor/pull/1). The Windows
  `master` C++ app remains intact.
- Read-only media selection uses a source-derived Rust `Selection` state
  (focus, anchor, selected file targets) and handles plain/Ctrl/Shift/Ctrl+Shift,
  keyboard ranges and Ctrl+A. Selection summary counts media types without
  pretending that batch editing is available. A direct file-manager launch
  can select a file outside the 10,000-entry list cap.
- Existing code still uses the presentation-specific `Browser.selected`
  path for a single displayed item. **It is not the selected batch target.**
  The `Selection` model owns selected paths; action targeting must read from
  it and must not infer selected files from the preview image.
- Source-level regression functions cover selection grammar, comparison pair
  ordering and type eligibility, virtual grid columns, navigation and image
  orientation; **they have not been executed here**. Tool access in this
  session permits source review and commits but not a local Rust/Arch build.
- R1 Rust type-check baseline is achieved at `9051ab2`, but no VERIFIED
  end-to-end L1/R1 runtime baseline is claimed. Static inspection, Cargo
  type-check, unit tests, C++ bridge link, native GUI interactions, screenshot
  parity and performance are distinct evidence categories.

Next implementation entry: run the Rust-only type-check CI (and the complete
Linux bridge test when available), repair compilation defects and then verify
selection/grid/fullscreen/zoom/comparison on Arch/KDE. Follow with the source
navigator, linked two-pane zoom, original Items layout and comparison metadata.
Do not implement destructive operations until the reviewed-action backend exists.

For the native preview's current compile and interactive check, after building
the existing Linux bridge as above:

```bash
LD_LIBRARY_PATH="$PWD/tmp/linux-build${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  cargo test --manifest-path rust/diffractor-ui/Cargo.toml --locked
LD_LIBRARY_PATH="$PWD/tmp/linux-build${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  cargo run --manifest-path rust/diffractor-ui/Cargo.toml --locked -- "$HOME/Pictures"
```

For feedback of reusable scope, update
[SKILLS rustify](https://github.com/tzerokyi/SKILLS/blob/main/skills/rustify/SKILL.md)
or its [native UI migration reference](https://github.com/tzerokyi/SKILLS/blob/main/skills/rustify/references/native-ui-ports.md).
This file remains the **single Diffractor-specific** current-state/migration ledger.
