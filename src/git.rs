//! Everything that talks to git. We shell out to the `git` binary so we behave exactly like the
//! user's git (config, hooks off, alternates, sparse checkouts) and need no libgit2.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Runs git with the given arguments and returns stdout when it exits 0.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn git_ok(args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// One linked worktree as `git worktree list --porcelain` reports it.
#[derive(Debug, Clone)]
pub struct Listed {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub locked: bool,
    pub prunable: bool,
}

/// The linked worktrees of a repository (the main worktree is left out).
pub fn list_worktrees(common_dir: &Path) -> Vec<Listed> {
    let gd = format!("--git-dir={}", common_dir.display());
    let Some(out) = git(&[&gd, "worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let mut all = Vec::new();
    // The first block is always the main worktree (or the bare repo itself): never a candidate.
    for block in out.split("\n\n").skip(1) {
        let mut item = Listed {
            path: PathBuf::new(),
            head: None,
            branch: None,
            locked: false,
            prunable: false,
        };
        let mut bare = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                item.path = PathBuf::from(p);
            } else if let Some(h) = line.strip_prefix("HEAD ") {
                item.head = Some(h.to_string());
            } else if let Some(b) = line.strip_prefix("branch ") {
                item.branch = Some(b.trim_start_matches("refs/heads/").to_string());
            } else if line == "bare" {
                bare = true;
            } else if line == "locked" || line.starts_with("locked ") {
                item.locked = true;
            } else if line == "prunable" || line.starts_with("prunable ") {
                item.prunable = true;
            }
        }
        if !item.path.as_os_str().is_empty() && !bare {
            all.push(item);
        }
    }
    all
}

/// The branches a worktree is compared against: the remote default branch and its local twin.
pub fn targets(common_dir: &Path) -> Vec<String> {
    let gd = format!("--git-dir={}", common_dir.display());
    let mut names = Vec::new();
    if let Some(r) = git(&[&gd, "symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]) {
        if let Some(b) = r.strip_prefix("refs/remotes/origin/") {
            names.push(format!("origin/{b}"));
            names.push(b.to_string());
        }
    }
    for b in ["main", "master", "trunk", "develop"] {
        names.push(format!("origin/{b}"));
        names.push(b.to_string());
    }
    let mut found: Vec<String> = Vec::new();
    for n in names {
        if !found.contains(&n)
            && git_ok(&[
                &gd,
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{n}^{{commit}}"),
            ])
        {
            found.push(n);
        }
    }
    // Keep the first family only (origin/main + main), not main *and* master.
    if let Some(first) = found.first().cloned() {
        let base = first.trim_start_matches("origin/").to_string();
        found.retain(|n| n.trim_start_matches("origin/") == base);
    }
    found
}

/// How a worktree's commits reached the default branch, if they did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merged {
    /// HEAD is an ancestor of the default branch (fast-forward, merge commit, or no new commits).
    Ancestor,
    /// The branch's combined diff landed as one commit (squash merge) or each commit by rebase.
    Squashed,
    No,
}

pub fn merged(common_dir: &Path, head: &str, targets: &[String]) -> Merged {
    let gd = format!("--git-dir={}", common_dir.display());
    for t in targets {
        if git_ok(&[&gd, "merge-base", "--is-ancestor", head, t]) {
            return Merged::Ancestor;
        }
    }
    for t in targets {
        if squashed_into(&gd, head, t) {
            return Merged::Squashed;
        }
    }
    Merged::No
}

/// The squash-merge check: build a throwaway commit holding the branch's whole diff on top of
/// the merge base, and ask `git cherry` whether an equivalent patch is already upstream.
fn squashed_into(gd: &str, head: &str, target: &str) -> bool {
    let Some(base) = git(&[gd, "merge-base", target, head]) else {
        return false;
    };
    let Some(tree) = git(&[gd, "rev-parse", &format!("{head}^{{tree}}")]) else {
        return false;
    };
    let Some(base_tree) = git(&[gd, "rev-parse", &format!("{base}^{{tree}}")]) else {
        return false;
    };
    if tree == base_tree {
        return true; // the branch's commits add up to no change at all
    }
    let out = Command::new("git")
        .args([
            gd,
            "commit-tree",
            &tree,
            "-p",
            &base,
            "-m",
            "worktrash squash probe",
        ])
        .env("GIT_AUTHOR_NAME", "worktrash")
        .env("GIT_AUTHOR_EMAIL", "worktrash@localhost")
        .env("GIT_COMMITTER_NAME", "worktrash")
        .env("GIT_COMMITTER_EMAIL", "worktrash@localhost")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else { return false };
    if !out.status.success() {
        return false;
    }
    let probe = String::from_utf8_lossy(&out.stdout).trim().to_string();
    git(&[gd, "cherry", target, &probe]).is_some_and(|c| c.starts_with('-'))
}

/// What only this folder holds, the way wisp counts it: one per changed, staged or untracked (not
/// ignored) path, plus one when HEAD has left the branch tip (those commits live nowhere else). A
/// detached HEAD counts one when no branch, tag or remote ref contains it. None: git couldn't tell.
pub fn folder_only(common_dir: &Path, path: &Path, branch: Option<&str>) -> Option<u32> {
    let p = path.display().to_string();
    let status = git(&[
        "-C",
        &p,
        "status",
        "--porcelain",
        "--untracked-files=normal",
    ])?;
    let changed =
        u32::try_from(status.lines().filter(|l| !l.trim().is_empty()).count()).unwrap_or(u32::MAX);
    let head = git(&["-C", &p, "rev-parse", "HEAD"])?;
    let gd = format!("--git-dir={}", common_dir.display());
    let off = if let Some(b) = branch {
        let tip = git(&[
            &gd,
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{b}"),
        ])?;
        u32::from(tip != head)
    } else {
        let holders = git(&[
            &gd,
            "for-each-ref",
            "--contains",
            &head,
            "--count=1",
            "--format=%(refname)",
        ])?;
        u32::from(holders.is_empty())
    };
    Some(changed.saturating_add(off))
}

/// Commits on `branch` that the default branch doesn't have (`rev-list --count target..branch`).
pub fn unique_commits(common_dir: &Path, branch: &str, target: &str) -> Option<u32> {
    let gd = format!("--git-dir={}", common_dir.display());
    git(&[
        &gd,
        "rev-list",
        "--count",
        &format!("{target}..refs/heads/{branch}"),
    ])?
    .trim()
    .parse()
    .ok()
}

/// Commits on `branch` not on its upstream, or None when it has no upstream.
pub fn unpushed(common_dir: &Path, branch: &str) -> Option<u32> {
    let gd = format!("--git-dir={}", common_dir.display());
    let up = git(&[
        &gd,
        "rev-parse",
        "--abbrev-ref",
        "--symbolic-full-name",
        &format!("{branch}@{{upstream}}"),
    ])?;
    git(&[
        &gd,
        "rev-list",
        "--count",
        &format!("{up}..refs/heads/{branch}"),
    ])?
    .trim()
    .parse()
    .ok()
}

/// Deletes a local branch with `-d` semantics against the default branch (only when nothing is on it alone).
pub fn delete_branch(common_dir: &Path, branch: &str) -> bool {
    let gd = format!("--git-dir={}", common_dir.display());
    git_ok(&[&gd, "branch", "-D", "--", branch])
}

/// Open and ended pull requests by head branch, from the `gh` CLI when it's installed and signed in.
/// Empty when it isn't: then a merge is read from git alone.
pub fn pull_requests(repo_root: &Path) -> Vec<(String, u32, String)> {
    let out = Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "all",
            "--limit",
            "300",
            "--json",
            "headRefName,number,state",
            "--template",
            "{{range .}}{{.headRefName}}\t{{.number}}\t{{.state}}\n{{end}}",
        ])
        .current_dir(repo_root)
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            Some((
                it.next()?.to_string(),
                it.next()?.parse().ok()?,
                it.next()?.to_string(),
            ))
        })
        .collect()
}

/// The per-worktree git dir (`<common>/worktrees/<name>`) from the folder's `.git` file.
pub fn worktree_gitdir(path: &Path) -> Option<PathBuf> {
    dirs_from_dotgit_file(&path.join(".git")).map(|(g, _)| g)
}

/// `git worktree remove` without --force: git itself refuses anything dirty.
pub fn remove(common_dir: &Path, path: &Path) -> Result<(), String> {
    let gd = format!("--git-dir={}", common_dir.display());
    let out = Command::new("git")
        .args([&gd, "worktree", "remove", &path.display().to_string()])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn prune(common_dir: &Path) -> bool {
    let gd = format!("--git-dir={}", common_dir.display());
    git_ok(&[&gd, "worktree", "prune"])
}

/// From a worktree's `.git` file, the per-worktree git dir and the repo's common dir.
pub fn dirs_from_dotgit_file(dotgit: &Path) -> Option<(PathBuf, PathBuf)> {
    let text = std::fs::read_to_string(dotgit).ok()?;
    let raw = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let mut gitdir = PathBuf::from(raw);
    if gitdir.is_relative() {
        gitdir = dotgit.parent()?.join(gitdir);
    }
    // Linked worktrees live in <common>/worktrees/<name>; submodules in <common>/modules/<name>.
    let parent = gitdir.parent()?;
    if parent.file_name()? != "worktrees" {
        return None;
    }
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(c) => {
            let c = PathBuf::from(c.trim());
            if c.is_relative() { gitdir.join(c) } else { c }
        }
        Err(_) => parent.parent()?.to_path_buf(),
    };
    Some((gitdir, canon(&common)))
}

/// The real path, in a form git accepts: on Windows `canonicalize` returns verbatim `\\?\C:\…` paths,
/// which git can't use, so the prefix comes off again (`\\?\UNC\host\share` → `\\host\share`).
pub fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).map_or_else(|_| p.to_path_buf(), |c| plain(&c))
}

pub fn plain(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::plain;
    use std::path::{Path, PathBuf};

    #[test]
    fn verbatim_windows_paths_lose_their_prefix() {
        assert_eq!(
            plain(Path::new(r"\\?\C:\Users\me\x")),
            PathBuf::from(r"C:\Users\me\x")
        );
        assert_eq!(
            plain(Path::new(r"\\?\UNC\host\share\x")),
            PathBuf::from(r"\\host\share\x")
        );
        assert_eq!(
            plain(Path::new("/Users/me/x")),
            PathBuf::from("/Users/me/x")
        );
    }
}
