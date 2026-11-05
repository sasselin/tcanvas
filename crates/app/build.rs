//! Windows build only:
//! - embeds the icon (`assets/icon.ico`) in the executable's resources, so it shows up in
//!   Explorer and the taskbar. When cross-compiling from WSL, `embed-resource` uses
//!   `x86_64-w64-mingw32-windres` (mingw-w64 package);
//! - builds `tcanvas-agent` as a static Linux binary (musl target) and drops it in `OUT_DIR`,
//!   where `agent_link::install` embeds it: the exe installs its own agent in WSL, so sharing
//!   the exe alone is enough. `TCANVAS_AGENT_BIN` points to a prebuilt agent instead.

use std::{env, fs, path::PathBuf, process::Command};

const AGENT_TARGET: &str = "x86_64-unknown-linux-musl";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ico = manifest_dir.join("../../assets/icon.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    embed_agent(&manifest_dir, &out_dir);

    let ico = ico.canonicalize().expect("assets/icon.ico not found");
    let ico = ico.to_string_lossy().replace('\\', "/");
    let rc = out_dir.join("tcanvas.rc");
    fs::write(&rc, format!("1 ICON \"{ico}\"\n")).expect("writing tcanvas.rc");
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().expect("compiling Windows resources");
}

/// Writes `OUT_DIR/tcanvas-agent` (the binary) and `OUT_DIR/tcanvas-agent.id` (its content hash).
fn embed_agent(manifest_dir: &std::path::Path, out_dir: &std::path::Path) {
    println!("cargo:rerun-if-env-changed=TCANVAS_AGENT_BIN");
    let bin = match env::var("TCANVAS_AGENT_BIN") {
        Ok(path) => {
            println!("cargo:rerun-if-changed={path}");
            PathBuf::from(path)
        }
        Err(_) => build_agent(manifest_dir, out_dir),
    };
    let bytes = fs::read(&bin).unwrap_or_else(|e| panic!("reading the agent {}: {e}", bin.display()));
    fs::write(out_dir.join("tcanvas-agent"), &bytes).expect("copying the agent");
    fs::write(out_dir.join("tcanvas-agent.id"), format!("{:016x}", fnv1a(&bytes))).expect("writing the agent id");
}

/// `cargo build` of the agent for musl, in its own target dir (the outer build holds the lock
/// on the workspace one). Static, so it runs on any WSL distro whatever its glibc.
fn build_agent(manifest_dir: &std::path::Path, out_dir: &std::path::Path) -> PathBuf {
    let root = manifest_dir.join("../..");
    for watched in ["crates/agent", "crates/protocol", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", root.join(watched).display());
    }
    let target_dir = out_dir.join("agent-target");
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(&root).args(["build", "--release", "-p", "tcanvas-agent", "--target", AGENT_TARGET, "--target-dir"]).arg(&target_dir);
    // Flags meant for the outer (Windows) build must not leak into the agent's.
    for var in ["CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "CARGO_BUILD_TARGET", "CARGO_TARGET_DIR", "CARGO_BUILD_RUSTFLAGS"] {
        cmd.env_remove(var);
    }
    let status = cmd.status().expect("launching cargo for tcanvas-agent");
    assert!(status.success(), "building tcanvas-agent for {AGENT_TARGET} failed (rustup target add {AGENT_TARGET})");
    target_dir.join(AGENT_TARGET).join("release").join("tcanvas-agent")
}

/// Stable 64-bit FNV-1a: identifies the embedded agent build.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3))
}
