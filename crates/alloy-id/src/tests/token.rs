use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use alloy_crypto::al::PublicKey;
use proptest::prelude::*;

use crate::base32;
use crate::ids::{ClusterId, ClusterKeyFp, Role, Roles};
use crate::token::{
    JoinToken, MAX_TOKEN_LEN, TokenAddress, TokenAnchor, TokenError, TokenId, TokenSecret,
};

fn v6_anchor(n: u8, addresses: usize) -> TokenAnchor {
    TokenAnchor {
        mesh_pub: PublicKey::from_bytes([n; 32]),
        addresses: (0..addresses)
            .map(|i| TokenAddress {
                ip: IpAddr::V6(Ipv6Addr::new(
                    0x2001,
                    0xdb8,
                    0,
                    0,
                    0,
                    0,
                    u16::from(n),
                    i as u16,
                )),
                mesh_port: 7476,
                stream_port: 443,
            })
            .collect(),
    }
}

fn v4_anchor(n: u8) -> TokenAnchor {
    TokenAnchor {
        mesh_pub: PublicKey::from_bytes([n; 32]),
        addresses: vec![TokenAddress {
            ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, n)),
            mesh_port: 7476,
            stream_port: 0,
        }],
    }
}

fn token(anchors: Vec<TokenAnchor>) -> JoinToken {
    JoinToken {
        cluster_id: ClusterId::from_bytes([1; 16]),
        cluster_key_fp: ClusterKeyFp::from_bytes([2; 16]),
        token_id: TokenId([3; 8]),
        secret: TokenSecret::from_bytes([4; 16]),
        preset_roles: Roles::of(&[Role::Compute, Role::Storage]),
        anchors,
    }
}

fn same(a: &JoinToken, b: &JoinToken) {
    assert_eq!(a.cluster_id, b.cluster_id);
    assert_eq!(a.cluster_key_fp, b.cluster_key_fp);
    assert_eq!(a.token_id, b.token_id);
    assert_eq!(a.secret.as_bytes(), b.secret.as_bytes());
    assert_eq!(a.preset_roles, b.preset_roles);
    assert_eq!(a.anchors, b.anchors);
}

/// The body bytes of a valid token, without its checksum.
fn body(text: &str) -> Vec<u8> {
    let mut bytes = base32::decode(&text[5..]).expect("base32");
    bytes.truncate(bytes.len() - 4);
    bytes
}

/// A token string for `body` with a correct checksum.
fn with_checksum(body: &[u8]) -> String {
    let mut bytes = body.to_vec();
    bytes.extend_from_slice(&alloy_crypto::hash(body)[..4]);
    format!("alyt_{}", base32::encode(&bytes))
}

#[test]
fn one_anchor_with_four_ipv6_addresses_is_293_characters() {
    let original = token(vec![v6_anchor(9, 4)]);
    let text = original.encode().expect("encode");
    assert_eq!(text.len(), 293);
    assert!(text.starts_with("alyt_"));
    assert!(
        text[5..]
            .bytes()
            .all(|c| c.is_ascii_lowercase() || (b'2'..=b'7').contains(&c))
    );
    assert_eq!(body(&text).len() + 4, 180);
    same(&JoinToken::decode(&text).expect("decode"), &original);
    assert_eq!(format!("{:?}", original.secret), "TokenSecret { .. }");
}

#[test]
fn leaves_out_what_does_not_fit() {
    // Two anchors of 4 IPv6 addresses take 481 characters; a third, or even
    // a one-address IPv4 anchor, would pass 512.
    let text = token(vec![
        v6_anchor(1, 4),
        v6_anchor(2, 4),
        v6_anchor(3, 4),
        v4_anchor(4),
    ])
    .encode()
    .expect("encode");
    assert!(text.len() <= MAX_TOKEN_LEN);
    let decoded = JoinToken::decode(&text).expect("decode");
    assert_eq!(decoded.anchors, vec![v6_anchor(1, 4), v6_anchor(2, 4)]);

    // An anchor without addresses is skipped, at most 4 anchors are listed,
    // and each keeps its first 4 addresses.
    let text = token(vec![
        v6_anchor(1, 0),
        v6_anchor(2, 6),
        v4_anchor(3),
        v4_anchor(4),
        v4_anchor(5),
        v4_anchor(6),
    ])
    .encode()
    .expect("encode");
    let decoded = JoinToken::decode(&text).expect("decode");
    assert_eq!(
        decoded.anchors,
        vec![v6_anchor(2, 4), v4_anchor(3), v4_anchor(4), v4_anchor(5)]
    );

    assert_eq!(
        token(vec![v6_anchor(1, 0)]).encode(),
        Err(TokenError::NoAnchor)
    );
    assert_eq!(token(Vec::new()).encode(), Err(TokenError::NoAnchor));
}

#[test]
fn refuses_every_malformed_string() {
    let valid = token(vec![v4_anchor(7)]).encode().expect("encode");
    let good = body(&valid);
    assert_eq!(with_checksum(&good), valid);
    // header 59 bytes: version 0, cluster_id 1..17, fp 17..33, token_id
    // 33..41, secret 41..57, roles 57, anchor count 58; anchor: key 59..91,
    // address count 91, family 92.
    let edit = |at: usize, value: u8| {
        let mut bytes = good.clone();
        bytes[at] = value;
        with_checksum(&bytes)
    };
    let mut flipped = valid.clone().into_bytes();
    flipped[40] = if flipped[40] == b'a' { b'b' } else { b'a' };
    let flipped = String::from_utf8(flipped).expect("ascii");
    let mut extra = good.clone();
    extra.push(0);
    let cases: Vec<(String, TokenError)> = vec![
        (valid.replacen("alyt_", "alyx_", 1), TokenError::Prefix),
        (valid[5..].to_owned(), TokenError::Prefix),
        (format!("alyt_{}", "a".repeat(508)), TokenError::TooLong),
        (
            valid.to_uppercase().replacen("ALYT_", "alyt_", 1),
            TokenError::Encoding,
        ),
        (format!("{valid}="), TokenError::Encoding),
        (format!("{valid}a"), TokenError::Encoding),
        (flipped, TokenError::Checksum),
        (edit(0, 2), TokenError::Version),
        (edit(0, 0), TokenError::Version),
        (edit(57, 0x10), TokenError::Malformed),
        (edit(57, 0x80), TokenError::Malformed),
        (edit(58, 0), TokenError::Malformed),
        (edit(58, 5), TokenError::Malformed),
        (edit(58, 2), TokenError::Malformed),
        (edit(91, 0), TokenError::Malformed),
        (edit(91, 5), TokenError::Malformed),
        (edit(92, 5), TokenError::Malformed),
        (edit(92, 6), TokenError::Malformed),
        (with_checksum(&extra), TokenError::Malformed),
        (
            with_checksum(&good[..good.len() - 1]),
            TokenError::Malformed,
        ),
        ("alyt_".to_owned(), TokenError::Malformed),
        (with_checksum(&[]), TokenError::Malformed),
    ];
    for (text, error) in cases {
        assert_eq!(JoinToken::decode(&text).map(|_| ()), Err(error), "{text}");
    }
    // Every preset role bit 0 to 3 is accepted.
    assert_eq!(
        JoinToken::decode(&edit(57, 0x0f))
            .expect("decode")
            .preset_roles,
        Roles::of(&Role::ALL)
    );
}

fn arb_anchor() -> impl Strategy<Value = TokenAnchor> {
    let address = (any::<bool>(), any::<[u8; 16]>(), any::<u16>(), any::<u16>()).prop_map(
        |(v4, bytes, mesh_port, stream_port)| TokenAddress {
            ip: if v4 {
                IpAddr::V4(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]))
            } else {
                IpAddr::V6(Ipv6Addr::from(bytes))
            },
            mesh_port,
            stream_port,
        },
    );
    (any::<[u8; 32]>(), proptest::collection::vec(address, 1..=4)).prop_map(|(key, addresses)| {
        TokenAnchor {
            mesh_pub: PublicKey::from_bytes(key),
            addresses,
        }
    })
}

proptest! {
    #[test]
    fn decode_of_encode_is_identity(
        ids in any::<([u8; 16], [u8; 16], [u8; 8], [u8; 16])>(),
        roles in 0u8..16,
        anchors in proptest::collection::vec(arb_anchor(), 1..=4),
    ) {
        let original = JoinToken {
            cluster_id: ClusterId::from_bytes(ids.0),
            cluster_key_fp: ClusterKeyFp::from_bytes(ids.1),
            token_id: TokenId(ids.2),
            secret: TokenSecret::from_bytes(ids.3),
            preset_roles: Roles::from_bits(roles).expect("4 bits"),
            anchors,
        };
        let text = original.encode().expect("encode");
        prop_assert!(text.len() <= MAX_TOKEN_LEN);
        let decoded = JoinToken::decode(&text).expect("decode");
        // The first anchor always fits; the rest are a prefix-preserving
        // subsequence of what was given.
        prop_assert_eq!(&decoded.anchors[0], &original.anchors[0]);
        let mut rest = original.anchors.iter();
        for anchor in &decoded.anchors {
            prop_assert!(rest.any(|a| a == anchor));
        }
        prop_assert_eq!(decoded.secret.as_bytes(), original.secret.as_bytes());
        prop_assert_eq!(decoded.preset_roles, original.preset_roles);
        prop_assert_eq!(JoinToken::decode(&text).expect("again").encode().expect("re-encode"), text);
    }

    #[test]
    fn decode_never_panics(text in "alyt_[a-z2-7]{0,120}") {
        let _ = JoinToken::decode(&text);
    }
}
