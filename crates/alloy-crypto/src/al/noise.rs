//! The Noise building blocks the AL handshake uses: `HASH`, `HMAC-HASH`,
//! `HKDF`, the cipher functions and the symmetric state of the Noise
//! specification revision 34, sections 4 and 5.2, for the hash BLAKE2s.
//!
//! Every function writes its secret outputs into buffers its caller owns, so
//! that the caller decides how they are erased. Every secret this module holds
//! itself, in locals or in [`SymmetricState`], is erased before it is released.

use std::marker::PhantomData;

use blake2::Blake2s256;
use blake2::digest::{Digest, Output};
use chacha20poly1305::aead::array::Array;
use chacha20poly1305::{AeadInOut, KeyInit, XChaCha20Poly1305};
use zeroize::{Zeroize, Zeroizing};

/// `HASHLEN` of BLAKE2s, and the length of every key in the handshake.
pub(crate) const HASH_LEN: usize = 32;

/// `BLOCKLEN` of BLAKE2s, the HMAC block size.
const BLOCK_LEN: usize = 64;

/// The length of a Poly1305 tag.
pub(crate) const TAG_LEN: usize = 16;

const IPAD: u8 = 0x36;
const OPAD: u8 = 0x5c;

/// The AEAD failed: a tag did not verify, or a length was out of range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AeadFailure;

/// Writes BLAKE2s-256 of the concatenation of `parts` into `out`.
pub(crate) fn hash_into(parts: &[&[u8]], out: &mut [u8; HASH_LEN]) {
    let mut state = Blake2s256::new();
    for part in parts {
        state.update(part);
    }
    // The state erases its own buffer and chaining value when it is dropped
    // here (the `zeroize` feature of `blake2`).
    state.finalize_into(<&mut Output<Blake2s256>>::from(out));
}

/// Writes `HMAC-BLAKE2s(key, parts[0] || parts[1] || ...)` into `out`.
///
/// This is HMAC as RFC 2104 defines it, with BLAKE2s-256 as the hash and a
/// 64-byte block, which is `HMAC-HASH` of the Noise specification section
/// 4.3. A key longer than the block is hashed first, as RFC 2104 requires.
/// The padded key blocks and the inner hash are erased before it returns.
pub(crate) fn hmac_into(key: &[u8], parts: &[&[u8]], out: &mut [u8; HASH_LEN]) {
    let mut block = Zeroizing::new([0u8; BLOCK_LEN]);
    if key.len() > BLOCK_LEN {
        let mut digest = Zeroizing::new([0u8; HASH_LEN]);
        hash_into(&[key], &mut digest);
        block[..HASH_LEN].copy_from_slice(&digest[..]);
    } else {
        block[..key.len()].copy_from_slice(key);
    }

    for byte in block.iter_mut() {
        *byte ^= IPAD;
    }
    let mut inner = Blake2s256::new();
    inner.update(&block[..]);
    for part in parts {
        inner.update(part);
    }
    let mut inner_hash = Zeroizing::new([0u8; HASH_LEN]);
    inner.finalize_into(<&mut Output<Blake2s256>>::from(&mut *inner_hash));

    for byte in block.iter_mut() {
        *byte ^= IPAD ^ OPAD;
    }
    let mut outer = Blake2s256::new();
    outer.update(&block[..]);
    outer.update(&inner_hash[..]);
    outer.finalize_into(<&mut Output<Blake2s256>>::from(out));
}

/// Noise `HKDF(chaining_key, input_key_material, N)` of section 4.3, for `N`
/// of 2 or 3, writing output `i` into `out[i]`.
///
/// `temp_key = HMAC(chaining_key, input_key_material)`, then
/// `out[0] = HMAC(temp_key, 0x01)` and `out[i] = HMAC(temp_key, out[i-1] ||
/// i+1)`. This equals RFC 5869 HKDF-Extract with `chaining_key` as salt,
/// followed by HKDF-Expand with empty info. `temp_key` is erased.
pub(crate) fn hkdf<const N: usize>(
    chaining_key: &[u8; HASH_LEN],
    input_key_material: &[u8],
    out: &mut [[u8; HASH_LEN]; N],
) {
    let mut temp_key = Zeroizing::new([0u8; HASH_LEN]);
    hmac_into(chaining_key, &[input_key_material], &mut temp_key);
    let mut index: u8 = 1;
    let mut previous: Option<usize> = None;
    for i in 0..N {
        let (done, rest) = out.split_at_mut(i);
        let counter = [index];
        match previous {
            None => hmac_into(&temp_key[..], &[&counter], &mut rest[0]),
            Some(p) => hmac_into(&temp_key[..], &[&done[p], &counter], &mut rest[0]),
        }
        previous = Some(i);
        index = index.wrapping_add(1);
    }
}

/// RFC 5869 HKDF-Extract over HMAC-BLAKE2s: `prk = HMAC(salt, ikm)`.
pub(crate) fn hkdf_extract(salt: &[u8], ikm: &[&[u8]], prk: &mut [u8; HASH_LEN]) {
    hmac_into(salt, ikm, prk);
}

/// RFC 5869 HKDF-Expand over HMAC-BLAKE2s for an output of at most one hash
/// length: `okm = T(1)[..len] = HMAC(prk, info || 0x01)[..len]`.
///
/// It fills the whole of `okm`, whose length is at most 32 by its type.
pub(crate) fn hkdf_expand_one<const L: usize>(prk: &[u8; HASH_LEN], info: &[u8], okm: &mut [u8; L])
where
    [u8; L]: OneBlock,
{
    let mut block = Zeroizing::new([0u8; HASH_LEN]);
    hmac_into(prk, &[info, &[1]], &mut block);
    okm.copy_from_slice(&block[..L]);
}

/// Marks the output lengths [`hkdf_expand_one`] accepts: those that fit in one
/// HKDF-Expand block. The handshake uses 32 and 14.
pub(crate) trait OneBlock {}
impl OneBlock for [u8; 32] {}
impl OneBlock for [u8; 14] {}

/// A cipher function pair `ENCRYPT(k, n, ad, plaintext)` and `DECRYPT(k, n,
/// ad, ciphertext)` of the Noise specification section 4.2.
///
/// Both work in place on `buf` and keep the tag apart. A cipher builds its
/// AEAD state from `key` for one call and erases it on return.
pub(crate) trait NoiseCipher {
    /// Encrypts `buf` in place and returns the tag.
    fn encrypt(
        key: &[u8; HASH_LEN],
        nonce: u64,
        ad: &[u8],
        buf: &mut [u8],
    ) -> Result<[u8; TAG_LEN], AeadFailure>;

    /// Decrypts `buf` in place if `tag` verifies. On failure `buf` is left as
    /// it was.
    fn decrypt(
        key: &[u8; HASH_LEN],
        nonce: u64,
        ad: &[u8],
        buf: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> Result<(), AeadFailure>;
}

/// XChaCha20-Poly1305 with the AL handshake nonce of ARFC 0003 section 3.1:
/// 16 zero bytes, then `n` as 8 bytes little-endian (ADR 0043).
#[derive(Debug)]
pub(crate) struct AlCipher;

/// Returns the 24-byte XChaCha20-Poly1305 nonce `zero (16) || n (8, LE)`.
pub(crate) fn al_nonce(n: u64) -> [u8; 24] {
    let mut nonce = [0u8; 24];
    nonce[16..].copy_from_slice(&n.to_le_bytes());
    nonce
}

impl NoiseCipher for AlCipher {
    fn encrypt(
        key: &[u8; HASH_LEN],
        nonce: u64,
        ad: &[u8],
        buf: &mut [u8],
    ) -> Result<[u8; TAG_LEN], AeadFailure> {
        // The cipher copies the key and erases its copy on drop (the
        // `zeroize` feature of `chacha20poly1305`).
        let aead = XChaCha20Poly1305::new(<&Array<u8, _>>::from(key));
        let nonce = al_nonce(nonce);
        aead.encrypt_inout_detached(<&Array<u8, _>>::from(&nonce), ad, buf.into())
            .map(Into::into)
            .map_err(|_| AeadFailure)
    }

    fn decrypt(
        key: &[u8; HASH_LEN],
        nonce: u64,
        ad: &[u8],
        buf: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> Result<(), AeadFailure> {
        let aead = XChaCha20Poly1305::new(<&Array<u8, _>>::from(key));
        let nonce = al_nonce(nonce);
        aead.decrypt_inout_detached(
            <&Array<u8, _>>::from(&nonce),
            ad,
            buf.into(),
            <&Array<u8, _>>::from(tag),
        )
        .map_err(|_| AeadFailure)
    }
}

/// The Noise `SymmetricState` of section 5.2 with its `CipherState` of
/// section 5.1, over BLAKE2s and the cipher `C`.
///
/// It holds the chaining key `ck`, the handshake hash `h`, the cipher key `k`
/// and the nonce `n`. All three keys are erased when it is dropped.
pub(crate) struct SymmetricState<C> {
    ck: [u8; HASH_LEN],
    h: [u8; HASH_LEN],
    k: [u8; HASH_LEN],
    has_key: bool,
    n: u64,
    cipher: PhantomData<C>,
}

impl<C> std::fmt::Debug for SymmetricState<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SymmetricState").finish_non_exhaustive()
    }
}

impl<C> Drop for SymmetricState<C> {
    fn drop(&mut self) {
        self.erase();
    }
}

impl<C: NoiseCipher> SymmetricState<C> {
    /// `InitializeSymmetric(protocol_name)`: `h` is the name padded with zero
    /// bytes when it fits in `HASHLEN`, otherwise its hash, and `ck = h`. The
    /// cipher state has no key.
    pub(crate) fn initialize(protocol_name: &[u8]) -> Self {
        let mut h = [0u8; HASH_LEN];
        if protocol_name.len() <= HASH_LEN {
            h[..protocol_name.len()].copy_from_slice(protocol_name);
        } else {
            hash_into(&[protocol_name], &mut h);
        }
        Self {
            ck: h,
            h,
            k: [0u8; HASH_LEN],
            has_key: false,
            n: 0,
            cipher: PhantomData,
        }
    }

    /// `MixHash(data)`: `h = HASH(h || data)`.
    pub(crate) fn mix_hash(&mut self, data: &[u8]) {
        let previous = self.h;
        hash_into(&[&previous, data], &mut self.h);
    }

    /// `MixKey(input_key_material)`: `ck, temp_k = HKDF(ck, ikm, 2)`, then
    /// `InitializeKey(temp_k)`.
    pub(crate) fn mix_key(&mut self, input_key_material: &[u8]) {
        let mut out = Zeroizing::new([[0u8; HASH_LEN]; 2]);
        hkdf(&self.ck, input_key_material, &mut out);
        self.ck = out[0];
        self.k = out[1];
        self.has_key = true;
        self.n = 0;
    }

    /// `MixKeyAndHash(input_key_material)`: `ck, temp_h, temp_k = HKDF(ck,
    /// ikm, 3)`, `MixHash(temp_h)`, then `InitializeKey(temp_k)`.
    pub(crate) fn mix_key_and_hash(&mut self, input_key_material: &[u8]) {
        let mut out = Zeroizing::new([[0u8; HASH_LEN]; 3]);
        hkdf(&self.ck, input_key_material, &mut out);
        self.ck = out[0];
        self.mix_hash(&out[1]);
        self.k = out[2];
        self.has_key = true;
        self.n = 0;
    }

    /// `EncryptAndHash(plaintext)` when the cipher state has a key, which is
    /// the only case the AL handshake reaches: encrypts `buf` in place with
    /// `h` as associated data, writes the tag to `tag`, and mixes `buf || tag`
    /// into `h`. On failure nothing changes.
    pub(crate) fn encrypt_and_hash(
        &mut self,
        buf: &mut [u8],
        tag: &mut [u8; TAG_LEN],
    ) -> Result<(), AeadFailure> {
        if !self.has_key || self.n == u64::MAX {
            return Err(AeadFailure);
        }
        *tag = C::encrypt(&self.k, self.n, &self.h, buf)?;
        self.n += 1;
        let previous = self.h;
        hash_into(&[&previous, buf, tag], &mut self.h);
        Ok(())
    }

    /// `DecryptAndHash(ciphertext)` when the cipher state has a key: decrypts
    /// `buf` in place with `h` as associated data and, if `tag` verifies,
    /// mixes the ciphertext and tag into `h`. On failure nothing changes.
    pub(crate) fn decrypt_and_hash(
        &mut self,
        buf: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> Result<(), AeadFailure> {
        if !self.has_key || self.n == u64::MAX {
            return Err(AeadFailure);
        }
        let mut next_h = [0u8; HASH_LEN];
        hash_into(&[&self.h, buf, tag], &mut next_h);
        C::decrypt(&self.k, self.n, &self.h, buf, tag)?;
        self.n += 1;
        self.h = next_h;
        Ok(())
    }

    /// `Split()`: writes `k1, k2 = HKDF(ck, zerolen, 2)` into `out`. `k1` is
    /// the initiator's sending direction.
    pub(crate) fn split(&self, out: &mut [[u8; HASH_LEN]; 2]) {
        hkdf(&self.ck, &[], out);
    }

    /// `GetHandshakeHash()`: the current `h`.
    pub(crate) fn handshake_hash(&self) -> &[u8; HASH_LEN] {
        &self.h
    }
}

impl<C> SymmetricState<C> {
    /// A second state equal to this one, which erases itself on drop like
    /// any other. The initiator reads a response into a duplicate, so that a
    /// response that fails leaves its state as it was.
    ///
    /// The duplicate is built on the heap and filled in place, so its keys
    /// are not left in a stack frame on the way.
    pub(crate) fn duplicate(&self) -> Box<Self> {
        let mut copy = Box::new(Self {
            ck: [0u8; HASH_LEN],
            h: [0u8; HASH_LEN],
            k: [0u8; HASH_LEN],
            has_key: false,
            n: 0,
            cipher: PhantomData,
        });
        copy.ck.copy_from_slice(&self.ck);
        copy.h.copy_from_slice(&self.h);
        copy.k.copy_from_slice(&self.k);
        copy.has_key = self.has_key;
        copy.n = self.n;
        copy
    }

    /// Overwrites `ck`, `h` and `k` with zeros and drops the key. Dropping the
    /// state calls this.
    pub(crate) fn erase(&mut self) {
        self.ck.zeroize();
        self.h.zeroize();
        self.k.zeroize();
        self.has_key = false;
        self.n = 0;
    }

    /// Whether every secret the state holds is zero. For tests of erasure.
    #[cfg(test)]
    pub(crate) fn is_erased(&self) -> bool {
        self.ck == [0; HASH_LEN]
            && self.h == [0; HASH_LEN]
            && self.k == [0; HASH_LEN]
            && !self.has_key
    }
}
