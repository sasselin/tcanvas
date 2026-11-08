//! Release notes: the CHANGELOG written by release-plz (embedded in the app, read after an
//! update) and the release bodies (fetched before one), shown with a tiny Markdown subset.

use super::version;

/// The changelog of this build, for "What's new" after an update (no network needed).
pub const EMBEDDED: &str = include_str!("../../../../../CHANGELOG.md");

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub version: String,
    /// Markdown body, without the version heading.
    pub body: String,
}

/// The sections of versions in `(after, upto]`, newest first. `after == None`: only `upto`.
/// Headings look like `## [0.1.1](https://...) - 2026-11-06`; `[Unreleased]` is skipped.
pub fn sections_between(markdown: &str, after: Option<&str>, upto: &str) -> Vec<Section> {
    let (Some(upto), after) = (version::parse(upto), after.and_then(version::parse)) else { return Vec::new() };
    sections(markdown, |v| v <= upto && after.is_none_or(|a| v > a) && (after.is_some() || v == upto))
}

/// Every version up to `upto` (the running one), newest first: the whole history of this build.
pub fn history(markdown: &str, upto: &str) -> Vec<Section> {
    let Some(upto) = version::parse(upto) else { return Vec::new() };
    sections(markdown, |v| v <= upto)
}

/// The sections whose version satisfies `wanted`, newest first.
fn sections(markdown: &str, wanted: impl Fn((u32, u32, u32)) -> bool) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut current: Option<Section> = None;
    for line in markdown.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            out.extend(current.take());
            let v = heading.trim_start_matches('[').split([']', ' ']).next().unwrap_or_default();
            if version::parse(v).is_some_and(&wanted) {
                current = Some(Section { version: version::base(v), body: String::new() });
            }
        } else if let Some(s) = current.as_mut() {
            s.body.push_str(line);
            s.body.push('\n');
        }
    }
    out.extend(current);
    for s in &mut out {
        s.body = s.body.trim().to_string();
    }
    out.sort_by_key(|s| std::cmp::Reverse(version::parse(&s.version)));
    out
}

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Heading(String),
    Bullet(String),
    Text(String),
}

/// Markdown subset used by the changelog: `#` headings, `-`/`*` bullets, paragraphs. Inline
/// markup is flattened: `**bold**` and `` `code` `` lose their marks, `[text](url)` keeps the text.
pub fn lines(markdown: &str) -> Vec<Line> {
    markdown
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let t = l.trim_start();
            if t.starts_with('#') {
                Line::Heading(inline(t.trim_start_matches('#').trim()))
            } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
                Line::Bullet(inline(rest))
            } else {
                Line::Text(inline(t))
            }
        })
        .collect()
}

fn inline(s: &str) -> String {
    let s = s.replace("**", "").replace('`', "");
    // [text](url) → text
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        match after.find("](").and_then(|close| after[close + 2..].find(')').map(|end| (close, close + 2 + end))) {
            Some((close, end)) => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..close]);
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANGELOG: &str = "# Changelog\n\nIntro.\n\n## [Unreleased]\n\n- not yet\n\n## [0.2.0](https://x/compare/v0.1.1...v0.2.0) - 2026-12-01\n\n### Added\n\n- Big thing\n\n## [0.1.1](https://x/compare/v0.1.0...v0.1.1) - 2026-11-06\n\n### Fixed\n\n- Small thing\n\n## [0.1.0](https://x/releases/tag/v0.1.0) - 2026-11-01\n\nFirst public release.\n";

    fn versions(s: &[Section]) -> Vec<&str> {
        s.iter().map(|s| s.version.as_str()).collect()
    }

    #[test]
    fn one_version() {
        let s = sections_between(CHANGELOG, None, "0.1.1");
        assert_eq!(s, vec![Section { version: "0.1.1".into(), body: "### Fixed\n\n- Small thing".into() }]);
    }

    #[test]
    fn every_skipped_version_newest_first() {
        assert_eq!(versions(&sections_between(CHANGELOG, Some("0.1.0"), "0.2.0")), ["0.2.0", "0.1.1"]);
        assert_eq!(versions(&sections_between(CHANGELOG, Some("0.1.1"), "0.2.0")), ["0.2.0"]);
        assert!(sections_between(CHANGELOG, Some("0.2.0"), "0.2.0").is_empty());
    }

    #[test]
    fn dev_builds_use_their_release_and_unknown_versions_give_nothing() {
        assert_eq!(versions(&sections_between(CHANGELOG, None, "0.1.1+3-gabc1234")), ["0.1.1"]);
        assert!(sections_between(CHANGELOG, None, "0.3.0").is_empty());
        assert!(sections_between(CHANGELOG, None, "X.X.X").is_empty());
    }

    #[test]
    fn history_lists_every_version_up_to_the_running_one() {
        assert_eq!(versions(&history(CHANGELOG, "0.2.0")), ["0.2.0", "0.1.1", "0.1.0"]);
        assert_eq!(versions(&history(CHANGELOG, "0.1.1+3-gabc1234")), ["0.1.1", "0.1.0"]);
        assert_eq!(versions(&history(CHANGELOG, "0.1.0")), ["0.1.0"]);
        assert!(history(CHANGELOG, "X.X.X").is_empty());
    }

    #[test]
    fn the_embedded_changelog_has_the_first_release() {
        assert_eq!(sections_between(EMBEDDED, None, "0.1.0")[0].body.lines().next(), Some("First public release."));
    }

    #[test]
    fn markdown_subset() {
        let md = "First release.\n\n### Added\n\n- **Bold** and `Ctrl+Shift+1`\n* See [the docs](https://x/y) now\n- [unclosed bracket\n";
        assert_eq!(
            lines(md),
            vec![
                Line::Text("First release.".into()),
                Line::Heading("Added".into()),
                Line::Bullet("Bold and Ctrl+Shift+1".into()),
                Line::Bullet("See the docs now".into()),
                Line::Bullet("[unclosed bracket".into()),
            ]
        );
    }
}
