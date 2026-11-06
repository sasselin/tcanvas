//! The running build's version: the workspace version (set by release-plz), plus the commits
//! made since the last release tag for a development build (`0.1.0+3-gae43b4d`).

use std::sync::OnceLock;

/// The version shown in the toolbar and the window title.
pub fn current() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| of_build(env!("CARGO_PKG_VERSION"), env!("TCANVAS_GIT_DESCRIBE")))
}

/// `describe` is `git describe --long` output (`v0.1.0-3-gae43b4d`), or empty without git.
/// A build on the tag itself, or one without git information, is the plain version.
pub fn of_build(package: &str, describe: &str) -> String {
    let mut parts = describe.rsplitn(3, '-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(hash), Some(distance), Some(_tag)) if hash.starts_with('g') && distance.parse::<u32>().is_ok_and(|n| n > 0) => format!("{package}+{distance}-{hash}"),
        _ => package.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_on_the_release_tag_is_the_plain_version() {
        assert_eq!(of_build("0.1.1", "v0.1.1-0-g1234567"), "0.1.1");
    }

    #[test]
    fn commits_after_the_tag_show_their_count_and_hash() {
        assert_eq!(of_build("0.1.0", "v0.1.0-3-gae43b4d"), "0.1.0+3-gae43b4d");
    }

    #[test]
    fn no_git_information_gives_the_plain_version() {
        assert_eq!(of_build("0.1.0", ""), "0.1.0");
        assert_eq!(of_build("0.1.0", "ae43b4d"), "0.1.0");
    }
}
