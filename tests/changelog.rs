//! `CHANGELOG.md` has notes for the version in `Cargo.toml`, so a release
//! never goes out without them. `scripts/pack-release.ps1` reads the same
//! section, with the same rule, into the release notes.

const CHANGELOG: &str = include_str!("../CHANGELOG.md");
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The notes under the line `## {version}`, up to the next `## ` heading,
/// with blank lines at either end dropped. `None` if there's no such
/// heading or nothing under it.
fn section(changelog: &str, version: &str) -> Option<String> {
    let heading = format!("## {version}");
    let mut lines = changelog.lines().map(str::trim_end);
    lines.find(|line| *line == heading)?;
    let body: Vec<&str> = lines.take_while(|line| !line.starts_with("## ")).collect();
    let text = body.join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[test]
fn changelog_has_notes_for_this_version() {
    assert!(
        section(CHANGELOG, VERSION).is_some(),
        "CHANGELOG.md has no notes for version {VERSION}. Add a \"## {VERSION}\" heading with \
         what's new for users under it, in the same commit as the version bump."
    );
}

#[test]
fn changelog_is_ascii() {
    // The pack script reads it, and PowerShell 5.1 is easily confused by
    // anything else.
    let bad = CHANGELOG.lines().position(|line| !line.is_ascii());
    assert_eq!(
        bad,
        None,
        "CHANGELOG.md line {} isn't plain ASCII",
        bad.map_or(0, |i| i + 1)
    );
}

#[test]
fn section_rule() {
    let text = "# Changelog\n\n## 1.1.0\n\n- New.\n\n## 1.0.0\n\nFirst.\n\n## 0.9.0\n\n";
    assert_eq!(section(text, "1.1.0").as_deref(), Some("- New."));
    assert_eq!(section(text, "1.0.0").as_deref(), Some("First."));
    assert_eq!(
        section(text, "0.9.0"),
        None,
        "an empty section counts as missing"
    );
    assert_eq!(section(text, "1.1"), None, "the heading must match exactly");
    assert_eq!(section(text, "2.0.0"), None);
}
