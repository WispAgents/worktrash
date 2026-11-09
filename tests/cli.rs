//! End to end: real git repos in a temp folder, the real binary, every state the policy knows.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("worktrash-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    let c = std::fs::canonicalize(&d).expect("canon");
    // git can't use Windows' verbatim `\\?\C:\…` form
    let s = c.to_string_lossy();
    s.strip_prefix(r"\\?\")
        .map_or_else(|| c.clone(), PathBuf::from)
}

fn commit(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).expect("write");
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", file]);
}

/// Makes a folder look untouched for `days`: the folder and its git dir files.
fn age(path: &Path, days: u64) {
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
    let mut files = vec![path.to_path_buf()];
    let dotgit = std::fs::read_to_string(path.join(".git")).expect(".git file");
    let gd = PathBuf::from(dotgit.trim().trim_start_matches("gitdir:").trim());
    for f in ["index", "HEAD", "logs/HEAD"] {
        files.push(gd.join(f));
    }
    for f in files {
        if let Ok(file) = std::fs::File::options()
            .write(true)
            .open(&f)
            .or_else(|_| std::fs::File::open(&f))
        {
            let _ = file.set_modified(t);
        }
    }
}

fn worktrash(root: &Path, extra: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_worktrash"))
        .arg(root)
        .args(extra)
        .output()
        .expect("runs");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.success(),
    )
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario with every state side by side"
)]
fn clean_removes_only_what_holds_nothing_and_keeps_every_branch() {
    let root = tmp("e2e");
    let repo = root.join("app");
    std::fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, "a.txt", "a");
    let wts = root.join("wts");

    // merged: a fast-forwarded branch
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "merged",
            wts.join("merged").to_str().expect("p"),
        ],
    );
    commit(&wts.join("merged"), "m.txt", "m");
    // ignored build output counts toward the size but isn't work
    commit(&wts.join("merged"), ".gitignore", "node_modules/\n");
    std::fs::create_dir_all(wts.join("merged").join("node_modules")).expect("nm");
    std::fs::write(
        wts.join("merged").join("node_modules").join("big.bin"),
        vec![7u8; 200_000],
    )
    .expect("big");
    git(&repo, &["merge", "-q", "--ff-only", "merged"]);
    // squashed: its diff landed on main as one new commit
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "squashed",
            wts.join("squashed").to_str().expect("p"),
        ],
    );
    commit(&wts.join("squashed"), "s1.txt", "1");
    commit(&wts.join("squashed"), "s2.txt", "2");
    git(&repo, &["merge", "-q", "--squash", "squashed"]);
    git(&repo, &["commit", "-qm", "squash"]);
    // unmerged but clean: its commits are on its branch, so the folder may go
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "unmerged",
            wts.join("unmerged").to_str().expect("p"),
        ],
    );
    commit(&wts.join("unmerged"), "u.txt", "u");
    // dirty: an untracked file only it holds
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "dirty",
            wts.join("dirty").to_str().expect("p"),
        ],
    );
    std::fs::write(wts.join("dirty").join("notes.md"), "mine").expect("write");
    // detached with a commit no ref holds
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            wts.join("detached").to_str().expect("p"),
        ],
    );
    commit(&wts.join("detached"), "d.txt", "d");
    // in use: clean but touched just now
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "busy",
            wts.join("busy").to_str().expect("p"),
        ],
    );
    // gone: the folder was deleted by hand
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "gone",
            wts.join("gone").to_str().expect("p"),
        ],
    );
    std::fs::remove_dir_all(wts.join("gone")).expect("rm");
    for name in ["merged", "squashed", "unmerged", "dirty", "detached"] {
        age(&wts.join(name), 3);
    }

    let (list, ok) = worktrash(&root, &["--all"]);
    assert!(ok, "{list}");
    assert!(list.contains("1 uncommitted file"), "{list}");
    assert!(list.contains("commits off its branch"), "{list}");
    assert!(list.contains("in use"), "{list}");
    assert!(list.contains("folder gone"), "{list}");
    let line = |name: &str| {
        list.lines()
            .find(|l| l.ends_with(&format!("/{name}")) || l.ends_with(&format!("\\{name}")))
            .unwrap_or_default()
            .to_string()
    };
    assert!(
        line("merged").contains("will go") && line("merged").contains("merged"),
        "{list}"
    );
    assert!(
        line("squashed").contains("will go") && line("squashed").contains("merged"),
        "squash merge seen: {list}"
    );
    assert!(
        line("unmerged").contains("will go") && line("unmerged").contains("clean"),
        "{list}"
    );
    assert!(!line("dirty").contains("will go"), "{list}");
    assert!(!line("detached").contains("will go"), "{list}");
    assert!(!line("busy").contains("will go"), "{list}");
    assert!(list.contains("3 of them"), "{list}");

    let (json, ok) = worktrash(&root, &["--json"]);
    assert!(
        ok && json.starts_with("{\"worktrees\":[") && json.contains("\"can_go\":true"),
        "{json}"
    );

    let (out, ok) = worktrash(&root, &["--clean", "--yes", "--branches"]);
    assert!(ok, "{out}");
    assert!(out.contains("removed 3 folders"), "{out}");
    assert!(out.contains("1 stale entry pruned"), "{out}");
    assert!(
        !wts.join("merged").exists()
            && !wts.join("squashed").exists()
            && !wts.join("unmerged").exists()
    );
    for keep in ["dirty", "detached", "busy"] {
        assert!(wts.join(keep).exists(), "{keep} stays");
    }
    let branches = git(&repo, &["branch", "--format=%(refname:short)"]);
    assert!(
        !branches.lines().any(|b| b == "merged"),
        "a branch main fully has goes with --branches: {branches}"
    );
    assert!(
        branches.lines().any(|b| b == "squashed"),
        "a squashed branch stays (its commits aren't on main): {branches}"
    );
    assert!(
        branches.lines().any(|b| b == "unmerged"),
        "an unmerged branch always stays: {branches}"
    );
    let listed = git(&repo, &["worktree", "list", "--porcelain"]);
    assert!(!listed.contains("/gone"), "pruned: {listed}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn without_yes_and_no_terminal_it_refuses_to_remove() {
    let root = tmp("noyes");
    let repo = root.join("app");
    std::fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, "a.txt", "a");
    let wt = root.join("w");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "x", wt.to_str().expect("p")],
    );
    age(&wt, 3);
    let out = Command::new(env!("CARGO_BIN_EXE_worktrash"))
        .arg(&root)
        .arg("--clean")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("runs");
    assert!(!out.status.success());
    assert!(wt.exists(), "nothing removed without --yes");
    let _ = std::fs::remove_dir_all(&root);
}
