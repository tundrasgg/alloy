//! `line-endings`: no file is committed as text with CRLF line ends. The root
//! `.gitattributes` stores text as LF, so a CRLF in the index came from before
//! it or around it, and it breaks the scripts that read the file, as it broke
//! `packaging/set-version.sh` on `Cargo.toml`.

use super::{Rule, Violation};
use crate::tree::Tree;

pub(super) fn check(tree: &Tree) -> Vec<Violation> {
    tree.files()
        .filter(|path| tree.has_crlf(path))
        .map(|path| {
            Violation::new(
                Rule::LineEndings,
                path,
                None,
                format!(
                    "committed with CRLF line ends; store it as LF with `git add --renormalize {path}`"
                ),
            )
        })
        .collect()
}
