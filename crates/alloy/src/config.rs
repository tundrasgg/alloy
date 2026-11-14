//! The CLI's configuration (ARFC 0019 section 2 and clause 3.1.9): its
//! contexts, in the file `contexts` of the configuration directory, and the
//! access key secrets they use, in the file `credentials`, which only the
//! user may read.
//!
//! Both files are UTF-8 text, one field per line, as a first line naming
//! the format and its version, then `<key> <value>` lines; no value holds
//! whitespace or a control character. Each file is replaced whole: it is
//! written to `<name>.new` and renamed over the old one, so a reader sees
//! the old file or the new.
//!
//! `contexts` holds `current <name>` at most once, then each context as a
//! `context <name>` line followed by its fields. `credentials` holds one
//! `key <access key id> <secret>` line per key.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use alloy_crypto::Zeroizing;
use alloy_rt::{
    Access, Create, DiskError, DiskIo, IoBuf, OpenOptions, Protection, ReadOp, RtPath, SyncKind,
    WriteOp,
};

use crate::text::escape;
use crate::{EXIT_FAILURE, Failure};

/// The file of the contexts.
pub(crate) const CONTEXTS: &str = "contexts";

/// The file of the access key secrets.
pub(crate) const CREDENTIALS: &str = "credentials";

/// The first line of the contexts file.
const CONTEXTS_HEAD: &str = "alloy-contexts 1";

/// The first line of the credentials file.
const CREDENTIALS_HEAD: &str = "alloy-credentials 1";

/// The longest file `alloy` reads back: far more than 1,000 contexts.
const MAX_FILE_LEN: usize = 1 << 20;

/// The longest context name.
pub(crate) const MAX_CONTEXT_NAME: usize = 255;

/// The most API endpoints a context holds (ARFC 0019 section 2).
pub(crate) const MAX_ENDPOINTS: usize = 4;

/// One context (ARFC 0019 section 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Context {
    /// Its name.
    pub(crate) name: String,
    /// Its API endpoints, 1 to 4, in the order they are tried.
    pub(crate) endpoints: Vec<SocketAddr>,
    /// The pinned API CA fingerprint (ARFC 0002 section 3.7).
    pub(crate) api_ca_fp: [u8; 16],
    /// The cluster's id.
    pub(crate) cluster_id: [u8; 16],
    /// The user the access key belongs to.
    pub(crate) user: String,
    /// The default org: the one the user lives in.
    pub(crate) org: String,
    /// The default project, empty for none.
    pub(crate) project: String,
    /// The id of the access key, whose secret `credentials` holds.
    pub(crate) access_key: String,
}

/// Every context, and which one is current.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Contexts {
    /// The current context's name.
    pub(crate) current: Option<String>,
    /// The contexts, in the order they were first saved.
    pub(crate) list: Vec<Context>,
}

impl Contexts {
    /// The context named `name`.
    pub(crate) fn get(&self, name: &str) -> Option<&Context> {
        self.list.iter().find(|context| context.name == name)
    }

    /// The current context, when one is set and exists.
    pub(crate) fn current(&self) -> Option<&Context> {
        self.current.as_deref().and_then(|name| self.get(name))
    }

    /// Adds `context`, or replaces the one of the same name in place, and
    /// makes it current.
    pub(crate) fn put_current(&mut self, context: Context) {
        self.current = Some(context.name.clone());
        match self.list.iter_mut().find(|c| c.name == context.name) {
            Some(slot) => *slot = context,
            None => self.list.push(context),
        }
    }

    /// The file's text.
    pub(crate) fn render(&self) -> String {
        let mut text = format!("{CONTEXTS_HEAD}\n");
        if let Some(current) = &self.current {
            let _ = writeln!(text, "current {current}");
        }
        for c in &self.list {
            let _ = writeln!(text, "context {}", c.name);
            for endpoint in &c.endpoints {
                let _ = writeln!(text, "endpoint {endpoint}");
            }
            let _ = writeln!(text, "api-ca-fp {}", hex(&c.api_ca_fp));
            let _ = writeln!(text, "cluster-id {}", hex(&c.cluster_id));
            let _ = writeln!(text, "user {}", c.user);
            let _ = writeln!(text, "org {}", c.org);
            if !c.project.is_empty() {
                let _ = writeln!(text, "project {}", c.project);
            }
            let _ = writeln!(text, "access-key {}", c.access_key);
        }
        text
    }

    /// Parses the file's text.
    ///
    /// # Errors
    ///
    /// What is wrong and on which line.
    pub(crate) fn parse(text: &str) -> Result<Contexts, String> {
        let mut lines = text.lines().enumerate();
        match lines.next() {
            Some((_, CONTEXTS_HEAD)) => {}
            _ => return Err(format!("its first line is not `{CONTEXTS_HEAD}`")),
        }
        let mut contexts = Contexts::default();
        let mut open: Option<Partial> = None;
        for (index, line) in lines {
            let at = |what: &str| format!("line {}: {what}", index + 1);
            let (key, value) = field(line).ok_or_else(|| at("it is not a key and a value"))?;
            match key {
                "current" if contexts.current.is_none() && open.is_none() => {
                    contexts.current = Some(value.to_owned());
                }
                "context" => {
                    if let Some(done) = open.take() {
                        contexts.list.push(done.finish().map_err(|e| at(&e))?);
                    }
                    if contexts.get(value).is_some() {
                        return Err(at("the context is named twice"));
                    }
                    open = Some(Partial::named(value));
                }
                _ => {
                    let partial = open
                        .as_mut()
                        .ok_or_else(|| at("a field comes before any context"))?;
                    partial.set(key, value).map_err(|e| at(&e))?;
                }
            }
        }
        if let Some(done) = open.take() {
            contexts
                .list
                .push(done.finish().map_err(|e| format!("at its end: {e}"))?);
        }
        Ok(contexts)
    }
}

/// A context being read.
struct Partial {
    name: String,
    endpoints: Vec<SocketAddr>,
    api_ca_fp: Option<[u8; 16]>,
    cluster_id: Option<[u8; 16]>,
    user: Option<String>,
    org: Option<String>,
    project: Option<String>,
    access_key: Option<String>,
}

impl Partial {
    fn named(name: &str) -> Partial {
        Partial {
            name: name.to_owned(),
            endpoints: Vec::new(),
            api_ca_fp: None,
            cluster_id: None,
            user: None,
            org: None,
            project: None,
            access_key: None,
        }
    }

    fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        fn once<T>(slot: &mut Option<T>, value: T, key: &str) -> Result<(), String> {
            if slot.is_some() {
                return Err(format!("`{key}` is given twice"));
            }
            *slot = Some(value);
            Ok(())
        }
        match key {
            "endpoint" => {
                if self.endpoints.len() == MAX_ENDPOINTS {
                    return Err(format!("a context has at most {MAX_ENDPOINTS} endpoints"));
                }
                let endpoint = value
                    .parse()
                    .map_err(|_| "the endpoint is not an address and port".to_owned())?;
                self.endpoints.push(endpoint);
                Ok(())
            }
            "api-ca-fp" => once(&mut self.api_ca_fp, unhex(value)?, key),
            "cluster-id" => once(&mut self.cluster_id, unhex(value)?, key),
            "user" => once(&mut self.user, value.to_owned(), key),
            "org" => once(&mut self.org, value.to_owned(), key),
            "project" => once(&mut self.project, value.to_owned(), key),
            "access-key" => once(&mut self.access_key, value.to_owned(), key),
            _ => Err(format!("`{}` is not a field of a context", escape(key))),
        }
    }

    fn finish(self) -> Result<Context, String> {
        let missing = |what: &str| format!("context {} has no `{what}`", self.name);
        if self.endpoints.is_empty() {
            return Err(missing("endpoint"));
        }
        Ok(Context {
            api_ca_fp: self.api_ca_fp.ok_or_else(|| missing("api-ca-fp"))?,
            cluster_id: self.cluster_id.ok_or_else(|| missing("cluster-id"))?,
            user: self.user.clone().ok_or_else(|| missing("user"))?,
            org: self.org.clone().ok_or_else(|| missing("org"))?,
            access_key: self
                .access_key
                .clone()
                .ok_or_else(|| missing("access-key"))?,
            project: self.project.unwrap_or_default(),
            name: self.name,
            endpoints: self.endpoints,
        })
    }
}

/// The access key secrets, by key id.
#[derive(Default)]
pub(crate) struct Credentials(BTreeMap<String, Zeroizing<String>>);

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

impl Credentials {
    /// The secret of the key `id`.
    pub(crate) fn get(&self, id: &str) -> Option<&Zeroizing<String>> {
        self.0.get(id)
    }

    /// Stores the secret of the key `id`.
    pub(crate) fn put(&mut self, id: &str, secret: Zeroizing<String>) {
        self.0.insert(id.to_owned(), secret);
    }

    /// Forgets the key `id`.
    pub(crate) fn remove(&mut self, id: &str) {
        self.0.remove(id);
    }

    /// The file's text, which erases itself when dropped.
    pub(crate) fn render(&self) -> Zeroizing<String> {
        let mut text = Zeroizing::new(format!("{CREDENTIALS_HEAD}\n"));
        for (id, secret) in &self.0 {
            text.push_str("key ");
            text.push_str(id);
            text.push(' ');
            text.push_str(secret);
            text.push('\n');
        }
        text
    }

    /// Parses the file's text.
    ///
    /// # Errors
    ///
    /// What is wrong and on which line, never naming a secret.
    pub(crate) fn parse(text: &str) -> Result<Credentials, String> {
        let mut lines = text.lines().enumerate();
        match lines.next() {
            Some((_, CREDENTIALS_HEAD)) => {}
            _ => return Err(format!("its first line is not `{CREDENTIALS_HEAD}`")),
        }
        let mut credentials = Credentials::default();
        for (index, line) in lines {
            let mut words = line.split(' ');
            match (words.next(), words.next(), words.next(), words.next()) {
                (Some("key"), Some(id), Some(secret), None) if word_ok(id) && word_ok(secret) => {
                    credentials.put(id, Zeroizing::new(secret.to_owned()));
                }
                _ => return Err(format!("line {} is not `key <id> <secret>`", index + 1)),
            }
        }
        Ok(credentials)
    }
}

/// Whether `word` may stand as a value in either file: 1 to 255 bytes, no
/// whitespace, no control character.
pub(crate) fn word_ok(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= MAX_CONTEXT_NAME
        && !word.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// A context name that `--context` may give: a value either file may hold,
/// without a quote, so that a command naming it can be printed quoted.
pub(crate) fn context_name_ok(name: &str) -> bool {
    word_ok(name) && !name.contains(['\'', '"', '`'])
}

fn field(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(' ')?;
    word_ok(key).then_some(())?;
    word_ok(value).then_some((key, value))
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn unhex<const N: usize>(text: &str) -> Result<[u8; N], String> {
    let wrong = || format!("the value is not {} hexadecimal digits", N * 2);
    if text.len() != N * 2 || !text.is_ascii() {
        return Err(wrong());
    }
    let mut bytes = [0u8; N];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(|_| wrong())?;
    }
    Ok(bytes)
}

/// The configuration directory on a `disk-io` instance.
#[derive(Clone, Debug)]
pub(crate) struct Store {
    disk: Arc<dyn DiskIo>,
    dir: RtPath,
    native: PathBuf,
}

impl Store {
    /// The configuration directory `native`, which `dir` names on `disk`.
    pub(crate) fn new(disk: Arc<dyn DiskIo>, dir: RtPath, native: PathBuf) -> Store {
        Store { disk, dir, native }
    }

    /// The native path of the file `name`, for messages.
    pub(crate) fn path_of(&self, name: &str) -> PathBuf {
        self.native.join(name)
    }

    fn failure(&self, name: &str, what: &str, reason: &str) -> Failure {
        Failure::new(
            EXIT_FAILURE,
            format!(
                "alloy could not {what} {}: {reason}.",
                escape(&self.path_of(name).display().to_string())
            ),
        )
    }

    /// The contexts, none when the file does not exist.
    ///
    /// # Errors
    ///
    /// A [`Failure`] naming the file when it cannot be read or parsed.
    pub(crate) async fn contexts(&self) -> Result<Contexts, Failure> {
        match self.read(CONTEXTS).await? {
            None => Ok(Contexts::default()),
            Some(bytes) => std::str::from_utf8(&bytes)
                .map_err(|_| "it is not UTF-8".to_owned())
                .and_then(Contexts::parse)
                .map_err(|e| self.failure(CONTEXTS, "read", &e)),
        }
    }

    /// The access key secrets, none when the file does not exist.
    ///
    /// # Errors
    ///
    /// A [`Failure`] naming the file when it cannot be read or parsed.
    pub(crate) async fn credentials(&self) -> Result<Credentials, Failure> {
        match self.read(CREDENTIALS).await? {
            None => Ok(Credentials::default()),
            Some(bytes) => std::str::from_utf8(&bytes)
                .map_err(|_| "it is not UTF-8".to_owned())
                .and_then(Credentials::parse)
                .map_err(|e| self.failure(CREDENTIALS, "read", &e)),
        }
    }

    /// Replaces the contexts file.
    ///
    /// # Errors
    ///
    /// A [`Failure`] naming the file.
    pub(crate) async fn save_contexts(&self, contexts: &Contexts) -> Result<(), Failure> {
        self.write(CONTEXTS, contexts.render().as_bytes(), false)
            .await
            .map_err(|e| self.failure(CONTEXTS, "write", &e.to_string()))
    }

    /// Replaces the credentials file, readable only by the user.
    ///
    /// # Errors
    ///
    /// A [`Failure`] naming the file.
    pub(crate) async fn save_credentials(&self, credentials: &Credentials) -> Result<(), Failure> {
        self.write(CREDENTIALS, credentials.render().as_bytes(), true)
            .await
            .map_err(|e| self.failure(CREDENTIALS, "write", &e.to_string()))
    }

    async fn read(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, Failure> {
        let fail = |e: DiskError| self.failure(name, "read", &e.to_string());
        let path = self.dir.join(name).map_err(fail)?;
        let options = OpenOptions {
            access: Access::Read,
            create: Create::Never,
            direct: false,
            exclusive: false,
        };
        let file = match self.disk.open(&path, options).await {
            Ok(file) => file,
            Err(DiskError::NotFound) => return Ok(None),
            Err(e) => return Err(fail(e)),
        };
        let len = file.info().await.map_err(fail)?.len;
        let len = usize::try_from(len)
            .ok()
            .filter(|&len| len <= MAX_FILE_LEN)
            .ok_or_else(|| self.failure(name, "read", "it is larger than 1 MiB"))?;
        let mut done = file
            .read_at(vec![ReadOp {
                offset: 0,
                buf: IoBuf::zeroed(len).map_err(fail)?,
            }])
            .await;
        let mut read = done.pop().ok_or_else(|| fail(DiskError::Io))?;
        let bytes = match read.result {
            Ok(count) if count == len => Ok(Zeroizing::new(read.buf.to_vec())),
            Ok(_) => Err(fail(DiskError::Io)),
            Err(e) => Err(fail(e)),
        };
        // The buffer may hold a secret; erase it before it is freed.
        read.buf.fill(0);
        bytes.map(Some)
    }

    /// Creates the configuration directory and each missing directory
    /// above it, the last one readable only by the user on Linux and macOS.
    async fn ensure_dir(&self) -> Result<(), DiskError> {
        let mut missing = Vec::new();
        let mut at = Some(self.dir.clone());
        while let Some(path) = at {
            match self.disk.metadata(&path).await {
                Ok(_) => break,
                Err(DiskError::NotFound) => {
                    at = path.parent();
                    missing.push(path);
                }
                Err(e) => return Err(e),
            }
        }
        while let Some(path) = missing.pop() {
            let result = if path == self.dir {
                self.disk.create_dir_protected(&path, private()).await
            } else {
                self.disk.create_dir(&path).await
            };
            match result {
                Ok(()) | Err(DiskError::AlreadyExists) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Writes the file `name` whole, through `<name>.new` and a rename,
    /// readable only by the user when `secret`.
    async fn write(&self, name: &str, bytes: &[u8], secret: bool) -> Result<(), DiskError> {
        self.ensure_dir().await?;
        let path = self.dir.join(name)?;
        let temp = self.dir.join(&format!("{name}.new"))?;
        match self.disk.remove(&temp).await {
            Ok(()) | Err(DiskError::NotFound) => {}
            Err(e) => return Err(e),
        }
        let protection = if secret {
            private()
        } else {
            Protection::Default
        };
        let file = self.disk.create_file(&temp, protection).await?;
        let buf = IoBuf::from_slice(bytes)?;
        for mut done in file.write_at(vec![WriteOp { offset: 0, buf }]).await {
            // The buffer may hold a secret; erase it before it is freed.
            done.buf.fill(0);
            done.result?;
        }
        file.sync(SyncKind::All).await?;
        drop(file);
        self.disk.replace(&temp, &path).await
    }
}

/// The protection of what only the user may read: mode `0600` or `0700` on
/// Linux and macOS. On Windows a file in the user's profile already has a
/// list that grants only the user, `SYSTEM`, and `Administrators`, which it
/// inherits, and [`Protection::Private`] would name `Administrators` as
/// owner, which a user who is not an administrator may not.
fn private() -> Protection {
    if cfg!(windows) {
        Protection::Default
    } else {
        Protection::Private
    }
}

/// The name of a context made from a login code's first endpoint (ARFC
/// 0019 clause 3.1.11): its address, with `:<port>` when the port is not
/// 443, an IPv6 address then in brackets.
pub(crate) fn default_context_name(endpoint: SocketAddr) -> String {
    if endpoint.port() == 443 {
        endpoint.ip().to_string()
    } else {
        endpoint.to_string()
    }
}

/// `path` for a message.
pub(crate) fn shown(path: &Path) -> String {
    escape(&path.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::*;

    fn context(name: &str) -> Context {
        Context {
            name: name.to_owned(),
            endpoints: vec![
                SocketAddr::from((Ipv4Addr::new(192, 168, 1, 10), 443)),
                SocketAddr::from((Ipv6Addr::LOCALHOST, 8443)),
            ],
            api_ca_fp: [0xAB; 16],
            cluster_id: [0x01; 16],
            user: "admin".to_owned(),
            org: "default".to_owned(),
            project: String::new(),
            access_key: "AKIDEXAMPLE000000001".to_owned(),
        }
    }

    #[test]
    fn contexts_round_trip() {
        let mut contexts = Contexts::default();
        assert_eq!(Contexts::parse(&contexts.render()), Ok(contexts.clone()));
        contexts.put_current(context("192.168.1.10"));
        let mut other = context("[::1]:8443");
        other.project = "web".to_owned();
        contexts.put_current(other);
        let text = contexts.render();
        assert_eq!(Contexts::parse(&text), Ok(contexts.clone()));
        assert!(text.starts_with("alloy-contexts 1\ncurrent [::1]:8443\ncontext 192.168.1.10\n"));
        assert_eq!(
            contexts.current().map(|c| c.name.as_str()),
            Some("[::1]:8443")
        );
        // Saving a context again replaces it in place.
        contexts.put_current(context("192.168.1.10"));
        assert_eq!(contexts.list.len(), 2);
        assert_eq!(contexts.list[0].name, "192.168.1.10");
        assert_eq!(contexts.current.as_deref(), Some("192.168.1.10"));
    }

    #[test]
    fn contexts_parsing_is_total_and_names_the_line() {
        let good = {
            let mut c = Contexts::default();
            c.put_current(context("a"));
            c.render()
        };
        let cases = [
            ("", "first line"),
            ("alloy-contexts 2\n", "first line"),
            (
                "alloy-contexts 1\nuser admin\n",
                "line 2: a field comes before",
            ),
            ("alloy-contexts 1\ncontext a\n", "has no `endpoint`"),
            (
                "alloy-contexts 1\ncontext a\nendpoint x\n",
                "line 3: the endpoint",
            ),
            (
                "alloy-contexts 1\ncontext a\napi-ca-fp 12\n",
                "line 3: the value is not 32",
            ),
            ("alloy-contexts 1\ncontext a\nfrob x\n", "not a field"),
            (
                "alloy-contexts 1\ncontext a\nuser a b\n",
                "line 3: it is not a key",
            ),
            (
                "alloy-contexts 1\ncontext a\nuser\u{1b}x a\n",
                "line 3: it is not a key",
            ),
        ];
        for (text, wanted) in cases {
            let e = Contexts::parse(text).unwrap_err();
            assert!(e.contains(wanted), "{text:?}: {e}");
        }
        let twice = format!("{good}context a\n");
        assert!(Contexts::parse(&twice).unwrap_err().contains("named twice"));
        let user_twice = good.replace("user admin\n", "user admin\nuser b\n");
        assert!(Contexts::parse(&user_twice).unwrap_err().contains("twice"));
        let five = good.replace(
            "endpoint 192.168.1.10:443\n",
            &"endpoint 192.168.1.10:443\n".repeat(4),
        );
        assert!(Contexts::parse(&five).unwrap_err().contains("at most 4"));
        // A current name with no context is kept and is not current.
        let dangling = Contexts::parse("alloy-contexts 1\ncurrent gone\n").unwrap();
        assert_eq!(dangling.current(), None);
    }

    #[test]
    fn credentials_round_trip_and_never_show_a_secret() {
        let mut credentials = Credentials::default();
        credentials.put("AKID1", Zeroizing::new("s3cr3t-value".to_owned()));
        credentials.put("AKID0", Zeroizing::new("other".to_owned()));
        let text = credentials.render();
        assert_eq!(
            text.as_str(),
            "alloy-credentials 1\nkey AKID0 other\nkey AKID1 s3cr3t-value\n"
        );
        let back = Credentials::parse(&text).unwrap();
        assert_eq!(back.get("AKID1").map(|s| s.as_str()), Some("s3cr3t-value"));
        assert!(!format!("{back:?}").contains("s3cr3t"));
        let e = Credentials::parse("alloy-credentials 1\nkey AKID1 s3cr3t extra\n").unwrap_err();
        assert!(!e.contains("s3cr3t"), "{e}");
        assert!(Credentials::parse("nope\n").is_err());
        credentials.remove("AKID0");
        assert!(credentials.get("AKID0").is_none());
    }

    #[test]
    fn a_default_context_name_follows_clause_3_1_11() {
        let v4 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10));
        let v6: IpAddr = "fd00::1".parse().unwrap();
        assert_eq!(
            default_context_name(SocketAddr::new(v4, 443)),
            "192.168.1.10"
        );
        assert_eq!(
            default_context_name(SocketAddr::new(v4, 8443)),
            "192.168.1.10:8443"
        );
        assert_eq!(default_context_name(SocketAddr::new(v6, 443)), "fd00::1");
        assert_eq!(
            default_context_name(SocketAddr::new(v6, 8443)),
            "[fd00::1]:8443"
        );
        assert!(context_name_ok("[fd00::1]:8443"));
        assert!(!context_name_ok("a b"));
        assert!(!context_name_ok("it's"));
        assert!(!context_name_ok(&"x".repeat(256)));
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [0x00, 0x7F, 0xFF, 0x10];
        assert_eq!(hex(&bytes), "007fff10");
        assert_eq!(unhex::<4>("007fff10"), Ok(bytes));
        assert!(unhex::<4>("007fff1").is_err());
        assert!(unhex::<2>("zz00").is_err());
        assert!(unhex::<2>("é00").is_err());
    }
}
