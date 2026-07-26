pub(crate) fn is_newer(latest: &str, current: &str) -> Option<bool> {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => Some(l > c),
        _ => None,
    }
}

pub(crate) fn is_apohl79_fork_release(version: &str) -> bool {
    parse_apohl79_fork_release(version).is_some()
}

pub(crate) fn is_newer_apohl79_fork_release(latest: &str, current: &str) -> Option<bool> {
    match (
        parse_apohl79_fork_release(latest),
        parse_apohl79_fork_release(current),
    ) {
        (Some(latest), Some(current)) => Some(latest > current),
        _ => None,
    }
}

pub(crate) fn extract_version_from_latest_tag(latest_tag_name: &str) -> anyhow::Result<String> {
    latest_tag_name
        .strip_prefix("rust-v")
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse latest tag name '{latest_tag_name}'"))
}

pub(crate) fn is_source_build_version(version: &str) -> bool {
    parse_version(version) == Some((0, 0, 0))
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut iter = v.trim().split('.');
    let maj = iter.next()?.parse::<u64>().ok()?;
    let min = iter.next()?.parse::<u64>().ok()?;
    let pat = iter.next()?.parse::<u64>().ok()?;
    Some((maj, min, pat))
}

fn parse_apohl79_fork_release(version: &str) -> Option<(u64, u64, u64, u64)> {
    let (base_version, build_number) = version.trim().rsplit_once("-apohl79-")?;
    let (major, minor, patch) = parse_version(base_version)?;
    Some((major, minor, patch, build_number.parse::<u64>().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn extracts_version_from_latest_tag() {
        assert_eq!(
            extract_version_from_latest_tag("rust-v1.5.0").expect("failed to parse version"),
            "1.5.0"
        );
    }

    #[test]
    fn latest_tag_without_prefix_is_invalid() {
        assert!(extract_version_from_latest_tag("v1.5.0").is_err());
    }

    #[test]
    fn prerelease_version_is_not_considered_newer() {
        assert_eq!(is_newer("0.11.0-beta.1", "0.11.0"), None);
        assert_eq!(is_newer("1.0.0-rc.1", "1.0.0"), None);
    }

    #[test]
    fn plain_semver_comparisons_work() {
        assert_eq!(is_newer("0.11.1", "0.11.0"), Some(true));
        assert_eq!(is_newer("0.11.0", "0.11.1"), Some(false));
        assert_eq!(is_newer("1.0.0", "0.9.9"), Some(true));
        assert_eq!(is_newer("0.9.9", "1.0.0"), Some(false));
    }

    #[test]
    fn apohl79_fork_release_is_recognized() {
        assert_eq!(is_apohl79_fork_release("0.144.0-apohl79-31"), true);
    }

    #[test]
    fn newer_apohl79_fork_build_is_available() {
        assert_eq!(
            is_newer_apohl79_fork_release("0.144.0-apohl79-32", "0.144.0-apohl79-31"),
            Some(true)
        );
    }

    #[test]
    fn base_version_increase_is_newer_apohl79_fork_build() {
        assert_eq!(
            is_newer_apohl79_fork_release("0.145.0-apohl79-1", "0.144.0-apohl79-31"),
            Some(true)
        );
    }

    #[test]
    fn equal_apohl79_fork_build_is_not_available() {
        assert_eq!(
            is_newer_apohl79_fork_release("0.144.0-apohl79-31", "0.144.0-apohl79-31"),
            Some(false)
        );
    }

    #[test]
    fn source_build_version_is_not_checked() {
        assert!(is_source_build_version("0.0.0"));
        assert!(!is_source_build_version("0.1.0"));
    }

    #[test]
    fn whitespace_is_ignored() {
        assert_eq!(parse_version(" 1.2.3 \n"), Some((1, 2, 3)));
        assert_eq!(is_newer(" 1.2.3 ", "1.2.2"), Some(true));
    }
}
