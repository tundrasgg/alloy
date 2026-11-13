//! The Noise `IKpsk2` handshake of ARFC 0003 section 3.1, and the session
//! keys it yields (section 3.3, ADR 0043).

use std::fmt;

use thiserror::Error;
use zeroize::{ZeroizeOnDrop, Zeroizing};

use super::keys::{EphemeralSecret, KEY_LEN, PresharedKey, PublicKey, StaticSecret};
use super::noise::{AeadFailure, AlCipher, HASH_LEN, NoiseCipher, SymmetricState, TAG_LEN};
use super::session::{Role, SessionKeys};

/// The Noise protocol name of AL version 1: the 21 ASCII bytes
/// `alloy/al/handshake/v1`. It is shorter than 32 bytes, so the initial `h` is
/// the name followed by 11 zero bytes.
pub const PROTOCOL_NAME: &[u8] = b"alloy/al/handshake/v1";

/// The length of the initiation payload: `{counter: u64, queues: u16, flags:
/// u16, reserved: u32}`.
pub const INITIATION_PAYLOAD_LEN: usize = 16;

/// The length of the response payload: `{queues: u16, flags: u16}`.
pub const RESPONSE_PAYLOAD_LEN: usize = 4;

/// The length of the Noise bytes of an initiation: `ephemeral` (32),
/// `encrypted static` (32 + 16) and `encrypted payload` (16 + 16), in that
/// order. They are bytes 8 to 119 of the 152-byte message.
pub const INITIATION_NOISE_LEN: usize =
    KEY_LEN + KEY_LEN + TAG_LEN + INITIATION_PAYLOAD_LEN + TAG_LEN;

/// The length of the Noise bytes of a response: `ephemeral` (32) and
/// `encrypted payload` (4 + 16), in that order. They are bytes 12 to 63 of the
/// 96-byte message.
pub const RESPONSE_NOISE_LEN: usize = KEY_LEN + RESPONSE_PAYLOAD_LEN + TAG_LEN;

/// Why a handshake message was not written or not accepted.
///
/// No variant carries a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum HandshakeError {
    /// An encrypted field did not decrypt: the message was altered, or it was
    /// made for a different static key, ephemeral key or pre-shared key.
    #[error("handshake message does not authenticate")]
    Unauthentic,
    /// A Diffie-Hellman result was all zero bytes, because a public key in
    /// the handshake has small order. No session can rest on it.
    #[error("Diffie-Hellman result is all zero")]
    NonContributory,
    /// The AEAD refused to encrypt. It does so only for inputs far longer
    /// than any handshake field, so it does not happen in section 3.1.
    #[error("handshake field could not be encrypted")]
    Encrypt,
}

/// The initiator's side of a handshake, between sending the initiation and
/// processing the response.
///
/// It holds the chaining key, the handshake hash, the handshake cipher key and
/// the ephemeral private key, on the heap in one place, and erases them when
/// it is dropped or consumed. Its `Debug` output shows none of them.
pub struct InitiatorHandshake {
    core: Box<InitiatorCore<AlCipher>>,
}

impl InitiatorHandshake {
    /// Starts a handshake and writes the Noise bytes of the initiation.
    ///
    /// `local` is this node's static key, `remote` the responder's static
    /// public key, `ephemeral` a fresh ephemeral key the caller drew from the
    /// `alloy-rt` randomness trait, and `payload` the encoded initiation
    /// payload. This runs the tokens `e, es, s, ss` and encrypts the payload,
    /// which is two Diffie-Hellman operations.
    ///
    /// The returned bytes are the `ephemeral`, `encrypted static` and
    /// `encrypted payload` fields of the initiation, in order. The caller puts
    /// them in the message, and computes `mac1` and `mac2` over it
    /// ([`Mac1Key`](crate::al::Mac1Key), [`Cookie`](crate::al::Cookie)).
    ///
    /// # Errors
    ///
    /// [`HandshakeError::NonContributory`] when `remote` has small order.
    /// The ephemeral key is erased then.
    pub fn write_initiation(
        local: &StaticSecret,
        remote: &PublicKey,
        ephemeral: EphemeralSecret,
        payload: &[u8; INITIATION_PAYLOAD_LEN],
    ) -> Result<(Self, [u8; INITIATION_NOISE_LEN]), HandshakeError> {
        let mut message = [0u8; INITIATION_NOISE_LEN];
        message[INITIATION_PAYLOAD_AT..INITIATION_PAYLOAD_AT + INITIATION_PAYLOAD_LEN]
            .copy_from_slice(payload);
        let core = initiator_write::<AlCipher>(
            PROTOCOL_NAME,
            &[],
            local,
            remote,
            ephemeral,
            &mut message,
        )?;
        Ok((Self { core }, message))
    }

    /// Processes the Noise bytes of a response and, if it authenticates,
    /// returns the session keys and the response payload.
    ///
    /// `local` must be the static key given to
    /// [`write_initiation`](Self::write_initiation), and `psk` the pre-shared
    /// key for this pair of nodes, [`PresharedKey::none`] when none is
    /// configured. This runs the tokens `e, ee, se, psk` and decrypts the
    /// payload, which is two Diffie-Hellman operations.
    ///
    /// On success the handshake is consumed: its ephemeral private key,
    /// chaining key and cipher key are erased, as are the `Split()` keys and
    /// `prk` used to derive the session keys (section 3.3). The caller checks
    /// the payload (section 3.1) and abandons the session by dropping the keys
    /// if it does not pass.
    ///
    /// # Errors
    ///
    /// [`ResponseRejected`] when the response does not authenticate or a
    /// Diffie-Hellman result is zero. It hands the handshake back unchanged,
    /// so a forged response does not end it, and a genuine response can still
    /// be processed.
    pub fn read_response(
        self,
        local: &StaticSecret,
        psk: &PresharedKey,
        response: &[u8; RESPONSE_NOISE_LEN],
    ) -> Result<(SessionKeys, [u8; RESPONSE_PAYLOAD_LEN]), ResponseRejected> {
        let mut message = *response;
        match initiator_read(&self.core, local, psk, &mut message) {
            Ok(state) => {
                let keys = finish(&state, Role::Initiator);
                drop(state);
                let mut payload = [0u8; RESPONSE_PAYLOAD_LEN];
                payload.copy_from_slice(&message[KEY_LEN..KEY_LEN + RESPONSE_PAYLOAD_LEN]);
                Ok((keys, payload))
            }
            Err(error) => Err(ResponseRejected {
                error,
                handshake: self,
            }),
        }
    }
}

/// Shows only the type name.
impl fmt::Debug for InitiatorHandshake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InitiatorHandshake").finish_non_exhaustive()
    }
}

impl ZeroizeOnDrop for InitiatorHandshake {}

/// A response the initiator did not accept, and its handshake, unchanged.
#[derive(Debug, Error)]
#[error("response rejected: {error}")]
pub struct ResponseRejected {
    /// Why the response was not accepted.
    pub error: HandshakeError,
    /// The handshake, as it was before the response. The caller keeps it
    /// pending or drops it, which erases it.
    pub handshake: InitiatorHandshake,
}

/// The responder's side of a handshake, between processing the initiation
/// and writing the response.
///
/// It holds the chaining key, the handshake hash and the handshake cipher key,
/// on the heap in one place, and erases them when it is dropped or consumed.
/// Its `Debug` output shows none of them.
pub struct ResponderHandshake {
    core: Box<ResponderCore<AlCipher>>,
}

impl ResponderHandshake {
    /// Processes the Noise bytes of an initiation addressed to `local`.
    ///
    /// This runs the tokens `e, es, s, ss` and decrypts the payload, which is
    /// two Diffie-Hellman operations. The caller checks `mac1` first, and
    /// under load `mac2` (section 3.1); this does neither.
    ///
    /// On success it returns the pending handshake, the initiator's static
    /// public key and the decrypted payload. The caller then checks the
    /// payload and the counter (sections 3.1 and 3.2) and chooses the
    /// pre-shared key by the initiator's static key. If a check fails it drops
    /// the handshake, which erases it.
    ///
    /// # Errors
    ///
    /// [`HandshakeError::Unauthentic`] when the encrypted static key or the
    /// payload does not decrypt, and [`HandshakeError::NonContributory`] when
    /// a Diffie-Hellman result is zero. Nothing is kept then.
    pub fn read_initiation(
        local: &StaticSecret,
        initiation: &[u8; INITIATION_NOISE_LEN],
    ) -> Result<(Self, PublicKey, [u8; INITIATION_PAYLOAD_LEN]), HandshakeError> {
        let mut message = *initiation;
        let core = responder_read::<AlCipher>(PROTOCOL_NAME, &[], local, &mut message)?;
        let initiator = core.rs;
        let mut payload = [0u8; INITIATION_PAYLOAD_LEN];
        payload.copy_from_slice(
            &message[INITIATION_PAYLOAD_AT..INITIATION_PAYLOAD_AT + INITIATION_PAYLOAD_LEN],
        );
        Ok((Self { core }, initiator, payload))
    }

    /// The initiator's static public key, revealed by the initiation.
    #[must_use]
    pub fn initiator(&self) -> PublicKey {
        self.core.rs
    }

    /// Writes the Noise bytes of the response and returns the session keys.
    ///
    /// `ephemeral` is a fresh ephemeral key the caller drew from the
    /// `alloy-rt` randomness trait, `psk` the pre-shared key for the
    /// initiator ([`PresharedKey::none`] when none is configured), and
    /// `payload` the encoded response payload. This runs the tokens `e, ee,
    /// se, psk`, which is two Diffie-Hellman operations, and encrypts the
    /// payload.
    ///
    /// The returned bytes are the `ephemeral` and `encrypted payload` fields
    /// of the response. The handshake is consumed: whatever the result, its
    /// chaining key and cipher key, the ephemeral private key, the `Split()`
    /// keys and `prk` are erased when this returns.
    ///
    /// # Errors
    ///
    /// [`HandshakeError::NonContributory`] when a Diffie-Hellman result is
    /// zero, and [`HandshakeError::Encrypt`], which does not happen for these
    /// sizes.
    pub fn write_response(
        self,
        ephemeral: EphemeralSecret,
        psk: &PresharedKey,
        payload: &[u8; RESPONSE_PAYLOAD_LEN],
    ) -> Result<(SessionKeys, [u8; RESPONSE_NOISE_LEN]), HandshakeError> {
        let mut message = [0u8; RESPONSE_NOISE_LEN];
        message[KEY_LEN..KEY_LEN + RESPONSE_PAYLOAD_LEN].copy_from_slice(payload);
        let core = responder_write(self.core, ephemeral, psk, &mut message)?;
        let keys = finish(&core.ss, Role::Responder);
        drop(core);
        Ok((keys, message))
    }
}

/// Shows only the type name.
impl fmt::Debug for ResponderHandshake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponderHandshake").finish_non_exhaustive()
    }
}

impl ZeroizeOnDrop for ResponderHandshake {}

/// Where the encrypted payload starts in the initiation's Noise bytes.
const INITIATION_PAYLOAD_AT: usize = KEY_LEN + KEY_LEN + TAG_LEN;

/// The initiator's state between its two messages. Both fields erase
/// themselves on drop.
pub(crate) struct InitiatorCore<C> {
    pub(crate) ss: SymmetricState<C>,
    e: x25519_dalek::StaticSecret,
}

/// The responder's state between its two messages: the symmetric state, the
/// initiator's ephemeral public key `re` and static public key `rs`.
pub(crate) struct ResponderCore<C> {
    pub(crate) ss: SymmetricState<C>,
    re: PublicKey,
    rs: PublicKey,
}

/// One Diffie-Hellman operation, mixed into `ss` with `MixKey`. The shared
/// secret is erased when it goes out of scope.
fn mix_dh<C: NoiseCipher>(
    ss: &mut SymmetricState<C>,
    private: &x25519_dalek::StaticSecret,
    public: &PublicKey,
) -> Result<(), HandshakeError> {
    let shared = private.diffie_hellman(&public.to_dalek());
    if !shared.was_contributory() {
        return Err(HandshakeError::NonContributory);
    }
    ss.mix_key(shared.as_bytes());
    Ok(())
}

/// Splits `buf` into its plaintext and the 16-byte tag at its end.
fn split_tag(buf: &mut [u8]) -> Option<(&mut [u8], &mut [u8; TAG_LEN])> {
    let at = buf.len().checked_sub(TAG_LEN)?;
    let (body, tag) = buf.split_at_mut(at);
    let tag: &mut [u8; TAG_LEN] = tag.try_into().ok()?;
    Some((body, tag))
}

fn encrypt_field<C: NoiseCipher>(
    ss: &mut SymmetricState<C>,
    field: &mut [u8],
) -> Result<(), HandshakeError> {
    let (body, tag) = split_tag(field).ok_or(HandshakeError::Encrypt)?;
    ss.encrypt_and_hash(body, tag)
        .map_err(|AeadFailure| HandshakeError::Encrypt)
}

fn decrypt_field<C: NoiseCipher>(
    ss: &mut SymmetricState<C>,
    field: &mut [u8],
) -> Result<(), HandshakeError> {
    let (body, tag) = split_tag(field).ok_or(HandshakeError::Unauthentic)?;
    ss.decrypt_and_hash(body, tag)
        .map_err(|AeadFailure| HandshakeError::Unauthentic)
}

/// Starts the symmetric state of a responder static key `responder`: the
/// protocol name, the prologue and the pre-message `<- s`.
fn start<C: NoiseCipher>(
    protocol_name: &[u8],
    prologue: &[u8],
    responder: &PublicKey,
) -> SymmetricState<C> {
    let mut ss = SymmetricState::initialize(protocol_name);
    ss.mix_hash(prologue);
    ss.mix_hash(responder.as_bytes());
    ss
}

/// The token `e` of a pattern with a pre-shared key: `MixHash(e.public_key)`
/// and `MixKey(e.public_key)` (Noise section 9.2).
fn mix_ephemeral<C: NoiseCipher>(ss: &mut SymmetricState<C>, public: &PublicKey) {
    ss.mix_hash(public.as_bytes());
    ss.mix_key(public.as_bytes());
}

/// Writes the first message `-> e, es, s, ss` and its payload into
/// `message`, which is `e (32) || s (32 + 16) || payload || tag room (16)`
/// with the payload already in place.
pub(crate) fn initiator_write<C: NoiseCipher>(
    protocol_name: &[u8],
    prologue: &[u8],
    local: &StaticSecret,
    remote: &PublicKey,
    ephemeral: EphemeralSecret,
    message: &mut [u8],
) -> Result<Box<InitiatorCore<C>>, HandshakeError> {
    let e_public = ephemeral.public_key();
    let mut core = Box::new(InitiatorCore {
        ss: start(protocol_name, prologue, remote),
        e: ephemeral.into_dalek(),
    });
    let (e_out, rest) = message
        .split_at_mut_checked(KEY_LEN)
        .ok_or(HandshakeError::Encrypt)?;
    let (s_out, payload) = rest
        .split_at_mut_checked(KEY_LEN + TAG_LEN)
        .ok_or(HandshakeError::Encrypt)?;

    e_out.copy_from_slice(e_public.as_bytes());
    mix_ephemeral(&mut core.ss, &e_public);
    let InitiatorCore { ss, e } = &mut *core;
    mix_dh(ss, e, remote)?;
    s_out[..KEY_LEN].copy_from_slice(local.public_key().as_bytes());
    encrypt_field(ss, s_out)?;
    mix_dh(ss, local.dalek(), remote)?;
    encrypt_field(ss, payload)?;
    Ok(core)
}

/// Reads the first message `-> e, es, s, ss` and decrypts it in place.
pub(crate) fn responder_read<C: NoiseCipher>(
    protocol_name: &[u8],
    prologue: &[u8],
    local: &StaticSecret,
    message: &mut [u8],
) -> Result<Box<ResponderCore<C>>, HandshakeError> {
    let (e_in, rest) = message
        .split_at_mut_checked(KEY_LEN)
        .ok_or(HandshakeError::Unauthentic)?;
    let (s_in, payload) = rest
        .split_at_mut_checked(KEY_LEN + TAG_LEN)
        .ok_or(HandshakeError::Unauthentic)?;
    let mut re = [0u8; KEY_LEN];
    re.copy_from_slice(e_in);
    let re = PublicKey::from_bytes(re);

    let mut core = Box::new(ResponderCore {
        ss: start(protocol_name, prologue, &local.public_key()),
        re,
        rs: PublicKey::from_bytes([0u8; KEY_LEN]),
    });
    mix_ephemeral(&mut core.ss, &re);
    mix_dh(&mut core.ss, local.dalek(), &re)?;
    decrypt_field(&mut core.ss, s_in)?;
    let mut rs = [0u8; KEY_LEN];
    rs.copy_from_slice(&s_in[..KEY_LEN]);
    let rs = PublicKey::from_bytes(rs);
    core.rs = rs;
    mix_dh(&mut core.ss, local.dalek(), &rs)?;
    decrypt_field(&mut core.ss, payload)?;
    Ok(core)
}

/// Writes the second message `<- e, ee, se, psk` and its payload into
/// `message`, which is `e (32) || payload || tag room (16)` with the payload
/// already in place. It returns the state after the handshake, from which the
/// caller splits; the ephemeral private key is erased by then.
pub(crate) fn responder_write<C: NoiseCipher>(
    mut core: Box<ResponderCore<C>>,
    ephemeral: EphemeralSecret,
    psk: &PresharedKey,
    message: &mut [u8],
) -> Result<Box<ResponderCore<C>>, HandshakeError> {
    let (e_out, payload) = message
        .split_at_mut_checked(KEY_LEN)
        .ok_or(HandshakeError::Encrypt)?;
    let e_public = ephemeral.public_key();
    let e = ephemeral.into_dalek();

    e_out.copy_from_slice(e_public.as_bytes());
    let ResponderCore { ss, re, rs } = &mut *core;
    mix_ephemeral(ss, &e_public);
    mix_dh(ss, &e, re)?;
    mix_dh(ss, &e, rs)?;
    drop(e);
    ss.mix_key_and_hash(psk.as_bytes());
    encrypt_field(ss, payload)?;
    Ok(core)
}

/// Reads the second message `<- e, ee, se, psk` into a copy of the
/// initiator's state and decrypts it in place. On success it returns the copy,
/// the state after the handshake, from which the caller splits. On failure
/// the copy is erased and `core` is unchanged.
pub(crate) fn initiator_read<C: NoiseCipher>(
    core: &InitiatorCore<C>,
    local: &StaticSecret,
    psk: &PresharedKey,
    message: &mut [u8],
) -> Result<Box<SymmetricState<C>>, HandshakeError> {
    let (e_in, payload) = message
        .split_at_mut_checked(KEY_LEN)
        .ok_or(HandshakeError::Unauthentic)?;
    let mut re = [0u8; KEY_LEN];
    re.copy_from_slice(e_in);
    let re = PublicKey::from_bytes(re);

    let mut ss = core.ss.duplicate();
    mix_ephemeral(&mut ss, &re);
    mix_dh(&mut ss, &core.e, &re)?;
    mix_dh(&mut ss, local.dalek(), &re)?;
    ss.mix_key_and_hash(psk.as_bytes());
    decrypt_field(&mut ss, payload)?;
    Ok(ss)
}

/// `Split()`, then the section 3.3 derivation from `k1`, `k2` and `h`. The
/// split keys are erased before it returns; the caller drops `ss`.
fn finish<C: NoiseCipher>(ss: &SymmetricState<C>, role: Role) -> SessionKeys {
    let mut split = Zeroizing::new([[0u8; HASH_LEN]; 2]);
    ss.split(&mut split);
    SessionKeys::derive(&split, ss.handshake_hash(), role)
}
