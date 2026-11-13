//! Pagination of `List` methods (ARFC 0008 clause 3.4).
//!
//! A page token is opaque to clients. It is the lower case hexadecimal form of
//! a [`PageToken`] message holding the last name the page returned and the
//! `filter` and `order_by` of the call that returned it. Changing it can only
//! move where a listing starts, which any caller may choose anyway.

use alloy_proto::common::v1alpha1::{Error, ErrorCode};
use prost::Message;

use crate::store::NodeName;

/// The page size of a `page_size` of 0.
pub const DEFAULT_PAGE_SIZE: usize = 100;

/// The largest page. A larger `page_size` is reduced to it.
pub const MAX_PAGE_SIZE: usize = 1_000;

/// The longest page token accepted, in characters. Every token the server
/// returns is far shorter.
pub(crate) const MAX_TOKEN_TEXT: usize = 1_024;

/// The decoded form of a page token.
#[derive(Clone, PartialEq, Eq, Message)]
pub(crate) struct PageToken {
    /// The name of the last resource of the page that returned the token.
    #[prost(string, tag = "1")]
    pub(crate) after: String,
    /// The `filter` of the call that returned the token.
    #[prost(string, tag = "2")]
    pub(crate) filter: String,
    /// The `order_by` of the call that returned the token.
    #[prost(string, tag = "3")]
    pub(crate) order_by: String,
}

impl PageToken {
    /// The token's text.
    pub(crate) fn encode_text(&self) -> String {
        let bytes = self.encode_to_vec();
        let mut text = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            text.push(char::from(HEX[usize::from(b >> 4)]));
            text.push(char::from(HEX[usize::from(b & 0xF)]));
        }
        text
    }

    /// The token a text holds, or `None` for any text that
    /// [`PageToken::encode_text`] does not produce from some token.
    pub(crate) fn decode_text(text: &str) -> Option<PageToken> {
        if text.len() > MAX_TOKEN_TEXT || !text.len().is_multiple_of(2) {
            return None;
        }
        let bytes = text
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[high, low]| Some(nibble(high)? << 4 | nibble(low)?))
            .collect::<Option<Vec<u8>>>()?;
        let token = PageToken::decode(bytes.as_slice()).ok()?;
        // Only the canonical encoding is a token, so one token has one text.
        (token.encode_to_vec() == bytes).then_some(token)
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// A `List` request checked against clause 3.4.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Page {
    /// The most items to return.
    pub(crate) size: usize,
    /// Return only items whose names are greater than this.
    pub(crate) after: Option<NodeName>,
    /// The `filter` of the call, carried into the next token.
    pub(crate) filter: String,
    /// The `order_by` of the call, carried into the next token.
    pub(crate) order_by: String,
}

impl Page {
    /// Checks a request's paging fields, listing every invalid one.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` naming each invalid field and its rule.
    pub(crate) fn of(
        page_size: i32,
        page_token: &str,
        filter: &str,
        order_by: &str,
    ) -> Result<Page, Error> {
        let mut error = Error::new(
            ErrorCode::InvalidArgument,
            "The request has invalid fields, so nothing was listed. Correct the fields listed and \
             send it again.",
        );
        let size = match usize::try_from(page_size) {
            Ok(0) => DEFAULT_PAGE_SIZE,
            Ok(n) => n.min(MAX_PAGE_SIZE),
            Err(_) => {
                error = error.with_violation("page_size", "must not be negative");
                DEFAULT_PAGE_SIZE
            }
        };
        if !filter.is_empty() {
            error = error.with_violation(
                "filter",
                "must be empty: no filter grammar is specified yet",
            );
        }
        if !order_by.is_empty() && order_by != "name" {
            error = error.with_violation("order_by", "must be empty or `name`");
        }
        let mut after = None;
        if !page_token.is_empty() {
            match PageToken::decode_text(page_token) {
                None => {
                    error = error.with_violation(
                        "page_token",
                        "must be a `next_page_token` this method returned",
                    );
                }
                Some(token) if token.filter != filter || token.order_by != order_by => {
                    error = error.with_violation(
                        "page_token",
                        "must be sent with the `filter` and `order_by` of the call that returned \
                         it",
                    );
                }
                Some(token) => match NodeName::new(&token.after) {
                    Ok(name) => after = Some(name),
                    Err(_) => {
                        error = error.with_violation(
                            "page_token",
                            "must be a `next_page_token` this method returned",
                        );
                    }
                },
            }
        }
        if error.field_violations.is_empty() {
            Ok(Page {
                size,
                after,
                filter: filter.to_owned(),
                order_by: order_by.to_owned(),
            })
        } else {
            Err(error)
        }
    }

    /// The `next_page_token` after a page whose last item is `last`.
    pub(crate) fn next_token(&self, last: &str) -> String {
        PageToken {
            after: last.to_owned(),
            filter: self.filter.clone(),
            order_by: self.order_by.clone(),
        }
        .encode_text()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn a_token_decodes_to_itself(after in "[a-z][a-z0-9-]{0,62}", filter in ".{0,40}", order_by in ".{0,10}") {
            let token = PageToken { after, filter, order_by };
            prop_assert_eq!(PageToken::decode_text(&token.encode_text()), Some(token));
        }

        #[test]
        fn decoding_any_text_never_panics(text in ".{0,200}") {
            if let Some(token) = PageToken::decode_text(&text) {
                prop_assert_eq!(token.encode_text(), text);
            }
        }
    }

    fn violations(error: &Error) -> Vec<&str> {
        error
            .field_violations
            .iter()
            .map(|v| v.field.as_str())
            .collect()
    }

    #[test]
    fn sizes_default_and_clamp() {
        assert_eq!(Page::of(0, "", "", "").map(|p| p.size), Ok(100));
        assert_eq!(Page::of(7, "", "", "name").map(|p| p.size), Ok(7));
        assert_eq!(Page::of(5_000, "", "", "").map(|p| p.size), Ok(1_000));
    }

    #[test]
    fn every_invalid_field_is_listed() {
        let error = Page::of(-1, "zz", "labels.a = b", "zone").expect_err("invalid");
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert_eq!(
            violations(&error),
            ["page_size", "filter", "order_by", "page_token"]
        );
    }

    #[test]
    fn a_token_is_bound_to_its_order() {
        let first = Page::of(1, "", "", "").expect("valid");
        let token = first.next_token("linux-1");
        let next = Page::of(1, &token, "", "").expect("valid");
        assert_eq!(next.after.as_ref().map(NodeName::as_str), Some("linux-1"));
        let error = Page::of(1, &token, "", "name").expect_err("other order");
        assert_eq!(violations(&error), ["page_token"]);
    }
}
