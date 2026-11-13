//! The `IKpsk2` state machine against the cacophony test vector for
//! `Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s`.
//!
//! AL instantiates `IKpsk2` with XChaCha20-Poly1305 and its own protocol name,
//! so no published vector covers AL itself. The state machine here is generic
//! over the cipher, so this test runs it with the Noise `ChaChaPoly` cipher
//! (RFC 8439, nonce `4 zero bytes || n` little-endian) and the vector's
//! protocol name and prologue, and checks every message byte, the handshake
//! hash, and the `Split()` keys through the four transport messages.
//!
//! Source: `tests/vectors/cacophony.txt` in the `snow` 0.10.0 crate source
//! (SHA-256 of the file
//! 3bde7c09a6f349ee11c825c50fcc02649f8f02a47c857a459206b357f9386cae), the
//! entry whose `protocol_name` is `Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s`.
//! The cacophony vectors are those of the Haskell implementation
//! (https://github.com/centromere/cacophony).

use chacha20poly1305::aead::array::Array;
use chacha20poly1305::{AeadInOut, ChaCha20Poly1305, KeyInit};

use super::super::handshake::{initiator_read, initiator_write, responder_read, responder_write};
use super::super::noise::{AeadFailure, NoiseCipher, SymmetricState, TAG_LEN};
use super::super::{EphemeralSecret, PresharedKey, PublicKey, StaticSecret};
use super::{hex, hex_array};

/// The Noise `ChaChaPoly` cipher functions (Noise section 12.3).
struct ChaChaPoly;

fn chachapoly_nonce(n: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&n.to_le_bytes());
    nonce
}

impl NoiseCipher for ChaChaPoly {
    fn encrypt(
        key: &[u8; 32],
        nonce: u64,
        ad: &[u8],
        buf: &mut [u8],
    ) -> Result<[u8; TAG_LEN], AeadFailure> {
        let aead = ChaCha20Poly1305::new(key.into());
        let nonce = chachapoly_nonce(nonce);
        aead.encrypt_inout_detached(<&Array<u8, _>>::from(&nonce), ad, buf.into())
            .map(Into::into)
            .map_err(|_| AeadFailure)
    }

    fn decrypt(
        key: &[u8; 32],
        nonce: u64,
        ad: &[u8],
        buf: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> Result<(), AeadFailure> {
        let aead = ChaCha20Poly1305::new(key.into());
        let nonce = chachapoly_nonce(nonce);
        aead.decrypt_inout_detached(
            <&Array<u8, _>>::from(&nonce),
            ad,
            buf.into(),
            <&Array<u8, _>>::from(tag),
        )
        .map_err(|_| AeadFailure)
    }
}

const PROTOCOL_NAME: &[u8] = b"Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &str = "4a6f686e2047616c74";
const PSK: &str = "54686973206973206d7920417573747269616e20706572737065637469766521";
const INIT_STATIC: &str = "e61ef9919cde45dd5f82166404bd08e38bceb5dfdfded0a34c8df7ed542214d1";
const INIT_EPHEMERAL: &str = "893e28b9dc6ca8d611ab664754b8ceb7bac5117349a4439a6b0569da977c464a";
const INIT_REMOTE_STATIC: &str = "31e0303fd6418d2f8c0e78b91f22e8caed0fbe48656dcf4767e4834f701b8f62";
const RESP_STATIC: &str = "4a3acbfdb163dec651dfa3194dece676d437029c62a408b4c5ea9114246e4893";
const RESP_EPHEMERAL: &str = "bbdb4cdbd309f1a1f2e1456967fe288cadd6f712d65dc7b7793d5e63da6b375b";
const HANDSHAKE_HASH: &str = "f5191b875290abcd41347ac3622d9679688a7e980229cb937ef748336cfde0e5";

/// `(payload, ciphertext)` of the six messages: two handshake messages, then
/// transport messages alternating from the initiator.
const MESSAGES: [(&str, &str); 6] = [
    (
        "4c756477696720766f6e204d69736573",
        concat!(
            "ca35def5ae56cec33dc2036731ab14896bc4c75dbb07a61f879f8e3afa4c7944",
            "001e21de9f98ddd8e2ad57527207feb56253c9c94a9e496782ecfcb2a75fbcaf",
            "1b52948cc48daefe660c62119ab5000980c84831215f2441eba616548e832985",
            "464cf17e51ee93109008399a21f7e13f",
        ),
    ),
    (
        "4d757272617920526f746862617264",
        concat!(
            "95ebc60d2b1fa672c1f46a8aa265ef51bfe38e7ccb39ec5be34069f144808843",
            "cb765f2caef0751b8f007572dab0322217755c0632f365717edbf34d33e87a",
        ),
    ),
    (
        "462e20412e20486179656b",
        "8153ca9833bc3c1b91a7e66e5f4d4f5b59bf9e64c2f20d15f0bba7",
    ),
    (
        "4361726c204d656e676572",
        "07af0c9c86e1b4e80f36b04ff7688d51141af3debd0332f0a705ef",
    ),
    (
        "4a65616e2d426170746973746520536179",
        "6ab1467c0448cc78394494abaaf23afce0e234315d6e2624dcbfa8a21c1c4d073d",
    ),
    (
        "457567656e2042f6686d20766f6e2042617765726b",
        "dfc346c0d2296ae6cf1acf6f12b8456a1dba228cf8d8b774aacf1c47fc53aa80ebc7a4c292",
    ),
];

/// Encrypts a transport message with a `Split()` key and empty associated
/// data, as a Noise `CipherState` does.
fn transport(key: &[u8; 32], n: u64, payload: &[u8]) -> Vec<u8> {
    let mut buf = payload.to_vec();
    let tag = ChaChaPoly::encrypt(key, n, &[], &mut buf).expect("encrypt");
    buf.extend_from_slice(&tag);
    buf
}

fn split(ss: &SymmetricState<ChaChaPoly>) -> [[u8; 32]; 2] {
    let mut out = [[0u8; 32]; 2];
    ss.split(&mut out);
    out
}

#[test]
fn ikpsk2_matches_cacophony_vector() {
    let prologue = hex(PROLOGUE);
    let psk = PresharedKey::from_bytes(hex_array(PSK));
    let initiator = StaticSecret::from_bytes(hex_array(INIT_STATIC));
    let responder = StaticSecret::from_bytes(hex_array(RESP_STATIC));
    let responder_public = PublicKey::from_bytes(hex_array(INIT_REMOTE_STATIC));
    assert_eq!(responder.public_key(), responder_public);

    // Message 1, `-> e, es, s, ss`, written by the initiator.
    let payload1 = hex(MESSAGES[0].0);
    let mut message1 = vec![0u8; 32 + 48 + payload1.len() + TAG_LEN];
    message1[80..80 + payload1.len()].copy_from_slice(&payload1);
    let initiator_core = initiator_write::<ChaChaPoly>(
        PROTOCOL_NAME,
        &prologue,
        &initiator,
        &responder_public,
        EphemeralSecret::from_bytes(hex_array(INIT_EPHEMERAL)),
        &mut message1,
    )
    .expect("write message 1");
    assert_eq!(message1, hex(MESSAGES[0].1));

    // Message 1, read by the responder.
    let responder_core =
        responder_read::<ChaChaPoly>(PROTOCOL_NAME, &prologue, &responder, &mut message1)
            .expect("read message 1");
    assert_eq!(&message1[80..80 + payload1.len()], &payload1[..]);
    assert_eq!(&message1[32..64], initiator.public_key().as_bytes());

    // Message 2, `<- e, ee, se, psk`, written by the responder.
    let payload2 = hex(MESSAGES[1].0);
    let mut message2 = vec![0u8; 32 + payload2.len() + TAG_LEN];
    message2[32..32 + payload2.len()].copy_from_slice(&payload2);
    let responder_final = responder_write(
        responder_core,
        EphemeralSecret::from_bytes(hex_array(RESP_EPHEMERAL)),
        &psk,
        &mut message2,
    )
    .expect("write message 2");
    assert_eq!(message2, hex(MESSAGES[1].1));

    // Message 2, read by the initiator.
    let initiator_final =
        initiator_read(&initiator_core, &initiator, &psk, &mut message2).expect("read message 2");
    assert_eq!(&message2[32..32 + payload2.len()], &payload2[..]);

    let hash: [u8; 32] = hex_array(HANDSHAKE_HASH);
    assert_eq!(initiator_final.handshake_hash(), &hash);
    assert_eq!(responder_final.ss.handshake_hash(), &hash);

    // Transport messages: the initiator sends with k1, the responder with k2,
    // each with its own nonce from 0.
    let [k1, k2] = split(&initiator_final);
    assert_eq!(split(&responder_final.ss), [k1, k2]);
    for (index, (payload, ciphertext)) in MESSAGES[2..].iter().enumerate() {
        let from_initiator = index % 2 == 0;
        let key = if from_initiator { &k1 } else { &k2 };
        let n = u64::try_from(index / 2).expect("small");
        assert_eq!(
            transport(key, n, &hex(payload)),
            hex(ciphertext),
            "message {}",
            index + 3
        );
    }
}
