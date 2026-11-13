//! Stand-in for tokio 1.54.1 with the features `fs`, `macros`, `net`,
//! `process`, `rt-multi-thread`, `test-util`, and `time`: every item that
//! `clippy.toml` names, at the path, and under the platform `cfg`, that the
//! real crate gives it (`src/{time,net,fs,task,runtime,process}/mod.rs` of
//! tokio 1.54.1), and what the fixture's calls need around them. Items that
//! need `--cfg tokio_unstable`, such as `tokio::task::Builder`, are absent, as
//! they are from the real crate without it.
//!
//! Nothing here reaches the operating system or is meant to run: operations
//! fail with `ErrorKind::Unsupported`, and futures finish at once.

use std::io;

pub use task::spawn;
pub use tokio_macros::{main, test};

fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// The clock.
pub mod time {
    use std::future::Future;

    pub use std::time::Duration;

    /// A point on the clock.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct Instant(Duration);

    impl Instant {
        /// Reads the clock, in the real crate.
        pub fn now() -> Self {
            Self(Duration::ZERO)
        }

        /// Reads the clock and subtracts, in the real crate.
        pub fn elapsed(&self) -> Duration {
            Duration::ZERO.saturating_sub(self.0)
        }
    }

    /// Why a timeout finished.
    pub mod error {
        /// The deadline passed.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Elapsed(());
    }

    /// Ticks at a period.
    #[derive(Debug)]
    pub struct Interval {
        period: Duration,
    }

    impl Interval {
        /// The period.
        pub fn period(&self) -> Duration {
            self.period
        }
    }

    /// Waits for `duration`.
    pub async fn sleep(_duration: Duration) {}

    /// Waits until `deadline`.
    pub async fn sleep_until(_deadline: Instant) {}

    /// Ticks every `period`.
    pub fn interval(period: Duration) -> Interval {
        Interval { period }
    }

    /// Ticks every `period` from `start`.
    pub fn interval_at(_start: Instant, period: Duration) -> Interval {
        Interval { period }
    }

    /// Runs `future` for at most `duration`.
    pub async fn timeout<F: Future>(
        _duration: Duration,
        future: F,
    ) -> Result<F::Output, error::Elapsed> {
        Ok(future.await)
    }

    /// Runs `future` until at most `deadline`.
    pub async fn timeout_at<F: Future>(
        _deadline: Instant,
        future: F,
    ) -> Result<F::Output, error::Elapsed> {
        Ok(future.await)
    }

    /// Stops the runtime's clock (`test-util`).
    pub fn pause() {}

    /// Starts the runtime's clock again (`test-util`).
    pub fn resume() {}

    /// Moves the runtime's stopped clock forward (`test-util`).
    pub async fn advance(_duration: Duration) {}
}

/// Sockets and pipes.
pub mod net {
    use std::io;
    use std::net::SocketAddr;

    use super::unsupported;

    /// Resolves `host`.
    pub async fn lookup_host<T>(_host: T) -> io::Result<std::vec::IntoIter<SocketAddr>> {
        unsupported()
    }

    /// A TCP listener.
    #[derive(Debug)]
    pub struct TcpListener(());

    /// A TCP socket before it connects or listens.
    #[derive(Debug)]
    pub struct TcpSocket(());

    /// A TCP connection.
    #[derive(Debug)]
    pub struct TcpStream(());

    impl TcpStream {
        /// Connects to `addr`.
        pub async fn connect<A>(_addr: A) -> io::Result<Self> {
            unsupported()
        }
    }

    /// A UDP socket.
    #[derive(Debug)]
    pub struct UdpSocket(());

    #[cfg(unix)]
    pub use unix::{UnixDatagram, UnixListener, UnixSocket, UnixStream};

    /// Unix sockets and pipes.
    #[cfg(unix)]
    pub mod unix {
        /// A Unix datagram socket.
        #[derive(Debug)]
        pub struct UnixDatagram(());

        /// A Unix listener.
        #[derive(Debug)]
        pub struct UnixListener(());

        /// A Unix socket before it connects or listens.
        #[derive(Debug)]
        pub struct UnixSocket(());

        /// A Unix stream.
        #[derive(Debug)]
        pub struct UnixStream(());

        /// Unix pipes.
        pub mod pipe {
            use std::io;
            use std::path::Path;

            use crate::unsupported;

            /// An anonymous pipe.
            pub fn pipe() -> io::Result<(Sender, Receiver)> {
                unsupported()
            }

            /// Opens a named pipe.
            #[derive(Debug, Default)]
            pub struct OpenOptions(());

            impl OpenOptions {
                /// The default options.
                pub fn new() -> Self {
                    Self(())
                }

                /// Opens the reading end of the pipe at `path`.
                pub fn open_receiver<P: AsRef<Path>>(&self, _path: P) -> io::Result<Receiver> {
                    unsupported()
                }
            }

            /// The writing end of a pipe.
            #[derive(Debug)]
            pub struct Sender(());

            /// The reading end of a pipe.
            #[derive(Debug)]
            pub struct Receiver(());
        }
    }

    /// Windows named pipes.
    #[cfg(windows)]
    pub mod windows {
        /// Named pipes.
        pub mod named_pipe {
            use std::ffi::OsStr;
            use std::io;

            use crate::unsupported;

            /// Opens the client end of a named pipe.
            #[derive(Debug, Default)]
            pub struct ClientOptions(());

            impl ClientOptions {
                /// The default options.
                pub fn new() -> Self {
                    Self(())
                }

                /// Opens the pipe at `addr`.
                pub fn open(&self, _addr: impl AsRef<OsStr>) -> io::Result<NamedPipeClient> {
                    unsupported()
                }
            }

            /// Creates the server end of a named pipe.
            #[derive(Debug, Default)]
            pub struct ServerOptions(());

            impl ServerOptions {
                /// The default options.
                pub fn new() -> Self {
                    Self(())
                }

                /// Creates the pipe at `addr`.
                pub fn create(&self, _addr: impl AsRef<OsStr>) -> io::Result<NamedPipeServer> {
                    unsupported()
                }
            }

            /// The client end of a named pipe.
            #[derive(Debug)]
            pub struct NamedPipeClient(());

            /// The server end of a named pipe.
            #[derive(Debug)]
            pub struct NamedPipeServer(());
        }
    }
}

/// The file system.
pub mod fs {
    use std::io;
    use std::path::{Path, PathBuf};

    use super::unsupported;

    /// Each function takes the paths of the real one, and fails.
    macro_rules! operations {
        ($($name:ident($($arg:ident),*) -> $out:ty;)*) => {
            $(
                #[doc = concat!("`", stringify!($name), "` in the real crate.")]
                pub async fn $name($($arg: impl AsRef<Path>),*) -> io::Result<$out> {
                    $(let _ = $arg;)*
                    unsupported()
                }
            )*
        };
    }

    operations! {
        canonicalize(path) -> PathBuf;
        copy(from, to) -> u64;
        create_dir(path) -> ();
        create_dir_all(path) -> ();
        hard_link(original, link) -> ();
        metadata(path) -> std::fs::Metadata;
        read(path) -> Vec<u8>;
        read_dir(path) -> ReadDir;
        read_link(path) -> PathBuf;
        read_to_string(path) -> String;
        remove_dir(path) -> ();
        remove_dir_all(path) -> ();
        remove_file(path) -> ();
        rename(from, to) -> ();
        symlink_metadata(path) -> std::fs::Metadata;
        try_exists(path) -> bool;
    }

    #[cfg(unix)]
    operations! {
        symlink(original, link) -> ();
    }

    #[cfg(windows)]
    operations! {
        symlink_dir(original, link) -> ();
        symlink_file(original, link) -> ();
    }

    /// Sets the permissions of `path`.
    pub async fn set_permissions(
        _path: impl AsRef<Path>,
        _permissions: std::fs::Permissions,
    ) -> io::Result<()> {
        unsupported()
    }

    /// Writes `contents` to `path`.
    pub async fn write(_path: impl AsRef<Path>, _contents: impl AsRef<[u8]>) -> io::Result<()> {
        unsupported()
    }

    /// The entries of a directory.
    #[derive(Debug)]
    pub struct ReadDir(());

    /// Creates directories.
    #[derive(Debug, Default)]
    pub struct DirBuilder(());

    /// An open file.
    #[derive(Debug)]
    pub struct File(());

    /// Opens files.
    #[derive(Debug, Default)]
    pub struct OpenOptions(());
}

/// Tasks.
pub mod task {
    use std::future::Future;
    use std::marker::PhantomData;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// Why a task gave no output.
    #[derive(Debug)]
    pub struct JoinError(());

    /// The output of a spawned task.
    #[derive(Debug)]
    pub struct JoinHandle<T>(PhantomData<T>);

    impl<T> Future for JoinHandle<T> {
        type Output = Result<T, JoinError>;

        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Ready(Err(JoinError(())))
        }
    }

    /// Spawns `future` on the runtime.
    pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        drop(future);
        JoinHandle(PhantomData)
    }

    /// Runs `f` on a blocking thread.
    pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        drop(f);
        JoinHandle(PhantomData)
    }

    /// Spawns `future` on the current `LocalSet`.
    pub fn spawn_local<F: Future + 'static>(future: F) -> JoinHandle<F::Output> {
        drop(future);
        JoinHandle(PhantomData)
    }

    /// Runs `f` on this worker thread, moving its tasks elsewhere.
    pub fn block_in_place<F: FnOnce() -> R, R>(f: F) -> R {
        f()
    }

    /// A set of spawned tasks.
    #[derive(Debug, Default)]
    pub struct JoinSet<T>(PhantomData<T>);

    /// Tasks that run on one thread.
    #[derive(Debug, Default)]
    pub struct LocalSet(());
}

/// Runtimes.
pub mod runtime {
    use std::future::Future;
    use std::io;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// Builds a runtime.
    #[derive(Debug)]
    pub struct Builder(());

    impl Builder {
        /// A multi-threaded runtime.
        pub fn new_multi_thread() -> Self {
            Self(())
        }

        /// A runtime on the current thread.
        pub fn new_current_thread() -> Self {
            Self(())
        }

        /// Turns on every driver.
        pub fn enable_all(&mut self) -> &mut Self {
            self
        }

        /// The runtime.
        pub fn build(&mut self) -> io::Result<Runtime> {
            Ok(Runtime(()))
        }
    }

    /// A runtime.
    #[derive(Debug)]
    pub struct Runtime(());

    impl Runtime {
        /// Polls `future` until it finishes.
        pub fn block_on<F: Future>(&self, future: F) -> F::Output {
            let mut future = pin!(future);
            let mut cx = Context::from_waker(Waker::noop());
            loop {
                if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
                    return output;
                }
            }
        }
    }

    /// A runtime on the current thread that is not `Send`.
    #[derive(Debug)]
    pub struct LocalRuntime(());

    /// A handle to a runtime.
    #[derive(Debug, Clone)]
    pub struct Handle(());

    impl Handle {
        /// The current runtime's handle, if there is one.
        pub fn try_current() -> Result<Self, TryCurrentError> {
            Err(TryCurrentError(()))
        }
    }

    /// There is no current runtime.
    #[derive(Debug)]
    pub struct TryCurrentError(());
}

/// Child processes.
pub mod process {
    use std::ffi::OsStr;
    use std::io;

    use super::unsupported;

    /// Starts a child process.
    #[derive(Debug)]
    pub struct Command(());

    impl Command {
        /// A command that runs `program`.
        pub fn new(program: impl AsRef<OsStr>) -> Self {
            let _ = program;
            Self(())
        }

        /// Starts the child.
        pub fn spawn(&mut self) -> io::Result<Child> {
            unsupported()
        }
    }

    /// A child process.
    #[derive(Debug)]
    pub struct Child(());
}
