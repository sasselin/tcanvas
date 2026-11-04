//! Embarque l'icône (`assets/icon.ico`) dans les ressources de l'exécutable Windows,
//! pour qu'elle apparaisse dans l'Explorateur et la barre des tâches. Sans effet sur
//! les autres cibles. En compilation croisée depuis WSL, `embed-resource` utilise
//! `x86_64-w64-mingw32-windres` (paquet mingw-w64).

use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ico = manifest_dir.join("../../assets/icon.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let ico = ico.canonicalize().expect("assets/icon.ico not found");
    let ico = ico.to_string_lossy().replace('\\', "/");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let rc = out_dir.join("tcanvas.rc");
    fs::write(&rc, format!("1 ICON \"{ico}\"\n")).expect("writing tcanvas.rc");
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().expect("compiling Windows resources");
}
