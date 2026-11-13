use super::{Hasher, hash};

/// The `hash` cases of the official BLAKE3 test vectors, truncated to the
/// default 32-byte output, as the vector file says implementations without
/// extended output should check.
///
/// Source: `test_vectors/test_vectors.json` at tag 1.8.7 of
/// https://github.com/BLAKE3-team/BLAKE3 (SHA-256 of the file
/// dcb91ea8accc77e6d6e632af7cdc1a99a9f3ae78cf648da595c7d064db32f624).
/// Each input is `input_len` bytes of the repeating sequence 0, 1, ..., 250.
const VECTORS: [(usize, &str); 35] = [
    (
        0,
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
    ),
    (
        1,
        "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213",
    ),
    (
        2,
        "7b7015bb92cf0b318037702a6cdd81dee41224f734684c2c122cd6359cb1ee63",
    ),
    (
        3,
        "e1be4d7a8ab5560aa4199eea339849ba8e293d55ca0a81006726d184519e647f",
    ),
    (
        4,
        "f30f5ab28fe047904037f77b6da4fea1e27241c5d132638d8bedce9d40494f32",
    ),
    (
        5,
        "b40b44dfd97e7a84a996a91af8b85188c66c126940ba7aad2e7ae6b385402aa2",
    ),
    (
        6,
        "06c4e8ffb6872fad96f9aaca5eee1553eb62aed0ad7198cef42e87f6a616c844",
    ),
    (
        7,
        "3f8770f387faad08faa9d8414e9f449ac68e6ff0417f673f602a646a891419fe",
    ),
    (
        8,
        "2351207d04fc16ade43ccab08600939c7c1fa70a5c0aaca76063d04c3228eaeb",
    ),
    (
        63,
        "e9bc37a594daad83be9470df7f7b3798297c3d834ce80ba85d6e207627b7db7b",
    ),
    (
        64,
        "4eed7141ea4a5cd4b788606bd23f46e212af9cacebacdc7d1f4c6dc7f2511b98",
    ),
    (
        65,
        "de1e5fa0be70df6d2be8fffd0e99ceaa8eb6e8c93a63f2d8d1c30ecb6b263dee",
    ),
    (
        127,
        "d81293fda863f008c09e92fc382a81f5a0b4a1251cba1634016a0f86a6bd640d",
    ),
    (
        128,
        "f17e570564b26578c33bb7f44643f539624b05df1a76c81f30acd548c44b45ef",
    ),
    (
        129,
        "683aaae9f3c5ba37eaaf072aed0f9e30bac0865137bae68b1fde4ca2aebdcb12",
    ),
    (
        1023,
        "10108970eeda3eb932baac1428c7a2163b0e924c9a9e25b35bba72b28f70bd11",
    ),
    (
        1024,
        "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7",
    ),
    (
        1025,
        "d00278ae47eb27b34faecf67b4fe263f82d5412916c1ffd97c8cb7fb814b8444",
    ),
    (
        2048,
        "e776b6028c7cd22a4d0ba182a8bf62205d2ef576467e838ed6f2529b85fba24a",
    ),
    (
        2049,
        "5f4d72f40d7a5f82b15ca2b2e44b1de3c2ef86c426c95c1af0b6879522563030",
    ),
    (
        3072,
        "b98cb0ff3623be03326b373de6b9095218513e64f1ee2edd2525c7ad1e5cffd2",
    ),
    (
        3073,
        "7124b49501012f81cc7f11ca069ec9226cecb8a2c850cfe644e327d22d3e1cd3",
    ),
    (
        4096,
        "015094013f57a5277b59d8475c0501042c0b642e531b0a1c8f58d2163229e969",
    ),
    (
        4097,
        "9b4052b38f1c5fc8b1f9ff7ac7b27cd242487b3d890d15c96a1c25b8aa0fb995",
    ),
    (
        5120,
        "9cadc15fed8b5d854562b26a9536d9707cadeda9b143978f319ab34230535833",
    ),
    (
        5121,
        "628bd2cb2004694adaab7bbd778a25df25c47b9d4155a55f8fbd79f2fe154cff",
    ),
    (
        6144,
        "3e2e5b74e048f3add6d21faab3f83aa44d3b2278afb83b80b3c35164ebeca205",
    ),
    (
        6145,
        "f1323a8631446cc50536a9f705ee5cb619424d46887f3c376c695b70e0f0507f",
    ),
    (
        7168,
        "61da957ec2499a95d6b8023e2b0e604ec7f6b50e80a9678b89d2628e99ada77a",
    ),
    (
        7169,
        "a003fc7a51754a9b3c7fae0367ab3d782dccf28855a03d435f8cfe74605e7817",
    ),
    (
        8192,
        "aae792484c8efe4f19e2ca7d371d8c467ffb10748d8a5a1ae579948f718a2a63",
    ),
    (
        8193,
        "bab6c09cb8ce8cf459261398d2e7aef35700bf488116ceb94a36d0f5f1b7bc3b",
    ),
    (
        16384,
        "f875d6646de28985646f34ee13be9a576fd515f76b5b0a26bb324735041ddde4",
    ),
    (
        31744,
        "62b6960e1a44bcc1eb1a611a8d6235b6b4b78f32e7abc4fb4c6cdcce94895c47",
    ),
    (
        102400,
        "bc3e3d41a1146b069abffad3c0d44860cf664390afce4d9661f7902e7943e085",
    ),
];

/// The vector input of `len` bytes: 0, 1, ..., 250, 0, 1, ...
fn vector_input(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i % 251).expect("i % 251 is below 256"))
        .collect()
}

fn decode_hex(hex: &str) -> [u8; 32] {
    assert_eq!(hex.len(), 64, "a 32-byte hash is 64 hex digits");
    let mut out = [0u8; 32];
    let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
    assert!(rest.is_empty(), "hex digits come in pairs");
    for (byte, pair) in out.iter_mut().zip(pairs) {
        let text = std::str::from_utf8(pair.as_slice()).expect("hex is ASCII");
        *byte = u8::from_str_radix(text, 16).expect("valid hex digit pair");
    }
    out
}

fn incremental(pieces: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    for piece in pieces {
        hasher.update(piece);
    }
    hasher.finalize()
}

#[test]
fn one_shot_matches_official_vectors() {
    for (len, expected) in VECTORS {
        assert_eq!(
            hash(&vector_input(len)),
            decode_hex(expected),
            "input_len {len}"
        );
    }
}

#[test]
fn incremental_matches_official_vectors() {
    for (len, expected) in VECTORS {
        let input = vector_input(len);
        assert_eq!(
            incremental(&[&input]),
            decode_hex(expected),
            "input_len {len}"
        );
    }
}

/// Feeding the vector inputs in fixed-size pieces, sizes chosen around the
/// 64-byte block and 1024-byte chunk boundaries, gives the one-shot hash.
#[test]
fn one_shot_and_incremental_agree_for_every_piece_size() {
    const PIECE_SIZES: [usize; 11] = [1, 2, 7, 63, 64, 65, 1023, 1024, 1025, 4096, 10_000];
    for (len, _) in VECTORS {
        let input = vector_input(len);
        let expected = hash(&input);
        for size in PIECE_SIZES {
            let pieces: Vec<&[u8]> = input.chunks(size).collect();
            assert_eq!(
                incremental(&pieces),
                expected,
                "input_len {len}, piece {size}"
            );
        }
    }
}

/// Splitting the input in two at every position, with an empty piece at each
/// end as well, gives the one-shot hash. Covers every block and chunk
/// boundary up to three chunks.
#[test]
fn one_shot_and_incremental_agree_for_every_split_point() {
    let input = vector_input(3073);
    let expected = hash(&input);
    for at in 0..=input.len() {
        let (head, tail) = input.split_at(at);
        assert_eq!(
            incremental(&[&[], head, &[], tail, &[]]),
            expected,
            "split at {at}"
        );
    }
}

#[test]
fn new_and_default_hash_the_empty_input() {
    let empty = decode_hex(VECTORS[0].1);
    assert_eq!(Hasher::new().finalize(), empty);
    assert_eq!(Hasher::default().finalize(), empty);
    assert_eq!(hash(&[]), empty);
}

/// `finalize` leaves the hasher unchanged: repeating it gives the same hash,
/// and later updates continue the same input.
#[test]
fn finalize_does_not_change_the_hasher() {
    let input = vector_input(2049);
    let (head, tail) = input.split_at(1000);
    let mut hasher = Hasher::new();
    hasher.update(head);
    let first = hasher.finalize();
    assert_eq!(first, hash(head));
    assert_eq!(hasher.finalize(), first);
    hasher.update(tail);
    assert_eq!(hasher.finalize(), hash(&input));
}

#[test]
fn clone_continues_independently() {
    let input = vector_input(1500);
    let (head, tail) = input.split_at(700);
    let mut original = Hasher::new();
    original.update(head);
    let mut copy = original.clone();
    copy.update(tail);
    assert_eq!(original.finalize(), hash(head));
    assert_eq!(copy.finalize(), hash(&input));
}

#[test]
fn update_chains() {
    let mut hasher = Hasher::new();
    let digest = hasher.update(b"ab").update(b"").update(b"c").finalize();
    assert_eq!(digest, hash(b"abc"));
}

#[test]
fn debug_shows_no_state() {
    let mut hasher = Hasher::new();
    hasher.update(b"payload");
    assert_eq!(format!("{hasher:?}"), "Hasher { .. }");
}
