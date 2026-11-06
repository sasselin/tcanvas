# Contributing to tcanvas

Thanks for helping! Bug reports, ideas and pull requests are welcome.

## Ground rules

- **English only** in the repository: code, comments, docs, tests, commit messages. The sole
  exception is the French UI table (`pub const FR: Texts` in `crates/app/src/shell/i18n.rs`) and
  the test strings that check it. `crates/app/tests/english_only.rs` enforces it.
- **UI text** goes through `Texts` in `shell/i18n.rs`, filled in both `EN` and `FR`.
- **Vertical slices**: a feature lives in `crates/app/src/features/<name>/` with its state, UI,
  logic and tests. `shell/` is shared plumbing only.
- **Tests come with every change**: unit tests next to the logic, plus UI (`egui_kittest`) or
  end-to-end tests when behavior changes.
- **Never touch the `default` tmux socket.** tcanvas uses `tmux -L tcanvas`; tests use a
  throwaway socket through `TCANVAS_TMUX_SOCKET`.

`CLAUDE.md` describes the architecture and conventions in more detail.

## Commit messages

Releases, version numbers and `CHANGELOG.md` are generated from commit subjects
([Conventional Commits](https://www.conventionalcommits.org/)), so each subject reads:

```
<type>: <Subject starting with a capital letter>

- Optional bullets describing the change per area
```

| Type | Use for |
|---|---|
| `feat` | something users can see or do |
| `fix` | a bug fix |
| `perf` | a speedup |
| `refactor`, `docs`, `test`, `style`, `build`, `ci`, `chore`, `revert` | everything else |

A breaking change uses `feat!:` (or a `BREAKING CHANGE:` footer). Before 1.0, a breaking change
bumps the minor version and anything else the patch version; from 1.0 on, `feat` bumps the minor
version too. A scope is optional:
`fix(agent): Keep the socket on restart`. Pull requests are squash-merged, so the **PR title**
must follow the same format (CI checks it). To check your local commits:

```sh
git config core.hooksPath scripts/git-hooks
```

## Before opening a pull request

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo build --release --target x86_64-pc-windows-gnu -p tcanvas   # Windows cross-build
```

`./scripts/test-windows.sh` runs the suite on Windows (real ConPTY and `wsl.exe`) from WSL. If
the UI looks different, regenerate the README images with `./scripts/screenshots.sh`.

## Releases (maintainers)

Nothing to do by hand: on every push to `main`, release-plz keeps a `chore: Release vX.Y.Z` pull
request up to date with the version bump and the new `CHANGELOG.md` section. Merging it tags the
release, creates the GitHub release, builds the Windows installer, the portable exe and the
Linux archive, attaches them with their checksums and build attestations, and updates winget.
