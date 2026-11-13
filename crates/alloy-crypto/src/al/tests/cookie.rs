//! `mac1`, `mac2`, cookies and the cookie reply (ARFC 0003 section 3.1).

use super::super::{CookieError, CookieReplyKey, CookieSecret, Mac1Key, StaticSecret, verify_mac2};
use super::{key_bytes, xchacha_open, xchacha_seal};
use blake2::digest::consts::U16;
use blake2::digest::{Digest, FixedOutput, KeyInit, Update};
use blake2::{Blake2s256, Blake2sMac};

/// The section 3.1 `MAC`, computed with the `blake2` crate directly.
fn reference_mac(key: &[u8], message: &[u8]) -> [u8; 16] {
    let mut mac = <Blake2sMac<U16> as KeyInit>::new_from_slice(key).expect("key length");
    Update::update(&mut mac, message);
    mac.finalize_fixed().into()
}

fn reference_key(label: &[u8], public: &[u8; 32]) -> [u8; 32] {
    let mut hash = Blake2s256::new();
    Digest::update(&mut hash, label);
    Digest::update(&mut hash, public);
    hash.finalize().into()
}

/// The first 120 bytes of an initiation stand in for `m1`.
fn message() -> Vec<u8> {
    (0u8..120).collect()
}

#[test]
fn mac1_matches_section_3_1_and_verifies() {
    let responder = StaticSecret::from_bytes(key_bytes(2)).public_key();
    let key = Mac1Key::new(&responder);
    let m1 = message();
    let mac1 = key.mac1(&m1);
    let expected = reference_mac(
        &reference_key(b"alloy/al/mac1/v1", responder.as_bytes()),
        &m1,
    );
    assert_eq!(mac1, expected);
    assert!(Mac1Key::new(&responder).verify_mac1(&m1, &mac1));
}

#[test]
fn mac1_fails_for_any_change() {
    let responder = StaticSecret::from_bytes(key_bytes(2)).public_key();
    let other = StaticSecret::from_bytes(key_bytes(3)).public_key();
    let key = Mac1Key::new(&responder);
    let m1 = message();
    let mac1 = key.mac1(&m1);
    assert!(!Mac1Key::new(&other).verify_mac1(&m1, &mac1));
    for index in 0..m1.len() {
        let mut tampered = m1.clone();
        tampered[index] ^= 1;
        assert!(!key.verify_mac1(&tampered, &mac1), "byte {index}");
    }
    for index in 0..16 {
        let mut tampered = mac1;
        tampered[index] ^= 0x80;
        assert!(!key.verify_mac1(&m1, &tampered), "mac byte {index}");
    }
    assert!(!key.verify_mac1(&m1[..119], &mac1));
}

#[test]
fn cookie_and_mac2_match_section_3_1() {
    let secret_bytes = key_bytes(20);
    let secret = CookieSecret::from_bytes(secret_bytes);
    // An IPv4 source: address then port, in header byte order.
    let src = [192, 0, 2, 7, 0x1f, 0x90];
    let m2: Vec<u8> = (0u8..136).collect();
    let cookie_bytes = reference_mac(&secret_bytes, &src);
    let mac2 = secret.cookie(&src).mac2(&m2);
    assert_eq!(mac2, reference_mac(&cookie_bytes, &m2));
    assert!(verify_mac2(&secret, None, &src, &m2, &mac2));
}

#[test]
fn mac2_verifies_under_current_or_previous_secret_only() {
    let old = CookieSecret::from_bytes(key_bytes(21));
    let current = CookieSecret::from_bytes(key_bytes(22));
    let retired = CookieSecret::from_bytes(key_bytes(23));
    let src = [198, 51, 100, 1, 0x00, 0x35];
    let m2: Vec<u8> = (0u8..136).collect();

    let under_old = old.cookie(&src).mac2(&m2);
    assert!(verify_mac2(&current, Some(&old), &src, &m2, &under_old));
    assert!(!verify_mac2(&current, None, &src, &m2, &under_old));
    assert!(!verify_mac2(
        &current,
        Some(&retired),
        &src,
        &m2,
        &under_old
    ));

    let under_current = current.cookie(&src).mac2(&m2);
    assert!(verify_mac2(&current, Some(&old), &src, &m2, &under_current));

    // Another source, another message, or zero bytes do not verify.
    let other_src = [198, 51, 100, 1, 0x00, 0x36];
    assert!(!verify_mac2(
        &current,
        Some(&old),
        &other_src,
        &m2,
        &under_current
    ));
    let mut other_m2 = m2.clone();
    other_m2[0] ^= 1;
    assert!(!verify_mac2(
        &current,
        Some(&old),
        &src,
        &other_m2,
        &under_current
    ));
    assert!(!verify_mac2(&current, Some(&old), &src, &m2, &[0u8; 16]));
}

#[test]
fn cookie_reply_round_trip_matches_section_3_1() {
    let responder = StaticSecret::from_bytes(key_bytes(2)).public_key();
    let secret = CookieSecret::from_bytes(key_bytes(24));
    let src = [203, 0, 113, 9, 0x30, 0x39];
    let mac1 = [0x5au8; 16];
    let nonce = [0x33u8; 24];
    let cookie = secret.cookie(&src);

    let encrypted = CookieReplyKey::new(&responder)
        .seal(&nonce, &cookie, &mac1)
        .expect("seal");

    // Independently: XChaCha20-Poly1305 under BLAKE2s(label || S), with the
    // reply nonce and the initiation's mac1 as associated data.
    let key = reference_key(b"alloy/al/cookie/v1", responder.as_bytes());
    let plain = xchacha_open(&key, &nonce, &mac1, &encrypted).expect("reference open");
    assert_eq!(
        xchacha_seal(&key, &nonce, &mac1, &plain),
        encrypted.to_vec()
    );
    let m2: Vec<u8> = (0u8..136).collect();
    let mac2_from_plain = reference_mac(&plain, &m2);
    assert_eq!(mac2_from_plain, cookie.mac2(&m2));

    let opened = CookieReplyKey::new(&responder)
        .open(&nonce, &encrypted, &mac1)
        .expect("open");
    assert_eq!(opened.mac2(&m2), cookie.mac2(&m2));
}

#[test]
fn cookie_reply_fails_for_any_change() {
    let responder = StaticSecret::from_bytes(key_bytes(2)).public_key();
    let other = StaticSecret::from_bytes(key_bytes(3)).public_key();
    let cookie = CookieSecret::from_bytes(key_bytes(25)).cookie(&[10, 0, 0, 1, 0, 80]);
    let mac1 = [0x11u8; 16];
    let nonce = [0x44u8; 24];
    let key = CookieReplyKey::new(&responder);
    let encrypted = key.seal(&nonce, &cookie, &mac1).expect("seal");

    assert_eq!(
        CookieReplyKey::new(&other)
            .open(&nonce, &encrypted, &mac1)
            .map(|_| ()),
        Err(CookieError)
    );
    for index in 0..encrypted.len() {
        let mut tampered = encrypted;
        tampered[index] ^= 1;
        assert!(
            key.open(&nonce, &tampered, &mac1).is_err(),
            "encrypted byte {index}"
        );
    }
    for index in 0..nonce.len() {
        let mut tampered = nonce;
        tampered[index] ^= 1;
        assert!(
            key.open(&tampered, &encrypted, &mac1).is_err(),
            "nonce byte {index}"
        );
    }
    for index in 0..mac1.len() {
        let mut tampered = mac1;
        tampered[index] ^= 1;
        assert!(
            key.open(&nonce, &encrypted, &tampered).is_err(),
            "mac1 byte {index}"
        );
    }
}

#[test]
fn cookie_types_show_no_secret() {
    let secret = CookieSecret::from_bytes(key_bytes(26));
    let responder = StaticSecret::from_bytes(key_bytes(2)).public_key();
    assert_eq!(format!("{secret:?}"), "CookieSecret { .. }");
    assert_eq!(format!("{:?}", secret.cookie(&[1, 2])), "Cookie { .. }");
    assert_eq!(format!("{:?}", Mac1Key::new(&responder)), "Mac1Key { .. }");
    assert_eq!(
        format!("{:?}", CookieReplyKey::new(&responder)),
        "CookieReplyKey { .. }"
    );
}
