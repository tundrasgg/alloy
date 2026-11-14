//! The `os` runtime the commands that reach the API run on, and where on
//! this computer `alloy` keeps its configuration (ARFC 0019 clause 3.1.9).

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use alloy_api::Transport;
use std::sync::Arc;

use alloy_rt::{Host, HostSeam, OsHost, RtPath, Runtime, RuntimeConfig, register_os};
use alloy_seam::{Binding, ConfigValue, ImplName, RegistryBuilder};

use crate::setup::DataPath;
use crate::{EXIT_FAILURE, Env, Failure};

/// How long the runtime's tasks get to end when a command is done.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Starts an `os` runtime whose `disk-io` is rooted at the root of the
/// file system that holds the configuration directory, runs `body` on it,
/// and shuts it down.
///
/// # Errors
///
/// A [`Failure`] with [`EXIT_FAILURE`] when the configuration directory
/// cannot be found or the runtime does not start.
pub(crate) fn run<T>(
    env: &Env,
    body: impl AsyncFnOnce(&Runtime, Transport) -> T,
) -> Result<T, Failure> {
    let dir = config_dir(env)?;
    let (root, _) = split(&dir)?;
    let host = OsHost::start(2).map_err(|e| start_failure(&e.to_string()))?;
    let outcome = os_runtime(&host, &root)
        .map(|(runtime, transport)| host.block_on(body(&runtime, transport)));
    host.shutdown(SHUTDOWN_GRACE);
    outcome
}

/// Starts an `os` runtime whose `disk-io` is rooted at the root of the file
/// system that holds the data directory (ARFC 0002 section 3.12), builds the
/// `os` instance of `host` beside it (ARFC 0005 section 2.8), runs `body`,
/// and shuts the runtime down.
///
/// # Errors
///
/// A [`Failure`] with [`EXIT_FAILURE`] when the data directory cannot be
/// found or the runtime or `host` does not start.
pub(crate) fn run_setup<T>(
    env: &Env,
    body: impl AsyncFnOnce(&Runtime, Transport, &dyn Host, DataPath) -> T,
) -> Result<T, Failure> {
    let native = data_dir(env)?;
    let data = DataPath::of(&native)?;
    let (root, _) = split(&native)?;
    let host = OsHost::start(2).map_err(|e| start_failure(&e.to_string()))?;
    let outcome = os_runtime(&host, &root).and_then(|(runtime, transport)| {
        let machine = os_machine(&host)?;
        Ok(host.block_on(body(&runtime, transport, machine.as_ref(), data)))
    });
    host.shutdown(SHUTDOWN_GRACE);
    outcome
}

/// The data directory's native path: `/var/lib/alloyd`, and on Windows
/// `Alloy` in `%ProgramData%` (ARFC 0002 section 3.12).
fn data_dir(env: &Env) -> Result<PathBuf, Failure> {
    if !env.windows {
        return Ok(PathBuf::from(alloyd::LINUX_DATA_DIR));
    }
    alloyd::default_data_dir(env.var("ProgramData")).map_err(|e| {
        Failure::new(
            EXIT_FAILURE,
            format!(
                "alloy cannot find the data directory: {}.",
                crate::text::escape(&e.to_string())
            ),
        )
    })
}

/// The `os` instance of `host` on `host`.
fn os_machine(host: &OsHost) -> Result<Arc<dyn Host>, Failure> {
    let failed = |e: &dyn std::fmt::Display| start_failure(&e.to_string());
    let mut builder = RegistryBuilder::new();
    register_os(&mut builder, host).map_err(|e| failed(&e))?;
    let os = ImplName::new("os").map_err(|e| failed(&e))?;
    let empty = ConfigValue::defaults(1).map_err(|e| failed(&e))?;
    builder
        .build()
        .instantiate::<HostSeam>(&(), &os, &empty)
        .map_err(|e| failed(&e))
}

fn start_failure(reason: &str) -> Failure {
    Failure::new(
        EXIT_FAILURE,
        format!("alloy could not start its runtime: {reason}."),
    )
}

/// The `os` runtime on `host`, with `disk-io` rooted at the native
/// directory `root`, and the API's transport on it.
///
/// # Errors
///
/// A [`Failure`] when a seam does not build.
pub(crate) fn os_runtime(host: &OsHost, root: &str) -> Result<(Runtime, Transport), Failure> {
    let failed = |e: &dyn std::fmt::Display| start_failure(&e.to_string());
    let mut builder = RegistryBuilder::new();
    register_os(&mut builder, host).map_err(|e| failed(&e))?;
    let registry = builder.build();
    let os = ImplName::new("os").map_err(|e| failed(&e))?;
    let mut config = RuntimeConfig::all(os.clone());
    config.disk_io = Binding {
        name: os,
        config: disk_io_config(root).map_err(|e| failed(&e))?,
    };
    let runtime = Runtime::from_registry(&registry, &config).map_err(|e| failed(&e))?;
    Ok((runtime, Transport::of(&config)))
}

/// Version 1 of the `os` `disk-io` configuration, `message OsDiskIoConfig {
/// string root = 1; }` (ARFC 0005 section 4.2), holding `root`.
fn disk_io_config(root: &str) -> Result<ConfigValue, alloy_seam::ConfigError> {
    // Field 1, wire type 2 (length-delimited).
    const ROOT_KEY: u8 = 0x0A;
    let mut payload = vec![ROOT_KEY];
    let mut len = root.len();
    while len >= 0x80 {
        payload.push((len & 0x7F) as u8 | 0x80);
        len >>= 7;
    }
    payload.push(len as u8);
    payload.extend_from_slice(root.as_bytes());
    ConfigValue::new(1, payload)
}

/// The configuration directory (ARFC 0019 clause 3.1.9):
/// `$XDG_CONFIG_HOME/alloy` when that is set to an absolute path, else
/// `alloy` in the platform's equivalent: `%APPDATA%` on Windows,
/// `~/Library/Application Support` on macOS, and `~/.config` elsewhere.
///
/// # Errors
///
/// A [`Failure`] with [`EXIT_FAILURE`] when no such directory is known.
pub(crate) fn config_dir(env: &Env) -> Result<PathBuf, Failure> {
    let absolute = |name: &str| {
        env.var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let base = if let Some(xdg) = absolute("XDG_CONFIG_HOME") {
        Some(xdg)
    } else if env.windows {
        absolute("APPDATA")
    } else if cfg!(target_os = "macos") {
        absolute("HOME").map(|home| home.join("Library").join("Application Support"))
    } else {
        absolute("HOME").map(|home| home.join(".config"))
    };
    let variable = if env.windows { "APPDATA" } else { "HOME" };
    base.map(|base| base.join("alloy")).ok_or_else(|| {
        Failure::new(
            EXIT_FAILURE,
            format!(
                "alloy cannot find its configuration directory, since neither \
                 XDG_CONFIG_HOME nor {variable} is set to an absolute path."
            ),
        )
    })
}

/// The root of the absolute native path `path`, for the `disk-io`
/// configuration, and the path below it as `disk-io` names it.
///
/// # Errors
///
/// A [`Failure`] with [`EXIT_FAILURE`] when the path is not absolute, has a
/// component that is not UTF-8, or names `..`.
pub(crate) fn split(path: &Path) -> Result<(String, RtPath), Failure> {
    let unusable = || {
        Failure::new(
            EXIT_FAILURE,
            format!(
                "alloy cannot use the directory {}: it must be an absolute path of \
                 UTF-8 names without `..`.",
                crate::text::escape(&path.display().to_string())
            ),
        )
    };
    if !path.is_absolute() {
        return Err(unusable());
    }
    let mut root = PathBuf::new();
    let mut rt = RtPath::root();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => root.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => return Err(unusable()),
            Component::Normal(name) => {
                let name = name.to_str().ok_or_else(unusable)?;
                rt = rt.join(name).map_err(|_| unusable())?;
            }
        }
    }
    let root = root
        .into_os_string()
        .into_string()
        .map_err(|_| unusable())?;
    Ok((root, rt))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn env(vars: &[(&str, &str)], windows: bool) -> Env {
        Env {
            windows,
            vars: vars
                .iter()
                .map(|&(k, v)| (k.to_owned(), OsString::from(v)))
                .collect(),
            ..Env::default()
        }
    }

    #[test]
    fn the_configuration_directory_follows_clause_3_1_9() {
        let home = std::env::temp_dir();
        let home = home.to_str().unwrap();
        let xdg = std::env::temp_dir().join("xdg");
        let xdg = xdg.to_str().unwrap();
        let dir = config_dir(&env(&[("XDG_CONFIG_HOME", xdg), ("HOME", home)], false)).unwrap();
        assert_eq!(dir, Path::new(xdg).join("alloy"));
        let dir = config_dir(&env(
            &[("XDG_CONFIG_HOME", "relative"), ("HOME", home)],
            false,
        ))
        .unwrap();
        if !cfg!(target_os = "macos") {
            assert_eq!(dir, Path::new(home).join(".config").join("alloy"));
        }
        let dir = config_dir(&env(&[("APPDATA", home), ("HOME", "/x")], true)).unwrap();
        assert_eq!(dir, Path::new(home).join("alloy"));
        let none = config_dir(&env(&[], false)).unwrap_err();
        assert_eq!(none.code, EXIT_FAILURE);
        assert!(none.message.contains("HOME"), "{none:?}");
        let none = config_dir(&env(&[("HOME", home)], true)).unwrap_err();
        assert!(none.message.contains("APPDATA"), "{none:?}");
    }

    #[test]
    fn split_names_the_root_and_the_path_below_it() {
        let temp = std::env::temp_dir().join("alloy").join("x");
        let (root, rt) = split(&temp).unwrap();
        assert!(Path::new(&root).is_absolute(), "{root}");
        let below: Vec<&str> = rt.as_str().split('/').filter(|s| !s.is_empty()).collect();
        assert_eq!(below.last(), Some(&"x"));
        assert!(split(Path::new("relative")).is_err());
        assert!(split(&std::env::temp_dir().join("..").join("x")).is_err());
    }

    #[test]
    fn disk_io_config_encodes_the_root_field() {
        let value = disk_io_config("/").unwrap();
        assert_eq!(value.version(), 1);
        assert_eq!(value.payload(), [0x0A, 1, b'/']);
        let long = "r".repeat(200);
        let value = disk_io_config(&long).unwrap();
        assert_eq!(value.payload()[..3], [0x0A, 0xC8, 0x01]);
    }
}
