//! Config-scoped managed bearer credentials.
//!
//! This module deliberately exposes no credential path or raw value to output
//! code. Storage delegates to the descriptor-relative private-state primitives
//! shared with durable sessions; values are bounded, owner-only, single-link
//! regular files and are atomically replaced.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

const NAMESPACE: &str = "credentials";
const MAX_GATEWAY_NAME_BYTES: usize = 96;
pub(crate) const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;
const SAFE_STORE_ERROR: &str = "managed credential store is unavailable or unsafe; verify that Kitsu private state is owned by the current user and contains no links";

fn credential_name(gateway: &str) -> Result<String, String> {
    let bytes = gateway.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_GATEWAY_NAME_BYTES {
        return Err(format!(
            "gateway name must be 1-{MAX_GATEWAY_NAME_BYTES} bytes for managed authentication"
        ));
    }
    let mut encoded = String::with_capacity(3 + bytes.len() * 2 + 6);
    encoded.push_str("v1-");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded.push_str(".token");
    Ok(encoded)
}

fn safe_store_error(_: String) -> String {
    SAFE_STORE_ERROR.to_string()
}

/// Internal path used only to prevent approved developer-command mounts from
/// intersecting the credential namespace. It is never included in CLI output.
pub(crate) fn directory_path(state_dir: &Path) -> PathBuf {
    state_dir.join(NAMESPACE)
}

pub(crate) fn decode(bytes: Vec<u8>) -> Result<Zeroizing<String>, String> {
    decode_zeroizing(Zeroizing::new(bytes))
}

pub(crate) fn decode_zeroizing(mut bytes: Zeroizing<Vec<u8>>) -> Result<Zeroizing<String>, String> {
    if bytes.is_empty() {
        return Err("managed credential is empty".to_string());
    }
    if bytes.len() > MAX_CREDENTIAL_BYTES {
        return Err("managed credential exceeds the 16 KiB safety limit".to_string());
    }
    if bytes
        .iter()
        .any(|byte| byte.is_ascii_whitespace() || *byte == 0)
    {
        return Err("managed credential contains whitespace or control bytes".to_string());
    }
    let text = String::from_utf8(std::mem::take(&mut *bytes))
        .map_err(|_| "managed credential is not valid UTF-8".to_string())?;
    Ok(Zeroizing::new(text))
}

pub(crate) fn load(state_dir: &Path, gateway: &str) -> Result<Option<Zeroizing<String>>, String> {
    let name = credential_name(gateway)?;
    let bytes = crate::session::read_private_value(
        state_dir,
        OsStr::new(NAMESPACE),
        OsStr::new(&name),
        MAX_CREDENTIAL_BYTES as u64,
        "managed credential",
    )
    .map_err(safe_store_error)?;
    bytes.map(decode).transpose()
}

pub(crate) fn store(state_dir: &Path, gateway: &str, token: &str) -> Result<(), String> {
    let name = credential_name(gateway)?;
    crate::session::write_private_value(
        state_dir,
        OsStr::new(NAMESPACE),
        OsStr::new(&name),
        token.as_bytes(),
        MAX_CREDENTIAL_BYTES as u64,
        "managed credential",
    )
    .map_err(safe_store_error)
}

pub(crate) fn remove(state_dir: &Path, gateway: &str) -> Result<bool, String> {
    let name = credential_name(gateway)?;
    crate::session::remove_private_value(
        state_dir,
        OsStr::new(NAMESPACE),
        OsStr::new(&name),
        MAX_CREDENTIAL_BYTES as u64,
        "managed credential",
    )
    .map_err(safe_store_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_encoding_is_exact_collision_free_and_filename_safe() {
        assert_eq!(credential_name("a").unwrap(), "v1-61.token");
        assert_eq!(credential_name("A").unwrap(), "v1-41.token");
        assert_eq!(credential_name("a/b").unwrap(), "v1-612f62.token");
        assert_ne!(
            credential_name("ab").unwrap(),
            credential_name("a-b").unwrap()
        );
        assert!(credential_name("").is_err());
        assert!(credential_name(&"x".repeat(MAX_GATEWAY_NAME_BYTES + 1)).is_err());
    }

    #[test]
    fn credential_decode_is_bounded_and_rejects_whitespace() {
        assert_eq!(
            decode(b"hf_example".to_vec()).unwrap().as_str(),
            "hf_example"
        );
        for invalid in [b"".as_slice(), b"a b", b"a\n", b"a\0b"] {
            assert!(decode(invalid.to_vec()).is_err());
        }
        assert!(decode(vec![b'x'; MAX_CREDENTIAL_BYTES + 1]).is_err());
    }
}
