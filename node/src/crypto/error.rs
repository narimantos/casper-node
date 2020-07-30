use std::result;

use base64::DecodeError;
use ed25519_dalek::SignatureError;
use hex::FromHexError;
use pem::PemError;
use thiserror::Error;

use crate::utils::ReadFileError;

/// A specialized `std::result::Result` type for cryptographic errors.
pub type Result<T> = result::Result<T, Error>;

/// Cryptographic errors.
#[derive(Debug, Error)]
pub enum Error {
    /// Error resulting from creating or using asymmetric key types.
    #[error("asymmetric key error: {0}")]
    AsymmetricKey(String),
    /// Error resulting when decoding a type from a hex-encoded representation.
    #[error("parsing from hex: {0}")]
    FromHex(#[from] FromHexError),
    /// Error trying to read a private key.
    #[error("private key load failed: {0}")]
    PrivateKeyLoad(ReadFileError),
    /// Error resulting when decoding a type from a base64 representation.
    #[error("decoding error: {0}")]
    FromBase64(#[from] DecodeError),
    /// Pem format error.
    #[error("pem error: {0}")]
    FromPem(String),
}

impl From<SignatureError> for Error {
    fn from(signature_error: SignatureError) -> Self {
        Error::AsymmetricKey(signature_error.to_string())
    }
}

impl From<PemError> for Error {
    fn from(error: PemError) -> Self {
        Error::FromPem(error.to_string())
    }
}
