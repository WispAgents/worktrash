//! Disk usage of a directory tree: blocks actually allocated (like `du`), symlinks not followed,
//! and a hard-linked file counted once per tree (pnpm and some caches hard-link heavily).

use std::collections::HashSet;
use std::fs;
use std::path::Path;

pub fn disk_usage(root: &Path) -> u64 {
    let mut seen = HashSet::new();
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(meta) = e.metadata() else { continue }; // DirEntry::metadata never follows symlinks
            if meta.is_dir() {
                stack.push(e.path());
            }
            total += allocated(&meta, &mut seen);
        }
    }
    total
}

#[cfg(unix)]
fn allocated(meta: &fs::Metadata, seen: &mut HashSet<(u64, u64)>) -> u64 {
    use std::os::unix::fs::MetadataExt;
    if meta.nlink() > 1 && !meta.is_dir() && !seen.insert((meta.dev(), meta.ino())) {
        return 0;
    }
    meta.blocks() * 512
}

#[cfg(not(unix))]
fn allocated(meta: &fs::Metadata, _seen: &mut HashSet<(u64, u64)>) -> u64 {
    meta.len()
}

/// 1.2 GB / 340 MB / 12 KB, decimal units like Finder and most disk tools.
pub fn human(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    #[allow(
        clippy::cast_precision_loss,
        reason = "a display value with one decimal"
    )]
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else if v < 10.0 {
        format!("{v:.1} {}", U[i])
    } else {
        format!("{v:.0} {}", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::human;

    #[test]
    fn human_sizes() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(999), "999 B");
        assert_eq!(human(1_500), "1.5 KB");
        assert_eq!(human(37_200_000_000), "37 GB");
        assert_eq!(human(2_340_000_000), "2.3 GB");
    }
}
