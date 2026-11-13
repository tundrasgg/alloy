//! Little-endian byte reading and writing shared by every message.

use crate::error::{DecodeError, EncodeError};

/// Reads fields from the front of a byte slice. Every read is checked, so a
/// reader never panics whatever the input.
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    rest: &'a [u8],
    /// Length of the whole input, for error reports.
    total: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self {
            rest: bytes,
            total: bytes.len(),
        }
    }

    /// Takes the next `N` bytes, or reports the input as too short.
    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        match self.rest.split_first_chunk::<N>() {
            Some((head, tail)) => {
                self.rest = tail;
                Ok(*head)
            }
            None => Err(DecodeError::TooShort {
                minimum: self.total - self.rest.len() + N,
                found: self.total,
            }),
        }
    }

    pub(crate) fn u8(&mut self) -> Result<u8, DecodeError> {
        self.array::<1>().map(|[b]| b)
    }

    pub(crate) fn u16(&mut self) -> Result<u16, DecodeError> {
        self.array().map(u16::from_le_bytes)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, DecodeError> {
        self.array().map(u32::from_le_bytes)
    }

    pub(crate) fn u64(&mut self) -> Result<u64, DecodeError> {
        self.array().map(u64::from_le_bytes)
    }

    /// The bytes not yet read.
    pub(crate) fn rest(self) -> &'a [u8] {
        self.rest
    }
}

/// Returns the first `needed` bytes of `out`, or the error that says `out` is
/// too small.
pub(crate) fn room(out: &mut [u8], needed: usize) -> Result<&mut [u8], EncodeError> {
    let available = out.len();
    out.get_mut(..needed)
        .ok_or(EncodeError::BufferTooSmall { needed, available })
}

/// Writes fields to a buffer whose length the caller has already made exactly
/// the length of the encoding, so the writes always fit.
#[derive(Debug)]
pub(crate) struct Writer<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub(crate) fn put(&mut self, src: &[u8]) {
        let end = self.pos + src.len();
        self.buf[self.pos..end].copy_from_slice(src);
        self.pos = end;
    }

    pub(crate) fn u8(&mut self, v: u8) {
        self.put(&[v]);
    }

    pub(crate) fn u16(&mut self, v: u16) {
        self.put(&v.to_le_bytes());
    }

    pub(crate) fn u32(&mut self, v: u32) {
        self.put(&v.to_le_bytes());
    }

    pub(crate) fn u64(&mut self, v: u64) {
        self.put(&v.to_le_bytes());
    }
}

/// Checks the `type` byte and, when present, the `version` byte.
///
/// The type is checked first and the version second, before any length check:
/// a message of a later version may have another length, so a wrong version is
/// reported as such.
fn check_type_version(bytes: &[u8], ty: u8) -> Result<(), DecodeError> {
    let Some(&found) = bytes.first() else {
        return Err(DecodeError::Empty);
    };
    if found != ty {
        return Err(DecodeError::WrongType {
            expected: ty,
            found,
        });
    }
    if let Some(&version) = bytes.get(1)
        && version != crate::VERSION
    {
        return Err(DecodeError::UnsupportedVersion(version));
    }
    Ok(())
}

/// Checks the type, the version, and the exact length `len` of a message, and
/// returns a reader positioned after the version.
pub(crate) fn exact_header(bytes: &[u8], ty: u8, len: usize) -> Result<Reader<'_>, DecodeError> {
    check_type_version(bytes, ty)?;
    if bytes.len() != len {
        return Err(DecodeError::WrongLength {
            expected: len,
            found: bytes.len(),
        });
    }
    let mut r = Reader::new(bytes);
    r.array::<2>()?;
    Ok(r)
}
