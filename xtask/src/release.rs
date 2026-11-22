//! `cargo xtask <patch|minor|major> [-m MESSAGE]...`: bumps the workspace version, commits
//! it, tags the commit and pushes both. The pushed tag starts the release workflow
//! (`.github/workflows/release-cli.yaml`), which builds and publishes the release.
//!
//! `-m` works as in `git commit`: `-m TEXT`, `-mTEXT`, `--message TEXT` or `--message=TEXT`,
//! and several of them become separate paragraphs. The message is the tag's annotation and the
//! release notes' opening text.
use std::{
    path::{Path, PathBuf},
    process::Command,
};

const MANIFEST_SECTION: &str = "[workspace.package]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Patch,
    Minor,
    Major,
}

impl Level {
    pub fn parse(task: &str) -> Option<Self> {
        match task {
            "patch" => Some(Self::Patch),
            "minor" | "minior" => Some(Self::Minor),
            "major" => Some(Self::Major),
            _ => None,
        }
    }
}

/// The version after `version` at `level`: lower parts start over at 0.
fn bump(version: &str, level: Level) -> Result<String, String> {
    let parts: Vec<u64> = version
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| format!("version `{version}` is not MAJOR.MINOR.PATCH"))?;
    let [major, minor, patch] = parts[..] else {
        return Err(format!("version `{version}` is not MAJOR.MINOR.PATCH"));
    };
    Ok(match level {
        Level::Patch => format!("{major}.{minor}.{}", patch + 1),
        Level::Minor => format!("{major}.{}.0", minor + 1),
        Level::Major => format!("{}.0.0", major + 1),
    })
}

/// The highest `MAJOR.MINOR.PATCH` among `tags` (a leading `v` allowed); other tags are not
/// releases.
fn latest_release<'a>(tags: impl Iterator<Item = &'a str>) -> Option<String> {
    tags.filter_map(|tag| {
        let version = tag.trim().trim_start_matches('v');
        let parts: Vec<u64> = version.split('.').map(str::parse).collect::<Result<_, _>>().ok()?;
        let [major, minor, patch] = parts[..] else { return None };
        Some(((major, minor, patch), version.to_owned()))
    })
    .max_by_key(|(key, _)| *key)
    .map(|(_, version)| version)
}

/// The `-m` messages joined as git joins them: one paragraph each.
fn parse_messages(args: impl Iterator<Item = String>) -> Result<Option<String>, String> {
    let mut messages = Vec::new();
    let mut args = args;
    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "-m" | "--message" => args.next().ok_or_else(|| format!("`{arg}` needs a message"))?,
            other => {
                if let Some(value) = other.strip_prefix("--message=") {
                    value.to_owned()
                } else if let Some(value) = other.strip_prefix("-m") {
                    value.to_owned()
                } else {
                    return Err(format!("unknown argument `{other}`"));
                }
            }
        };
        messages.push(value.trim().to_owned());
    }
    Ok((!messages.is_empty()).then(|| messages.join("\n\n")))
}

/// The workspace version in `manifest` and the byte range of its value.
fn find_version(manifest: &str) -> Result<(String, std::ops::Range<usize>), String> {
    let mut offset = 0;
    let mut in_section = false;
    for line in manifest.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == MANIFEST_SECTION;
        } else if in_section
            && let Some(rest) = trimmed.strip_prefix("version")
            && let Some(value) = rest.trim_start().strip_prefix('=')
        {
            let value = value.trim();
            let version = value
                .strip_prefix('"')
                .and_then(|value| value.split('"').next())
                .ok_or_else(|| format!("cannot read `{trimmed}`"))?;
            let start = offset + line.find(version).unwrap_or_default();
            return Ok((version.to_owned(), start..start + version.len()));
        }
        offset += line.len();
    }
    Err(format!("no `version` in {MANIFEST_SECTION}"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Runs `program` in the workspace, failing unless it succeeds.
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .current_dir(workspace_root())
        .status()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    if status.success() { Ok(()) } else { Err(format!("`{program} {}` failed", args.join(" "))) }
}

/// Output of `git args`, trimmed.
fn git_output(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace_root())
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn run_release(level: Level, args: impl Iterator<Item = String>) -> Result<(), String> {
    let message = parse_messages(args)?;

    // Only the version change goes into the release commit.
    if !git_output(&["status", "--porcelain"])?.is_empty() {
        return Err("the working tree has changes: commit or stash them first".into());
    }

    let manifest_path = workspace_root().join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let (in_manifest, range) = find_version(&manifest)?;
    // The releases are the tags: the manifest was not always bumped with them (it said 0.1.0
    // after 0.0.3 was tagged, and 0.1.1 came out instead of 0.0.4).
    let tags = git_output(&["tag", "--list"])?;
    let current = latest_release(tags.lines()).unwrap_or(in_manifest);
    let next = bump(&current, level)?;
    if !git_output(&["tag", "--list", &next])?.is_empty() {
        return Err(format!("tag {next} already exists"));
    }

    let mut updated = manifest;
    updated.replace_range(range, &next);
    std::fs::write(&manifest_path, updated)
        .map_err(|error| format!("cannot write {}: {error}", manifest_path.display()))?;
    // Brings the workspace crates' versions in Cargo.lock up to date, nothing else.
    run("cargo", &["update", "--workspace", "--quiet"])?;
    println!("version {current} -> {next}");

    let message = message.unwrap_or_else(|| format!("Release {next}"));
    run("git", &["add", "Cargo.toml", "Cargo.lock"])?;
    run("git", &["commit", "--quiet", "-m", &format!("chore(release): {next}")])?;
    run("git", &["tag", "--annotate", &next, "-m", &message])?;
    // Atomic: the branch and the tag land together or neither does.
    run("git", &["push", "--atomic", "origin", "HEAD", &next])?;
    println!("pushed tag {next}: the release workflow builds and publishes it");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bumps_each_level() {
        assert_eq!(bump("0.1.9", Level::Patch).unwrap(), "0.1.10");
        assert_eq!(bump("0.1.9", Level::Minor).unwrap(), "0.2.0");
        assert_eq!(bump("1.4.2", Level::Major).unwrap(), "2.0.0");
        assert!(bump("1.2", Level::Patch).is_err());
    }

    #[test]
    fn releases_follow_the_tags() {
        let tags = ["0.0.2", "v0.0.10", "0.0.3", "nightly", "1.2"];
        assert_eq!(latest_release(tags.into_iter()).as_deref(), Some("0.0.10"));
        assert_eq!(latest_release(["latest"].into_iter()), None);
    }

    #[test]
    fn messages_like_git() {
        let args = ["-m", "first", "-msecond", "--message", "third", "--message=fourth"];
        let message = parse_messages(args.into_iter().map(String::from)).unwrap();
        assert_eq!(message.as_deref(), Some("first\n\nsecond\n\nthird\n\nfourth"));
        assert_eq!(parse_messages(std::iter::empty()).unwrap(), None);
        assert!(parse_messages(["-m".to_owned()].into_iter()).is_err());
        assert!(parse_messages(["--oops".to_owned()].into_iter()).is_err());
    }

    #[test]
    fn finds_workspace_version_only() {
        let manifest = "[package]\nversion = \"9.9.9\"\n[workspace.package]\nversion = \"0.1.0\"\n";
        let (version, range) = find_version(manifest).unwrap();
        assert_eq!(version, "0.1.0");
        assert_eq!(&manifest[range], "0.1.0");
    }
}
