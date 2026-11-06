//! Version comparison for update checks. Build metadata (`0.1.1+3-gae43b4d`, a development
//! build made after the 0.1.1 tag) is ignored: such a build is only offered a later release.

/// `"v0.1.2"`, `"0.1.2"` or `"0.1.2+3-gabc"` → `(0, 1, 2)`. `None` if it is not `X.Y.Z`.
pub fn parse(v: &str) -> Option<(u32, u32, u32)> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['+', '-']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u32>().ok());
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Some(a)), Some(Some(b)), Some(Some(c)), None) => Some((a, b, c)),
        _ => None,
    }
}

/// `candidate` is a strictly later release than `running`. Unparsable versions never are.
pub fn is_newer(candidate: &str, running: &str) -> bool {
    matches!((parse(candidate), parse(running)), (Some(c), Some(r)) if c > r)
}

/// `0.1.1+3-gae43b4d` → `0.1.1`: the release a build comes from.
pub fn base(v: &str) -> String {
    match parse(v) {
        Some((a, b, c)) => format!("{a}.{b}.{c}"),
        None => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tags_plain_versions_and_dev_builds() {
        assert_eq!(parse("v0.1.2"), Some((0, 1, 2)));
        assert_eq!(parse("1.20.3"), Some((1, 20, 3)));
        assert_eq!(parse("0.1.1+3-gae43b4d"), Some((0, 1, 1)));
        assert_eq!(parse("1.2"), None);
        assert_eq!(parse("1.2.3.4"), None);
        assert_eq!(parse("X.X.X"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn only_strictly_later_releases_are_newer() {
        assert!(is_newer("v0.1.2", "0.1.1"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        assert!(is_newer("v0.1.10", "0.1.9"), "numeric, not lexical");
        assert!(!is_newer("v0.1.1", "0.1.1"));
        assert!(!is_newer("v0.1.0", "0.1.1"));
    }

    #[test]
    fn a_dev_build_is_not_offered_its_own_release() {
        assert!(!is_newer("v0.1.1", "0.1.1+3-gae43b4d"));
        assert!(is_newer("v0.1.2", "0.1.1+3-gae43b4d"));
        assert_eq!(base("0.1.1+3-gae43b4d"), "0.1.1");
    }

    #[test]
    fn unparsable_versions_are_never_newer() {
        assert!(!is_newer("nightly", "0.1.1"));
        assert!(!is_newer("v9.9.9", "X.X.X"));
    }
}
