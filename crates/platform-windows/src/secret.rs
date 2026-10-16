//! DPAPI-backed [`SecretProtector`] (current-user scope).
//!
//! `identity.bin` (the device private key) is encrypted with `CryptProtectData` so it is only
//! readable by the same Windows user on the same machine. A fixed application entropy is mixed
//! in so other programs running as the same user cannot trivially decrypt the blob via
//! `CryptUnprotectData` without knowing it (defence in depth only, the constant is public).

use deskbridge_platform::{PlatformError, Result, secret::SecretProtector};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

/// Application entropy mixed into every blob.
pub const DPAPI_ENTROPY: &[u8] = b"softwelop.deskbridge.identity";

/// DPAPI protector. `Default` uses [`DPAPI_ENTROPY`].
#[derive(Debug, Clone, Copy, Default)]
pub struct DpapiProtector;

impl DpapiProtector {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    // DPAPI rejects a null pointer even for zero-length data on some Windows versions, so an
    // empty slice is represented by a dangling-but-valid pointer from a static byte.
    static EMPTY: [u8; 1] = [0];
    let (ptr, len) = if data.is_empty() {
        (EMPTY.as_ptr(), 0)
    } else {
        (data.as_ptr(), data.len())
    };
    CRYPT_INTEGER_BLOB {
        cbData: len as u32,
        pbData: ptr.cast_mut(),
    }
}

fn os_error(what: &str, e: &windows::core::Error) -> PlatformError {
    PlatformError::Os {
        code: i64::from(e.code().0),
        msg: format!("{what}: {e}"),
    }
}

/// Copies the DPAPI-allocated output into a `Vec` and frees the OS buffer.
///
/// # Safety
/// `out` must have been filled by a successful `CryptProtectData`/`CryptUnprotectData` call.
unsafe fn take_output(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if out.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: pbData/cbData describe a LocalAlloc'ed buffer owned by us after the call.
    let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
    // SAFETY: the buffer was allocated by DPAPI with LocalAlloc and is freed exactly once.
    unsafe {
        let _ = LocalFree(HLOCAL(out.pbData.cast()));
    }
    v
}

impl SecretProtector for DpapiProtector {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>> {
        let input = blob(plain);
        let entropy = blob(DPAPI_ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: all blobs point to live memory for the duration of the call; `out` is written
        // by the API and released in `take_output`.
        unsafe {
            CryptProtectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .map_err(|e| os_error("CryptProtectData", &e))?;
            Ok(take_output(out))
        }
    }

    fn unprotect(&self, cipher: &[u8]) -> Result<Vec<u8>> {
        if cipher.is_empty() {
            return Err(PlatformError::other("DPAPI blob is empty"));
        }
        let input = blob(cipher);
        let entropy = blob(DPAPI_ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: see `protect`.
        unsafe {
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .map_err(|e| os_error("CryptUnprotectData", &e))?;
            Ok(take_output(out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let p = DpapiProtector::new();
        let secret = b"-----BEGIN PRIVATE KEY----- very secret \x00\x01\xff";
        let c = p.protect(secret).unwrap();
        assert_ne!(c.as_slice(), secret.as_slice());
        assert!(c.len() > secret.len());
        assert_eq!(p.unprotect(&c).unwrap(), secret);
    }

    #[test]
    fn two_encryptions_differ_but_both_decrypt() {
        let p = DpapiProtector::new();
        let a = p.protect(b"same").unwrap();
        let b = p.protect(b"same").unwrap();
        assert_ne!(a, b, "DPAPI uses a random salt");
        assert_eq!(p.unprotect(&a).unwrap(), b"same");
        assert_eq!(p.unprotect(&b).unwrap(), b"same");
    }

    #[test]
    fn empty_input() {
        let p = DpapiProtector::new();
        let c = p.protect(b"").unwrap();
        assert!(!c.is_empty());
        assert_eq!(p.unprotect(&c).unwrap(), b"");
        assert!(p.unprotect(b"").is_err());
    }

    #[test]
    fn tampering_is_detected() {
        let p = DpapiProtector::new();
        let mut c = p.protect(b"integrity matters").unwrap();
        let last = c.len() - 1;
        c[last] ^= 0x01;
        assert!(p.unprotect(&c).is_err());
        let mut c2 = p.protect(b"integrity matters").unwrap();
        let mid = c2.len() / 2;
        c2[mid] ^= 0x80;
        assert!(p.unprotect(&c2).is_err());
        assert!(p.unprotect(b"not a dpapi blob at all").is_err());
    }

    #[test]
    fn wrong_entropy_fails() {
        // A blob protected without our entropy must not decrypt through the protector.
        let plain = b"x";
        let input = blob(plain);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: valid blobs, output released via take_output.
        let c = unsafe {
            CryptProtectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .unwrap();
            take_output(out)
        };
        assert!(DpapiProtector::new().unprotect(&c).is_err());
    }
}
