# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.9](https://github.com/sasselin/tcanvas/compare/v0.1.8...v0.1.9) - 2026-11-14

### Added

- Add a tasks panel per project ([#50](https://github.com/sasselin/tcanvas/pull/50))
- Set up a new worktree from a recipe in the repository ([#48](https://github.com/sasselin/tcanvas/pull/48))
- Create a worktree from a group ([#47](https://github.com/sasselin/tcanvas/pull/47))
- Send text to a session from the app ([#46](https://github.com/sasselin/tcanvas/pull/46))
- Start a card with a command ([#45](https://github.com/sasselin/tcanvas/pull/45))
- Clean up merged worktrees and branches ([#42](https://github.com/sasselin/tcanvas/pull/42))
- Show a repository's worktrees and the branch of each card ([#36](https://github.com/sasselin/tcanvas/pull/36))
- Show the coding agent's last prompt on its card ([#34](https://github.com/sasselin/tcanvas/pull/34))

### Fixed

- *(agent)* Keep exit codes and durations right when the wall clock steps ([#43](https://github.com/sasselin/tcanvas/pull/43))

## [0.1.8](https://github.com/sasselin/tcanvas/compare/v0.1.7...v0.1.8) - 2026-11-12

### Fixed

- *(agent)* Survive wall clock steps and same-second output ([#37](https://github.com/sasselin/tcanvas/pull/37))

## [0.1.7](https://github.com/sasselin/tcanvas/compare/v0.1.6...v0.1.7) - 2026-11-08

### Added

- Show every version's changes from the version label

## [0.1.6](https://github.com/sasselin/tcanvas/compare/v0.1.5...v0.1.6) - 2026-11-08

### Fixed

- Keep group chips from running under the toolbar's right-hand widgets

## [0.1.5](https://github.com/sasselin/tcanvas/compare/v0.1.4...v0.1.5) - 2026-11-08

### Added

- Paste clipboard images into terminals as the path of a saved PNG

## [0.1.4](https://github.com/sasselin/tcanvas/compare/v0.1.3...v0.1.4) - 2026-11-07

### Fixed

- Stop alerting on the terminal the user is looking at

## [0.1.3](https://github.com/sasselin/tcanvas/compare/v0.1.2...v0.1.3) - 2026-11-07

### Added

- List every shortcut and mouse gesture in a Keyboard shortcuts window
- Show coding agents waiting for input through their hooks
- Check for updates on demand from the What's new dialog
- Sort group chips by name from their right-click menu
- Reorder group chips in the top bar by dragging them

### Other

- Animated demo, badges and an up-to-date README

## [0.1.2](https://github.com/sasselin/tcanvas/compare/v0.1.1...v0.1.2) - 2026-11-06

### Added

- Update the app from the toolbar, with release notes before and after

## [0.1.1](https://github.com/sasselin/tcanvas/compare/v0.1.0...v0.1.1) - 2026-11-06

### Added

- Show commits since the last release in development builds
- Show the running version in the toolbar and window title

## [0.1.0](https://github.com/sasselin/tcanvas/releases/tag/v0.1.0) - 2026-11-06

First public release.

### Added

- Infinite canvas of terminals: pan, zoom, minimap, camera flights, readable previews when zoomed out
- Every card is a tmux session: close the app and the terminals keep running
- Projects as spaces (`Ctrl+Shift+1`...`9`), groups that fit their terminals, a "free grid" option
- Groups linked to a folder: new terminals open there, live changed files, diffs and recent commits
- Terminal statuses (stalled, waiting for input, failed, finished) with notifications
- Status and text filters, "go to" palette (`Ctrl+Shift+P`)
- Wires between related groups, linked groups shown in a row under the label
- Clipboard like Windows Terminal, bash and zsh hooks installed from the status bar
- English and French interface, following the system language
- Windows: installer (per-user, Start menu, uninstaller), portable exe with the Linux agent embedded
- Native Linux app: archive with `install.sh`, desktop notifications
