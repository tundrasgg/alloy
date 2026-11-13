//! Stand-in for tokio-macros 2.7.2: `#[tokio::main]` and `#[tokio::test]`.
//!
//! What matters to the fixture is where the expansion names
//! `tokio::runtime::Builder`, since that is what Clippy reports. The real
//! `parse_knobs` (`src/entry.rs` of tokio-macros 2.7.2) gives the builder
//! tokens the span of the first token of the function's last statement, or
//! the call site when the body is empty, and gives the block around them the
//! span of the last token of that statement. This stand-in does the same,
//! with `proc_macro` alone. It finds the last statement as the tokens after
//! the last top-level `;` of the body, or before it when nothing follows.

use proc_macro::{Delimiter, Group, Ident, Span, TokenStream, TokenTree};

/// `#[tokio::main]`: a multi-threaded runtime unless the arguments name
/// `current_thread`.
#[proc_macro_attribute]
pub fn main(args: TokenStream, item: TokenStream) -> TokenStream {
    let multi_thread = !args.to_string().contains("current_thread");
    expand(item, multi_thread, false)
}

/// `#[tokio::test]`: a current-thread runtime unless the arguments name
/// `multi_thread`.
#[proc_macro_attribute]
pub fn test(args: TokenStream, item: TokenStream) -> TokenStream {
    let multi_thread = args.to_string().contains("multi_thread");
    expand(item, multi_thread, true)
}

/// `source` parsed, with every token, delimiters included, given `span`.
fn spanned(source: &str, span: Span) -> TokenStream {
    let parsed: TokenStream = source.parse().unwrap_or_default();
    respan(parsed, span)
}

fn respan(stream: TokenStream, span: Span) -> TokenStream {
    stream
        .into_iter()
        .map(|token| match token {
            TokenTree::Group(group) => {
                let mut out = Group::new(group.delimiter(), respan(group.stream(), span));
                out.set_span(span);
                TokenTree::Group(out)
            }
            mut other => {
                other.set_span(span);
                other
            }
        })
        .collect()
}

/// The spans of the first and the last token of the body's last statement.
fn last_statement_spans(body: &Group) -> Option<(Span, Span)> {
    let tokens: Vec<TokenTree> = body.stream().into_iter().collect();
    let mut statements: Vec<&[TokenTree]> = tokens
        .split_inclusive(|token| matches!(token, TokenTree::Punct(p) if p.as_char() == ';'))
        .filter(|statement| !statement.is_empty())
        .collect();
    let last = statements.pop()?;
    Some((last.first()?.span(), last.last()?.span()))
}

fn expand(item: TokenStream, multi_thread: bool, is_test: bool) -> TokenStream {
    let mut tokens: Vec<TokenTree> = item.into_iter().collect();
    let Some(TokenTree::Group(body)) = tokens.pop() else {
        return spanned("compile_error!(\"expected a function\");", Span::call_site());
    };
    if body.delimiter() != Delimiter::Brace {
        return spanned("compile_error!(\"expected a function body\");", body.span());
    }
    tokens.retain(|token| !matches!(token, TokenTree::Ident(ident) if ident.to_string() == "async"));
    let (start, end) = last_statement_spans(&body).unwrap_or((Span::call_site(), Span::call_site()));

    let mut out = TokenStream::new();
    if is_test {
        out.extend(spanned("#[::core::prelude::v1::test]", Span::call_site()));
    }
    out.extend(tokens);

    let mut inner = TokenStream::new();
    inner.extend(spanned("let body = async", Span::call_site()));
    inner.extend([TokenTree::Group(body)]);
    inner.extend(spanned(";", Span::call_site()));
    if is_test {
        inner.extend(spanned("let body = ::core::pin::pin!(body);", Span::call_site()));
    }
    inner.extend(spanned(
        "#[allow(clippy::expect_used, clippy::diverging_sub_expression, clippy::needless_return, clippy::unwrap_in_result)]",
        end,
    ));
    let mut block = TokenStream::new();
    block.extend(spanned("use", Span::call_site().located_at(start)));
    block.extend([TokenTree::Ident(Ident::new(
        "tokio",
        Span::call_site().located_at(start),
    ))]);
    block.extend(spanned("::runtime::Builder;", Span::call_site().located_at(start)));
    block.extend(spanned("return", end));
    let flavor = if multi_thread {
        "Builder::new_multi_thread()"
    } else {
        "Builder::new_current_thread()"
    };
    block.extend(spanned(flavor, start));
    block.extend(spanned(
        ".enable_all().build().expect(\"Failed building the Runtime\").block_on(body);",
        end,
    ));
    let mut block = Group::new(Delimiter::Brace, block);
    block.set_span(end);
    inner.extend([TokenTree::Group(block)]);

    out.extend([TokenTree::Group(Group::new(Delimiter::Brace, inner))]);
    out
}
