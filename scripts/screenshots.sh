#!/usr/bin/env bash
# Regenerates the README screenshots (docs/screenshots/*.png) from crates/app/tests/screenshots.rs:
# the real app rendered headless with wgpu, fed with canned terminal output and agent events.
# Run it after any change that alters what the UI looks like, then commit the PNGs.
set -euo pipefail
cd "$(dirname "$0")/.."

# The WSL GPU driver (dzn) crashes in headless rendering: use Mesa's software Vulkan (lavapipe)
# when it is installed (apt install mesa-vulkan-drivers).
lavapipe=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
if [[ -z "${VK_ICD_FILENAMES:-}" && -f "$lavapipe" ]]; then
    export VK_ICD_FILENAMES="$lavapipe" WGPU_BACKEND=vulkan
fi

cargo test -p tcanvas --test screenshots -- --ignored --test-threads=1 "$@"
git status --short docs/screenshots
