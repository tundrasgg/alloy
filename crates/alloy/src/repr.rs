//! The API representation of a `Node` under `--output json` and `yaml`
//! (ARFC 0019 clauses 3.1.4 and 3.1.15, ARFC 0008 section 2): the JSON
//! mapping of Protocol Buffers (<https://protobuf.dev/programming-guides/json/>)
//! with its defaults, so field names in lowerCamelCase, a field at its
//! default value left out, enums by name, `bytes` in base64 with padding,
//! 64-bit integers as strings, and timestamps in RFC 3339 in UTC with 0, 3,
//! 6, or 9 fractional digits. YAML carries the same tree, every string in
//! double quotes. Map keys are sorted, so the output does not depend on a
//! hash order.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use alloy_proto::cluster::v1alpha1::{Mesh, Node, NodeSpec, NodeState, NodeStatus, Role};
use prost_types::Timestamp;

/// A machine-readable format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    /// `--output json`.
    Json,
    /// `--output yaml`.
    Yaml,
}

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Bool(bool),
    /// A number, as written.
    Num(String),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

/// The fields of an object, each left out at its default.
#[derive(Default)]
struct Fields(Vec<(String, Value)>);

impl Fields {
    fn str(&mut self, key: &str, value: &str) {
        if !value.is_empty() {
            self.0.push((key.to_owned(), Value::Str(value.to_owned())));
        }
    }

    fn bytes(&mut self, key: &str, value: &[u8]) {
        if !value.is_empty() {
            self.0.push((key.to_owned(), Value::Str(base64(value))));
        }
    }

    fn int(&mut self, key: &str, value: impl Into<i128>) {
        let value = value.into();
        if value != 0 {
            self.0.push((key.to_owned(), Value::Str(value.to_string())));
        }
    }

    fn bool(&mut self, key: &str, value: bool) {
        if value {
            self.0.push((key.to_owned(), Value::Bool(true)));
        }
    }

    fn time(&mut self, key: &str, value: Option<&Timestamp>) {
        if let Some(value) = value {
            self.0.push((key.to_owned(), Value::Str(rfc3339(value))));
        }
    }

    fn strs(&mut self, key: &str, values: &[String]) {
        if !values.is_empty() {
            let values = values.iter().map(|v| Value::Str(v.clone())).collect();
            self.0.push((key.to_owned(), Value::Array(values)));
        }
    }

    fn value(&mut self, key: &str, value: Value) {
        self.0.push((key.to_owned(), value));
    }

    fn object(self) -> Value {
        Value::Object(self.0)
    }
}

/// An enum value by name, or by number when this release does not know it
/// (the JSON mapping accepts either).
fn enum_value(name: Option<&str>, number: i32) -> Value {
    name.map_or_else(
        || Value::Num(number.to_string()),
        |name| Value::Str(name.to_owned()),
    )
}

fn node_value(node: &Node) -> Value {
    let mut f = Fields::default();
    f.str("name", &node.name);
    f.bytes("uid", &node.uid);
    f.int("generation", node.generation);
    f.time("createTime", node.create_time.as_ref());
    if !node.labels.is_empty() {
        let sorted: BTreeMap<&String, &String> = node.labels.iter().collect();
        let labels = sorted
            .into_iter()
            .map(|(k, v)| (k.clone(), Value::Str(v.clone())))
            .collect();
        f.value("labels", Value::Object(labels));
    }
    f.bool("deleting", node.deleting);
    if let Some(spec) = &node.spec {
        f.value("spec", spec_value(spec));
    }
    if let Some(status) = &node.status {
        f.value("status", status_value(status));
    }
    f.object()
}

fn spec_value(spec: &NodeSpec) -> Value {
    let mut f = Fields::default();
    if !spec.roles.is_empty() {
        let roles = spec
            .roles
            .iter()
            .map(|&r| enum_value(Role::try_from(r).ok().map(|r| r.as_str_name()), r))
            .collect();
        f.value("roles", Value::Array(roles));
    }
    f.str("zone", &spec.zone);
    f.strs("advertise", &spec.advertise);
    f.object()
}

fn status_value(status: &NodeStatus) -> Value {
    let mut f = Fields::default();
    if status.state != 0 {
        let name = NodeState::try_from(status.state)
            .ok()
            .map(|s| s.as_str_name());
        f.value("state", enum_value(name, status.state));
    }
    f.bytes("nodeId", &status.node_id);
    f.int("recordSerial", status.record_serial);
    f.time("recordNotAfter", status.record_not_after.as_ref());
    f.bool("reported", status.reported);
    f.time("unreachableSince", status.unreachable_since.as_ref());
    f.str("runningRelease", &status.running_release);
    f.strs("addresses", &status.addresses);
    f.time("lastHeartbeatTime", status.last_heartbeat_time.as_ref());
    if status.mesh != 0 {
        let name = Mesh::try_from(status.mesh).ok().map(|m| m.as_str_name());
        f.value("mesh", enum_value(name, status.mesh));
    }
    f.object()
}

/// One array of every node, in the order given, in `format`, ending in a
/// newline.
pub(crate) fn nodes(nodes: &[Node], format: Format) -> String {
    let value = Value::Array(nodes.iter().map(node_value).collect());
    let mut text = String::new();
    match format {
        Format::Json => {
            json(&value, 0, &mut text);
            text.push('\n');
        }
        Format::Yaml => yaml_top(&value, &mut text),
    }
    text
}

fn json(value: &Value, indent: usize, out: &mut String) {
    let pad = |n: usize| " ".repeat(n);
    match value {
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => out.push_str(n),
        Value::Str(s) => quote(s, out),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Object(fields) if fields.is_empty() => out.push_str("{}"),
        Value::Array(items) => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad(indent + 2));
                json(item, indent + 2, out);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(indent));
            out.push(']');
        }
        Value::Object(fields) => {
            out.push_str("{\n");
            for (i, (key, item)) in fields.iter().enumerate() {
                out.push_str(&pad(indent + 2));
                quote(key, out);
                out.push_str(": ");
                json(item, indent + 2, out);
                out.push_str(if i + 1 < fields.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(indent));
            out.push('}');
        }
    }
}

/// `text` as a JSON string, which is also a YAML double-quoted scalar:
/// quotes, backslashes, and every control character escaped.
fn quote(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn yaml_key(key: &str, out: &mut String) {
    if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        out.push_str(key);
    } else {
        quote(key, out);
    }
}

fn yaml_top(value: &Value, out: &mut String) {
    match value {
        Value::Array(items) if !items.is_empty() => yaml_items(items, 0, out),
        Value::Object(fields) if !fields.is_empty() => yaml_fields(fields, 0, out),
        other => {
            yaml_scalar(other, out);
            out.push('\n');
        }
    }
}

/// A scalar, `[]`, or `{}`, on the current line.
fn yaml_scalar(value: &Value, out: &mut String) {
    match value {
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => out.push_str(n),
        Value::Str(s) => quote(s, out),
        Value::Array(_) => out.push_str("[]"),
        Value::Object(_) => out.push_str("{}"),
    }
}

fn is_block(value: &Value) -> bool {
    match value {
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
        Value::Bool(_) | Value::Num(_) | Value::Str(_) => false,
    }
}

fn yaml_fields(fields: &[(String, Value)], indent: usize, out: &mut String) {
    for (i, (key, value)) in fields.iter().enumerate() {
        // The first field of a list item follows its `- `.
        if i > 0 || out.ends_with('\n') || out.is_empty() {
            out.push_str(&" ".repeat(indent));
        }
        yaml_key(key, out);
        out.push(':');
        yaml_after(value, indent + 2, out);
    }
}

fn yaml_items(items: &[Value], indent: usize, out: &mut String) {
    for item in items {
        out.push_str(&" ".repeat(indent));
        out.push('-');
        match item {
            Value::Object(fields) if !fields.is_empty() => {
                out.push(' ');
                yaml_fields(fields, indent + 2, out);
            }
            Value::Array(inner) if !inner.is_empty() => {
                out.push('\n');
                yaml_items(inner, indent + 2, out);
            }
            scalar => {
                out.push(' ');
                yaml_scalar(scalar, out);
                out.push('\n');
            }
        }
    }
}

/// What follows `key:`: a scalar on the same line, or a block below it.
fn yaml_after(value: &Value, indent: usize, out: &mut String) {
    if !is_block(value) {
        out.push(' ');
        yaml_scalar(value, out);
        out.push('\n');
        return;
    }
    out.push('\n');
    match value {
        Value::Array(items) => yaml_items(items, indent, out),
        Value::Object(fields) => yaml_fields(fields, indent, out),
        Value::Bool(_) | Value::Num(_) | Value::Str(_) => {}
    }
}

/// Standard base64 with padding (RFC 4648 section 4).
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 0x3F) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A timestamp in RFC 3339 in UTC, `Z`, with 0, 3, 6, or 9 fractional
/// digits, its nanoseconds first carried into its seconds when outside 0
/// to 999,999,999.
fn rfc3339(time: &Timestamp) -> String {
    let total = i128::from(time.seconds) * 1_000_000_000 + i128::from(time.nanos);
    let seconds = total.div_euclid(1_000_000_000);
    let nanos = total.rem_euclid(1_000_000_000);
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil(days);
    let mut text = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        of_day / 3_600,
        of_day % 3_600 / 60,
        of_day % 60
    );
    if nanos != 0 {
        let digits = format!("{nanos:09}");
        let keep = if nanos % 1_000_000 == 0 {
            3
        } else if nanos % 1_000 == 0 {
            6
        } else {
            9
        };
        text.push('.');
        text.push_str(&digits[..keep]);
    }
    text.push('Z');
    text
}

/// The proleptic Gregorian date of a day count from 1970-01-01
/// (<https://howardhinnant.github.io/date_algorithms.html#civil_from_days>).
fn civil(days: i128) -> (i128, i128, i128) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i128::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::tests::three;

    #[test]
    fn base64_follows_rfc_4648() {
        // RFC 4648 section 10.
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (input, output) in cases {
            assert_eq!(base64(input.as_bytes()), output);
        }
        assert_eq!(base64(&[0xFF, 0xFE]), "//4=");
    }

    #[test]
    fn timestamps_follow_rfc_3339() {
        let at = |seconds, nanos| rfc3339(&Timestamp { seconds, nanos });
        assert_eq!(at(0, 0), "1970-01-01T00:00:00Z");
        assert_eq!(at(1_779_966_000, 0), "2026-05-28T11:00:00Z");
        assert_eq!(at(951_782_400, 0), "2000-02-29T00:00:00Z");
        assert_eq!(at(-1, 0), "1969-12-31T23:59:59Z");
        assert_eq!(at(0, 21_000_000), "1970-01-01T00:00:00.021Z");
        assert_eq!(at(0, 21_000), "1970-01-01T00:00:00.000021Z");
        assert_eq!(at(0, 1), "1970-01-01T00:00:00.000000001Z");
        assert_eq!(at(0, -1), "1969-12-31T23:59:59.999999999Z");
        assert_eq!(at(253_402_300_799, 0), "9999-12-31T23:59:59Z");
        // No input panics.
        let _ = at(i64::MAX, i32::MAX);
        let _ = at(i64::MIN, i32::MIN);
    }

    #[test]
    fn json_is_the_api_representation() {
        let mut nodes = three();
        nodes.truncate(1);
        nodes[0].uid = vec![0xFF, 0xFE];
        nodes[0].generation = 3;
        nodes[0].create_time = Some(Timestamp {
            seconds: 0,
            nanos: 0,
        });
        nodes[0].labels.insert("b".to_owned(), "2".to_owned());
        nodes[0].labels.insert("a".to_owned(), "\"1\"".to_owned());
        if let Some(status) = nodes[0].status.as_mut() {
            status.mesh = i32::from(Mesh::Direct);
            status.record_serial = 7;
            status.reported = true;
        }
        assert_eq!(
            super::nodes(&nodes, Format::Json),
            r#"[
  {
    "name": "linux-2",
    "uid": "//4=",
    "generation": "3",
    "createTime": "1970-01-01T00:00:00Z",
    "labels": {
      "a": "\"1\"",
      "b": "2"
    },
    "spec": {
      "roles": [
        "ROLE_STORAGE",
        "ROLE_COMPUTE"
      ],
      "zone": "default",
      "advertise": [
        "192.168.1.21"
      ]
    },
    "status": {
      "state": "NODE_STATE_ACTIVE",
      "recordSerial": "7",
      "reported": true,
      "runningRelease": "0.0.1-preview.2",
      "mesh": "MESH_DIRECT"
    }
  }
]
"#
        );
        assert_eq!(super::nodes(&[], Format::Json), "[]\n");
    }

    #[test]
    fn yaml_carries_the_same_tree() {
        let mut nodes = three();
        nodes.truncate(2);
        nodes[1].name = "a\u{1b}b".to_owned();
        assert_eq!(
            super::nodes(&nodes, Format::Yaml),
            r#"- name: "linux-2"
  spec:
    roles:
      - "ROLE_STORAGE"
      - "ROLE_COMPUTE"
    zone: "default"
    advertise:
      - "192.168.1.21"
  status:
    state: "NODE_STATE_ACTIVE"
    runningRelease: "0.0.1-preview.2"
- name: "a\u001bb"
  spec:
    zone: "default"
  status:
    state: "NODE_STATE_ACTIVE"
    runningRelease: "0.0.1-preview.2"
    addresses:
      - "192.168.1.50"
"#
        );
        assert_eq!(super::nodes(&[], Format::Yaml), "[]\n");
        let unknown = Node {
            status: Some(NodeStatus {
                state: 42,
                ..NodeStatus::default()
            }),
            ..Node::default()
        };
        assert_eq!(
            super::nodes(&[unknown], Format::Yaml),
            "- status:\n    state: 42\n"
        );
    }
}
