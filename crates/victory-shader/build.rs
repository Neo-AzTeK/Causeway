//! Hashes everything a translation is compiled from into
//! `VICTORY_SHADER_BUILD_HASH` (`victory_shader::BUILD_HASH`): the crate's
//! source, the manifests and the lock file (dependency versions), the
//! compiler, and the target, features and profile it compiles with.

use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let mut files = Vec::new();
    list(&root, Path::new("src"), &mut files);
    // Sorted as text, the same on every file system.
    files.sort();
    // The lock file covers every dependency of the workspace, more than the
    // translator's: one that changes only costs translating again.
    files.extend(
        [
            "build.rs",
            "Cargo.toml",
            "../../Cargo.toml",
            "../../Cargo.lock",
        ]
        .map(str::to_owned),
    );
    let mut h = blake3::Hasher::new();
    for file in &files {
        let bytes = std::fs::read(root.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        field(&mut h, file.as_bytes());
        field(&mut h, &bytes);
        println!("cargo:rerun-if-changed={file}");
    }
    // A file added or removed.
    println!("cargo:rerun-if-changed=src");

    let rustc = std::env::var_os("RUSTC").expect("set by cargo");
    let version = std::process::Command::new(rustc)
        .arg("-vV")
        .output()
        .expect("rustc runs");
    assert!(version.status.success(), "rustc -vV failed");
    field(&mut h, &version.stdout);

    let mut vars: Vec<_> = std::env::vars()
        .filter(|(name, _)| {
            name.starts_with("CARGO_CFG_")
                || name.starts_with("CARGO_FEATURE_")
                || matches!(
                    name.as_str(),
                    "CARGO_ENCODED_RUSTFLAGS" | "OPT_LEVEL" | "PROFILE" | "TARGET"
                )
        })
        .collect();
    vars.sort();
    for (name, value) in &vars {
        field(&mut h, name.as_bytes());
        field(&mut h, value.as_bytes());
    }
    println!(
        "cargo:rustc-env=VICTORY_SHADER_BUILD_HASH={}",
        h.finalize().to_hex()
    );
    // The name VIctory's caches use for the same hash.
    println!(
        "cargo:rustc-env=VICTORY_SHADER_SOURCE_HASH={}",
        h.finalize().to_hex()
    );
}

/// The files under `dir`, relative to `root`, with `/` between folders.
fn list(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(root.join(dir)).expect("source folder") {
        let path = dir.join(entry.expect("source entry").file_name());
        if root.join(&path).is_dir() {
            list(root, &path, out);
        } else {
            out.push(path.to_str().expect("UTF-8 path").replace('\\', "/"));
        }
    }
}

/// Length first, so that no two lists of fields hash alike.
fn field(h: &mut blake3::Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}
