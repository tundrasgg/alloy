//! Section 5.2: configuration of kind `state-store`, and the `mem` options
//! that model restarts and storage failures.

use std::path::Path;

use alloy_seam::{BuildError, ConfigError, ConfigValue};

use crate::model::StateModel;
use crate::store::expect_io;
use crate::support::{
    Ctx, apply_ok, batch, build, expect_model, fail, id, install, mem_config, native_config,
    place_config, proto_bytes, proto_varint, put,
};
use crate::{Applies, Check, Env, Implementation, Outcome};

pub(crate) fn checks() -> Vec<Check> {
    vec![
        check!(
            "arfc-0006#5.2",
            Applies::Only(Implementation::Mem),
            arfc0006_s5_2_mem_fail_after_fails_later_mutations_with_io
        ),
        check!(
            "arfc-0006#5.2",
            Applies::Only(Implementation::Mem),
            arfc0006_s5_2_mem_second_live_instance_of_a_name_fails
        ),
        check!(
            "arfc-0006#5.2",
            Applies::Only(Implementation::Native),
            arfc0006_s5_2_native_second_live_instance_of_a_dir_fails
        ),
        check!(
            "arfc-0006#5.2",
            Applies::Only(Implementation::Mem),
            arfc0006_s5_2_mem_invalid_name_fails_config_invalid
        ),
        check!(
            "arfc-0006#5.2",
            Applies::Only(Implementation::Native),
            arfc0006_s5_2_native_invalid_dir_fails_config_invalid
        ),
        check!(
            "arfc-0006#5.2",
            Applies::All,
            arfc0006_s5_2_only_version_1_is_read
        ),
        check!(
            "arfc-0006#5.2",
            Applies::All,
            arfc0006_s5_2_unknown_fields_are_ignored
        ),
    ]
}

/// Fails unless `result` is `BuildError::Failed`.
fn expect_failed<T>(result: Result<T, BuildError>, what: &str) -> Outcome {
    match result {
        Err(BuildError::Failed(_)) => Ok(()),
        Err(other) => Err(fail(format!("{what}: got {other:?}, expected Failed"))),
        Ok(_) => Err(fail(format!("{what}: built, expected Failed"))),
    }
}

/// Fails unless `result` is `Config(Invalid { .. })` naming `field`.
fn expect_invalid<T>(result: Result<T, BuildError>, field: &str, what: &str) -> Outcome {
    match result {
        Err(BuildError::Config(ConfigError::Invalid { field: named, .. }))
            if named.as_str() == field =>
        {
            Ok(())
        }
        Err(other) => Err(fail(format!(
            "{what}: got {other:?}, expected Config(Invalid) naming {field}"
        ))),
        Ok(_) => Err(fail(format!(
            "{what}: built, expected Config(Invalid) naming {field}"
        ))),
    }
}

/// With `fail_after` above 0, once that many mutating calls (`apply` and
/// `finish`) have succeeded on the shared contents, every later one fails
/// with `Io` and changes nothing. Failed calls are not counted.
async fn arfc0006_s5_2_mem_fail_after_fails_later_mutations_with_io(env: Env) -> Outcome {
    let config = mem_config("store-0", 2)?;
    let store = build(&env, &config)?.ctx("building with fail_after 2")?;
    let mut model = StateModel::default();
    apply_ok(&*store, &mut model, batch(5, vec![put(b"a", b"1")])).await?;
    let stale = store.apply(batch(5, vec![put(b"a", b"2")])).await;
    ensure!(stale.is_err(), "a stale apply succeeded");
    let incoming = StateModel::with(Some(id(2, 9)), &[(b"b", b"2")]);
    install(&*store, &incoming.export(), 64)
        .await?
        .ctx("the second mutating call, an install")?;
    model = incoming;
    expect_io(
        store.apply(batch(10, vec![put(b"c", b"3")])).await,
        "apply after fail_after",
    )?;
    drop(store);

    // The contents outlive the instance; a new one on the same name holds
    // only the two successful calls and still fails every mutation.
    let again = build(&env, &config)?.ctx("building again after the failure")?;
    expect_model(&*again, &model, "after the failed apply").await?;
    expect_io(
        again.apply(batch(10, Vec::new())).await,
        "apply on a new instance",
    )?;
    drop(again);
    let again = build(&env, &config)?.ctx("building a third instance")?;
    let pending = again.begin_install().await.ctx("begin_install")?;
    pending
        .write(StateModel::default().export())
        .await
        .ctx("write")?;
    expect_io(pending.finish().await, "finish past fail_after")?;
    drop(again);
    let last = build(&env, &config)?.ctx("building a fourth instance")?;
    expect_model(&*last, &model, "after the failed mutations").await
}

/// While an instance lives, building another of the same kind and `name`
/// fails with `Failed`; another name builds, and the name builds again once
/// the instance is dropped.
async fn arfc0006_s5_2_mem_second_live_instance_of_a_name_fails(env: Env) -> Outcome {
    let config = mem_config("store-0", 0)?;
    let first = build(&env, &config)?.ctx("the first instance")?;
    expect_failed(build(&env, &config)?, "a second instance of the same name")?;
    let other = build(&env, &mem_config("store-1", 0)?)?.ctx("an instance of another name")?;
    drop(other);
    drop(first);
    build(&env, &config)?.ctx("the name after its instance is dropped")?;
    Ok(())
}

/// While an instance lives, building another of the same kind on the same
/// `dir` fails with `Failed`; another dir builds, and the dir builds again
/// once the instance is dropped.
async fn arfc0006_s5_2_native_second_live_instance_of_a_dir_fails(env: Env) -> Outcome {
    let config = place_config(&env, 0)?;
    let first = build(&env, &config)?.ctx("the first instance")?;
    expect_failed(build(&env, &config)?, "a second instance on the same dir")?;
    let other = build(&env, &place_config(&env, 1)?)?.ctx("an instance on another dir")?;
    drop(other);
    drop(first);
    build(&env, &config)?.ctx("the dir after its instance is dropped")?;
    Ok(())
}

/// A missing or invalid `name` fails with `Config(Invalid)` naming `name`;
/// names of 1 to 64 bytes of `[a-z0-9-]` build.
async fn arfc0006_s5_2_mem_invalid_name_fails_config_invalid(env: Env) -> Outcome {
    let missing = ConfigValue::defaults(1).ctx("an empty configuration")?;
    expect_invalid(build(&env, &missing)?, "name", "a missing name")?;
    let long = "a".repeat(65);
    for bad in [
        "",
        long.as_str(),
        "Store",
        "a_b",
        "a b",
        "a.b",
        "caf\u{e9}",
        "a/b",
    ] {
        expect_invalid(
            build(&env, &mem_config(bad, 0)?)?,
            "name",
            &format!("name {bad:?}"),
        )?;
    }
    let longest = "z".repeat(64);
    for good in ["a", "0", "-", "a-0-z", longest.as_str()] {
        let store = build(&env, &mem_config(good, 0)?)?.ctx(&format!("name {good:?}"))?;
        drop(store);
    }
    Ok(())
}

/// A missing, relative, or absent `dir` fails with `Config(Invalid)` naming
/// `dir`.
async fn arfc0006_s5_2_native_invalid_dir_fails_config_invalid(env: Env) -> Outcome {
    let missing = ConfigValue::defaults(1).ctx("an empty configuration")?;
    expect_invalid(build(&env, &missing)?, "dir", "a missing dir")?;
    let base = env
        .dirs
        .first()
        .ctx("the harness gives a native check a directory")?;
    let absent = Path::new(base).join("absent");
    let absent = absent.to_str().ctx("a native path")?;
    for bad in ["relative-dir", "./relative", absent] {
        expect_invalid(
            build(&env, &native_config(bad)?)?,
            "dir",
            &format!("dir {bad:?}"),
        )?;
    }
    Ok(())
}

/// Both implementations read version 1 only.
async fn arfc0006_s5_2_only_version_1_is_read(env: Env) -> Outcome {
    let valid = place_config(&env, 0)?;
    let newer = ConfigValue::new(2, valid.payload().to_vec()).ctx("a version 2 value")?;
    match build(&env, &newer)? {
        Err(BuildError::Config(ConfigError::UnsupportedVersion {
            version: 2,
            min: 1,
            max: 1,
        })) => Ok(()),
        Err(other) => Err(fail(format!(
            "version 2: got {other:?}, expected UnsupportedVersion"
        ))),
        Ok(_) => Err(fail("version 2: built, expected UnsupportedVersion")),
    }
}

/// Both implementations ignore unknown fields.
async fn arfc0006_s5_2_unknown_fields_are_ignored(env: Env) -> Outcome {
    let valid = place_config(&env, 0)?;
    let mut payload = valid.payload().to_vec();
    payload.extend(proto_varint(15, 7));
    payload.extend(proto_bytes(16, b"unknown"));
    let extended = ConfigValue::new(1, payload).ctx("a value with unknown fields")?;
    let store = build(&env, &extended)?.ctx("building with unknown fields")?;
    store
        .apply(batch(1, vec![put(b"k", b"v")]))
        .await
        .ctx("apply")?;
    ensure_eq!(
        store.get(b"k").await.ctx("get")?,
        Some(b"v".to_vec()),
        "get"
    );
    Ok(())
}
