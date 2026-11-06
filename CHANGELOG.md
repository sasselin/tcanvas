# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
