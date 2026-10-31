#!/bin/sh
# Lance depuis WSL toute la suite de tests compilée pour Windows (ConPTY + wsl.exe réels).
# Prérequis : rustup target add x86_64-pc-windows-gnu ; sudo apt install mingw-w64
set -eu
cd "$(dirname "$0")/.."
cargo build -q -p tcanvas-agent
export TCANVAS_E2E_AGENT="$PWD/target/debug/tcanvas-agent"
export WSLENV="TCANVAS_E2E_AGENT${WSLENV:+:$WSLENV}"
cargo test --target x86_64-pc-windows-gnu -p protocol -p tcanvas "$@"
