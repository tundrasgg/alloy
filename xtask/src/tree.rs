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
    /// The files committed as text with CRLF line ends, [`Tree::has_crlf`].
    crlf: BTreeSet<String>,
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
        let files: BTreeSet<String> = listing
            .split('\0')
            .filter(|path| !path.is_empty())
            .filter(|path| !exclude.iter().any(|prefix| path.starts_with(prefix)))
            .filter(|path| root.join(path).is_file())
            .map(str::to_owned)
            .collect();
        let eol = git(root, &["ls-files", "-z", "--eol", "--cached"])?;
        let crlf = crlf_in_index(&eol)
            .filter(|path| files.contains(*path))
            .map(str::to_owned)
            .collect();
        Ok(Self {
            root: root.to_path_buf(),
            files,
            crlf,
        })
    }

    /// Every file below `root`, found by walking the directory. The rule
    /// fixtures use it, because they are not git repositories.
    ///
    /// Fails when a directory cannot be read.
    #[cfg(test)]
    pub(crate) fn from_walk(root: &Path) -> Result<Self, String> {
        let mut files = BTreeSet::new();
        let mut crlf = BTreeSet::new();
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
                    let bytes = fs::read(&path)
                        .map_err(|err| format!("could not read `{}`: {err}", path.display()))?;
                    if is_crlf_text(&bytes) {
                        crlf.insert(rel.clone());
                    }
                    files.insert(rel);
                }
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
            files,
            crlf,
        })
    }

    /// True when `path` is text committed with CRLF line ends, in all or some
    /// of its lines. For a tree from git this is the copy in the index, as
    /// `git ls-files --eol` classes it (`i/crlf` or `i/mixed`), whatever the
    /// working tree holds. For a walked tree it is the file's content, judged
    /// by [`is_crlf_text`].
    pub(crate) fn has_crlf(&self, path: &str) -> bool {
        self.crlf.contains(path)
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

/// The paths that the output of `git ls-files -z --eol` classes as text with
/// CRLF line ends in the index. Each record is the index class, the working
/// tree class, and the attributes, then a tab and the path.
fn crlf_in_index(listing: &str) -> impl Iterator<Item = &str> {
    listing.split('\0').filter_map(|record| {
        let (info, path) = record.split_once('\t')?;
        let index = info.split_whitespace().next()?;
        matches!(index, "i/crlf" | "i/mixed").then_some(path)
    })
}

/// True when `bytes` hold at least one CRLF and are text by git's test for
/// `text=auto`: no NUL byte, and no CR that is not followed by LF.
#[cfg(test)]
fn is_crlf_text(bytes: &[u8]) -> bool {
    let mut crlf = false;
    for (i, &byte) in bytes.iter().enumerate() {
        match byte {
            0 => return false,
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => crlf = true,
            b'\r' => return false,
            _ => {}
        }
    }
    crlf
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

/// The text of `path` as it is on `rev`, or `None` when `rev` has no file there.
///
/// Fails when git fails, for example because `rev` does not exist.
pub(crate) fn file_at(root: &Path, rev: &str, path: &str) -> Result<Option<String>, String> {
    let listed = git(root, &["ls-tree", "--name-only", rev, "--", path])?;
    if listed.trim().is_empty() {
        return Ok(None);
    }
    git(root, &["show", &format!("{rev}:{path}")]).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[&str]) -> Tree {
        Tree {
            root: PathBuf::new(),
            files: files.iter().map(|f| (*f).to_owned()).collect(),
            crlf: BTreeSet::new(),
        }
    }

    #[test]
    fn crlf_in_index_reads_the_index_class_only() {
        let listing = concat!(
            "i/crlf  w/lf    attr/text=auto eol=lf \tCargo.toml\0",
            "i/mixed w/mixed attr/                 \tdocs/a b.md\0",
            "i/lf    w/crlf  attr/text=auto eol=lf \tsrc/lib.rs\0",
            "i/-text w/-text attr/-text            \tseeds/x\0",
            "i/none  w/none  attr/                 \tempty\0",
        );
        assert_eq!(
            crlf_in_index(listing).collect::<Vec<_>>(),
            ["Cargo.toml", "docs/a b.md"]
        );
    }

    #[test]
    fn crlf_text_is_text_with_a_crlf() {
        assert!(is_crlf_text(b"a\r\nb\r\n"));
        assert!(is_crlf_text(b"a\nb\r\nc\n"));
        assert!(!is_crlf_text(b"a\nb\n"));
        assert!(!is_crlf_text(b""));
        assert!(!is_crlf_text(b"a\r\n\0"));
        assert!(!is_crlf_text(b"a\r\nb\rc"));
        assert!(!is_crlf_text(b"a\r"));
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
