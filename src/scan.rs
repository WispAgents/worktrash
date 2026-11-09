//! Finds every git repository under the roots that has (or had) linked worktrees.
//! Walks in parallel, never follows symlinks, and skips the big dirs that never hold a repo.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::git;

/// Directory names we never descend into: dependency caches, build output, OS folders.
const SKIP: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    ".hg",
    ".svn",
    "Library",
    ".Trash",
    ".cache",
    ".npm",
    ".pnpm-store",
    ".yarn",
    ".cargo",
    ".rustup",
    ".gradle",
    ".m2",
    ".nuget",
    "Pods",
    ".venv",
    "venv",
    "__pycache__",
    ".tox",
    ".mypy_cache",
    ".next",
    ".turbo",
    "DerivedData",
    "Applications",
    "Movies",
    "Music",
    "Pictures",
];

/// Returns the common git dirs (`<repo>/.git`, or a bare repo) that own linked worktrees.
pub fn find_repos(roots: &[PathBuf], max_depth: usize) -> BTreeSet<PathBuf> {
    let found = Mutex::new(BTreeSet::new());
    // Fan out over each root's children so big home folders scan on every core.
    let mut starts: Vec<(PathBuf, usize)> = Vec::new();
    for root in roots {
        visit(root, &found);
        if let Ok(rd) = fs::read_dir(root) {
            for e in rd.flatten() {
                if is_walkable(&e) {
                    starts.push((e.path(), 1));
                }
            }
        }
    }
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZero::get)
        .min(16);
    let queue = Mutex::new(starts);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let next = queue.lock().ok().and_then(|mut q| q.pop());
                    let Some((dir, depth)) = next else { break };
                    walk(&dir, depth, max_depth, &found);
                }
            });
        }
    });
    found.into_inner().unwrap_or_default()
}

fn walk(dir: &Path, depth: usize, max_depth: usize, found: &Mutex<BTreeSet<PathBuf>>) {
    visit(dir, found);
    if depth >= max_depth {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if is_walkable(&e) {
            walk(&e.path(), depth + 1, max_depth, found);
        }
    }
}

/// Records the repo `dir` belongs to if `dir` is a repo root or a linked worktree.
fn visit(dir: &Path, found: &Mutex<BTreeSet<PathBuf>>) {
    let dotgit = dir.join(".git");
    let Ok(meta) = fs::symlink_metadata(&dotgit) else {
        return;
    };
    let common = if meta.is_dir() {
        // Only repos that have ever had a linked worktree are interesting.
        if !dotgit.join("worktrees").is_dir() {
            return;
        }
        git::canon(&dotgit)
    } else if meta.is_file() {
        match git::dirs_from_dotgit_file(&dotgit) {
            Some((_, common)) => common,
            None => return,
        }
    } else {
        return;
    };
    if let Ok(mut f) = found.lock() {
        f.insert(common);
    }
}

fn is_walkable(e: &fs::DirEntry) -> bool {
    let Ok(ft) = e.file_type() else { return false };
    if !ft.is_dir() || ft.is_symlink() {
        return false;
    }
    let name = e.file_name();
    let name = name.to_string_lossy();
    !SKIP.contains(&name.as_ref())
}
