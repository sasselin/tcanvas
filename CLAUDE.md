# CLAUDE.md

Guide for Claude sessions working on **tcanvas**, an open-source native Windows app (also built
as a native Linux app) that shows WSL terminals as cards on an infinite canvas (egui/eframe +
wgpu, `alacritty_terminal` for emulation). Every card is a tmux session inside WSL; a Linux companion binary,
`tcanvas-agent`, watches those sessions and talks to the app over stdin/stdout.

## Working with the maintainer

- The maintainer writes in French: **answer them in French**, but write **everything that
  lands in the repo in English** (see "Language rule").
- Commit or push **only when asked**. Commit messages are **Conventional Commits** (CHANGELOG
  and versions are generated from them): `<type>: <Capitalized English subject>`, type in
  `feat fix perf refactor docs test style build ci chore revert` (optional scope:
  `fix(agent): ...`, breaking: `feat!:`), blank line, then `- ` bullets describing the change per
  area. `scripts/git-hooks/commit-msg` checks it (`git config core.hooksPath scripts/git-hooks`).
  Older pushed commits (some in French, no type) stay as they are; never rewrite history.
- "Done" means tested and working on both Linux and Windows (see "Verification").

## Workspace layout

Cargo workspace (`Cargo.toml`, edition 2024, resolver 3):

- `crates/protocol` - shared message types (`AgentCmd`, `AgentEvent`, `TermStatus`,
  `Problem`, ...), JSON lines via `encode`/`decode`. Also owns the tmux socket name and the
  session prefix (`SESSION_PREFIX = "tc_"`).
- `crates/agent` - binary `tcanvas-agent` (runs in WSL). Subcommands:
  `serve` (default), `install-hooks`, `uninstall-hooks`, `hook <event>`, `hooks [bash|zsh]`,
  `kill <session>`. Code in `src/features/*.rs` (serve loop, tmux access, status from tmux +
  /proc, shell hooks for bash and zsh following `$SHELL`, coding agent hooks in
  `agent_hooks.rs`, git changes, session kill).
- Coding agents (Claude Code, Codex, Gemini CLI, Copilot CLI, Qwen Code) report "working" /
  "waiting" through their own hooks: `install-hooks` merges `tcanvas-agent hook <event>` into
  their settings, the hook sets the pane option `@tc_agent` (state + its ancestor pids), and
  the status loop trusts it only while one of those pids is in the foreground. Without
  hooks, a known agent CLI quiet for 5 s counts as waiting.
- `crates/app` - package `tcanvas` (lib + Windows exe). `src/lib.rs` declares the modules;
  `build.rs` (Windows target only) embeds `assets/icon.ico` (needs mingw-w64 `windres`) and
  builds `tcanvas-agent` for `x86_64-unknown-linux-musl` (static, any distro) in its own target
  dir under `OUT_DIR`. `agent_link/install.rs` embeds it and, on startup, copies it to
  `~/.local/bin/tcanvas-agent` in WSL when `~/.local/share/tcanvas/agent.id` holds another
  build hash: the exe alone is the whole distribution. `TCANVAS_AGENT` skips this.
- `scripts/` - `deploy.sh`, `watch.sh`, `test-windows.sh`, `screenshots.sh`, `git-hooks/`.
- `installer/` - `tcanvas.nsi` (Windows installer, NSIS, built on Linux with `makensis`) and
  `install.sh` (Linux archive installer, into `~/.local`).
- `.github/workflows/` - `ci.yml` (tests, clippy, fmt, Windows cross-build, PR title check) and
  `release.yml` (release-plz, then builds attached to the GitHub release; see "Releases").
- `docs/screenshots/` - README images, generated (see "Screenshots").
- `assets/` - icons (SVG/PNG/ICO).

## App architecture: vertical slices

- `crates/app/src/features/<name>/` - one folder per feature (canvas, terminal_card, wires,
  projects, changes, filters, jump, spaces, notifications, persistence, agent_link, updates, ...).
  A slice owns its state, UI, logic and its unit tests.
- `crates/app/src/shell/` - shared plumbing only: `App`/`Config` (`mod.rs`, global
  shortcuts), `world.rs` (shared data read by every slice), `event.rs` (event bus: the
  only way a slice acts on another), `i18n.rs`, `theme.rs`, `fonts.rs`, `widgets.rs`,
  `launch.rs` (spawning `tcanvas-agent` through `wsl.exe`).
- New feature -> new slice folder + `pub mod` line in `lib.rs`. Do not grow `shell/` with
  feature logic.
- Keep logic **pure and testable outside egui** (e.g. `terminal_card/input.rs` maps keys to
  bytes with no UI); the egui code just wires inputs to those functions.
- The agent sends **typed values, not human text**: errors are `protocol::Problem` variants
  and the app translates them (`Texts::problem`). Never put UI sentences in the protocol.

## i18n

- All UI text lives in the `Texts` struct in `crates/app/src/shell/i18n.rs`, with two
  tables: `EN` and `FR`. Adding a field forces both tables to be filled (it will not
  compile otherwise). Never hard-code UI strings in slices; use `t().<field>`.
- Language: `TCANVAS_LANG=fr|en` overrides; otherwise the OS display language via
  `sys-locale` (Windows user language, `LANG` on Linux). Any `fr*` locale gives French,
  everything else falls back to English. Stored per thread so each test can pick one.
- Tests: `crates/app/tests/ui.rs` builds its harness with `Lang::Fr` by default and has at
  least one test using `Lang::En` (`harness_in`); inline slice tests call
  `i18n::set(Lang::Fr)` before asserting on texts. Keep that pattern: assert on FR strings,
  plus at least one English check for any new screen.

## Language rule (enforced)

Everything in the repo is English: code, identifiers, comments, doc comments, README,
tests, scripts, SVG comments, commit messages. **Sole exceptions:** the `pub const FR: Texts`
table in `shell/i18n.rs`, and string literals in test code asserting those French strings.
`crates/app/tests/english_only.rs` scans the root docs and TOML files and every
`.rs/.sh/.svg/.toml/.md/.yml/.nsi/.desktop` under `crates/`, `scripts/`, `assets/`, `installer/`,
`.github/`; it fails on accented letters
or common French words (its word list includes short words, so check comments and examples).

## Tests

Every slice ships with automated tests; a feature is not finished without them.

- **Unit tests**: inline `#[cfg(test)] mod tests` next to the pure logic (input mapping,
  camera, layout, persistence, protocol round trips, agent parsing, ...).
- **Agent integration tests** (`crates/agent/tests/live_*.rs`): drive a real tmux server on
  a throwaway socket via `TCANVAS_TMUX_SOCKET`.
- **Headless UI tests** (`crates/app/tests/ui.rs`): `egui_kittest` harness around the real
  `App`, with launcher and agent disabled through `Config` and a temp config dir.
- **End to end** (`crates/app/tests/e2e_tmux.rs`): real PTY + real tmux (throwaway socket) +
  real `tcanvas-agent`. On Linux it builds the agent itself; on Windows it needs
  `TCANVAS_E2E_AGENT` (set by `scripts/test-windows.sh`) and is skipped without it.
- `crates/app/tests/english_only.rs`: the language rule above.

## Verification (before saying "done")

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --check
cargo build --release --target x86_64-pc-windows-gnu -p tcanvas   # Windows cross-build
./scripts/test-windows.sh   # suite compiled for Windows: real ConPTY + wsl.exe
./scripts/screenshots.sh    # only if the UI looks different: regenerate the README images
```

- Windows builds need `rustup target add x86_64-pc-windows-gnu x86_64-unknown-linux-musl` and
  `mingw-w64`; `test-windows.sh` extra args go to `cargo test`.
- The Windows e2e test can occasionally exceed its timeout: rerun it before concluding it
  fails.
- Formatting: `rustfmt.toml` keeps the long-line style (`max_width = 200`); run `cargo fmt`
  before committing, CI fails on `cargo fmt --check`.

## Releases

- Never bump versions, tag or edit `CHANGELOG.md` by hand. release-plz (`release-plz.toml`) keeps
  a `chore: Release vX.Y.Z` PR up to date on every push to `main`; merging it tags `vX.Y.Z`
  and creates the GitHub release, then `release.yml` attaches `tcanvas.exe`,
  `tcanvas-vX.Y.Z-setup-x64.exe`, `tcanvas-vX.Y.Z-linux-x86_64.tar.gz`, `SHA256SUMS` and build
  attestations, and updates winget (`sasselin.tcanvas`) when `vars.WINGET_ENABLED` is set.
- The whole workspace shares one version (`version.workspace`), released as `tcanvas`.
- The app updates itself from these releases (`features/updates`): it looks for `tcanvas.exe`,
  `tcanvas-vX.Y.Z-linux-x86_64.tar.gz` and `SHA256SUMS`, shows the release body (the CHANGELOG
  section) before updating and the embedded `CHANGELOG.md` after. Renaming those assets or
  changing the archive layout breaks updates for every installed copy.
- Native Linux: same app, `cfg(not(windows))` paths run tmux and the agent directly (agent next
  to the exe or in the `PATH`), notifications through D-Bus (`notify-rust`). Texts that mention
  WSL have a Linux variant (`Texts::agent_missing_help`, `Texts::folder_path_hint`).

## Screenshots

- The README images (`docs/screenshots/*.png`) are **generated**, never hand-made:
  `crates/app/tests/screenshots.rs` renders the real `App` headless with wgpu, with canned
  terminal output (`TerminalCards::show_canned`, no PTY) and agent events dispatched by hand.
  The scenes are `#[ignore]`d tests; `./scripts/screenshots.sh` runs them.
- **Keep them current**: any change that alters what the UI looks like (layout, colors, texts,
  new panel...) reruns `./scripts/screenshots.sh`; look at the PNGs, then commit them with the
  change. A notable new screen gets its own scene and a README image.
- `demo.gif` (top of the README) comes from the `demo` scene: it writes frames to
  `target/demo-frames/` and `screenshots.sh` assembles them with `ffmpeg` (12 fps, own palette).
  Keep it short and light (a few hundred KB).
- Scenes use the English UI and fake but realistic content (`/home/me/...`, no real data).
- The WSL GPU driver crashes in headless rendering: the script uses Mesa's lavapipe
  (`VK_ICD_FILENAMES`, needs `mesa-vulkan-drivers`).
- The harness loads Windows' Segoe UI Symbol from `/mnt/c/Windows/Fonts` (the app's fallback on
  Windows): Linux fonts lack glyphs Claude Code draws (`⎿`, `⏵`), and a missing glyph fails the
  render.

## Deploy and dev loop

- `./scripts/deploy.sh` - builds first (a failed build leaves the running app untouched),
  copies `tcanvas.exe` (which updates the agent in WSL on startup) to
  `%USERPROFILE%\tcanvas`, stops and restarts the app. tmux terminals survive the restart.
  `--no-start` skips relaunching.
- `./scripts/watch.sh` - dev mode: redeploys on every source change under `crates/`,
  `scripts/`, `Cargo.*`.

## tmux safety

- The maintainer runs a personal tmux on the **`default` socket: never touch it** (no bare
  `tmux` commands, no `kill-server`, no listing/killing its sessions).
- tcanvas uses its own socket: `protocol::TMUX_SOCKET = "tcanvas"` (`tmux -L tcanvas`),
  overridable with `TCANVAS_TMUX_SOCKET`. Always go through `protocol::tmux_socket()`.
- Tests must use a throwaway socket (e.g. `tcanvas-e2e-<pid>`) via `TCANVAS_TMUX_SOCKET`
  and clean it up; never run tests against `tcanvas` or `default`.
- Windows env vars do not cross `wsl.exe`: `shell/launch.rs` forwards the socket explicitly.

## UX decisions to preserve

- **Ctrl+C always sends ^C** to the terminal, never copy (egui's `Event::Copy` becomes
  `0x03`). Plain Ctrl+V pastes through egui's `Event::Paste` (bracketed when enabled).
- Copy/paste: **Ctrl+Shift+C** (selection is already copied by tmux on mouse release; must
  not send ^C) and **Ctrl+Shift+V** / **right-click** to paste (`terminal_card/input.rs`
  `clip_action`, `terminal_card/clipboard.rs` via `arboard`).
- **Alt+right-click** goes to tmux (its menu); wires link **groups**, not terminals: **Alt+drag**
  from a group's label onto another group (Alt+click in a card goes to the terminal);
  Shift+click belongs to the hand tool (pans the view), not the terminal.
- Global shortcuts are Ctrl+Shift+<key> (`shortcut_of` in `shell/mod.rs`); digits use the
  physical key so they work on AZERTY layouts.
