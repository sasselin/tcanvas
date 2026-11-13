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

# The animated demo: the frames rendered by the `demo` scene, assembled with ffmpeg
# (apt install ffmpeg) into a GIF with its own palette, at 12 frames per second.
frames=target/demo-frames
if [[ -f "$frames/0000.png" && "$frames/0000.png" -nt docs/screenshots/demo.gif ]]; then
    ffmpeg -loglevel error -y -framerate 12 -i "$frames/%04d.png" \
        -vf "split[a][b];[a]palettegen=max_colors=256:stats_mode=diff[p];[b][p]paletteuse=dither=none:diff_mode=rectangle" \
        -loop 0 docs/screenshots/demo.gif
    echo "wrote docs/screenshots/demo.gif ($(du -h docs/screenshots/demo.gif | cut -f1))"
fi
git status --short docs/screenshots
