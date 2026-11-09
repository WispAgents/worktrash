//! Which folders may go. This is wisp's own cleanup policy (Wisp Agents, `plugins/worktrees`), the
//! "Clean up now" path, for any worktree on the machine:
//!
//! - Only the folder goes. The branch stays, so no commit is ever lost and the folder can come back
//!   with `git worktree add <path> <branch>`.
//! - A folder goes only when nothing lives in it alone: no changed, staged or untracked file, and HEAD
//!   still on its branch's tip (or, detached, on a commit some ref holds). Can't tell = keep.
//! - Kept whatever happens: in use (touched within the idle window, a day by default), an open pull
//!   request, `git worktree lock`, and folders another tool manages (Conductor's, wisp's own).
//! - Asked again right before removal; one that changed since the scan stays.
//! - With `--branches`, a branch goes too only when the default branch already has all its commits.

pub const HOUR_MS: i64 = 3_600_000;
pub const DAY_MS: i64 = 24 * HOUR_MS;

/// One worktree as the scan saw it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snap {
    pub exists: bool,
    /// managed by another tool: never removed
    pub owner: Option<&'static str>,
    pub locked: bool,
    /// None: git couldn't tell. Some(n): what only this folder holds.
    pub only: Option<u32>,
    /// Some(changed files) when the scan counted them (for the label).
    pub changed: Option<u32>,
    pub pr_open: Option<u32>,
    pub pr_ended: Option<(u32, bool)>,
    pub merged: bool,
    pub unpushed: Option<u32>,
    pub idle_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// the folder is gone; git still lists it (prune)
    Gone,
    Foreign(&'static str),
    Locked,
    PrOpen(u32),
    Unreadable,
    /// holds work: n paths and/or commits off its branch
    Holds(u32),
    InUse,
    /// clean and idle: may go
    Idle,
}

/// The first that applies wins, in wisp's order: never ours, in use, open PR, then what it holds.
pub fn standing(s: &Snap, min_idle_ms: i64) -> Standing {
    if !s.exists {
        return Standing::Gone;
    }
    if let Some(o) = s.owner {
        return Standing::Foreign(o);
    }
    if s.locked {
        return Standing::Locked;
    }
    if let Some(n) = s.pr_open {
        return Standing::PrOpen(n);
    }
    match s.only {
        None => Standing::Unreadable,
        Some(n) if n > 0 => Standing::Holds(n),
        Some(_) if s.idle_ms < min_idle_ms => Standing::InUse,
        Some(_) => Standing::Idle,
    }
}

pub fn removable(st: &Standing) -> bool {
    matches!(st, Standing::Idle)
}

/// The words wisp's Done dialog uses for a folder's state, first that applies.
pub fn label(s: &Snap, st: &Standing) -> String {
    match st {
        Standing::Gone => "folder gone (prune)".into(),
        Standing::Foreign(o) => format!("{o}'s, left alone"),
        Standing::Locked => "locked".into(),
        Standing::PrOpen(n) => format!("PR #{n} open"),
        Standing::Unreadable => "can't read it".into(),
        Standing::Holds(n) => match s.changed {
            Some(c) if c > 0 => {
                format!("{c} uncommitted {}", if c == 1 { "file" } else { "files" })
            }
            _ if *n > 0 => "commits off its branch".into(),
            _ => "holds work".into(),
        },
        Standing::InUse => "in use".into(),
        Standing::Idle => {
            if let Some((n, merged)) = s.pr_ended {
                format!("PR #{n} {}", if merged { "merged" } else { "closed" })
            } else if s.merged {
                "merged".into()
            } else if let Some(u) = s.unpushed.filter(|u| *u > 0) {
                format!(
                    "{u} {} not pushed (branch kept)",
                    if u == 1 { "commit" } else { "commits" }
                )
            } else {
                "clean".into()
            }
        }
    }
}

/// Folders other tools create and clean up themselves. wisp never touches Conductor's; worktrash
/// also leaves wisp's own to wisp's hourly cleanup.
pub fn owner_of(path: &str, home: &str) -> Option<&'static str> {
    let segs: Vec<&str> = path.split(['/', '\\']).collect();
    let home = home.trim_end_matches('/');
    if segs.contains(&".conductor")
        || (!home.is_empty() && path.starts_with(&format!("{home}/conductor/")))
    {
        return Some("Conductor");
    }
    if !home.is_empty() && path.starts_with(&format!("{home}/.wisp/worktrees/")) {
        return Some("wisp");
    }
    None
}

/// Delete the branch too? Only with `--branches` and when the default branch has every commit
/// (`rev-list --count default..branch` is 0). Can't tell: it stays.
pub fn branch_goes(asked: bool, unique: Option<u32>) -> bool {
    asked && unique == Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle() -> Snap {
        Snap {
            exists: true,
            only: Some(0),
            changed: Some(0),
            idle_ms: 3 * DAY_MS,
            ..Snap::default()
        }
    }

    #[test]
    fn only_clean_idle_folders_go() {
        let d = DAY_MS;
        assert_eq!(standing(&idle(), d), Standing::Idle);
        assert_eq!(
            standing(
                &Snap {
                    only: Some(2),
                    changed: Some(2),
                    ..idle()
                },
                d
            ),
            Standing::Holds(2)
        );
        assert_eq!(
            standing(
                &Snap {
                    only: None,
                    ..idle()
                },
                d
            ),
            Standing::Unreadable,
            "can't tell = keep"
        );
        assert_eq!(
            standing(
                &Snap {
                    idle_ms: HOUR_MS,
                    ..idle()
                },
                d
            ),
            Standing::InUse
        );
        assert_eq!(
            standing(
                &Snap {
                    pr_open: Some(7),
                    ..idle()
                },
                d
            ),
            Standing::PrOpen(7)
        );
        assert_eq!(
            standing(
                &Snap {
                    locked: true,
                    ..idle()
                },
                d
            ),
            Standing::Locked
        );
        assert_eq!(
            standing(
                &Snap {
                    owner: Some("Conductor"),
                    ..idle()
                },
                d
            ),
            Standing::Foreign("Conductor")
        );
        assert_eq!(
            standing(
                &Snap {
                    exists: false,
                    only: None,
                    ..idle()
                },
                d
            ),
            Standing::Gone
        );
    }

    #[test]
    fn labels_follow_wisps_words() {
        let d = DAY_MS;
        let s = Snap {
            only: Some(3),
            changed: Some(3),
            ..idle()
        };
        assert_eq!(label(&s, &standing(&s, d)), "3 uncommitted files");
        let s = Snap {
            only: Some(1),
            changed: Some(0),
            ..idle()
        };
        assert_eq!(label(&s, &standing(&s, d)), "commits off its branch");
        let s = Snap {
            pr_ended: Some((12, true)),
            ..idle()
        };
        assert_eq!(label(&s, &standing(&s, d)), "PR #12 merged");
        let s = Snap {
            merged: true,
            ..idle()
        };
        assert_eq!(label(&s, &standing(&s, d)), "merged");
        let s = Snap {
            unpushed: Some(2),
            ..idle()
        };
        assert_eq!(
            label(&s, &standing(&s, d)),
            "2 commits not pushed (branch kept)"
        );
        assert_eq!(label(&idle(), &Standing::Idle), "clean");
    }

    #[test]
    fn other_tools_folders_are_never_ours() {
        let h = "/Users/me";
        assert_eq!(
            owner_of("/Users/me/conductor/workspaces/app/x", h),
            Some("Conductor")
        );
        assert_eq!(owner_of("/src/app/.conductor/ws", h), Some("Conductor"));
        assert_eq!(owner_of("/Users/me/.wisp/worktrees/app-1", h), Some("wisp"));
        assert_eq!(owner_of("/Users/me/conductorish/x", h), None);
        assert_eq!(owner_of("/src/app/.claude/worktrees/x", h), None);
    }

    #[test]
    fn a_branch_goes_only_when_nothing_is_only_on_it() {
        assert!(!branch_goes(false, Some(0)));
        assert!(branch_goes(true, Some(0)));
        assert!(!branch_goes(true, Some(2)));
        assert!(!branch_goes(true, None), "can't tell: it stays");
    }

    #[test]
    fn prop_nothing_holding_work_or_in_use_ever_goes() {
        // A small xorshift so the property runs without a dependency.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut r = |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        for _ in 0..20_000 {
            let only = match r(3) {
                0 => None,
                1 => Some(0),
                _ => Some(u32::try_from(r(4)).unwrap_or(1)),
            };
            let s = Snap {
                exists: r(5) > 0,
                owner: (r(5) == 0).then_some("Conductor"),
                locked: r(6) == 0,
                only,
                changed: only,
                pr_open: (r(4) == 0).then_some(1),
                pr_ended: None,
                merged: r(2) == 0,
                unpushed: None,
                idle_ms: i64::try_from(r(5)).unwrap_or(0) * DAY_MS / 2,
            };
            if removable(&standing(&s, DAY_MS)) {
                assert!(s.exists && s.owner.is_none() && !s.locked && s.pr_open.is_none());
                assert_eq!(s.only, Some(0));
                assert!(s.idle_ms >= DAY_MS);
            }
        }
    }
}
