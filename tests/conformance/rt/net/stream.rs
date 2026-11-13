//! Streams and listeners, clauses 2.3 and 3.1.9.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::task::Poll;

use alloy_rt::{
    ListenOptions, Listener, Net, NetError, NetSeam, Stream, StreamAddr, StreamOptions, UnixPath,
};

use super::loopback_v4;
use crate::support::{Ctx, PATIENCE, SETTLE, fail, fresh, name, poll_once, skip, sleep, within};
use crate::{Applies, Check, Env, Failure, Implementation, Outcome};

pub(super) fn checks() -> Vec<Check> {
    vec![
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_connect_and_accept_over_tcp
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_connect_with_no_listener_is_connection_refused
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_a_closed_tcp_port_can_be_listened_on_again_at_once
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_listen_backlog_outside_1_to_65535_is_invalid_input
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_accept_returns_connections_in_arrival_order
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_dropped_accept_has_taken_no_connection
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_dropped_connect_leaves_no_connection_open
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_read_returns_between_1_and_len_bytes
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_read_into_an_empty_buf_returns_0_at_once
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_write_of_no_bytes_returns_0
        ),
        check!(
            "arfc-0005#2.3",
            Applies::All,
            arfc0005_s2_3_write_takes_bufs_in_order
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_shutdown_write_gives_the_peer_end_of_stream
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_dropping_a_stream_closes_both_directions
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_read_and_write_fail_with_connection_reset_after_a_reset
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_a_second_pending_read_is_busy
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_a_second_pending_write_is_busy
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_dropped_read_has_read_nothing
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_dropped_write_has_written_nothing
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::Only(Implementation::Mem),
            arfc0005_s3_1_9_mem_a_direction_holds_at_most_262144_bytes
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_unix_stream_round_trip
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_unix_listen_on_an_existing_path_is_addr_in_use
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_unix_listener_drop_removes_its_file
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_unix_addresses_are_unsupported_without_unix_sockets
        ),
        check!(
            "arfc-0005#3.1.9",
            Applies::All,
            arfc0005_s3_1_9_an_instance_holds_at_most_65536_streams_and_listeners
        ),
    ]
}

/// Open streams and listeners per `net` instance (clause 3.1.9).
const STREAMS: usize = 65_536;

/// `mem` buffer of one direction of a connection (clause 3.1.9).
const MEM_STREAM_BUFFER: usize = 262_144;

fn tcp(ip: IpAddr) -> StreamAddr {
    StreamAddr::Ip(SocketAddr::new(ip, 0))
}

fn listen(net: &dyn Net, addr: StreamAddr) -> Result<Box<dyn Listener>, Failure> {
    net.listen(addr, ListenOptions::default()).ctx("listen")
}

async fn connect(env: &Env, net: &dyn Net, addr: &StreamAddr) -> Result<Box<dyn Stream>, Failure> {
    within(
        env,
        PATIENCE,
        net.connect(addr.clone(), StreamOptions::default()),
    )
    .await
    .ctx("connect")?
    .ctx("connect")
}

async fn accept(env: &Env, listener: &dyn Listener) -> Result<Box<dyn Stream>, Failure> {
    within(env, PATIENCE, listener.accept())
        .await
        .ctx("accept")?
        .ctx("accept")
}

/// A listener on the IPv4 loopback address, a client connected to it, and
/// the server end of that connection.
struct Pair {
    listener: Box<dyn Listener>,
    client: Box<dyn Stream>,
    server: Box<dyn Stream>,
}

async fn tcp_pair(env: &Env) -> Result<Pair, Failure> {
    let ip = loopback_v4(env).await?;
    let net = env.runtime.net().clone();
    let listener = listen(net.as_ref(), tcp(ip))?;
    let client = connect(env, net.as_ref(), &listener.local_addr()).await?;
    let server = accept(env, listener.as_ref()).await?;
    Ok(Pair {
        listener,
        client,
        server,
    })
}

async fn write_all(env: &Env, stream: &dyn Stream, bytes: &[u8]) -> Result<(), Failure> {
    let mut done = 0;
    while done < bytes.len() {
        let bufs = [&bytes[done..]];
        let written = within(env, PATIENCE, stream.write(&bufs))
            .await
            .ctx("write")?
            .ctx("write")?;
        ensure!(
            written >= 1,
            "write of {} bytes wrote 0",
            bytes.len() - done
        );
        done += written;
    }
    Ok(())
}

/// Reads exactly `len` bytes, checking each read's count.
async fn read_exact(env: &Env, stream: &dyn Stream, len: usize) -> Result<Vec<u8>, Failure> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let mut buf = vec![0u8; len - out.len()];
        let count = within(env, PATIENCE, stream.read(&mut buf))
            .await
            .ctx("read")?
            .ctx("read")?;
        ensure!(
            count >= 1,
            "read returned end of stream after {} of {len} bytes",
            out.len()
        );
        ensure!(
            count <= buf.len(),
            "read returned {count} for a {}-byte buffer",
            buf.len()
        );
        out.extend_from_slice(&buf[..count]);
    }
    Ok(out)
}

/// Reads until end of stream and returns everything read.
async fn read_to_end(env: &Env, stream: &dyn Stream) -> Result<Vec<u8>, Failure> {
    let mut out = Vec::new();
    loop {
        let mut buf = vec![0u8; 65_536];
        let count = within(env, PATIENCE, stream.read(&mut buf))
            .await
            .ctx("read")?
            .ctx("read")?;
        if count == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..count]);
    }
}

/// Writes 1 MiB chunks until a write waits for buffer space, and returns the
/// bytes written before it with the waiting write's future.
async fn fill<'a>(
    stream: &'a dyn Stream,
    bufs: &'a [&'a [u8]],
) -> Result<(usize, alloy_rt::BoxFuture<'a, Result<usize, NetError>>), Failure> {
    let mut written = 0;
    for _ in 0..1_024 {
        let mut writing = stream.write(bufs);
        match poll_once(&mut writing).await {
            Poll::Ready(result) => written += result.ctx("write")?,
            Poll::Pending => return Ok((written, writing)),
        }
    }
    Err(fail("1 GiB of writes never waited for buffer space"))
}

/// `connect` to `Ip` opens TCP to a listener, `accept` returns the other end,
/// and both carry bytes and report each other's addresses.
async fn arfc0005_s3_1_9_connect_and_accept_over_tcp(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    let listen_addr = pair.listener.local_addr();
    match &listen_addr {
        StreamAddr::Ip(addr) => ensure!(addr.port() != 0, "a listener on port 0 kept port 0"),
        StreamAddr::Unix(_) => return Err(fail("a TCP listener reported a Unix address")),
    }
    ensure_eq!(
        pair.client.peer_addr(),
        Some(listen_addr),
        "the client's peer"
    );
    ensure_eq!(
        pair.server.peer_addr(),
        pair.client.local_addr(),
        "the server's peer against the client's local address"
    );
    write_all(&env, pair.client.as_ref(), b"ping").await?;
    ensure_eq!(
        read_exact(&env, pair.server.as_ref(), 4).await?,
        b"ping".to_vec(),
        "bytes at the server"
    );
    write_all(&env, pair.server.as_ref(), b"pong!").await?;
    ensure_eq!(
        read_exact(&env, pair.client.as_ref(), 5).await?,
        b"pong!".to_vec(),
        "bytes at the client"
    );
    Ok(())
}

/// `connect` to a port with no listener fails with `ConnectionRefused`.
async fn arfc0005_s3_1_9_connect_with_no_listener_is_connection_refused(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let addr = listen(net.as_ref(), tcp(ip))?.local_addr();
    let result = within(&env, PATIENCE, net.connect(addr, StreamOptions::default()))
        .await
        .ctx("connect")?;
    ensure_eq!(
        result.err(),
        Some(NetError::ConnectionRefused),
        "connect to a closed port"
    );
    Ok(())
}

/// A TCP port whose earlier listener has closed can be listened on again at
/// once, also after a connection through it closed.
async fn arfc0005_s3_1_9_a_closed_tcp_port_can_be_listened_on_again_at_once(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    let addr = pair.listener.local_addr();
    write_all(&env, pair.client.as_ref(), b"x").await?;
    read_exact(&env, pair.server.as_ref(), 1).await?;
    drop(pair.server);
    drop(pair.client);
    drop(pair.listener);
    let again = env
        .runtime
        .net()
        .listen(addr.clone(), ListenOptions::default());
    ensure!(again.is_ok(), "listen again on {addr:?}: {:?}", again.err());
    Ok(())
}

/// `listen` returns `InvalidInput` for a backlog outside 1 to 65,535.
async fn arfc0005_s3_1_9_listen_backlog_outside_1_to_65535_is_invalid_input(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    for backlog in [0, 65_536, u32::MAX] {
        let options = ListenOptions {
            backlog,
            ..ListenOptions::default()
        };
        let result = net.listen(tcp(ip), options);
        ensure!(
            matches!(result, Err(NetError::InvalidInput(_))),
            "listen with backlog {backlog}: {:?}",
            result.err()
        );
    }
    for backlog in [1, 65_535] {
        let options = ListenOptions {
            backlog,
            ..ListenOptions::default()
        };
        net.listen(tcp(ip), options)
            .ctx("listen with a backlog at a bound")?;
    }
    Ok(())
}

/// `accept` returns pending connections in arrival order.
async fn arfc0005_s3_1_9_accept_returns_connections_in_arrival_order(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let listener = listen(net.as_ref(), tcp(ip))?;
    let addr = listener.local_addr();
    let mut clients = Vec::new();
    for _ in 0..4 {
        clients.push(connect(&env, net.as_ref(), &addr).await?);
    }
    for (index, client) in clients.iter().enumerate() {
        let server = accept(&env, listener.as_ref()).await?;
        ensure_eq!(
            server.peer_addr(),
            client.local_addr(),
            "the peer of accepted connection {index}"
        );
    }
    Ok(())
}

/// A dropped `accept` future has taken no connection.
async fn arfc0005_s3_1_9_dropped_accept_has_taken_no_connection(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let listener = listen(net.as_ref(), tcp(ip))?;
    drop(listener.accept());
    let mut waiting = listener.accept();
    ensure!(
        poll_once(&mut waiting).await.is_pending(),
        "accept with nothing pending was ready"
    );
    drop(waiting);
    let client = connect(&env, net.as_ref(), &listener.local_addr()).await?;
    let server = accept(&env, listener.as_ref()).await?;
    ensure_eq!(server.peer_addr(), client.local_addr(), "the accepted peer");
    Ok(())
}

/// A `connect` future dropped before its first poll leaves no connection
/// open: nothing reaches the listener.
async fn arfc0005_s3_1_9_dropped_connect_leaves_no_connection_open(env: Env) -> Outcome {
    let ip = loopback_v4(&env).await?;
    let net = env.runtime.net().clone();
    let listener = listen(net.as_ref(), tcp(ip))?;
    drop(net.connect(listener.local_addr(), StreamOptions::default()));
    let accepted = within(&env, SETTLE, listener.accept()).await;
    ensure!(
        accepted.is_none(),
        "a connect dropped before its first poll reached the listener"
    );
    Ok(())
}

/// `read` waits for data and returns between 1 and `buf.len()` bytes.
async fn arfc0005_s3_1_9_read_returns_between_1_and_len_bytes(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    let bytes: Vec<u8> = (0..=255u8).cycle().take(1_000).collect();
    let mut waiting_buf = [0u8; 16];
    let mut waiting = pair.server.read(&mut waiting_buf);
    ensure!(
        poll_once(&mut waiting).await.is_pending(),
        "read with no data was ready"
    );
    drop(waiting);
    write_all(&env, pair.client.as_ref(), &bytes).await?;
    let mut got = Vec::new();
    for size in [1usize, 7, 64, 1_000] {
        if got.len() == bytes.len() {
            break;
        }
        got.extend(
            read_exact(
                &env,
                pair.server.as_ref(),
                size.min(bytes.len() - got.len()),
            )
            .await?,
        );
    }
    got.extend(read_exact(&env, pair.server.as_ref(), bytes.len() - got.len()).await?);
    ensure_eq!(got, bytes, "bytes read in chunks of several sizes");
    Ok(())
}

/// `read` with an empty `buf` returns 0 at once, with no data queued.
async fn arfc0005_s3_1_9_read_into_an_empty_buf_returns_0_at_once(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    let mut empty: [u8; 0] = [];
    let mut reading = pair.server.read(&mut empty);
    ensure_eq!(
        poll_once(&mut reading).await,
        Poll::Ready(Ok(0)),
        "first poll of a read into an empty buffer"
    );
    Ok(())
}

/// `write` with no bytes in `bufs` returns 0.
async fn arfc0005_s3_1_9_write_of_no_bytes_returns_0(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    let none: [&[u8]; 0] = [];
    let empties: [&[u8]; 2] = [&[], &[]];
    for bufs in [&none[..], &empties[..]] {
        let written = within(&env, PATIENCE, pair.client.write(bufs))
            .await
            .ctx("write")?;
        ensure_eq!(written, Ok(0), "write of {} empty buffers", bufs.len());
    }
    Ok(())
}

/// `write` writes from `bufs` in order and returns the bytes written.
async fn arfc0005_s2_3_write_takes_bufs_in_order(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    let parts: [&[u8]; 4] = [b"al", b"", b"lo", b"y-rt"];
    let whole: Vec<u8> = parts.concat();
    let mut sent = 0;
    while sent < whole.len() {
        let mut skip_bytes = sent;
        let mut rest: Vec<&[u8]> = Vec::new();
        for part in parts {
            if skip_bytes >= part.len() {
                skip_bytes -= part.len();
            } else {
                rest.push(&part[skip_bytes..]);
                skip_bytes = 0;
            }
        }
        let written = within(&env, PATIENCE, pair.client.write(&rest))
            .await
            .ctx("write")?
            .ctx("write")?;
        ensure!(
            (1..=whole.len() - sent).contains(&written),
            "write of {} bytes returned {written}",
            whole.len() - sent
        );
        sent += written;
    }
    ensure_eq!(
        read_exact(&env, pair.server.as_ref(), whole.len()).await?,
        whole,
        "bytes of a gathered write"
    );
    Ok(())
}

/// After `shutdown_write` completes, the peer reads every byte written
/// before it and then end of stream; `write` after `shutdown_write` fails
/// with `Closed`.
async fn arfc0005_s3_1_9_shutdown_write_gives_the_peer_end_of_stream(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    write_all(&env, pair.client.as_ref(), b"hello").await?;
    within(&env, PATIENCE, pair.client.shutdown_write())
        .await
        .ctx("shutdown_write")?
        .ctx("shutdown_write")?;
    ensure_eq!(
        read_to_end(&env, pair.server.as_ref()).await?,
        b"hello".to_vec(),
        "bytes before end of stream"
    );
    let mut buf = [0u8; 8];
    let again = within(&env, PATIENCE, pair.server.read(&mut buf))
        .await
        .ctx("read after end of stream")?;
    ensure_eq!(again, Ok(0), "a second read after end of stream");
    let bufs: [&[u8]; 1] = [b"late"];
    let late = within(&env, PATIENCE, pair.client.write(&bufs))
        .await
        .ctx("write after shutdown_write")?;
    ensure_eq!(late, Err(NetError::Closed), "write after shutdown_write");
    Ok(())
}

/// Dropping a stream closes both directions: the peer reads end of stream.
async fn arfc0005_s3_1_9_dropping_a_stream_closes_both_directions(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    write_all(&env, pair.client.as_ref(), b"last").await?;
    drop(pair.client);
    ensure_eq!(
        read_to_end(&env, pair.server.as_ref()).await?,
        b"last".to_vec(),
        "bytes before end of stream"
    );
    Ok(())
}

/// After a reset, `read` and `write` fail with `ConnectionReset`, and keep
/// failing with it. Dropping a stream with bytes unread resets the
/// connection.
async fn arfc0005_s3_1_9_read_and_write_fail_with_connection_reset_after_a_reset(
    env: Env,
) -> Outcome {
    let pair = tcp_pair(&env).await?;
    write_all(&env, pair.client.as_ref(), &[9u8; 1_000]).await?;
    sleep(&env, SETTLE).await;
    drop(pair.server);
    let mut buf = [0u8; 64];
    for attempt in 0..2 {
        let read = within(&env, PATIENCE, pair.client.read(&mut buf))
            .await
            .ctx("read after a reset")?;
        ensure_eq!(
            read,
            Err(NetError::ConnectionReset),
            "read {attempt} after a reset"
        );
    }
    let bufs: [&[u8]; 1] = [b"after"];
    let write = within(&env, PATIENCE, pair.client.write(&bufs))
        .await
        .ctx("write after a reset")?;
    ensure_eq!(write, Err(NetError::ConnectionReset), "write after a reset");
    Ok(())
}

/// A second `read` while one is pending fails at once with `Busy`.
async fn arfc0005_s3_1_9_a_second_pending_read_is_busy(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    let mut first_buf = [0u8; 16];
    let mut second_buf = [0u8; 16];
    let mut first = pair.server.read(&mut first_buf);
    ensure!(
        poll_once(&mut first).await.is_pending(),
        "read with no data was ready"
    );
    let mut second = pair.server.read(&mut second_buf);
    ensure_eq!(
        poll_once(&mut second).await,
        Poll::Ready(Err(NetError::Busy)),
        "first poll of a second read"
    );
    drop(second);
    drop(first);
    Ok(())
}

/// A second `write` while one is pending fails at once with `Busy`.
async fn arfc0005_s3_1_9_a_second_pending_write_is_busy(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    let chunk = vec![7u8; 1 << 20];
    let bufs: [&[u8]; 1] = [&chunk];
    let (_, waiting) = fill(pair.client.as_ref(), &bufs).await?;
    let mut second = pair.client.write(&bufs);
    ensure_eq!(
        poll_once(&mut second).await,
        Poll::Ready(Err(NetError::Busy)),
        "first poll of a second write"
    );
    drop(second);
    drop(waiting);
    Ok(())
}

/// A dropped `read` future has read nothing.
async fn arfc0005_s3_1_9_dropped_read_has_read_nothing(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    let mut lost = [0u8; 16];
    drop(pair.server.read(&mut lost));
    let mut waiting = pair.server.read(&mut lost);
    ensure!(
        poll_once(&mut waiting).await.is_pending(),
        "read with no data was ready"
    );
    drop(waiting);
    write_all(&env, pair.client.as_ref(), b"xyz").await?;
    ensure_eq!(
        read_exact(&env, pair.server.as_ref(), 3).await?,
        b"xyz".to_vec(),
        "bytes after dropped reads"
    );
    Ok(())
}

/// A dropped `write` future has written nothing.
async fn arfc0005_s3_1_9_dropped_write_has_written_nothing(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    let chunk = vec![7u8; 1 << 20];
    let bufs: [&[u8]; 1] = [&chunk];
    let (written, waiting) = fill(pair.client.as_ref(), &bufs).await?;
    drop(waiting);
    let marker = vec![0xEEu8; 4_096];
    let marker_bufs: [&[u8]; 1] = [&marker];
    drop(pair.client.write(&marker_bufs));

    let server = pair.server;
    let drain_env = env.clone();
    let reader = env
        .runtime
        .spawn(name("drain")?, async move {
            read_to_end(&drain_env, server.as_ref()).await
        })
        .ctx("spawn")?;
    within(&env, PATIENCE, pair.client.shutdown_write())
        .await
        .ctx("shutdown_write")?
        .ctx("shutdown_write")?;
    let got = within(&env, PATIENCE, reader.join())
        .await
        .ctx("join")?
        .ctx("join")??;
    ensure_eq!(got.len(), written, "bytes the peer read");
    ensure!(
        got.iter().all(|&byte| byte == 7),
        "bytes of a dropped write reached the peer"
    );
    Ok(())
}

/// `mem`: each direction holds at most 262,144 unread bytes; `write` writes
/// `min(free space, total length)` and waits while the free space is 0.
async fn arfc0005_s3_1_9_mem_a_direction_holds_at_most_262144_bytes(env: Env) -> Outcome {
    let pair = tcp_pair(&env).await?;
    let big = vec![1u8; 300_000];
    let bufs: [&[u8]; 1] = [&big];
    let first = within(&env, PATIENCE, pair.client.write(&bufs))
        .await
        .ctx("write")?
        .ctx("write")?;
    ensure_eq!(
        first,
        MEM_STREAM_BUFFER,
        "a write of 300000 bytes into an empty direction"
    );
    let rest: [&[u8]; 1] = [&big[..5_000]];
    let mut waiting = pair.client.write(&rest);
    ensure!(
        poll_once(&mut waiting).await.is_pending(),
        "a write into a full direction was ready"
    );
    ensure_eq!(
        read_exact(&env, pair.server.as_ref(), 1_000).await?.len(),
        1_000,
        "bytes read"
    );
    let second = within(&env, PATIENCE, waiting)
        .await
        .ctx("write after 1000 bytes were read")?
        .ctx("write")?;
    ensure_eq!(second, 1_000, "a write of 5000 bytes with 1000 free");
    Ok(())
}

/// A Unix socket path in the harness's directory.
fn unix_path(env: &Env, file: &str) -> Result<Option<UnixPath>, Failure> {
    let Some(dir) = &env.unix_dir else {
        return Ok(None);
    };
    UnixPath::new(&format!("{dir}/{file}"))
        .map(Some)
        .ctx("a Unix path in the harness directory")
}

/// `connect` to `Unix` opens a Unix stream socket to a listener, and the
/// streams carry bytes both ways.
async fn arfc0005_s3_1_9_unix_stream_round_trip(env: Env) -> Outcome {
    let Some(path) = unix_path(&env, "rt.sock")? else {
        return skip("the platform has no Unix domain sockets");
    };
    let net = env.runtime.net().clone();
    let addr = StreamAddr::Unix(path);
    let listener = listen(net.as_ref(), addr.clone())?;
    ensure_eq!(
        listener.local_addr(),
        addr.clone(),
        "the listener's address"
    );
    let client = connect(&env, net.as_ref(), &addr).await?;
    let server = accept(&env, listener.as_ref()).await?;
    ensure_eq!(client.peer_addr(), Some(addr), "the client's peer");
    write_all(&env, client.as_ref(), b"over unix").await?;
    ensure_eq!(
        read_exact(&env, server.as_ref(), 9).await?,
        b"over unix".to_vec(),
        "bytes at the server"
    );
    write_all(&env, server.as_ref(), b"back").await?;
    ensure_eq!(
        read_exact(&env, client.as_ref(), 4).await?,
        b"back".to_vec(),
        "bytes at the client"
    );
    Ok(())
}

/// `listen` on a Unix path that exists fails with `AddrInUse`.
async fn arfc0005_s3_1_9_unix_listen_on_an_existing_path_is_addr_in_use(env: Env) -> Outcome {
    let Some(path) = unix_path(&env, "dup.sock")? else {
        return skip("the platform has no Unix domain sockets");
    };
    let net = env.runtime.net().clone();
    let addr = StreamAddr::Unix(path);
    let _first = listen(net.as_ref(), addr.clone())?;
    let second = net.listen(addr, ListenOptions::default());
    ensure_eq!(
        second.err(),
        Some(NetError::AddrInUse),
        "listen on an existing path"
    );
    Ok(())
}

/// Dropping a Unix listener removes its socket file, so the path can be
/// listened on again.
async fn arfc0005_s3_1_9_unix_listener_drop_removes_its_file(env: Env) -> Outcome {
    let Some(path) = unix_path(&env, "gone.sock")? else {
        return skip("the platform has no Unix domain sockets");
    };
    let net = env.runtime.net().clone();
    let addr = StreamAddr::Unix(path);
    drop(listen(net.as_ref(), addr.clone())?);
    let again = net.listen(addr, ListenOptions::default());
    ensure!(
        again.is_ok(),
        "listen after the earlier listener was dropped: {:?}",
        again.err()
    );
    Ok(())
}

/// A platform without Unix sockets returns `Unsupported` for `Unix`
/// addresses.
async fn arfc0005_s3_1_9_unix_addresses_are_unsupported_without_unix_sockets(env: Env) -> Outcome {
    if env.unix_dir.is_some() {
        return skip("the platform has Unix domain sockets");
    }
    let net = env.runtime.net().clone();
    let addr = StreamAddr::Unix(UnixPath::new("/alloy-conformance.sock").ctx("UnixPath")?);
    let listened = net.listen(addr.clone(), ListenOptions::default());
    ensure_eq!(
        listened.err(),
        Some(NetError::Unsupported),
        "listen on a Unix address"
    );
    let connected = within(&env, PATIENCE, net.connect(addr, StreamOptions::default()))
        .await
        .ctx("connect")?;
    ensure_eq!(
        connected.err(),
        Some(NetError::Unsupported),
        "connect to a Unix address"
    );
    Ok(())
}

/// An instance holds at most 65,536 open streams and listeners together;
/// beyond that `listen` fails with `TooManyOpen`. Unix listeners are used
/// because they need no port. `os` may meet a lower platform limit first,
/// and must then still report `TooManyOpen`.
async fn arfc0005_s3_1_9_an_instance_holds_at_most_65536_streams_and_listeners(
    env: Env,
) -> Outcome {
    if env.unix_dir.is_none() {
        return skip("the platform has no Unix domain sockets");
    }
    let net: Arc<dyn Net> = fresh::<NetSeam>(&env, &env.config.net)?;
    let mut open = Vec::with_capacity(STREAMS);
    let refused = loop {
        if open.len() > STREAMS {
            return Err(fail("an instance held more than 65536 listeners"));
        }
        let path = unix_path(&env, &format!("{:05}", open.len()))?.ctx("a Unix path")?;
        match net.listen(StreamAddr::Unix(path), ListenOptions::default()) {
            Ok(listener) => open.push(listener),
            Err(error) => break error,
        }
    };
    ensure_eq!(refused, NetError::TooManyOpen, "listen beyond the limit");
    if env.implementation == Implementation::Mem {
        ensure_eq!(open.len(), STREAMS, "listeners a fresh mem instance held");
    }
    Ok(())
}
