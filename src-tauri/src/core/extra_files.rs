//! Extra files backed up alongside the skill library (e.g. `~/.claude/CLAUDE.md`).
//!
//! The per-device list lives in `<base_dir>/extra-backup-paths.json`, never in
//! the repo. Each path is mirrored into `<skills_dir>/.extra-files/home/<rel>`
//! (under the home dir) or `.extra-files/root/<abs>` before every commit and
//! written back to its real location after every merge / restore / clone.
//! Only paths configured on *this* device are written back, so entries from
//! other devices sit inert in the repo. The dot-prefixed dir is skipped by the
//! skill scanner and merged as residual files (newest wins) by the merge engine.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

use super::central_repo;

pub const EXTRA_DIR: &str = ".extra-files";
const LIST_FILE: &str = "extra-backup-paths.json";

fn list_path() -> PathBuf {
    central_repo::base_dir().join(LIST_FILE)
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")), home()) {
        (Some(rest), Some(h)) => h.join(rest),
        _ if p == "~" => home().unwrap_or_default(),
        _ => PathBuf::from(p),
    }
}

fn collapse_home(p: &Path) -> String {
    match home().and_then(|h| p.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rel) => format!("~/{}", rel.to_string_lossy().replace('\\', "/")),
        None => p.to_string_lossy().to_string(),
    }
}

/// Configured paths in display form (`~/...` when under the home dir).
pub fn list() -> Vec<String> {
    fs::read_to_string(list_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Save a normalized, de-duplicated list. Mirrors of removed paths leave the
/// repo on the next commit. Returns the saved list.
pub fn set(paths: &[String], skills_dir: &Path) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in paths {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let abs = expand_home(raw);
        if !abs.is_absolute() {
            anyhow::bail!("extra backup path must be absolute: {raw}");
        }
        let shown = collapse_home(&abs);
        if !out.contains(&shown) {
            out.push(shown);
        }
    }
    let old = list();
    fs::create_dir_all(central_repo::base_dir())?;
    fs::write(list_path(), serde_json::to_vec_pretty(&out)?)?;
    for gone in old.iter().filter(|p| !out.contains(p)) {
        if let Some(m) = mirror_path(skills_dir, gone) {
            remove_any(&m);
        }
    }
    Ok(out)
}

/// Where `shown` is mirrored inside the repo.
fn mirror_path(skills_dir: &Path, shown: &str) -> Option<PathBuf> {
    let abs = expand_home(shown);
    let base = skills_dir.join(EXTRA_DIR);
    if let Some(rel) = home().and_then(|h| abs.strip_prefix(h).ok().map(Path::to_path_buf)) {
        return Some(base.join("home").join(rel));
    }
    let mut out = base.join("root");
    for c in abs.components() {
        if let Component::Normal(n) = c {
            out.push(n);
        }
    }
    Some(out)
}

/// Copy every configured path into the repo mirror (before a commit).
/// A path missing on disk keeps its last mirror: never propagate a deletion
/// that may just be an unmounted volume.
pub fn mirror_in(skills_dir: &Path) -> Result<()> {
    for shown in list() {
        let src = expand_home(&shown);
        if !src.exists() {
            log::debug!("extra files: {shown} missing locally, keeping last mirror");
            continue;
        }
        if let Some(dst) = mirror_path(skills_dir, &shown) {
            sync_tree(&src, &dst).with_context(|| format!("mirror in {shown}"))?;
        }
    }
    Ok(())
}

/// Write the repo mirror back to every configured path (after a merge /
/// restore / clone). A path with no mirror is left alone.
pub fn mirror_out(skills_dir: &Path) -> Result<()> {
    // ponytail: no guard against local edits made between the pre-merge
    // commit and this write-back (seconds); add a HEAD@{1} comparison if it bites.
    for shown in list() {
        let Some(src) = mirror_path(skills_dir, &shown) else { continue };
        if !src.exists() {
            continue;
        }
        sync_tree(&src, &expand_home(&shown)).with_context(|| format!("mirror out {shown}"))?;
    }
    Ok(())
}

/// Configured paths with changes not yet committed: local differs from the
/// mirror, or the mirror itself is dirty in `porcelain` (`git status --porcelain`).
/// Dirty mirror lines no configured path owns (a removed path) count once.
pub fn changed_count(skills_dir: &Path, porcelain: &str) -> u32 {
    let dirty: Vec<&str> = porcelain
        .lines()
        .filter_map(|l| l.get(3..))
        .map(|p| p.trim().trim_matches('"'))
        .filter(|p| p.starts_with(EXTRA_DIR))
        .collect();
    let mut owned = 0usize;
    let mut n = 0u32;
    for shown in list() {
        let src = expand_home(&shown);
        let Some(dst) = mirror_path(skills_dir, &shown) else { continue };
        let rel = dst.strip_prefix(skills_dir).unwrap_or(&dst).to_string_lossy().into_owned();
        let mine = dirty.iter().filter(|p| p.starts_with(&rel)).count();
        owned += mine;
        if mine > 0 || (src.exists() && tree_differs(&src, &dst)) {
            n += 1;
        }
    }
    n + u32::from(dirty.len() > owned)
}

/// True when any configured path differs from its mirror (drives auto backup).
pub fn local_differs(skills_dir: &Path) -> bool {
    list().iter().any(|shown| {
        let src = expand_home(shown);
        match mirror_path(skills_dir, shown) {
            Some(dst) if src.exists() => tree_differs(&src, &dst),
            _ => false,
        }
    })
}

/// Relative file paths under `root` (`root` itself when it is a file).
fn files_under(root: &Path) -> BTreeSet<PathBuf> {
    if root.is_file() {
        return BTreeSet::from([PathBuf::new()]);
    }
    WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| e.file_name().to_string_lossy() != ".git")
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().strip_prefix(root).unwrap().to_path_buf())
        .collect()
}

/// `root/rel`, or `root` itself for the empty rel of a single file.
fn at(root: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() { root.to_path_buf() } else { root.join(rel) }
}

fn same_content(a: &Path, b: &Path) -> bool {
    match (fs::read(a), fs::read(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn tree_differs(src: &Path, dst: &Path) -> bool {
    if !dst.exists() {
        return true;
    }
    let want = files_under(src);
    want != files_under(dst) || want.iter().any(|r| !same_content(&at(src, r), &at(dst, r)))
}

/// Make `dst` an exact copy of `src` (file or directory), writing only what changed.
fn sync_tree(src: &Path, dst: &Path) -> Result<()> {
    if src.is_file() {
        if dst.is_dir() {
            fs::remove_dir_all(dst)?;
        }
        if !same_content(src, dst) {
            if let Some(p) = dst.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(src, dst)?;
        }
        return Ok(());
    }
    if dst.is_file() {
        fs::remove_file(dst)?;
    }
    fs::create_dir_all(dst)?;
    let want = files_under(src);
    for rel in files_under(dst).difference(&want) {
        remove_any(&dst.join(rel));
    }
    for rel in &want {
        let (s, d) = (src.join(rel), dst.join(rel));
        if !same_content(&s, &d) {
            if let Some(p) = d.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

fn remove_any(p: &Path) {
    let _ = if p.is_dir() { fs::remove_dir_all(p) } else { fs::remove_file(p) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_file_and_dir() {
        let _lock = central_repo::test_base_dir_lock();
        let tmp = tempfile::tempdir().unwrap();
        central_repo::set_test_base_dir_override(Some(tmp.path().join("base")));
        let skills = tmp.path().join("skills");
        let file = tmp.path().join("src").join("CLAUDE.md");
        let dir = tmp.path().join("src").join("agents");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&file, "one").unwrap();
        fs::write(dir.join("a.md"), "a").unwrap();

        let saved = set(
            &[file.to_string_lossy().to_string(), dir.to_string_lossy().to_string()],
            &skills,
        )
        .unwrap();
        assert_eq!(saved.len(), 2);
        assert!(local_differs(&skills));
        mirror_in(&skills).unwrap();
        assert!(!local_differs(&skills));
        let m_file = mirror_path(&skills, &saved[0]).unwrap();
        let m_dir = mirror_path(&skills, &saved[1]).unwrap();
        assert!(m_file.starts_with(skills.join(EXTRA_DIR)));
        assert_eq!(fs::read_to_string(&m_file).unwrap(), "one");

        // Remote edit + deletion flows back out.
        fs::write(&m_file, "two").unwrap();
        fs::remove_file(m_dir.join("a.md")).unwrap();
        fs::write(m_dir.join("b.md"), "b").unwrap();
        mirror_out(&skills).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "two");
        assert!(!dir.join("a.md").exists());
        assert_eq!(fs::read_to_string(dir.join("b.md")).unwrap(), "b");

        // Removing a path drops its mirror.
        set(&[saved[0].clone()], &skills).unwrap();
        assert!(!m_dir.exists());
        assert!(m_file.exists());
        central_repo::set_test_base_dir_override(None);
    }
}
