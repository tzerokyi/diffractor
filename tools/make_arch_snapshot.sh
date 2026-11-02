#!/usr/bin/env bash
# Build an Arch package from the current tree; reuse the managed build on subsequent runs.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
mkdir -p "$repo/tmp"
exec 9>"$repo/tmp/arch-package.lock"
flock -n 9 || { printf '%s\n' 'Another package build is using this worktree.' >&2; exit 1; }
marker="$repo/tmp/arch-package-current"
stage=''
if [[ ${DIFFRACTOR_ARCH_CLEAN:-0} != 1 && -f "$marker" ]]; then
    IFS= read -r stage < "$marker" || true
    case "$stage" in "$repo"/tmp/arch-package.*) ;; *) stage='' ;; esac
    if [[ ! -d "$stage" || -L "$stage" ]]; then stage=''; fi
fi
if [[ -z "$stage" ]]; then
    stage=$(mktemp -d "$repo/tmp/arch-package.XXXXXXXX")
    printf '%s\n' "$stage" > "$marker"
fi
archive="$stage/diffractor-0.1.0.tar.zst"
# The tree contains LFS fixtures used by check(); refuse an incomplete checkout.
if command -v rg >/dev/null; then
    lfs_check=(rg -l '^version https://git-lfs.github.com/spec/v1$' "$repo/exe")
else
    lfs_check=(grep -rIlx 'version https://git-lfs.github.com/spec/v1' "$repo/exe")
fi
if "${lfs_check[@]}" >/dev/null; then
    printf '%s\n' 'Git LFS files are missing; run git lfs pull first.' >&2
    exit 1
else
    status=$?
    if [[ $status -ne 1 ]]; then
        printf '%s\n' 'Could not verify Git LFS fixtures.' >&2
        exit "$status"
    fi
fi
tar --zstd -cf "$archive.new" --transform='s|^\.|diffractor|' \
    --exclude='./.git' --exclude='./tmp' --exclude='./rust/*/target' --exclude='./.taste-skill' \
    -C "$repo" .
mv -f -- "$archive.new" "$archive"
checksum=$(sha256sum "$archive")
checksum=${checksum%% *}
sed "s/@SNAPSHOT_SHA256@/$checksum/" "$repo/packaging/arch/PKGBUILD.in" > "$stage/PKGBUILD"
printf 'Package build directory: %s\n' "$stage"

args=()
work="$stage/src/diffractor"
if [[ -f "$work/tmp/linux-build/CMakeCache.txt" ]]; then
    # Compare against the exact archive. Preserve timestamps and outputs for unchanged files;
    # remove vanished source files only inside this script's managed source directory.
    incoming="$stage/incoming"
    mkdir -p "$incoming"
    find "$incoming" -mindepth 1 -depth -delete
    tar -xf "$archive" -C "$incoming"
    source_tree="$incoming/diffractor"
    while IFS= read -r -d '' old; do
        relative=${old#"$work/"}
        if [[ ! -e "$source_tree/$relative" && ! -L "$source_tree/$relative" ]]; then
            unlink -- "$old"
        fi
    done < <(find "$work" -mindepth 1 \( -path "$work/tmp" -o -path "$work/rust/*/target" \) -prune -o \( -type f -o -type l \) -print0)
    while IFS= read -r -d '' source; do
        relative=${source#"$source_tree/"}
        destination="$work/$relative"
        mkdir -p -- "$(dirname -- "$destination")"
        if [[ -L "$source" ]]; then
            if [[ ! -L "$destination" || $(readlink -- "$source") != "$(readlink -- "$destination")" ]]; then
                if [[ -e "$destination" || -L "$destination" ]]; then unlink -- "$destination"; fi
                cp -a -- "$source" "$destination"
            fi
        elif [[ ! -f "$destination" || -L "$destination" ]] || ! cmp -s -- "$source" "$destination"; then
            if [[ -L "$destination" ]]; then unlink -- "$destination"; fi
            cp -p --remove-destination -- "$source" "$destination"
            touch -m -- "$destination"
        elif [[ $(stat -c %a -- "$source") != "$(stat -c %a -- "$destination")" ]]; then
            chmod --reference="$source" -- "$destination"
        fi
    done < <(find "$source_tree" -mindepth 1 \( -type f -o -type l \) -print0)
    args+=(--noextract --force)
    printf '%s\n' 'Reusing CMake and Cargo build outputs.'
fi
cd "$stage"
makepkg "${args[@]}" "$@"
