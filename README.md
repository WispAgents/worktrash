# worktrash

**The git worktrees your coding agents left behind.**

Claude Code, Codex, Cursor and friends work in git worktrees: one checkout per task, each with its own
`node_modules` and build output. The task ends, the folder stays. A few weeks later you have hundreds of them.

`worktrash` finds every worktree on your machine, tells you what each one still holds and how much disk it
uses, and removes the ones that hold nothing. **Branches always stay**, so no commit is ever lost.

```console
$ worktrash
    SIZE   IDLE            STATE                                 PATH
  110 MB     2d  will go  PR #695 merged                        ~/code/app-channels-share
   75 MB     2d  will go  merged                                ~/code/app-grid
   30 MB    40h  will go  64 commits not pushed (branch kept)   ~/code/app-fix2
   29 MB    41h  will go  clean                                 /tmp/app-750
   …
+ 167 kept (hold work, in use, open PR, or another tool's). --all lists them.

445 worktrees in 35 repos use 8.0 GB. 278 of them (1.4 GB) hold nothing and can go. 43 more are already gone from disk.
run `worktrash --clean` to remove them. branches stay.
```

That's a real run on one of our machines (folder names shortened). Run it on yours: what's your number?

## Install

Download a binary for macOS, Linux or Windows from the
[latest release](https://github.com/WispAgents/worktrash/releases/latest), or build it with Rust:

```sh
cargo install --git https://github.com/WispAgents/worktrash
```

One binary, no dependencies, needs `git` on your PATH. `gh` is optional: when it's installed and signed
in, pull requests are read from GitHub too.

## Use

```sh
worktrash                 # scan your home folder, list what can go
worktrash ~/code          # scan somewhere else
worktrash --all           # list every worktree, kept ones too, with why
worktrash --clean         # remove the ones marked "will go" (asks first)
worktrash --clean --yes --branches   # no question; also delete branches main already has
worktrash --json          # for scripts and agents
```

| Option | |
|---|---|
| `--clean` | remove the folders marked `will go`, asking first |
| `-y`, `--yes` | don't ask (required when not in a terminal) |
| `--branches` | also delete a removed folder's branch, only when the default branch has every commit on it |
| `--idle <DAYS>` | a folder touched more recently counts as in use and stays (default `1`) |
| `--depth <N>` | how deep to look for repositories (default `8`) |
| `--all` | list kept worktrees too |
| `--json` | JSON output |

## What goes and what stays

These are the rules [wisp](https://github.com/WispAgents/wisp) uses to clean up after its own agents,
for any worktree on your machine.

**A folder goes only when nothing lives in it alone:**

- no modified, staged or untracked file (ignored files like `node_modules` and `target` don't count: they
  can be rebuilt);
- HEAD is still on its branch's tip, so every commit is on the branch. A detached HEAD must be on a commit
  some branch, tag or remote already holds;
- and git could tell. When it can't read the folder, it stays.

**It stays, whatever else is true, when:**

- it was touched in the last day (`--idle`): someone, or some agent, is probably still in it;
- its branch has an open pull request;
- it's locked (`git worktree lock`);
- another tool manages it: Conductor's workspaces, and wisp's own folders (wisp cleans those itself).

**When it goes:**

- only the folder goes, with `git worktree remove` (never `--force`). The branch stays: `git worktree add
  <path> <branch>` brings it back;
- each folder is checked again right before it goes, and one that changed since the scan stays;
- entries for folders already deleted by hand are pruned (`git worktree prune`);
- with `--branches`, a branch is deleted only when the default branch already has all of its commits.

The STATE column says why: `3 uncommitted files`, `commits off its branch`, `PR #12 open`,
`PR #12 merged`, `merged` (also squash merges, which `git branch --merged` misses),
`2 commits not pushed (branch kept)`, `clean`, `in use`, `locked`, `Conductor's, left alone`.

## How it works

It walks the folders you give it (in parallel, never following symlinks, skipping `node_modules`,
`target`, caches and the like), finds every repository that has linked worktrees, and asks git about
each one. Sizes are disk blocks actually used, like `du`, with hard-linked files counted once. Nothing is
written to your repos except the removals you ask for (and a throwaway object `git gc` drops, from the
squash-merge check).

## About

Made by the team building [wisp](https://github.com/WispAgents/wisp), which runs your coding agents as
code and keeps every task they did next to the code they wrote. Follow along at
[@trywispagents](https://x.com/trywispagents).

MIT licensed.
