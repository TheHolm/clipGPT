//! Checks that `Cargo.toml`'s `[package.metadata.deb]` section and
//! `debian/changelog` stay consistent with each other and with Debian
//! Policy, so a `.deb` built by `cargo deb` doesn't fail lintian's
//! `fields/mail-address`, `debian/changelog` or `fields/section` checks
//! (https://lintian.debian.org/tags/malformed-contact.html,
//! no-changelog.html, unknown-section.html). These ran into production
//! once already (a bare "TheHolm" maintainer, no changelog file at all,
//! and a `section = "utility"` that doesn't exist); this file exists so
//! the next edit to either file gets caught here instead of at release
//! time.

use std::fs;
use std::path::Path;

/// The sections lintian accepts in the `Section:` control field, from its
/// `fields/section` check's `unknown-section` tag message. Kept as a
/// plain list (instead of depending on lintian's own data at test time)
/// since it changes rarely and the test is only meant to catch a typo
/// like the "utility"/"utils" one that prompted this file.
const VALID_SECTIONS: &[&str] = &[
    "admin",
    "comm",
    "cli-mono",
    "database",
    "debug",
    "devel",
    "doc",
    "editors",
    "electronics",
    "embedded",
    "fonts",
    "games",
    "gnome",
    "gnu-r",
    "gnustep",
    "graphics",
    "hamradio",
    "haskell",
    "httpd",
    "interpreters",
    "java",
    "javascript",
    "kde",
    "libdevel",
    "libs",
    "lisp",
    "localization",
    "kernel",
    "mail",
    "math",
    "misc",
    "net",
    "news",
    "ocaml",
    "oldlibs",
    "otherosfs",
    "perl",
    "php",
    "python",
    "ruby",
    "rust",
    "science",
    "shells",
    "sound",
    "tex",
    "text",
    "utils",
    "vcs",
    "video",
    "web",
    "x11",
    "xfce",
    "zope",
];

/// Parses `Cargo.toml`'s `[package.metadata.deb]` table.
fn deb_metadata() -> toml::Table {
    let manifest: toml::Table = include_str!("../Cargo.toml")
        .parse()
        .expect("Cargo.toml must be valid TOML");
    manifest["package"]["metadata"]["deb"]
        .as_table()
        .expect("[package.metadata.deb] must be a table")
        .clone()
}

/// `maintainer` must look like `Name <address@domain>` (Debian Policy
/// 5.6.2 / lintian's `fields/mail-address` check), not a bare name. The
/// address itself need not accept mail (a GitHub noreply address, for
/// instance, does not): lintian only checks the shape of the field, and
/// contact in that case goes through the project's issue tracker
/// instead, which is why `repository` (below) must be set too.
#[test]
fn maintainer_has_an_address() {
    let maintainer = deb_metadata()["maintainer"]
        .as_str()
        .expect("maintainer must be a string")
        .to_string();
    let (name, rest) = maintainer
        .split_once('<')
        .expect("maintainer must contain '<address>'");
    assert!(!name.trim().is_empty(), "maintainer has no name before '<'");
    let addr = rest
        .strip_suffix('>')
        .expect("maintainer's '<...>' must be closed");
    assert!(
        addr.contains('@'),
        "maintainer address has no '@': {addr:?}"
    );
    let (local, domain) = addr.split_once('@').unwrap();
    assert!(
        !local.is_empty(),
        "maintainer address has no local part: {addr:?}"
    );
    assert!(
        domain.contains('.'),
        "maintainer address's domain looks wrong: {addr:?}"
    );
}

/// `section` must be one of the sections lintian recognizes
/// (`fields/section`'s `unknown-section` tag) - it previously was the
/// non-existent "utility" instead of "utils".
#[test]
fn section_is_valid() {
    let metadata = deb_metadata();
    let section = metadata["section"]
        .as_str()
        .expect("section must be a string");
    assert!(
        VALID_SECTIONS.contains(&section),
        "section {section:?} is not one of lintian's valid sections"
    );
}

/// A package built with a "~<distro>" `deb-revision` (every release build
/// here uses one, to tell Trixie/Ubuntu/etc. builds of the same upstream
/// version apart) is non-native, so Debian Policy 12.7 requires a
/// `changelog.Debian.gz` under `/usr/share/doc`. `cargo deb` only
/// generates one when `changelog` names a source file to read; this
/// checks that key is set and that file exists.
#[test]
fn changelog_key_points_at_a_real_file() {
    let metadata = deb_metadata();
    let changelog = metadata["changelog"]
        .as_str()
        .expect("changelog must be a string");
    assert_eq!(changelog, "debian/changelog");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(changelog);
    assert!(path.is_file(), "{path:?} does not exist");
}

/// `repository` becomes the control file's `Homepage:` field (`cargo
/// deb` falls back to it when `homepage` isn't set) - with a Maintainer
/// address that can't receive mail, this is the one place a user running
/// `dpkg -s clip-gpt` can find where to report a bug.
#[test]
fn repository_is_set() {
    let manifest: toml::Table = include_str!("../Cargo.toml")
        .parse()
        .expect("Cargo.toml must be valid TOML");
    let repository = manifest["package"]["repository"]
        .as_str()
        .expect("package.repository must be a string");
    assert!(
        repository.starts_with("https://"),
        "repository {repository:?} should be an https URL"
    );
}

/// The topmost `debian/changelog` entry's upstream version (the part
/// before the last "-revision") must match the crate version: a version
/// bump with no matching changelog entry would otherwise go unnoticed
/// until a release build's lintian step (or a human) caught the mismatch.
#[test]
fn changelog_top_entry_matches_crate_version() {
    let changelog =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("debian/changelog"))
            .expect("debian/changelog must exist");
    let first_line = changelog
        .lines()
        .next()
        .expect("debian/changelog must not be empty");
    // "clip-gpt (0.7.1-1) unstable; urgency=medium"
    let version_field = first_line
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(version, _)| version)
        .unwrap_or_else(|| panic!("can't parse version out of {first_line:?}"));
    let upstream_version = version_field
        .rsplit_once('-')
        .map(|(upstream, _revision)| upstream)
        .unwrap_or(version_field);
    assert_eq!(upstream_version, env!("CARGO_PKG_VERSION"));
}

/// The changelog's distribution must not be "unreleased" (lintian's
/// `unreleased-changelog-distribution`) or the literal suite names
/// Policy 5.6.14 says a version number, not this field, should encode
/// (lintian's `version-refers-to-distribution`).
#[test]
fn changelog_distribution_is_reasonable() {
    let changelog =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("debian/changelog"))
            .expect("debian/changelog must exist");
    let first_line = changelog
        .lines()
        .next()
        .expect("debian/changelog must not be empty");
    let distribution = first_line
        .split_once(')')
        .map(|(_, rest)| rest.trim_start())
        .and_then(|rest| rest.split(';').next())
        .map(str::trim)
        .unwrap_or_else(|| panic!("can't parse distribution out of {first_line:?}"));
    assert_ne!(distribution.to_ascii_lowercase(), "unreleased");
}
