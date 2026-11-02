use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=DIFFRACTOR_BRIDGE_DIR");
    let default =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../tmp/linux-build");
    let dir = env::var_os("DIFFRACTOR_BRIDGE_DIR")
        .map(PathBuf::from)
        .unwrap_or(default);
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=dylib=diffractor_linux_bridge");
}
