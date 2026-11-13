//! The primitives against their published vectors, and this crate's HMAC and
//! HKDF against independent implementations.

use blake2::digest::consts::{U16, U20, U28, U32};
use blake2::digest::{Digest, FixedOutput, KeyInit, Update};
use blake2::{Blake2s, Blake2s256, Blake2sMac};
use proptest::prelude::*;

use super::super::cookie::mac_into;
use super::super::noise::{hash_into, hkdf, hkdf_expand_one, hkdf_extract, hmac_into};
use super::super::{EphemeralSecret, StaticSecret};
use super::{hex, hex_array, xchacha_open, xchacha_seal};

// RFC 7748 section 5.2: the two X25519 test vectors, and the iterated
// function after 1 and 1,000 iterations.
#[test]
fn x25519_rfc7748_section_5_2_vectors() {
    let cases = [
        (
            "a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4",
            "e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c",
            "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552",
        ),
        (
            "4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d",
            "e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493",
            "95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957",
        ),
    ];
    for (scalar, u, expected) in cases {
        let out = x25519_dalek::x25519(hex_array(scalar), hex_array(u));
        assert_eq!(out, hex_array::<32>(expected));
    }

    let mut k: [u8; 32] =
        hex_array("0900000000000000000000000000000000000000000000000000000000000000");
    let mut u = k;
    for iteration in 1..=1000 {
        let next = x25519_dalek::x25519(k, u);
        u = k;
        k = next;
        if iteration == 1 {
            assert_eq!(
                k,
                hex_array::<32>("422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079")
            );
        }
    }
    assert_eq!(
        k,
        hex_array::<32>("684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51")
    );
}

// RFC 7748 section 6.1: the Diffie-Hellman example, through this crate's key
// types.
#[test]
fn x25519_rfc7748_section_6_1_through_key_types() {
    let alice = StaticSecret::from_bytes(hex_array(
        "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
    ));
    let bob = EphemeralSecret::from_bytes(hex_array(
        "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb",
    ));
    assert_eq!(
        alice.public_key().as_bytes(),
        &hex_array::<32>("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
    );
    assert_eq!(
        bob.public_key().as_bytes(),
        &hex_array::<32>("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")
    );
    let bob_public = bob.public_key();
    let bob = bob.into_dalek();
    let shared_ab = alice.dalek().diffie_hellman(&bob_public.to_dalek());
    let shared_ba = bob.diffie_hellman(&alice.public_key().to_dalek());
    let expected =
        hex_array::<32>("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
    assert_eq!(shared_ab.as_bytes(), &expected);
    assert_eq!(shared_ba.as_bytes(), &expected);
}

// draft-irtf-cfrg-xchacha-03 appendix A.3.1: the AEAD_XChaCha20_Poly1305
// example.
#[test]
fn xchacha20poly1305_draft_vector() {
    let key: [u8; 32] =
        hex_array("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
    let nonce: [u8; 24] = hex_array("404142434445464748494a4b4c4d4e4f5051525354555657");
    let aad = hex("50515253c0c1c2c3c4c5c6c7");
    let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let ciphertext = hex(concat!(
        "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb",
        "731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b452",
        "2f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff9",
        "21f9664c97637da9768812f615c68b13b52e",
    ));
    let tag = hex("c0875924c1c7987947deafd8780acf49");

    let sealed = xchacha_seal(&key, &nonce, &aad, plaintext);
    assert_eq!(&sealed[..plaintext.len()], &ciphertext[..]);
    assert_eq!(&sealed[plaintext.len()..], &tag[..]);
    let opened = xchacha_open(&key, &nonce, &aad, &sealed).expect("open");
    assert_eq!(&opened[..], &plaintext[..]);
    let mut tampered = sealed;
    tampered[0] ^= 1;
    assert!(xchacha_open(&key, &nonce, &aad, &tampered).is_none());
}

// RFC 7693 appendix B: BLAKE2s-256 of "abc", through this crate's hash.
#[test]
fn blake2s_rfc7693_appendix_b() {
    let mut out = [0u8; 32];
    hash_into(&[b"abc"], &mut out);
    assert_eq!(
        out,
        hex_array::<32>("508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982")
    );
    let mut split = [0u8; 32];
    hash_into(&[b"a", b"", b"bc"], &mut split);
    assert_eq!(split, out);
}

/// `selftest_seq` of RFC 7693 appendix E.
fn selftest_seq(len: usize, seed: u32) -> Vec<u8> {
    let mut a = 0xDEAD_4BADu32.wrapping_mul(seed);
    let mut b = 1u32;
    (0..len)
        .map(|_| {
            let t = a.wrapping_add(b);
            a = b;
            b = t;
            (t >> 24) as u8
        })
        .collect()
}

fn unkeyed(outlen: usize, input: &[u8]) -> Vec<u8> {
    match outlen {
        16 => Blake2s::<U16>::digest(input).to_vec(),
        20 => Blake2s::<U20>::digest(input).to_vec(),
        28 => Blake2s::<U28>::digest(input).to_vec(),
        _ => Blake2s::<U32>::digest(input).to_vec(),
    }
}

fn keyed(outlen: usize, key: &[u8], input: &[u8]) -> Vec<u8> {
    fn run<M: KeyInit + Update + FixedOutput>(key: &[u8], input: &[u8]) -> Vec<u8> {
        let mut mac = M::new_from_slice(key).expect("key length");
        mac.update(input);
        mac.finalize_fixed().to_vec()
    }
    match outlen {
        16 => {
            // The 16-byte keyed case is the AL `MAC`: check it through
            // this crate's own function as well.
            let key: [u8; 16] = key.try_into().expect("16-byte key");
            let mut ours = [0u8; 16];
            mac_into(&key, &[input], &mut ours);
            let theirs = run::<Blake2sMac<U16>>(&key, input);
            assert_eq!(ours.to_vec(), theirs);
            theirs
        }
        20 => run::<Blake2sMac<U20>>(key, input),
        28 => run::<Blake2sMac<U28>>(key, input),
        _ => run::<Blake2sMac<U32>>(key, input),
    }
}

// RFC 7693 appendix E: the BLAKE2s self test, keyed and unkeyed, over output
// lengths 16, 20, 28 and 32 and six input lengths, hashed into one grand
// hash.
#[test]
fn blake2s_rfc7693_appendix_e_selftest() {
    let mut grand = Blake2s256::new();
    for outlen in [16usize, 20, 28, 32] {
        for inlen in [0usize, 3, 64, 65, 255, 1024] {
            let input = selftest_seq(inlen, u32::try_from(inlen).expect("small"));
            Digest::update(&mut grand, unkeyed(outlen, &input));
            let key = selftest_seq(outlen, u32::try_from(outlen).expect("small"));
            Digest::update(&mut grand, keyed(outlen, &key, &input));
        }
    }
    assert_eq!(
        grand.finalize().to_vec(),
        hex("6a411f08ce25adcdfb02aba641451cec53c598b24f4fc787fbdc88797f4c1dfe")
    );
}

type ReferenceHmac = hmac::SimpleHmac<Blake2s256>;
type ReferenceHkdf = hkdf::SimpleHkdf<Blake2s256>;

fn reference_hmac(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut mac = <ReferenceHmac as KeyInit>::new_from_slice(key).expect("any key length");
    Update::update(&mut mac, message);
    mac.finalize_fixed().into()
}

proptest! {
    // HMAC-BLAKE2s equals the `hmac` crate's, for keys shorter than, equal
    // to and longer than the 64-byte block, and for messages split anywhere.
    #[test]
    fn hmac_matches_hmac_crate(
        key in proptest::collection::vec(any::<u8>(), 0..200),
        message in proptest::collection::vec(any::<u8>(), 0..300),
        cut in any::<prop::sample::Index>(),
    ) {
        let at = cut.index(message.len() + 1);
        let mut ours = [0u8; 32];
        hmac_into(&key, &[&message[..at], &message[at..]], &mut ours);
        prop_assert_eq!(ours, reference_hmac(&key, &message));
    }

    // Noise `HKDF(ck, ikm, 3)` equals RFC 5869 HKDF with `ck` as salt and
    // empty info, from the `hkdf` crate.
    #[test]
    fn noise_hkdf_matches_hkdf_crate(
        ck in any::<[u8; 32]>(),
        ikm in proptest::collection::vec(any::<u8>(), 0..100),
    ) {
        let mut two = [[0u8; 32]; 2];
        hkdf(&ck, &ikm, &mut two);
        let mut three = [[0u8; 32]; 3];
        hkdf(&ck, &ikm, &mut three);
        let mut expected = [0u8; 96];
        ReferenceHkdf::new(Some(&ck), &ikm).expand(&[], &mut expected).expect("length");
        prop_assert_eq!(&three[0][..], &expected[..32]);
        prop_assert_eq!(&three[1][..], &expected[32..64]);
        prop_assert_eq!(&three[2][..], &expected[64..]);
        prop_assert_eq!(two[0], three[0]);
        prop_assert_eq!(two[1], three[1]);
    }

    // The section 3.3 extract and one-block expand equal the `hkdf` crate.
    #[test]
    fn rfc5869_hkdf_matches_hkdf_crate(
        salt in any::<[u8; 32]>(),
        k1 in any::<[u8; 32]>(),
        k2 in any::<[u8; 32]>(),
        info in proptest::collection::vec(any::<u8>(), 0..40),
    ) {
        let mut prk = [0u8; 32];
        hkdf_extract(&salt, &[&k1, &k2], &mut prk);
        let ikm: Vec<u8> = k1.iter().chain(k2.iter()).copied().collect();
        let (reference_prk, reference) = ReferenceHkdf::extract(Some(&salt), &ikm);
        prop_assert_eq!(&prk[..], &reference_prk[..]);

        let mut okm32 = [0u8; 32];
        hkdf_expand_one(&prk, &info, &mut okm32);
        let mut expected32 = [0u8; 32];
        reference.expand(&info, &mut expected32).expect("length");
        prop_assert_eq!(okm32, expected32);

        let mut okm14 = [0u8; 14];
        hkdf_expand_one(&prk, &info, &mut okm14);
        let mut expected14 = [0u8; 14];
        reference.expand(&info, &mut expected14).expect("length");
        prop_assert_eq!(okm14, expected14);
    }
}
