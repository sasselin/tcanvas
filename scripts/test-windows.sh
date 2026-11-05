#!/bin/sh
# Run, from WSL, the whole test suite built for Windows (real ConPTY + wsl.exe).
# Requirements: rustup target add x86_64-pc-windows-gnu x86_64-unknown-linux-musl; sudo apt install mingw-w64
set -eu
cd "$(dirname "$0")/.."
cargo build -q -p tcanvas-agent
export TCANVAS_E2E_AGENT="$PWD/target/debug/tcanvas-agent"
export WSLENV="TCANVAS_E2E_AGENT${WSLENV:+:$WSLENV}"
cargo test --target x86_64-pc-windows-gnu -p protocol -p tcanvas "$@"
