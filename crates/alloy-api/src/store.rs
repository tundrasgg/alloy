//! The resource store as the API server reads it.

use std::fmt;

use alloy_proto::cluster::v1alpha1::Node;
use alloy_proto::common::v1alpha1::Error;
use alloy_rt::BoxFuture;

/// The longest node name, in bytes (`product/principles.md` A3).
pub const MAX_NAME_LEN: usize = 63;

/// A node's name: a lower case DNS label, a letter, then letters, digits, or
/// hyphens, 63 characters at most (`product/principles.md` A3).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeName(String);

impl NodeName {
    /// Accepts exactly the names of A3.
    ///
    /// # Errors
    ///
    /// The rule `name` breaks, written for a person.
    pub fn new(name: &str) -> Result<NodeName, &'static str> {
        let mut bytes = name.bytes();
        match bytes.next() {
            None => Err("must not be empty"),
            Some(_) if name.len() > MAX_NAME_LEN => Err("must be at most 63 characters"),
            Some(first) if !first.is_ascii_lowercase() => {
                Err("must start with a lower case letter")
            }
            Some(_)
                if !bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') =>
            {
                Err("must hold only lower case letters, digits, and hyphens")
            }
            Some(_) => Ok(NodeName(name.to_owned())),
        }
    }

    /// The name, as in `linux-1`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The node's full name, as in `nodes/linux-1` (ARFC 0008 section 2).
    #[must_use]
    pub fn full_name(&self) -> String {
        format!("nodes/{}", self.0)
    }
}

/// The nodes of the cluster as the API server reads them: one `Node` per node
/// that holds a record and per pending join request, with its status computed
/// when read (ARFC 0006 section 2.6).
///
/// Methods take `&self`, may be called concurrently, and never panic.
pub trait NodeStore: Send + Sync + fmt::Debug + 'static {
    /// The node named `name`, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// An error of ARFC 0008 clause 3.6, `Unavailable` when the store cannot
    /// answer.
    fn get<'a>(&'a self, name: &'a NodeName) -> BoxFuture<'a, Result<Option<Node>, Error>>;

    /// At most `limit` nodes, ascending by name in byte order, whose names
    /// are greater than `after`, or from the first when `after` is `None`.
    /// `limit` is 1 to 1,001.
    ///
    /// # Errors
    ///
    /// As [`NodeStore::get`].
    fn list<'a>(
        &'a self,
        after: Option<&'a NodeName>,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<Node>, Error>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_a3() {
        for good in ["a", "linux-1", "a-", &"a".repeat(63)] {
            assert!(NodeName::new(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "1a",
            "-a",
            "Linux",
            "a_b",
            "a.b",
            "nodes/a",
            &"a".repeat(64),
        ] {
            assert!(NodeName::new(bad).is_err(), "{bad}");
        }
        assert_eq!(
            NodeName::new("linux-1").map(|n| n.full_name()),
            Ok("nodes/linux-1".to_owned())
        );
    }
}
