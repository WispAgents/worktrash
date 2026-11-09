//! worktrash: find the git worktrees your coding agents left behind, see what they cost on disk, and
//! remove the ones that hold nothing, with the same rules wisp (Wisp Agents) uses for its own.

mod git;
mod policy;
mod scan;
mod size;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use policy::{Snap, Standing};

const HELP: &str = "worktrash: the git worktrees your coding agents left behind

USAGE
  worktrash [PATH...] [OPTIONS]

Scans PATH (default: your home folder) for git worktrees, shows what each one holds and how much
disk it uses, and with --clean removes the folders that hold nothing. Branches stay, so no commit is
lost: `git worktree add <path> <branch>` brings a folder back.

OPTIONS
  --clean          remove the folders marked `will go` (asks first)
  -y, --yes        don't ask (needed when not in a terminal)
  --branches       also delete a removed folder's branch when the default branch has all its commits
  --idle <DAYS>    a folder touched more recently is in use and stays (default: 1)
  --depth <N>      how deep to look for repositories (default: 8)
  --all            list every worktree, not only the ones that can go
  --json           print JSON instead of a table
  -h, --help       this help
  -V, --version    the version";

#[allow(
    clippy::struct_excessive_bools,
    reason = "one field per command-line flag"
)]
struct Args {
    roots: Vec<PathBuf>,
    clean: bool,
    yes: bool,
    branches: bool,
    idle_days: f64,
    depth: usize,
    all: bool,
    json: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        roots: Vec::new(),
        clean: false,
        yes: false,
        branches: false,
        idle_days: 1.0,
        depth: 8,
        all: false,
        json: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--clean" => a.clean = true,
            "-y" | "--yes" => a.yes = true,
            "--branches" => a.branches = true,
            "--all" => a.all = true,
            "--json" => a.json = true,
            "--idle" => {
                let v = it.next().ok_or("--idle needs a number of days")?;
                a.idle_days = v
                    .parse()
                    .map_err(|_| format!("--idle: not a number: {v}"))?;
                if a.idle_days < 0.0 {
                    return Err("--idle can't be negative".into());
                }
            }
            "--depth" => {
                let v = it.next().ok_or("--depth needs a number")?;
                a.depth = v
                    .parse()
                    .map_err(|_| format!("--depth: not a number: {v}"))?;
            }
            "-h" | "--help" => {
                println!("{HELP}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("worktrash {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            s if s.starts_with('-') => return Err(format!("unknown option {s} (see --help)")),
            p => a.roots.push(PathBuf::from(p)),
        }
    }
    if a.roots.is_empty() {
        a.roots
            .push(home().ok_or("no home folder: pass a path to scan")?);
    }
    for r in &a.roots {
        if !r.is_dir() {
            return Err(format!("not a folder: {}", r.display()));
        }
    }
    Ok(a)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "days typed on the command line, far inside i64"
)]
fn days_to_ms(days: f64) -> i64 {
    (days * policy::DAY_MS as f64) as i64
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn mtime_ms(p: &Path) -> Option<i64> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(i64::try_from(t.duration_since(UNIX_EPOCH).ok()?.as_millis()).unwrap_or(i64::MAX))
}

/// The last time anyone worked in the folder: its own mtime, or its index, HEAD or reflog moving.
fn touched_ms(path: &Path) -> i64 {
    let mut t = mtime_ms(path).unwrap_or(0);
    if let Some(gd) = git::worktree_gitdir(path) {
        for f in ["index", "HEAD", "logs/HEAD"] {
            t = t.max(mtime_ms(&gd.join(f)).unwrap_or(0));
        }
    }
    t
}

struct Row {
    repo: PathBuf,
    common: PathBuf,
    path: PathBuf,
    branch: Option<String>,
    snap: Snap,
    standing: Standing,
    label: String,
    size: u64,
}

struct RepoInfo {
    common: PathBuf,
    root: PathBuf,
    targets: Vec<String>,
    prs: Vec<(String, u32, String)>,
}

fn repo_root(common: &Path) -> PathBuf {
    if common.file_name().is_some_and(|n| n == ".git") {
        common
            .parent()
            .map_or_else(|| common.to_path_buf(), Path::to_path_buf)
    } else {
        common.to_path_buf() // bare repo
    }
}

fn look(info: &RepoInfo, wt: &git::Listed, home: &str, min_idle_ms: i64, now: i64) -> Row {
    let exists = wt.path.is_dir() && !wt.prunable;
    let path_s = wt.path.display().to_string();
    let mut snap = Snap {
        exists,
        locked: wt.locked,
        owner: policy::owner_of(&path_s, home),
        ..Snap::default()
    };
    if exists {
        snap.idle_ms = now - touched_ms(&wt.path);
        snap.only = git::folder_only(&info.common, &wt.path, wt.branch.as_deref());
        snap.changed = count_changed(&wt.path);
        if let Some(b) = &wt.branch {
            let mine: Vec<_> = info.prs.iter().filter(|(h, _, _)| h == b).collect();
            snap.pr_open = mine
                .iter()
                .find(|(_, _, s)| s == "OPEN")
                .map(|(_, n, _)| *n);
            snap.pr_ended = mine
                .iter()
                .find(|(_, _, s)| s == "MERGED" || s == "CLOSED")
                .map(|(_, n, s)| (*n, s == "MERGED"));
            snap.unpushed = git::unpushed(&info.common, b);
        }
        if let Some(h) = &wt.head {
            snap.merged = git::merged(&info.common, h, &info.targets) != git::Merged::No;
        }
    }
    let standing = policy::standing(&snap, min_idle_ms);
    let label = policy::label(&snap, &standing);
    // Every folder counts toward the total, even another tool's we'll never remove.
    let size = if exists {
        size::disk_usage(&wt.path)
    } else {
        0
    };
    Row {
        repo: info.root.clone(),
        common: info.common.clone(),
        path: wt.path.clone(),
        branch: wt.branch.clone(),
        snap,
        standing,
        label,
        size,
    }
}

fn count_changed(path: &Path) -> Option<u32> {
    let out = std::process::Command::new("git")
        .args([
            "-C",
            &path.display().to_string(),
            "status",
            "--porcelain",
            "--untracked-files=normal",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| {
        u32::try_from(String::from_utf8_lossy(&out.stdout).lines().count()).unwrap_or(u32::MAX)
    })
}

/// Runs `f` over `items` on every core.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = Mutex::new(0usize);
    let out: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::with_capacity(items.len()));
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZero::get)
        .min(12)
        .min(items.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = {
                        let Ok(mut n) = next.lock() else { break };
                        let i = *n;
                        *n += 1;
                        i
                    };
                    let Some(item) = items.get(i) else { break };
                    let r = f(item);
                    if let Ok(mut o) = out.lock() {
                        o.push((i, r));
                    }
                }
            });
        }
    });
    let mut v = out.into_inner().unwrap_or_default();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, r)| r).collect()
}

fn tilde(p: &Path, home: &str) -> String {
    let s = p.display().to_string();
    match s.strip_prefix(home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => s,
    }
}

fn idle_text(ms: i64) -> String {
    let h = ms / policy::HOUR_MS;
    if h < 1 {
        "now".into()
    } else if h < 48 {
        format!("{h}h")
    } else {
        format!("{}d", h / 24)
    }
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

#[allow(
    clippy::too_many_lines,
    reason = "one straight pass: scan, print, then clean"
)]
fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("worktrash: {e}");
            std::process::exit(2);
        }
    };
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("worktrash: git isn't installed or isn't on PATH");
        std::process::exit(2);
    }
    let home = home()
        .map(|h| h.display().to_string().trim_end_matches('/').to_string())
        .unwrap_or_default();
    let tty = std::io::stdout().is_terminal();
    let roots: Vec<PathBuf> = args.roots.iter().map(|r| git::canon(r)).collect();

    if tty && !args.json {
        eprint!(
            "looking for worktrees under {} …",
            roots
                .iter()
                .map(|r| tilde(r, &home))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let repos: Vec<PathBuf> = scan::find_repos(&roots, args.depth).into_iter().collect();
    let infos: Vec<RepoInfo> = par_map(&repos, |c| {
        let root = repo_root(c);
        RepoInfo {
            common: c.clone(),
            targets: git::targets(c),
            prs: git::pull_requests(&root),
            root,
        }
    });
    let jobs: Vec<(usize, git::Listed)> = infos
        .iter()
        .enumerate()
        .flat_map(|(i, info)| {
            git::list_worktrees(&info.common)
                .into_iter()
                .map(move |w| (i, w))
        })
        .collect();
    let min_idle_ms = days_to_ms(args.idle_days);
    let now = now_ms();
    let mut rows: Vec<Row> = par_map(&jobs, |(i, w)| look(&infos[*i], w, &home, min_idle_ms, now));
    rows.sort_by(|a, b| b.size.cmp(&a.size).then(a.path.cmp(&b.path)));
    if tty && !args.json {
        eprint!("\r\x1b[2K");
    }

    let total: u64 = rows.iter().map(|r| r.size).sum();
    let going: Vec<&Row> = rows
        .iter()
        .filter(|r| policy::removable(&r.standing))
        .collect();
    let going_size: u64 = going.iter().map(|r| r.size).sum();
    let gone_entries = rows.iter().filter(|r| r.standing == Standing::Gone).count();
    let repo_count = rows.iter().map(|r| &r.repo).collect::<BTreeSet<_>>().len();

    if args.json {
        print_json(&rows, total, going_size);
    } else {
        print_table(&rows, args.all, &home, tty);
        println!();
        if rows.is_empty() {
            println!("no linked worktrees found.");
            return;
        }
        println!(
            "{} worktrees in {} repos use {}. {} of them ({}) hold nothing and can go.{}",
            rows.len(),
            repo_count,
            size::human(total),
            going.len(),
            size::human(going_size),
            if gone_entries > 0 {
                format!(" {gone_entries} more are already gone from disk.")
            } else {
                String::new()
            }
        );
        if !args.clean && (!going.is_empty() || gone_entries > 0) {
            println!("run `worktrash --clean` to remove them. branches stay.");
        }
    }
    if !args.clean {
        return;
    }
    if going.is_empty() && gone_entries == 0 {
        return;
    }
    if !args.yes {
        if !std::io::stdin().is_terminal() {
            eprintln!("worktrash: not a terminal; pass --yes to remove without asking");
            std::process::exit(1);
        }
        print!(
            "remove {} folders ({})? branches stay. [y/N] ",
            going.len(),
            size::human(going_size)
        );
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("nothing removed.");
            return;
        }
    }
    clean(&rows, &going, &infos, &args, &home, min_idle_ms);
}

fn clean(
    rows: &[Row],
    going: &[&Row],
    infos: &[RepoInfo],
    args: &Args,
    home: &str,
    min_idle_ms: i64,
) {
    let mut removed = 0usize;
    let mut freed = 0u64;
    let mut branches = 0usize;
    for r in going {
        // Ask again right before it goes: someone may have started working in it since the scan.
        let still = git::folder_only(&r.common, &r.path, r.branch.as_deref()) == Some(0)
            && now_ms() - touched_ms(&r.path) >= min_idle_ms;
        if !still {
            println!("kept  {} (changed since the scan)", tilde(&r.path, home));
            continue;
        }
        match git::remove(&r.common, &r.path) {
            Ok(()) => {
                removed += 1;
                freed += r.size;
                if let Some(b) = &r.branch {
                    let info = infos.iter().find(|i| i.common == r.common);
                    let unique = info
                        .and_then(|i| i.targets.first())
                        .and_then(|t| git::unique_commits(&r.common, b, t));
                    if policy::branch_goes(args.branches, unique)
                        && git::delete_branch(&r.common, b)
                    {
                        branches += 1;
                    }
                }
            }
            Err(e) => println!("kept  {} ({e})", tilde(&r.path, home)),
        }
    }
    // Folders already gone: only git's record of them is left.
    let mut pruned: BTreeSet<&Path> = BTreeSet::new();
    for r in rows.iter().filter(|r| r.standing == Standing::Gone) {
        if pruned.insert(r.common.as_path()) {
            git::prune(&r.common);
        }
    }
    let gone = rows.iter().filter(|r| r.standing == Standing::Gone).count();
    print!(
        "removed {removed} {} · {} freed",
        if removed == 1 { "folder" } else { "folders" },
        size::human(freed)
    );
    if gone > 0 {
        print!(
            " · {gone} stale {} pruned",
            if gone == 1 { "entry" } else { "entries" }
        );
    }
    if branches > 0 {
        print!(
            " · {branches} merged {} deleted",
            if branches == 1 { "branch" } else { "branches" }
        );
    }
    println!();
}

fn print_table(rows: &[Row], all: bool, home: &str, tty: bool) {
    let shown: Vec<&Row> = rows
        .iter()
        .filter(|r| all || policy::removable(&r.standing))
        .collect();
    let hidden = rows.len() - shown.len();
    if shown.is_empty() {
        if hidden > 0 {
            println!(
                "nothing can go: every worktree holds work, is in use, or belongs to another tool. --all lists them."
            );
        }
        return;
    }
    let (dim, green, reset) = if tty {
        ("\x1b[2m", "\x1b[32m", "\x1b[0m")
    } else {
        ("", "", "")
    };
    println!(
        "{dim}{:>8}  {:>5}  {:<8}  {:<36}  PATH{reset}",
        "SIZE", "IDLE", "", "STATE"
    );
    for r in shown {
        let go = policy::removable(&r.standing);
        let mark = if go {
            format!("{green}will go{reset} ")
        } else {
            "        ".into()
        };
        let size = if r.snap.exists {
            size::human(r.size)
        } else {
            "-".into()
        };
        let idle = if r.snap.exists {
            idle_text(r.snap.idle_ms)
        } else {
            "-".into()
        };
        println!(
            "{size:>8}  {idle:>5}  {mark} {:<36}  {}",
            r.label,
            tilde(&r.path, home)
        );
    }
    if hidden > 0 {
        println!(
            "{dim}+ {hidden} kept (hold work, in use, open PR, or another tool's). --all lists them.{reset}"
        );
    }
}

fn print_json(rows: &[Row], total: u64, going: u64) {
    let mut o = String::from("{\"worktrees\":[");
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        let _ = write!(
            o,
            "{{\"path\":{},\"repo\":{},\"branch\":{},\"bytes\":{},\"idle_hours\":{},\"state\":{},\"can_go\":{}}}",
            json_str(&r.path.display().to_string()),
            json_str(&r.repo.display().to_string()),
            r.branch.as_deref().map_or("null".into(), json_str),
            r.size,
            if r.snap.exists {
                (r.snap.idle_ms / policy::HOUR_MS).to_string()
            } else {
                "null".into()
            },
            json_str(&r.label),
            policy::removable(&r.standing),
        );
    }
    let _ = write!(o, "],\"total_bytes\":{total},\"can_go_bytes\":{going}}}");
    println!("{o}");
}
