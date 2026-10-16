use crate::Result;

/// Protects small secrets (the device private key) at rest. Windows: DPAPI.
pub trait SecretProtector: Send + Sync {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>>;
    fn unprotect(&self, cipher: &[u8]) -> Result<Vec<u8>>;
}

/// Identity transform for tests and platforms without a secret store yet.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlainProtector;

impl SecretProtector for PlainProtector {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>> {
        Ok(plain.to_vec())
    }

    fn unprotect(&self, cipher: &[u8]) -> Result<Vec<u8>> {
        Ok(cipher.to_vec())
    }
}
