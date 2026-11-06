//! GitHub releases: which ones are newer than the running build, and which file to download.
//!
//! release-plz creates the release (tag, notes from the CHANGELOG) a few minutes before
//! `release.yml` attaches the binaries: a release only counts once this platform's file and
//! `SHA256SUMS` are there.

use serde::Deserialize;

use super::version;

pub const SUMS: &str = "SHA256SUMS";

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// `0.1.2` (without the `v`).
    pub version: String,
    /// The release notes: this version's CHANGELOG section (Markdown).
    pub notes: String,
    pub page: String,
    pub assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

/// `GET /repos/{owner}/{repo}/releases` of this repository.
pub fn api_url() -> String {
    let repo = env!("CARGO_PKG_REPOSITORY").trim_end_matches('/');
    let path = repo.strip_prefix("https://github.com/").unwrap_or(repo);
    format!("https://api.github.com/repos/{path}/releases?per_page=30")
}

pub fn release_page(version: &str) -> String {
    format!("{}/releases/tag/v{version}", env!("CARGO_PKG_REPOSITORY").trim_end_matches('/'))
}

/// The file this platform downloads from release `version`.
pub fn asset_name(windows: bool, version: &str) -> String {
    if windows { "tcanvas.exe".into() } else { format!("tcanvas-v{version}-linux-x86_64.tar.gz") }
}

pub fn parse(json: &str) -> Result<Vec<Release>, String> {
    let list: Vec<ApiRelease> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(list
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease && version::parse(&r.tag_name).is_some())
        .map(|r| Release {
            version: r.tag_name.trim_start_matches('v').to_string(),
            notes: r.body.unwrap_or_default(),
            page: r.html_url,
            assets: r.assets.into_iter().map(|a| Asset { name: a.name, url: a.browser_download_url }).collect(),
        })
        .collect())
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// Its binaries for this platform and their checksums are attached.
    pub fn installable(&self, windows: bool) -> bool {
        self.asset(&asset_name(windows, &self.version)).is_some() && self.asset(SUMS).is_some()
    }
}

/// Releases newer than `running`, newest first, as long as the newest one can be installed on
/// this platform; otherwise none (its files are still being built).
pub fn pending(mut releases: Vec<Release>, running: &str, windows: bool) -> Vec<Release> {
    releases.retain(|r| version::is_newer(&r.version, running));
    releases.sort_by_key(|r| std::cmp::Reverse(version::parse(&r.version)));
    match releases.first() {
        Some(latest) if latest.installable(windows) => releases,
        _ => Vec::new(),
    }
}

/// The SHA-256 of `name` in a `sha256sum` output (`<hex>  <name>`, or `<hex> *<name>`).
pub fn checksum_of(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        (file.trim_start().trim_start_matches('*') == name && hash.len() == 64).then(|| hash.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r####"[
      {"tag_name": "v0.1.3", "body": "draft", "html_url": "u3", "draft": true, "prerelease": false, "assets": []},
      {"tag_name": "v0.1.2", "body": "### Added\n\n- Update button", "html_url": "https://github.com/sasselin/tcanvas/releases/tag/v0.1.2", "draft": false, "prerelease": false,
       "assets": [{"name": "SHA256SUMS", "browser_download_url": "https://x/SHA256SUMS"},
                  {"name": "tcanvas.exe", "browser_download_url": "https://x/tcanvas.exe"},
                  {"name": "tcanvas-v0.1.2-linux-x86_64.tar.gz", "browser_download_url": "https://x/linux.tar.gz"}]},
      {"tag_name": "v0.1.1", "body": null, "html_url": "u1", "assets": [{"name": "tcanvas.exe", "browser_download_url": "https://x/old.exe"}]},
      {"tag_name": "latest", "body": "", "html_url": "u", "assets": []}
    ]"####;

    #[test]
    fn parses_published_releases_only() {
        let list = parse(JSON).unwrap();
        assert_eq!(list.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(), ["0.1.2", "0.1.1"]);
        assert_eq!(list[0].notes, "### Added\n\n- Update button");
        assert_eq!(list[1].notes, "");
        assert_eq!(list[0].asset("tcanvas.exe").unwrap().url, "https://x/tcanvas.exe");
        assert!(parse("{not json").is_err());
    }

    #[test]
    fn picks_this_platforms_file() {
        let r = &parse(JSON).unwrap()[0];
        assert_eq!(asset_name(true, "0.1.2"), "tcanvas.exe");
        assert_eq!(asset_name(false, "0.1.2"), "tcanvas-v0.1.2-linux-x86_64.tar.gz");
        assert!(r.installable(true) && r.installable(false));
    }

    #[test]
    fn pending_lists_every_newer_release_newest_first() {
        let pending = pending(parse(JSON).unwrap(), "0.1.0", true);
        assert_eq!(pending.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(), ["0.1.2", "0.1.1"]);
        assert!(super::pending(parse(JSON).unwrap(), "0.1.2", true).is_empty());
        assert!(super::pending(parse(JSON).unwrap(), "0.1.2+4-gabcdef0", false).is_empty());
    }

    #[test]
    fn a_release_whose_files_are_not_attached_yet_is_not_offered() {
        let mut list = parse(JSON).unwrap();
        list[0].assets.retain(|a| a.name != SUMS);
        assert!(pending(list, "0.1.0", true).is_empty());
    }

    #[test]
    fn reads_checksums() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  tcanvas.exe\n{b} *tcanvas-v0.1.2-linux-x86_64.tar.gz\nshort  other\n");
        assert_eq!(checksum_of(&sums, "tcanvas.exe"), Some(a));
        assert_eq!(checksum_of(&sums, "tcanvas-v0.1.2-linux-x86_64.tar.gz"), Some("b".repeat(64)));
        assert_eq!(checksum_of(&sums, "other"), None);
        assert_eq!(checksum_of(&sums, "tcanvas"), None);
    }

    #[test]
    fn urls_point_at_this_repository() {
        assert_eq!(api_url(), "https://api.github.com/repos/sasselin/tcanvas/releases?per_page=30");
        assert_eq!(release_page("0.1.2"), "https://github.com/sasselin/tcanvas/releases/tag/v0.1.2");
    }
}
