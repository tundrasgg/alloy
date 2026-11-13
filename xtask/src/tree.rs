//! The set of files the repository rules run over.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory and the files in it, as relative paths with `/` separators.
#[derive(Debug, Clone)]
pub(crate) struct Tree {
    root: PathBuf,
    files: BTreeSet<String>,
}

impl Tree {
    /// The files git would put in a commit from `root`: tracked files that still
    /// exist and untracked files that `.gitignore` does not exclude. Paths that
    /// start with one of `exclude` are left out.
    ///
    /// Fails when git cannot be run or exits non-zero.
    pub(crate) fn from_git(root: &Path, exclude: &[&str]) -> Result<Self, String> {
        let listing = git(
            root,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        )?;
        let files = listing
            .split('\0')
            .filter(|path| !path.is_empty())
            .filter(|path| !exclude.iter().any(|prefix| path.starts_with(prefix)))
            .filter(|path| root.join(path).is_file())
            .map(str::to_owned)
            .collect();
        Ok(Self {
            root: root.to_path_buf(),
            files,
        })
    }

    /// Every file below `root`, found by walking the directory. The rule
    /// fixtures use it, because they are not git repositories.
    ///
    /// Fails when a directory cannot be read.
    #[cfg(test)]
    pub(crate) fn from_walk(root: &Path) -> Result<Self, String> {
        let mut files = BTreeSet::new();
        let mut pending = vec![(root.to_path_buf(), String::new())];
        while let Some((dir, prefix)) = pending.pop() {
            let entries = fs::read_dir(&dir)
                .map_err(|err| format!("could not read `{}`: {err}", dir.display()))?;
            for entry in entries {
                let entry =
                    entry.map_err(|err| format!("could not read `{}`: {err}", dir.display()))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let rel = format!("{prefix}{name}");
                let path = entry.path();
                if path.is_dir() {
                    pending.push((path, format!("{rel}/")));
                } else {
                    files.insert(rel);
                }
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
            files,
        })
    }

    /// Every file, in byte order of its path.
    pub(crate) fn files(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(String::as_str)
    }

    /// Every file whose path starts with `dir`, which should end with `/`.
    pub(crate) fn under<'a>(&'a self, dir: &'a str) -> impl Iterator<Item = &'a str> {
        self.files
            .range(dir.to_owned()..)
            .map(String::as_str)
            .take_while(move |path| path.starts_with(dir))
    }

    /// True when `path` is a file in the tree, or a directory that holds one.
    /// The empty path is the root and always exists.
    pub(crate) fn contains(&self, path: &str) -> bool {
        let path = path.trim_end_matches('/');
        if path.is_empty() || self.files.contains(path) {
            return true;
        }
        let dir = format!("{path}/");
        self.under(&dir).next().is_some()
    }

    /// Reads a file as text. Bytes that are not UTF-8 are replaced.
    pub(crate) fn read(&self, path: &str) -> Result<String, String> {
        fs::read(self.root.join(path))
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|err| format!("could not read the file: {err}"))
    }
}

/// Runs git in `root` and returns its standard output.
///
/// Fails when git cannot be started or exits non-zero.
pub(crate) fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|err| format!("could not start `git {}`: {err}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "`git {}` exited with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[&str]) -> Tree {
        Tree {
            root: PathBuf::new(),
            files: files.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    #[test]
    fn contains_finds_files_and_directories() {
        let t = tree(&["a/b.md", "a/c/d.md", "ab.md"]);
        assert!(t.contains("a/b.md"));
        assert!(t.contains("a"));
        assert!(t.contains("a/c/"));
        assert!(t.contains(""));
        assert!(!t.contains("a/c/e.md"));
        assert!(!t.contains("a/b"));
    }

    #[test]
    fn under_stops_at_the_prefix() {
        let t = tree(&["a/b.md", "a/c/d.md", "ab.md"]);
        assert_eq!(t.under("a/").collect::<Vec<_>>(), ["a/b.md", "a/c/d.md"]);
    }
}
