//! Identity state on a one-voter group on `mem` stores, on the simulator:
//! create, get, and list of each kind (ARFC 0006 section 2.9, ARFC 0007
//! section 2.5), and login codes issued and exchanged once (ARFC 0002
//! section 3.7), also against a racing exchange and after expiry.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use alloy_crypto::aead::AeadKey;
use alloy_rt::{Runtime, WallTime};
use alloy_seam::{Detail, ImplName, RegistryBuilder, SeamConfig};
use alloy_sim::{Failure, Fault, HostConfig, HostId, RunConfig, RunResult, World, run};
use alloy_state::{
    ACCESS_KEY_KIND, AccessKeyId, ClusterRole, CodeId, CodeState, Group, GroupError, IdentityError,
    IssuedBy, JoinTokenSpec, LOGIN_CODE_KIND, LogStoreSeam, MemStoreConfig, NodeId, ResourceError,
    Role, SEALED_SECRET_LEN, Scope, StateStoreSeam, TokenId, register_mem,
};

const ME: NodeId = NodeId([0x11; 16]);
const SECRET: [u8; 16] = [0x42; 16];
const AT_REST: [u8; 32] = [0x24; 32];

fn fail(error: impl std::fmt::Debug) -> Failure {
    Failure(Detail::new(&format!("{error:?}")))
}

fn check(ok: bool, what: &str) -> Result<(), Failure> {
    if ok {
        Ok(())
    } else {
        Err(Failure(Detail::new(what)))
    }
}

/// A serving one-voter group on `mem` stores on a new simulated host.
async fn serving(world: &World) -> Result<(HostId, Runtime, Group), Failure> {
    let boot = Arc::new(|_| -> alloy_rt::BoxFuture<'static, ()> { Box::pin(async {}) });
    let host = world.add_host(HostConfig::new(boot)).map_err(fail)?;
    let runtime = world.runtime(host).map_err(fail)?;
    let mut builder = RegistryBuilder::new();
    register_mem(&mut builder).map_err(fail)?;
    let registry = builder.build();
    let config = MemStoreConfig {
        name: "global".to_owned(),
        fail_after: 0,
    }
    .to_value(1)
    .map_err(fail)?;
    let mem = ImplName::new("mem").map_err(fail)?;
    let log = registry
        .instantiate::<LogStoreSeam>(&runtime, &mem, &config)
        .map_err(fail)?;
    let state = registry
        .instantiate::<StateStoreSeam>(&runtime, &mem, &config)
        .map_err(fail)?;
    let group = Group::open(&runtime, ME, log, state).await.map_err(fail)?;
    group.initialize().await.map_err(fail)?;
    Ok((host, runtime, group))
}

/// The org `default` and its user `admin`, as `init` creates them (ARFC
/// 0002 section 3.15).
async fn init(group: &Group) -> Result<(), Failure> {
    group.create_org("default", true).await.map_err(fail)?;
    group
        .create_user(
            "default",
            "admin",
            true,
            BTreeSet::from([ClusterRole::CloudAdmin]),
        )
        .await
        .map_err(fail)?;
    Ok(())
}

async fn issue(group: &Group) -> Result<CodeId, Failure> {
    let code = group
        .issue_login_code(
            "default",
            "admin",
            alloy_crypto::hash(&SECRET),
            IssuedBy::Init,
        )
        .await
        .map_err(fail)?;
    Ok(code.code_id)
}

fn failing_seeds<F, Fut>(count: u64, scenario: F) -> Vec<(u64, Option<Detail>)>
where
    F: Fn(World) -> Fut,
    Fut: Future<Output = Result<(), Failure>> + Send + 'static,
{
    (0..count)
        .filter_map(|seed| {
            let summary = run(RunConfig::new(seed), &scenario);
            (summary.result != RunResult::Pass).then_some((seed, summary.failure))
        })
        .collect()
}

fn token_spec(runtime: &Runtime) -> JoinTokenSpec {
    JoinTokenSpec {
        token_id: TokenId([0xab; 8]),
        token_pub: [3; 32],
        cluster_key_fp: [4; 16],
        expire_time: WallTime(runtime.clock().wall().0 + 86_400_000_000_000),
        use_limit: 0,
        preset_roles: BTreeSet::from([Role::Compute]),
        preset_zone: String::new(),
        revoked: false,
    }
}

async fn kinds(world: World) -> Result<(), Failure> {
    let (_, runtime, group) = serving(&world).await?;
    let token = group
        .create_join_token(token_spec(&runtime))
        .await
        .map_err(fail)?;
    check(token.uses == 0 && token.generation == 1, "a new token")?;
    check(token.name() == "token-abababababababab", "its name")?;
    let read = group
        .get_join_token(TokenId([0xab; 8]))
        .await
        .map_err(fail)?;
    check(read.as_ref() == Some(&token), "the token reads back")?;
    let mut used = token.clone();
    used.uses = 1;
    let used = group.update_join_token(&used).await.map_err(fail)?;
    check(used.uses == 1 && used.generation == 2, "a counted use")?;
    let tokens = group.list_join_tokens().await.map_err(fail)?;
    check(tokens == vec![used], "one token is listed")?;
    let again = group.create_join_token(token_spec(&runtime)).await;
    check(
        matches!(
            again,
            Err(GroupError::Resource(ResourceError::AlreadyExists))
        ),
        "a token id is unique",
    )?;
    let mut zoned = token_spec(&runtime);
    zoned.token_id = TokenId([1; 8]);
    zoned.preset_zone = "z".repeat(64);
    check(
        matches!(
            group.create_join_token(zoned).await,
            Err(GroupError::Resource(ResourceError::Invalid(_)))
        ),
        "a preset zone over 63 bytes",
    )?;

    init(&group).await?;
    group.create_org("other", false).await.map_err(fail)?;
    let orgs = group.list_orgs().await.map_err(fail)?;
    check(
        orgs.iter()
            .map(|o| (o.name.as_str(), o.enabled))
            .collect::<Vec<_>>()
            == vec![("default", true), ("other", false)],
        "two orgs are listed",
    )?;
    let default = group.get_org("default").await.map_err(fail)?;
    check(default.is_some_and(|o| o.enabled), "default reads back")?;
    check(
        matches!(
            group.create_org("Bad", true).await,
            Err(GroupError::Resource(ResourceError::Invalid(_)))
        ),
        "an org name out of section 7",
    )?;
    let admin = group
        .get_user("default", "admin")
        .await
        .map_err(fail)?
        .ok_or_else(|| fail("admin is missing"))?;
    check(
        admin.enabled && admin.cluster_roles == BTreeSet::from([ClusterRole::CloudAdmin]),
        "admin reads back",
    )?;
    group
        .create_user("other", "admin", true, BTreeSet::new())
        .await
        .map_err(fail)?;
    let users = group.list_users().await.map_err(fail)?;
    check(
        users
            .iter()
            .map(|u| (u.org.as_str(), u.name.as_str()))
            .collect::<Vec<_>>()
            == vec![("default", "admin"), ("other", "admin")],
        "a user name is unique within its org only",
    )?;

    let code_id = issue(&group).await?;
    let code = group
        .get_login_code(code_id)
        .await
        .map_err(fail)?
        .ok_or_else(|| fail("the code is missing"))?;
    let wall = runtime.clock().wall();
    check(
        code.state == CodeState::Unused
            && code.user == admin.uid
            && code.org == "default"
            && code.issued_by == IssuedBy::Init
            && code.secret_hash == alloy_crypto::hash(&SECRET)
            && code.expire_time.0 > wall.0
            && code.expire_time.0 <= wall.0 + 86_400_000_000_000,
        "the code records its hash, issuer, user, and expiry",
    )?;
    check(
        group.list_login_codes().await.map_err(fail)? == vec![code],
        "one code is listed",
    )?;
    let at_rest = AeadKey::from_bytes(&AT_REST);
    let exchanged = group
        .exchange_login_code(code_id, &SECRET, "laptop", None, &at_rest)
        .await
        .map_err(fail)?;
    let key = group
        .get_access_key(&exchanged.key.id)
        .await
        .map_err(fail)?
        .ok_or_else(|| fail("the key is missing"))?;
    check(key == exchanged.key, "the key reads back")?;
    check(
        group.list_access_keys().await.map_err(fail)? == vec![key],
        "one key is listed",
    )?;

    // Neither kind is created through the resource operations.
    for kind in [ACCESS_KEY_KIND, LOGIN_CODE_KIND] {
        let made = group
            .create(
                kind,
                Scope::Org("default".to_owned()),
                "x",
                Vec::new(),
                BTreeMap::new(),
            )
            .await;
        check(
            matches!(made, Err(GroupError::Resource(ResourceError::Invalid(_)))),
            "a generic create of an AccessKey or LoginCode",
        )?;
    }
    let bare = group
        .create("User", Scope::Cluster, "admin", Vec::new(), BTreeMap::new())
        .await;
    check(
        matches!(bare, Err(GroupError::Resource(ResourceError::Invalid(_)))),
        "a User in the cluster scope",
    )?;
    group.shutdown().await.map_err(fail)
}

#[test]
fn each_kind_is_created_read_and_listed() {
    assert_eq!(failing_seeds(5, kinds), Vec::new());
}

async fn once(world: World) -> Result<(), Failure> {
    let (_, _, group) = serving(&world).await?;
    init(&group).await?;
    let at_rest = AeadKey::from_bytes(&AT_REST);
    let code_id = issue(&group).await?;
    let exchanged = group
        .exchange_login_code(code_id, &SECRET, "laptop", None, &at_rest)
        .await
        .map_err(fail)?;
    let key = &exchanged.key;
    check(
        key.key_name == "laptop"
            && key.org == "default"
            && key.principal == exchanged.user.uid
            && key.issued_by == IssuedBy::Init
            && !key.revoked
            && exchanged.user.name == "admin",
        "the key is the user's, named as asked",
    )?;
    let id = key.id.as_str();
    check(
        AccessKeyId::parse(id).as_ref() == Some(&key.id) && exchanged.secret.expose().len() == 40,
        "the id and secret have the forms of ARFC 0007 section 2.3",
    )?;
    // ARFC 0007 section 3.11: nonce, then the sealed secret, with the label
    // and the key id as associated data.
    check(
        key.secret_sealed.len() == SEALED_SECRET_LEN,
        "sealed length",
    )?;
    let (nonce, sealed) = key.secret_sealed.split_at(24);
    let nonce: [u8; 24] = nonce.try_into().map_err(fail)?;
    let mut aad = b"alloy/at-rest/access-key/v1".to_vec();
    aad.extend_from_slice(id.as_bytes());
    let opened = at_rest.open(&nonce, &aad, sealed).map_err(fail)?;
    check(
        opened.as_slice() == exchanged.secret.expose().as_bytes(),
        "the sealed secret opens to the secret",
    )?;
    let code = group.get_login_code(code_id).await.map_err(fail)?;
    check(
        code.is_some_and(|c| c.state == CodeState::Spent),
        "the code is spent",
    )?;
    let second = group
        .exchange_login_code(code_id, &SECRET, "laptop", None, &at_rest)
        .await;
    check(
        matches!(second, Err(IdentityError::LoginCodeUsed)),
        "the second exchange is refused",
    )?;
    check(
        group.list_access_keys().await.map_err(fail)?.len() == 1,
        "the second exchange creates no key",
    )?;

    // A wrong secret is refused as an unknown code is, and spends the code.
    let wrong = issue(&group).await?;
    let unknown = group
        .exchange_login_code(CodeId([0; 8]), &SECRET, "laptop", None, &at_rest)
        .await;
    let guessed = group
        .exchange_login_code(wrong, &[0; 16], "laptop", None, &at_rest)
        .await;
    check(
        matches!(unknown, Err(IdentityError::Unauthenticated))
            && matches!(guessed, Err(IdentityError::Unauthenticated)),
        "unknown and wrong are refused alike",
    )?;
    let after = group
        .exchange_login_code(wrong, &SECRET, "laptop", None, &at_rest)
        .await;
    check(
        matches!(after, Err(IdentityError::LoginCodeUsed)),
        "a wrong secret spends the code",
    )?;

    // A failure after the secret matches leaves the code unspent; a name in
    // use gets a number; `replaces` revokes the key it names.
    let next = issue(&group).await?;
    let bad = group
        .exchange_login_code(next, &SECRET, "Laptop", None, &at_rest)
        .await;
    check(
        matches!(bad, Err(IdentityError::InvalidName(_))),
        "a name out of section 7",
    )?;
    let numbered = group
        .exchange_login_code(next, &SECRET, "laptop", None, &at_rest)
        .await
        .map_err(fail)?;
    check(numbered.key.key_name == "laptop-2", "a name in use")?;
    let third = issue(&group).await?;
    let replaced = group
        .exchange_login_code(
            third,
            &SECRET,
            "laptop",
            Some(exchanged.key.id.as_str()),
            &at_rest,
        )
        .await
        .map_err(fail)?;
    check(
        replaced.key.key_name == "laptop",
        "the replacing key takes the name of the key it replaces",
    )?;
    let ids = group
        .list_access_keys()
        .await
        .map_err(fail)?
        .into_iter()
        .map(|k| k.id)
        .collect::<BTreeSet<_>>();
    check(
        ids == BTreeSet::from([numbered.key.id, replaced.key.id]),
        "the replaced key is revoked and deleted",
    )?;
    group.shutdown().await.map_err(fail)
}

#[test]
fn a_login_code_is_exchanged_once() {
    assert_eq!(failing_seeds(10, once), Vec::new());
}

async fn racing(world: World) -> Result<(), Failure> {
    let (_, _, group) = serving(&world).await?;
    init(&group).await?;
    let at_rest = AeadKey::from_bytes(&AT_REST);
    let code_id = issue(&group).await?;
    let (a, b) = futures_util::future::join(
        group.exchange_login_code(code_id, &SECRET, "one", None, &at_rest),
        group.exchange_login_code(code_id, &SECRET, "two", None, &at_rest),
    )
    .await;
    let won = usize::from(a.is_ok()) + usize::from(b.is_ok());
    let lost = [&a, &b]
        .iter()
        .filter(|r| matches!(r, Err(IdentityError::LoginCodeUsed)))
        .count();
    check(
        won == 1 && lost == 1,
        "exactly one of two racing exchanges succeeds",
    )?;
    check(
        group.list_access_keys().await.map_err(fail)?.len() == 1,
        "the race creates one key",
    )?;
    group.shutdown().await.map_err(fail)
}

#[test]
fn of_two_racing_exchanges_one_succeeds() {
    assert_eq!(failing_seeds(20, racing), Vec::new());
}

async fn expiry(world: World) -> Result<(), Failure> {
    let (host, _, group) = serving(&world).await?;
    init(&group).await?;
    let at_rest = AeadKey::from_bytes(&AT_REST);
    let code_id = issue(&group).await?;
    let expire_time = group
        .get_login_code(code_id)
        .await
        .map_err(fail)?
        .ok_or_else(|| fail("the code is missing"))?
        .expire_time;
    world
        .apply(Fault::ClockStep {
            host,
            step_ns: 86_400_000_000_000,
        })
        .map_err(fail)?;
    let late = group
        .exchange_login_code(code_id, &SECRET, "laptop", None, &at_rest)
        .await;
    check(
        late.as_ref().err() == Some(&IdentityError::LoginCodeExpired { expire_time }),
        "an exchange after expiry is refused with the expiry time",
    )?;
    let code = group.get_login_code(code_id).await.map_err(fail)?;
    check(
        code.is_some_and(|c| c.state == CodeState::Unused),
        "an expired exchange leaves the code as it was",
    )?;
    check(
        group.list_access_keys().await.map_err(fail)?.is_empty(),
        "no key is created",
    )?;
    group.shutdown().await.map_err(fail)
}

#[test]
fn a_login_code_is_refused_after_expiry() {
    assert_eq!(failing_seeds(5, expiry), Vec::new());
}

async fn ten(world: World) -> Result<(), Failure> {
    let (_, _, group) = serving(&world).await?;
    init(&group).await?;
    let mut issued = BTreeMap::new();
    for _ in 0..11 {
        let code_id = issue(&group).await?;
        let code = group
            .get_login_code(code_id)
            .await
            .map_err(fail)?
            .ok_or_else(|| fail("the code is missing"))?;
        issued.insert(code_id, code.expire_time);
    }
    let held = group
        .list_login_codes()
        .await
        .map_err(fail)?
        .into_iter()
        .map(|c| c.code_id)
        .collect::<BTreeSet<_>>();
    let gone = issued
        .iter()
        .filter(|(id, _)| !held.contains(id))
        .map(|(_, expire)| *expire)
        .collect::<Vec<_>>();
    let oldest = issued.values().min().copied();
    check(
        held.len() == 10 && gone.len() == 1 && gone.first().copied() == oldest,
        "the eleventh code deletes the oldest",
    )?;
    let missing = group
        .issue_login_code("default", "nobody", [0; 32], IssuedBy::Init)
        .await;
    check(
        matches!(
            missing,
            Err(IdentityError::Group(GroupError::Resource(
                ResourceError::NotFound
            )))
        ),
        "a code for no user",
    )?;
    group.create_org("off", false).await.map_err(fail)?;
    group
        .create_user("off", "admin", true, BTreeSet::new())
        .await
        .map_err(fail)?;
    let disabled = group
        .issue_login_code(
            "off",
            "admin",
            [0; 32],
            IssuedBy::LocalAdmin("ctl".to_owned()),
        )
        .await;
    check(
        matches!(disabled, Err(IdentityError::PermissionDenied)),
        "a code for a user of a disabled org",
    )?;
    group.shutdown().await.map_err(fail)
}

#[test]
fn a_user_holds_at_most_ten_unused_codes() {
    assert_eq!(failing_seeds(3, ten), Vec::new());
}
