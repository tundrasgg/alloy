//! Tests of the identity primitives: [`crate::ed25519`], [`crate::ecdsa`],
//! [`crate::x509`], and [`crate::aead`].

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::aead::{AeadKey, OpenError};
use crate::ecdsa::{self, EcdsaError};
use crate::ed25519::{self, Ed25519Error};
use crate::x509::{self, CertificateFields, RequestName, X509Error};

fn hex(s: &str) -> Vec<u8> {
    let s: String = s.split_whitespace().collect();
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digit"))
        .collect()
}

fn hex_array<const N: usize>(s: &str) -> [u8; N] {
    hex(s).try_into().expect("hex length")
}

/// Flips each bit of `bytes` in turn and checks that `accepts` refuses
/// every result.
fn every_flip_refused(bytes: &[u8], accepts: impl Fn(&[u8]) -> bool) {
    for bit in 0..bytes.len() * 8 {
        let mut flipped = bytes.to_vec();
        flipped[bit / 8] ^= 1 << (bit % 8);
        assert!(!accepts(&flipped), "bit {bit} flipped and still accepted");
    }
}

// RFC 8032 section 7.1, TEST 1 to TEST 3.
const ED25519_VECTORS: [(&str, &str, &str, &str); 3] = [
    (
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
        "",
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155
         5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
    ),
    (
        "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
        "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
        "72",
        "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da
         085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
    ),
    (
        "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
        "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
        "af82",
        "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac
         18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
    ),
];

#[test]
fn ed25519_matches_rfc_8032_vectors() {
    for (seed, public, message, signature) in ED25519_VECTORS {
        let key = ed25519::SigningKey::from_seed(&hex_array(seed));
        assert_eq!(key.verifying_key().as_bytes(), &hex_array::<32>(public));
        assert_eq!(*key.seed(), hex_array::<32>(seed));
        let message = hex(message);
        let signed = key.sign(&message);
        assert_eq!(signed.to_bytes(), hex_array::<64>(signature));
        assert_eq!(key.verifying_key().verify(&message, &signed), Ok(()));
    }
}

#[test]
fn ed25519_refuses_any_flipped_bit() {
    let key = ed25519::SigningKey::from_seed(&[7; 32]);
    let public = key.verifying_key();
    let message = b"alloy/id/node-record/v1 and a record";
    let signature = key.sign(message);
    every_flip_refused(&signature.to_bytes(), |bytes| {
        let signature = ed25519::Signature::from_bytes(bytes.try_into().expect("64 bytes"));
        public.verify(message, &signature).is_ok()
    });
    every_flip_refused(message, |bytes| public.verify(bytes, &signature).is_ok());
    every_flip_refused(public.as_bytes(), |bytes| {
        ed25519::VerifyingKey::from_bytes(bytes.try_into().expect("32 bytes"))
            .is_ok_and(|other| other.verify(message, &signature).is_ok())
    });
}

#[test]
fn ed25519_refuses_a_non_point_and_hides_the_seed() {
    // y = 2 has no x on the curve (RFC 8032 section 5.1.3 step 2 fails).
    let mut not_a_point = [0u8; 32];
    not_a_point[0] = 2;
    assert_eq!(
        ed25519::VerifyingKey::from_bytes(&not_a_point),
        Err(Ed25519Error::InvalidPublicKey)
    );
    let key = ed25519::SigningKey::from_seed(&[0xab; 32]);
    assert_eq!(format!("{key:?}"), "SigningKey { .. }");
}

// RFC 6979 appendix A.2.5: P-256 with SHA-256, messages "sample" and "test".
const P256_SCALAR: &str = "c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721";
const P256_PUBLIC: &str = "04
    60fed4ba255a9d31c961eb74c6356d68c049b8923b61fa6ce669622e60f29fb6
    7903fe1008b8bc99a41ae9e95628bc64f2f1b20c2d7e9f5177a3c294d4462299";

#[test]
fn ecdsa_matches_rfc_6979_vectors() {
    let key = ecdsa::SigningKey::from_scalar(&hex_array(P256_SCALAR)).expect("scalar");
    assert_eq!(
        key.public_key().to_uncompressed(),
        hex_array::<65>(P256_PUBLIC)
    );
    for (message, signature) in [
        (
            "sample",
            "efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716
             f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8",
        ),
        (
            "test",
            "f1abb023518351cd71d881567b1ea663ed3efcf6c5132b354f28d3b0b7d38367
             019f4113742a2b14bd25926b49c649155f267e60d3814b4c0cc84250e46f0083",
        ),
    ] {
        let signed = key.sign(message.as_bytes());
        assert_eq!(signed.len(), ecdsa::SIGNATURE_LEN);
        assert_eq!(signed, hex_array::<64>(signature));
        // The same key and message give the same bytes every time.
        assert_eq!(key.sign(message.as_bytes()), signed);
        assert_eq!(
            key.sign_der(message.as_bytes()),
            key.sign_der(message.as_bytes())
        );
        assert_eq!(key.public_key().verify(message.as_bytes(), &signed), Ok(()));
        let der = key.sign_der(message.as_bytes());
        assert_eq!(
            key.public_key().verify_der(message.as_bytes(), &der),
            Ok(())
        );
    }
}

#[test]
fn ecdsa_refuses_any_flipped_bit() {
    let key = ecdsa::SigningKey::from_scalar(&hex_array(P256_SCALAR)).expect("scalar");
    let public = key.public_key();
    let message = b"tbs";
    let signature = key.sign(message);
    every_flip_refused(&signature, |bytes| {
        public
            .verify(message, bytes.try_into().expect("64 bytes"))
            .is_ok()
    });
    let der = key.sign_der(message);
    every_flip_refused(&der, |bytes| public.verify_der(message, bytes).is_ok());
    every_flip_refused(message, |bytes| public.verify(bytes, &signature).is_ok());
}

#[test]
fn ecdsa_keys_encode_and_refuse_bad_input() {
    assert_eq!(
        ecdsa::SigningKey::from_scalar(&[0; 32]).map(|_| ()),
        Err(EcdsaError::InvalidScalar)
    );
    assert_eq!(
        ecdsa::SigningKey::from_scalar(&[0xff; 32]).map(|_| ()),
        Err(EcdsaError::InvalidScalar)
    );
    let key = ecdsa::SigningKey::from_scalar(&hex_array(P256_SCALAR)).expect("scalar");
    let pkcs8 = key.to_pkcs8_der().expect("pkcs8");
    let again = ecdsa::SigningKey::from_pkcs8_der(&pkcs8).expect("decode");
    assert_eq!(again.public_key(), key.public_key());
    assert_eq!(again.sign(b"sample"), key.sign(b"sample"));
    assert_eq!(
        ecdsa::SigningKey::from_pkcs8_der(&pkcs8[1..]).map(|_| ()),
        Err(EcdsaError::InvalidPkcs8)
    );

    let spki = key.public_key().spki_der().expect("spki");
    assert_eq!(spki.len(), ecdsa::SPKI_LEN);
    // SEQUENCE { SEQUENCE { id-ecPublicKey, secp256r1 }, BIT STRING point }.
    let prefix = hex("3059 3013 0607 2a8648ce3d0201 0608 2a8648ce3d030107 034200");
    assert_eq!(&spki[..prefix.len()], &prefix[..]);
    assert_eq!(&spki[prefix.len()..], &hex(P256_PUBLIC)[..]);

    let public = key.public_key();
    let compressed = {
        let point = public.to_uncompressed();
        let mut out = vec![2 | (point[64] & 1)];
        out.extend_from_slice(&point[1..33]);
        out
    };
    assert_eq!(ecdsa::PublicKey::from_sec1(&compressed), Ok(public));
    assert_eq!(
        ecdsa::PublicKey::from_sec1(&[4; 65]).map(|_| ()),
        Err(EcdsaError::InvalidPublicKey)
    );
    assert_eq!(format!("{key:?}"), "SigningKey { .. }");
}

#[test]
fn ecdsa_key_identifier_is_rfc_7093_method_1() {
    use sha2::Digest as _;
    let key = ecdsa::SigningKey::from_scalar(&hex_array(P256_SCALAR)).expect("scalar");
    let digest = sha2::Sha256::digest(hex(P256_PUBLIC));
    assert_eq!(key.public_key().key_identifier()[..], digest[..20]);
}

/// One DER element: its tag and the bytes of its whole encoding and of its
/// contents.
struct Tlv<'a> {
    tag: u8,
    whole: &'a [u8],
    contents: &'a [u8],
}

/// Splits DER `bytes` into its top-level elements. Test input only: definite
/// lengths of up to four bytes.
fn elements(mut bytes: &[u8]) -> Vec<Tlv<'_>> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let tag = bytes[0];
        let (len, header) = match bytes[1] {
            short if short < 0x80 => (usize::from(short), 2),
            long => {
                let count = usize::from(long & 0x7f);
                let len = bytes[2..2 + count]
                    .iter()
                    .fold(0usize, |acc, byte| (acc << 8) | usize::from(*byte));
                (len, 2 + count)
            }
        };
        out.push(Tlv {
            tag,
            whole: &bytes[..header + len],
            contents: &bytes[header..header + len],
        });
        bytes = &bytes[header + len..];
    }
    out
}

/// A parsed certificate: the whole `tbsCertificate`, its fields, and the
/// signature.
struct Parsed<'a> {
    tbs: &'a [u8],
    fields: Vec<Tlv<'a>>,
    signature_algorithm: &'a [u8],
    signature: &'a [u8],
}

fn parse(der: &[u8]) -> Parsed<'_> {
    let outer = elements(der);
    assert_eq!(outer.len(), 1);
    assert_eq!(outer[0].tag, 0x30);
    let parts = elements(outer[0].contents);
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[2].tag, 0x03);
    assert_eq!(parts[2].contents[0], 0, "no unused bits");
    Parsed {
        tbs: parts[0].whole,
        fields: elements(parts[0].contents),
        signature_algorithm: parts[1].whole,
        signature: &parts[2].contents[1..],
    }
}

/// The extensions of a parsed certificate as (OID contents, critical, value
/// contents), in order.
fn extensions<'a>(parsed: &Parsed<'a>) -> Vec<(&'a [u8], bool, &'a [u8])> {
    let wrapper = parsed.fields.last().expect("extensions");
    assert_eq!(wrapper.tag, 0xa3);
    let list = elements(wrapper.contents);
    assert_eq!(list.len(), 1);
    elements(list[0].contents)
        .iter()
        .map(|extension| {
            let parts = elements(extension.contents);
            let critical = parts.len() == 3 && parts[1].contents == [0xff];
            (parts[0].contents, critical, parts[parts.len() - 1].contents)
        })
        .collect()
}

const ECDSA_WITH_SHA256: &str = "300a 0608 2a8648ce3d040302";
const SKI: &str = "551d0e";
const KEY_USAGE: &str = "551d0f";
const SAN: &str = "551d11";
const BASIC_CONSTRAINTS: &str = "551d13";
const AKI: &str = "551d23";
const EKU: &str = "551d25";

/// 2023-11-14T22:13:20Z.
const ISSUED: i64 = 1_700_000_000;

#[test]
fn ca_certificate_has_exactly_its_profile() {
    let key = ecdsa::SigningKey::from_scalar(&hex_array(P256_SCALAR)).expect("scalar");
    let fields = CertificateFields {
        serial: [0x7f; 16],
        common_name: "Alloy API CA 00112233445566778899aabbccddeeff",
        not_before: ISSUED,
        not_after: ISSUED + 30 * 86_400,
    };
    let der = x509::self_signed_ca(&key, &fields).expect("certificate");
    assert_eq!(x509::self_signed_ca(&key, &fields).expect("again"), der);
    let parsed = parse(&der);

    assert_eq!(parsed.signature_algorithm, &hex(ECDSA_WITH_SHA256)[..]);
    key.public_key()
        .verify_der(parsed.tbs, parsed.signature)
        .expect("self signature verifies");
    // version v3, serial, signature algorithm, issuer, validity, subject,
    // subjectPublicKeyInfo, extensions.
    assert_eq!(parsed.fields.len(), 8);
    assert_eq!(parsed.fields[0].whole, &hex("a003 020102")[..]);
    assert_eq!(parsed.fields[1].contents, &[0x7f; 16]);
    assert_eq!(parsed.fields[2].whole, &hex(ECDSA_WITH_SHA256)[..]);
    assert_eq!(
        parsed.fields[3].whole, parsed.fields[5].whole,
        "issuer is subject"
    );
    let name = parsed.fields[5].whole;
    assert!(
        name.windows(fields.common_name.len())
            .any(|w| w == fields.common_name.as_bytes())
    );
    let mut validity = hex("301e 170d");
    validity.extend_from_slice(b"231114221320Z");
    validity.extend_from_slice(&hex("170d"));
    validity.extend_from_slice(b"231214221320Z");
    assert_eq!(parsed.fields[4].whole, &validity[..]);
    assert_eq!(
        parsed.fields[6].whole,
        &key.public_key().spki_der().expect("spki")[..]
    );

    let key_id = key.public_key().key_identifier();
    let mut ski = hex("0414");
    ski.extend_from_slice(&key_id);
    let found = extensions(&parsed);
    let expected: Vec<(Vec<u8>, bool, Vec<u8>)> = vec![
        (hex(KEY_USAGE), true, hex("03020106")),
        (hex(SKI), false, ski),
        (hex(BASIC_CONSTRAINTS), true, hex("3006 0101ff 020100")),
    ];
    let found: Vec<(Vec<u8>, bool, Vec<u8>)> = found
        .into_iter()
        .map(|(oid, critical, value)| (oid.to_vec(), critical, value.to_vec()))
        .collect();
    assert_eq!(found, expected);
}

#[test]
fn server_certificate_has_exactly_its_profile() {
    let ca_key = ecdsa::SigningKey::from_scalar(&hex_array(P256_SCALAR)).expect("scalar");
    let server_key = ecdsa::SigningKey::from_scalar(&[0x11; 32]).expect("scalar");
    let fields = CertificateFields {
        serial: [0x01; 16],
        common_name: "node-1",
        not_before: ISSUED,
        not_after: ISSUED + 30 * 86_400,
    };
    let addresses = [
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ];
    let der = x509::issue_server(
        &ca_key,
        "Alloy API CA 00112233445566778899aabbccddeeff",
        &server_key.public_key(),
        &fields,
        &addresses,
    )
    .expect("certificate");
    let parsed = parse(&der);
    ca_key
        .public_key()
        .verify_der(parsed.tbs, parsed.signature)
        .expect("CA signature verifies");
    assert_eq!(
        parsed.fields[6].whole,
        &server_key.public_key().spki_der().expect("spki")[..]
    );
    let issuer = parsed.fields[3].whole;
    assert!(issuer.windows(12).any(|w| w == b"Alloy API CA"));

    let mut aki = hex("3016 8014");
    aki.extend_from_slice(&ca_key.public_key().key_identifier());
    let mut ski = hex("0414");
    ski.extend_from_slice(&server_key.public_key().key_identifier());
    let san = hex("301e 8704c0000201 87047f000001 871000000000000000000000000000000001");
    let found: Vec<(Vec<u8>, bool, Vec<u8>)> = extensions(&parsed)
        .into_iter()
        .map(|(oid, critical, value)| (oid.to_vec(), critical, value.to_vec()))
        .collect();
    let expected: Vec<(Vec<u8>, bool, Vec<u8>)> = vec![
        (hex(AKI), false, aki),
        (hex(SAN), false, san),
        (hex(KEY_USAGE), true, hex("03020780")),
        (hex(EKU), false, hex("300a 0608 2b06010505070301")),
        (hex(SKI), false, ski),
        (hex(BASIC_CONSTRAINTS), true, hex("3000")),
    ];
    assert_eq!(found, expected);
}

/// The DER encoding of one element with a short-form length.
fn der(tag: u8, contents: &[u8]) -> Vec<u8> {
    let mut out = vec![tag, u8::try_from(contents.len()).expect("short length")];
    out.extend_from_slice(contents);
    out
}

#[test]
fn certificate_request_has_exactly_its_profile() {
    let key = ecdsa::SigningKey::from_scalar(&[0x11; 32]).expect("scalar");
    let names = [
        RequestName::Dns("api.example.com"),
        RequestName::Ip(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))),
    ];
    let der_request = x509::certificate_request(&key, &names).expect("request");
    assert_eq!(
        x509::certificate_request(&key, &names).expect("again"),
        der_request,
        "RFC 6979 makes the request deterministic"
    );
    let parsed = parse(&der_request);
    assert_eq!(parsed.signature_algorithm, &hex(ECDSA_WITH_SHA256)[..]);
    key.public_key()
        .verify_der(parsed.tbs, parsed.signature)
        .expect("the request is signed by its own key");
    every_flip_refused(parsed.tbs, |tbs| {
        key.public_key().verify_der(tbs, parsed.signature).is_ok()
    });

    // version 0, an empty subject, the key, and the attributes.
    assert_eq!(parsed.fields.len(), 4);
    assert_eq!(parsed.fields[0].whole, &hex("020100")[..]);
    assert_eq!(parsed.fields[1].whole, &hex("3000")[..]);
    assert_eq!(
        parsed.fields[2].whole,
        &key.public_key().spki_der().expect("spki")[..]
    );
    let mut general_names = der(0x82, b"api.example.com");
    general_names.extend(der(0x87, &[192, 0, 2, 1]));
    // Critical, since the subject is empty (RFC 5280 section 4.2.1.6).
    let mut extension = der(0x06, &hex(SAN));
    extension.extend(hex("0101ff"));
    extension.extend(der(0x04, &der(0x30, &general_names)));
    let extensions = der(0x30, &der(0x30, &extension));
    let mut attribute = der(0x06, &hex("2a864886f70d01090e"));
    attribute.extend(der(0x31, &extensions));
    assert_eq!(
        parsed.fields[3].whole,
        &der(0xa0, &der(0x30, &attribute))[..]
    );
}

#[test]
fn certificate_request_refuses_bad_names() {
    let key = ecdsa::SigningKey::from_scalar(&[0x11; 32]).expect("scalar");
    assert!(matches!(
        x509::certificate_request(&key, &[]),
        Err(X509Error::Encode(_))
    ));
    assert!(matches!(
        x509::certificate_request(&key, &[RequestName::Dns("bücher.example")]),
        Err(X509Error::Encode(_))
    ));
}

#[test]
fn certificates_refuse_bad_fields() {
    let key = ecdsa::SigningKey::from_scalar(&hex_array(P256_SCALAR)).expect("scalar");
    let mut fields = CertificateFields {
        serial: [0x80; 16],
        common_name: "x",
        not_before: ISSUED,
        not_after: ISSUED + 1,
    };
    assert_eq!(x509::self_signed_ca(&key, &fields), Err(X509Error::Serial));
    fields.serial = [0; 16];
    fields.not_after = ISSUED;
    assert_eq!(
        x509::self_signed_ca(&key, &fields),
        Err(X509Error::Validity)
    );
    fields.not_after = ISSUED + 1;
    assert!(matches!(
        x509::issue_server(&key, "ca", &key.public_key(), &fields, &[]),
        Err(X509Error::Encode(_))
    ));
}

#[test]
fn aead_matches_the_xchacha_draft_vector_and_refuses_tampering() {
    // draft-irtf-cfrg-xchacha-03 appendix A.3.1.
    let key = AeadKey::from_bytes(&hex_array(
        "808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f",
    ));
    let nonce: [u8; 24] = hex_array("404142434445464748494a4b4c4d4e4f5051525354555657");
    let aad = hex("50515253c0c1c2c3c4c5c6c7");
    let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let expected = hex(
        "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb
         731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b452
         2f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff9
         21f9664c97637da9768812f615c68b13b52e
         c0875924c1c7987947deafd8780acf49",
    );
    let sealed = key.seal(&nonce, &aad, plaintext).expect("seal");
    assert_eq!(sealed, expected);
    assert_eq!(
        &key.open(&nonce, &aad, &sealed).expect("open")[..],
        &plaintext[..]
    );
    every_flip_refused(&sealed, |bytes| key.open(&nonce, &aad, bytes).is_ok());
    every_flip_refused(&aad, |bytes| key.open(&nonce, bytes, &sealed).is_ok());
    assert_eq!(
        key.open(&nonce, &aad, &sealed[..15]).map(|_| ()),
        Err(OpenError)
    );
    assert_eq!(format!("{key:?}"), "AeadKey { .. }");
}

#[test]
fn x25519_static_secret_bytes_round_trip() {
    let bytes = [0x5a; 32];
    let key = crate::al::StaticSecret::from_bytes(bytes);
    assert_eq!(*key.to_bytes(), bytes);
    assert_eq!(
        crate::al::StaticSecret::from_bytes(*key.to_bytes()).public_key(),
        key.public_key()
    );
}
