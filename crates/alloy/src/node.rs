//! `alloy node list` (ARFC 0019 clauses 3.1.12 and 3.1.15).

use std::collections::BTreeSet;
use std::io::Write;

use alloy_api::client::{RetryPolicy, retry};
use alloy_proto::cluster::v1alpha1::{Mesh, Node, NodeState, Role};

use crate::api::api_failure;
use crate::login::{not_logged_in, signed_client, store};
use crate::repr::{self, Format};
use crate::text::escape;
use crate::{CommandSpec, Ctx, Failure, Invocation, Style, UsageError, emit};

/// The format `--output` names: `None` for the table.
///
/// # Errors
///
/// A [`UsageError`] for any other value.
pub(crate) fn check(
    spec: &CommandSpec,
    invocation: &Invocation,
) -> Result<Option<Format>, UsageError> {
    match invocation.value("--output") {
        None | Some("table") => Ok(None),
        Some("json") => Ok(Some(Format::Json)),
        Some("yaml") => Ok(Some(Format::Yaml)),
        Some(other) => Err(UsageError::of(
            spec,
            format!(
                "`--output` takes table, json, or yaml, and `{}` was given.",
                escape(other)
            ),
        )),
    }
}

/// `alloy node list`: every node of every page of `ListNodes`, then the
/// table, or with `--output json` or `yaml` the array of every `Node`.
pub(crate) async fn list(
    ctx: &Ctx<'_>,
    spec: &CommandSpec,
    invocation: &Invocation,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let format = check(spec, invocation)?;
    let store = store(ctx)?;
    let contexts = store.contexts().await?;
    let Some(context) = contexts.current() else {
        return Err(not_logged_in());
    };
    let client = signed_client(ctx, &store, context).await?;
    let nodes = retry(ctx.runtime, RetryPolicy::default(), || {
        client.list_all_nodes()
    })
    .await
    .map_err(|e| api_failure(&e))?;
    let text = match format {
        None => table(&nodes, ctx.styles.out),
        Some(format) => repr::nodes(&nodes, format),
    };
    // Rung 2 reports no standing warning (ARFC 0002 section 3.15), so none
    // follows the table (ARFC 0019 clause 3.2.5).
    emit(out, &text)
}

/// The value of a column, and how it is colored.
struct Cell {
    text: String,
    paint: Paint,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Paint {
    Plain,
    Red,
    Yellow,
}

impl Cell {
    fn plain(text: String) -> Cell {
        Cell {
            text,
            paint: Paint::Plain,
        }
    }
}

/// The node table of clause 3.1.15: one row per node, sorted by name in
/// ascending byte order, under `NAME`, `STATE`, `ROLES`, `ZONE` only when
/// the nodes are in more than one zone, `MESH`, `RELEASE`, and `ADDRESS`.
/// Each column is as wide as its widest value or name, two spaces apart,
/// left aligned, nothing shortened, and no line ends in a space. With
/// `style` on, the header is bold, `unreachable` red, and `pending` yellow.
pub(crate) fn table(nodes: &[Node], style: Style) -> String {
    let mut sorted: Vec<&Node> = nodes.iter().collect();
    sorted.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let zones: BTreeSet<&str> = sorted
        .iter()
        .map(|node| node.spec.as_ref().map_or("", |spec| spec.zone.as_str()))
        .collect();
    let with_zone = zones.len() > 1;
    let mut header = vec!["NAME", "STATE", "ROLES"];
    if with_zone {
        header.push("ZONE");
    }
    header.extend(["MESH", "RELEASE", "ADDRESS"]);
    let rows: Vec<Vec<Cell>> = sorted.iter().map(|node| row(node, with_zone)).collect();
    let mut widths: Vec<usize> = header.iter().map(|name| name.len()).collect();
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.text.chars().count());
        }
    }
    let mut text = style.bold(&line(
        header.iter().map(|&name| (name.to_owned(), Paint::Plain)),
        &widths,
        Style::default(),
    ));
    text.push('\n');
    for row in rows {
        text.push_str(&line(
            row.into_iter().map(|cell| (cell.text, cell.paint)),
            &widths,
            style,
        ));
        text.push('\n');
    }
    text
}

/// One line of the table: each value padded to its column, two spaces
/// apart, the last one unpadded.
fn line(cells: impl Iterator<Item = (String, Paint)>, widths: &[usize], style: Style) -> String {
    let cells: Vec<(String, Paint)> = cells.collect();
    let last = cells.len().saturating_sub(1);
    let mut text = String::new();
    for (index, ((value, paint), width)) in cells.into_iter().zip(widths).enumerate() {
        let pad = width.saturating_sub(value.chars().count());
        let shown = match paint {
            Paint::Plain => value,
            Paint::Red => style.red(&value),
            Paint::Yellow => style.yellow(&value),
        };
        text.push_str(&shown);
        if index < last {
            text.push_str(&" ".repeat(pad + 2));
        }
    }
    text
}

fn row(node: &Node, with_zone: bool) -> Vec<Cell> {
    let spec = node.spec.clone().unwrap_or_default();
    let status = node.status.clone().unwrap_or_default();
    let state = match NodeState::try_from(status.state) {
        Ok(NodeState::Pending) => Cell {
            text: "pending".to_owned(),
            paint: Paint::Yellow,
        },
        Ok(NodeState::Joining) => Cell::plain("joining".to_owned()),
        Ok(NodeState::Active) => Cell::plain("active".to_owned()),
        Ok(NodeState::Unreachable) => Cell {
            text: "unreachable".to_owned(),
            paint: Paint::Red,
        },
        Ok(NodeState::Unspecified) | Err(_) => Cell::plain("-".to_owned()),
    };
    let held: BTreeSet<i32> = spec.roles.iter().copied().collect();
    let roles: Vec<&str> = [
        (Role::Anchor, "anchor"),
        (Role::Control, "control"),
        (Role::Compute, "compute"),
        (Role::Storage, "storage"),
    ]
    .into_iter()
    .filter(|(role, _)| held.contains(&i32::from(*role)))
    .map(|(_, name)| name)
    .collect();
    let mesh = match Mesh::try_from(status.mesh) {
        Ok(Mesh::Direct) => "direct",
        Ok(Mesh::RelayOnly) => "relay-only",
        Ok(Mesh::Unspecified) | Err(_) => "-",
    };
    let address = spec
        .advertise
        .first()
        .or_else(|| status.addresses.first())
        .map_or("-", String::as_str);
    let or_dash = |text: &str| {
        if text.is_empty() {
            "-".to_owned()
        } else {
            escape(text)
        }
    };
    let mut cells = vec![Cell::plain(or_dash(&node.name)), state];
    cells.push(Cell::plain(if roles.is_empty() {
        "-".to_owned()
    } else {
        roles.join(",")
    }));
    if with_zone {
        cells.push(Cell::plain(or_dash(&spec.zone)));
    }
    cells.push(Cell::plain(mesh.to_owned()));
    cells.push(Cell::plain(or_dash(&status.running_release)));
    cells.push(Cell::plain(or_dash(address)));
    cells
}

#[cfg(test)]
pub(crate) mod tests {
    use alloy_proto::cluster::v1alpha1::{NodeSpec, NodeStatus};

    use super::*;

    pub(crate) fn node(name: &str, state: NodeState, roles: &[Role], address: &str) -> Node {
        Node {
            name: name.to_owned(),
            spec: Some(NodeSpec {
                roles: roles.iter().map(|&r| i32::from(r)).collect(),
                zone: "default".to_owned(),
                advertise: if address.is_empty() {
                    Vec::new()
                } else {
                    vec![address.to_owned()]
                },
            }),
            status: Some(NodeStatus {
                state: i32::from(state),
                running_release: "0.0.1-preview.2".to_owned(),
                ..NodeStatus::default()
            }),
            ..Node::default()
        }
    }

    const ALL: [Role; 4] = [Role::Anchor, Role::Control, Role::Compute, Role::Storage];

    /// The three-node cluster of clause 3.1.15, in another order than its
    /// names', with the laptop's address from its heartbeat.
    pub(crate) fn three() -> Vec<Node> {
        let mut laptop = node("laptop", NodeState::Active, &[], "");
        if let Some(status) = laptop.status.as_mut() {
            status.addresses = vec!["192.168.1.50".to_owned()];
        }
        vec![
            node(
                "linux-2",
                NodeState::Active,
                &[Role::Storage, Role::Compute],
                "192.168.1.21",
            ),
            laptop,
            node("linux-1", NodeState::Active, &ALL, "192.168.1.20"),
        ]
    }

    #[test]
    fn the_table_matches_clause_3_1_15() {
        assert_eq!(
            table(&three(), Style::default()),
            "NAME     STATE   ROLES                           MESH  RELEASE          ADDRESS\n\
             laptop   active  -                               -     0.0.1-preview.2  192.168.1.50\n\
             linux-1  active  anchor,control,compute,storage  -     0.0.1-preview.2  192.168.1.20\n\
             linux-2  active  compute,storage                 -     0.0.1-preview.2  192.168.1.21\n"
        );
    }

    #[test]
    fn the_reference_case_fits_100_columns() {
        let name = |i: usize| format!("node-{i:015}");
        let nodes: Vec<Node> = (0..3)
            .map(|i| {
                let mut n = node(&name(i), NodeState::Active, &ALL, "192.168.100.200");
                if let Some(status) = n.status.as_mut() {
                    status.mesh = i32::from(Mesh::Direct);
                    status.running_release = "1.2.3".to_owned();
                }
                n
            })
            .collect();
        let text = table(&nodes, Style::default());
        let widest = text.lines().map(|l| l.chars().count()).max();
        assert_eq!(widest, Some(94), "{text}");
    }

    #[test]
    fn zone_mesh_and_colors_appear_when_they_apply() {
        let mut nodes = three();
        if let Some(spec) = nodes[0].spec.as_mut() {
            spec.zone = "rack-b".to_owned();
        }
        if let Some(status) = nodes[0].status.as_mut() {
            status.state = i32::from(NodeState::Unreachable);
            status.mesh = i32::from(Mesh::RelayOnly);
            status.running_release = String::new();
        }
        if let Some(status) = nodes[1].status.as_mut() {
            status.state = i32::from(NodeState::Pending);
        }
        let plain = table(&nodes, Style::default());
        let lines: Vec<&str> = plain.lines().collect();
        assert_eq!(
            lines[0],
            "NAME     STATE        ROLES                           ZONE     MESH        RELEASE          ADDRESS"
        );
        assert_eq!(
            lines[3],
            "linux-2  unreachable  compute,storage                 rack-b   relay-only  -                192.168.1.21"
        );
        assert!(lines.iter().all(|l| !l.ends_with(' ')), "{plain}");
        let colored = table(&nodes, Style { on: true });
        assert!(colored.starts_with("\x1b[1mNAME "), "{colored}");
        assert!(colored.contains("\x1b[31munreachable\x1b[0m"), "{colored}");
        assert!(colored.contains("\x1b[33mpending\x1b[0m"), "{colored}");
        assert!(!colored.contains("\x1b[32m"), "{colored}");
        // Color removed, the words are the same (principle T10).
        let stripped = colored
            .replace("\x1b[1m", "")
            .replace("\x1b[31m", "")
            .replace("\x1b[33m", "")
            .replace("\x1b[0m", "");
        assert_eq!(stripped, plain);
    }

    #[test]
    fn server_text_is_escaped_and_nothing_is_shortened() {
        let long = "n".repeat(63);
        let nodes = vec![node(&long, NodeState::Joining, &[Role::Anchor], "fd00::1")];
        let mut text = table(&nodes, Style::default());
        assert!(text.contains(&long), "{text}");
        assert!(text.contains("joining  anchor"), "{text}");
        let mut odd = node("x", NodeState::Active, &[], "");
        if let Some(status) = odd.status.as_mut() {
            status.running_release = "\u{1b}[2J".to_owned();
            status.state = 99;
        }
        text = table(&[odd], Style::default());
        assert!(!text.contains('\u{1b}'), "{text}");
        assert!(
            text.contains("x     -      -      -     \\u{1b}[2J  -"),
            "{text}"
        );
    }
}
