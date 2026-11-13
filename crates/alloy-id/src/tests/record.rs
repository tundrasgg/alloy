use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use alloy_crypto::al;
use alloy_crypto::ed25519::SigningKey;
use alloy_rt::WallTime;
use proptest::prelude::*;
use prost::Message as _;

use crate::ids::{ClusterId, NodeId, Role, Roles};
use crate::proto;
use crate::record::{
    NodeRecord, RecordError, SignedNodeRecord, renew_message, sign_renew, verify_renew,
};

const NOT_AFTER: i64 = 1_800_000_000;

fn at(seconds: i64) -> WallTime {
    WallTime(seconds * 1_000_000_000)
}

fn record(identity: &SigningKey) -> NodeRecord {
    let cluster_id = ClusterId::from_bytes([0xc1; 16]);
    let mesh_pub = al::StaticSecret::from_bytes([5; 32]).public_key();
    NodeRecord {
        cluster_id,
        node_id: NodeId::of(&identity.verifying_key()),
        name: "node-1".to_owned(),
        identity_pub: identity.verifying_key(),
        mesh_pub: vec![mesh_pub],
        roles: Roles::of(&[Role::Anchor, Role::Control]),
        zone: "default".to_owned(),
        infra_addr: cluster_id.infra_addr(&mesh_pub),
        advertise: vec![
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)),
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 7)),
        ],
        mesh_port: 7476,
        api_port: 443,
        not_after: NOT_AFTER,
        serial: 3,
    }
}

#[test]
fn ids_and_infrastructure_address_follow_section_3_4() {
    let identity = SigningKey::from_seed(&[1; 32]);
    let id = NodeId::of(&identity.verifying_key());
    assert_eq!(
        id.as_bytes()[..],
        alloy_crypto::hash(identity.verifying_key().as_bytes())[..16]
    );
    let cluster_id = ClusterId::from_bytes([
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0xff,
    ]);
    assert_eq!(cluster_id.to_string(), "0123456789abcdef00000000000000ff");
    assert_eq!(
        cluster_id.infra_prefix(),
        "fd01:2345:6789::".parse::<Ipv6Addr>().expect("address")
    );
    let mesh = al::PublicKey::from_bytes([9; 32]);
    let addr = cluster_id.infra_addr(&mesh).octets();
    assert_eq!(addr[..6], [0xfd, 0x01, 0x23, 0x45, 0x67, 0x89]);
    assert_eq!(addr[6..8], [0, 0]);
    assert_eq!(addr[8..], alloy_crypto::hash(&[9; 32])[..8]);
}

#[test]
fn a_signed_record_verifies_and_keeps_its_bytes() {
    let cluster = SigningKey::from_seed(&[0xaa; 32]);
    let identity = SigningKey::from_seed(&[0xbb; 32]);
    let original = record(&identity);
    let signed = SignedNodeRecord::sign(&original, &cluster).expect("sign");
    assert_eq!(signed.record_bytes(), original.encode().expect("encode"));
    let wire = signed.to_proto();
    let again = SignedNodeRecord::from_proto(
        &proto::SignedNodeRecord::decode(&wire.encode_to_vec()[..]).expect("decode"),
    )
    .expect("from proto");
    assert_eq!(again, signed);
    assert_eq!(again.verify(&cluster.verifying_key()), Ok(original.clone()));
    let id = original.cluster_id;
    let public = cluster.verifying_key();
    assert_eq!(
        signed.accept(&id, &public, at(NOT_AFTER - 1)),
        Ok(original.clone())
    );
    assert!(original.is_valid_at(at(NOT_AFTER - 1)));
    assert_eq!(
        signed.accept(&id, &public, at(NOT_AFTER)),
        Err(RecordError::Expired)
    );
    assert_eq!(
        signed.accept(&ClusterId::from_bytes([0; 16]), &public, at(0)),
        Err(RecordError::Cluster)
    );
    assert_eq!(original.renew_at(), NOT_AFTER - 43_200);
    let other = SigningKey::from_seed(&[0xab; 32]).verifying_key();
    assert_eq!(signed.verify(&other), Err(RecordError::Signature));
}

#[test]
fn a_record_with_any_flipped_bit_is_refused() {
    let cluster = SigningKey::from_seed(&[0xaa; 32]);
    let signed = SignedNodeRecord::sign(&record(&SigningKey::from_seed(&[0xbb; 32])), &cluster)
        .expect("sign");
    let wire = signed.to_proto();
    for bit in 0..(wire.record.len() + wire.signature.len()) * 8 {
        let mut flipped = wire.clone();
        let (field, index) = if bit / 8 < wire.record.len() {
            (&mut flipped.record, bit / 8)
        } else {
            (&mut flipped.signature, bit / 8 - wire.record.len())
        };
        field[index] ^= 1 << (bit % 8);
        let flipped = SignedNodeRecord::from_proto(&flipped).expect("64 bytes");
        assert_eq!(
            flipped.verify(&cluster.verifying_key()).map(|_| ()),
            Err(RecordError::Signature),
            "bit {bit}"
        );
    }
}

#[test]
fn fields_out_of_range_are_refused() {
    let identity = SigningKey::from_seed(&[0xbb; 32]);
    let good = record(&identity);
    let wire = proto::NodeRecord::decode(&good.encode().expect("encode")[..]).expect("decode");
    let check = |change: &dyn Fn(&mut proto::NodeRecord), field: &'static str| {
        let mut bad = wire.clone();
        change(&mut bad);
        assert_eq!(
            NodeRecord::decode(&bad.encode_to_vec()),
            Err(RecordError::Field(field)),
            "{field}"
        );
    };
    check(&|r| r.cluster_id.truncate(15), "cluster_id");
    check(&|r| r.node_id[0] ^= 1, "node_id");
    check(&|r| r.identity_pub.push(0), "identity_pub");
    check(&|r| r.mesh_pub.clear(), "mesh_pub");
    check(&|r| r.mesh_pub = vec![vec![1; 32]; 3], "mesh_pub");
    check(&|r| r.mesh_pub = vec![vec![1; 31]], "mesh_pub");
    check(&|r| r.roles = vec![2, 1], "roles");
    check(&|r| r.roles = vec![1, 1], "roles");
    check(&|r| r.roles = vec![0], "roles");
    check(&|r| r.roles = vec![5], "roles");
    check(&|r| r.name.clear(), "name");
    check(&|r| r.name = "n".repeat(64), "name");
    check(&|r| r.zone = "z".repeat(64), "zone");
    check(&|r| r.infra_addr.truncate(4), "infra_addr");
    check(&|r| r.advertise = vec![vec![1; 5]], "advertise");
    check(&|r| r.advertise = vec![vec![1; 4]; 17], "advertise");
    check(&|r| r.mesh_port = 65_536, "mesh_port");
    check(&|r| r.api_port = u32::MAX, "api_port");
    assert_eq!(NodeRecord::decode(&[0xff]), Err(RecordError::Decode));
    let mut bad = good.clone();
    bad.mesh_pub.clear();
    assert_eq!(bad.encode(), Err(RecordError::Field("mesh_pub")));
    // A client node holds no role; two mesh keys during a rotation.
    let mut client = good;
    client.roles = Roles::NONE;
    client.mesh_pub.push(al::PublicKey::from_bytes([3; 32]));
    assert_eq!(
        NodeRecord::decode(&client.encode().expect("encode")),
        Ok(client)
    );
}

#[test]
fn renew_signature_covers_its_fields() {
    let identity = SigningKey::from_seed(&[0x11; 32]);
    let cluster_id = ClusterId::from_bytes([0x22; 16]);
    let node_id = NodeId::of(&identity.verifying_key());
    let message = renew_message(&cluster_id, &node_id, 0x0102_0304_0506_0708);
    assert_eq!(&message[..14], b"alloy-renew-v1");
    assert_eq!(&message[14..30], cluster_id.as_bytes());
    assert_eq!(&message[30..46], node_id.as_bytes());
    assert_eq!(&message[46..], [1, 2, 3, 4, 5, 6, 7, 8]);
    let signature = sign_renew(&identity, &cluster_id, &node_id, 7);
    let public = identity.verifying_key();
    assert!(verify_renew(&public, &cluster_id, &node_id, 7, &signature).is_ok());
    assert!(verify_renew(&public, &cluster_id, &node_id, 8, &signature).is_err());
    let other = ClusterId::from_bytes([0x23; 16]);
    assert!(verify_renew(&public, &other, &node_id, 7, &signature).is_err());
}

proptest! {
    #[test]
    fn decode_of_encode_is_identity(
        seed in any::<[u8; 32]>(),
        mesh in proptest::collection::vec(any::<[u8; 32]>(), 1..=2),
        roles in 0u8..16,
        name in "[a-z][a-z0-9-]{0,62}",
        zone in "[a-z0-9-]{0,63}",
        advertise in proptest::collection::vec(any::<[u8; 16]>(), 0..=16),
        ports in any::<(u16, u16)>(),
        not_after in any::<i64>(),
        serial in any::<u64>(),
    ) {
        let identity = SigningKey::from_seed(&seed);
        let mesh_pub: Vec<al::PublicKey> = mesh.into_iter().map(al::PublicKey::from_bytes).collect();
        let cluster_id = ClusterId::from_bytes([7; 16]);
        let original = NodeRecord {
            cluster_id,
            node_id: NodeId::of(&identity.verifying_key()),
            name,
            identity_pub: identity.verifying_key(),
            roles: Roles::from_bits(roles).expect("4 bits"),
            zone,
            infra_addr: cluster_id.infra_addr(&mesh_pub[0]),
            mesh_pub,
            advertise: advertise
                .into_iter()
                .map(|b| if b[0] % 2 == 0 {
                    IpAddr::V6(Ipv6Addr::from(b))
                } else {
                    IpAddr::V4(Ipv4Addr::new(b[1], b[2], b[3], b[4]))
                })
                .collect(),
            mesh_port: ports.0,
            api_port: ports.1,
            not_after,
            serial,
        };
        let bytes = original.encode().expect("encode");
        prop_assert_eq!(NodeRecord::decode(&bytes), Ok(original));
    }

    #[test]
    fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
        let _ = NodeRecord::decode(&bytes);
    }
}
