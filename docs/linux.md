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
Still-image previews support Fit, zoom buttons, Ctrl+wheel zoom anchored at the pointer, and drag
panning. Zoom requests a full-resolution decode on the worker, within the bridge's 256 MiB pixel
limit and the graphics device's maximum texture size; oversized or unsupported images retain their existing preview. This is a preliminary subset of
the original zoom model, without its navigator, zoom ladder, or progressive tile decoding.
The preview accepts a media file or folder as its command-line argument and advertises supported
media types to desktop file managers. A file argument opens its containing folder and selects the
file; resolving the path and reading the folder happen on the worker.

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
