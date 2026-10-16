//! Browser-side SCRAM-SHA-256 password hashing for psql `\password`.
//!
//! Produces the PostgreSQL storage format:
//!
//!     SCRAM-SHA-256$<iterations>:<base64-salt>$<base64-StoredKey>:<base64-ServerKey>
//!
//! The hash is what's sent over the wire in
//! `ALTER USER … PASSWORD '<hash>'`. The plaintext password never
//! leaves the browser.
//!
//! Algorithm (RFC 5802 + PG storage format):
//!
//!     SaltedPassword = PBKDF2-HMAC-SHA256(password, salt, iter, 32)
//!     ClientKey      = HMAC-SHA256(SaltedPassword, "Client Key")
//!     ServerKey      = HMAC-SHA256(SaltedPassword, "Server Key")
//!     StoredKey      = SHA256(ClientKey)
//!
//! NOTE: SASLprep (RFC 4013) normalization is skipped. PG itself
//! falls back to raw UTF-8 when SASLprep fails (commit b1ee16a), so
//! a plain UTF-8 encoding remains valid for authentication on the
//! common ASCII / NFC-already-normalized cases. Non-ASCII passwords
//! that REQUIRE NFKC normalization to match what PG stored will fail
//! to authenticate; a follow-up can pull in `unicode-normalization`
//! if a user hits this.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// PG's default iteration count for SCRAM-SHA-256 (`scram_iterations`).
pub(crate) const DEFAULT_ITERATIONS: u32 = 4096;

/// Hash a UTF-8 password with a fresh random salt (16 bytes from
/// Web Crypto) and the default iteration count.
pub(crate) fn scram_sha256_hash(password: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt)
        .map_err(|e| format!("\\password: random salt generation failed: {}", e))?;
    Ok(scram_sha256_hash_with(password, &salt, DEFAULT_ITERATIONS))
}

/// Deterministic variant — caller supplies the salt + iteration
/// count. Used by tests and for vector-locked reproducibility. Not
/// used by the live `\password` path.
pub(crate) fn scram_sha256_hash_with(
    password:   &str,
    salt:       &[u8],
    iterations: u32,
) -> String {
    let mut salted = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, iterations, &mut salted);

    let client_key = hmac_sha256(&salted, b"Client Key");
    let server_key = hmac_sha256(&salted, b"Server Key");
    let stored_key = Sha256::digest(&client_key);

    let b64 = base64::engine::general_purpose::STANDARD;
    format!(
        "SCRAM-SHA-256${}:{}${}:{}",
        iterations,
        b64.encode(salt),
        b64.encode(stored_key),
        b64.encode(server_key),
    )
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_shape_is_pg_storage_format() {
        let salt = [0u8; 16];
        let hash = scram_sha256_hash_with("secret", &salt, 4096);
        assert!(hash.starts_with("SCRAM-SHA-256$4096:"));
        // Three sections separated by '$', and the middle + tail
        // each have one ':'.
        let parts: Vec<&str> = hash.split('$').collect();
        assert_eq!(parts.len(), 3, "expected SCRAM-SHA-256$<iter>:<salt>$<sk>:<svk>");
        assert_eq!(parts[0], "SCRAM-SHA-256");
        let iter_salt: Vec<&str> = parts[1].split(':').collect();
        assert_eq!(iter_salt.len(), 2);
        assert_eq!(iter_salt[0], "4096");
        let keys: Vec<&str> = parts[2].split(':').collect();
        assert_eq!(keys.len(), 2);
        // StoredKey + ServerKey are both SHA-256 outputs (32 bytes
        // → 44 chars base64 with padding).
        assert_eq!(keys[0].len(), 44);
        assert_eq!(keys[1].len(), 44);
    }

    #[test]
    fn deterministic_with_fixed_salt_and_iter() {
        let salt = [0xAB; 16];
        let a = scram_sha256_hash_with("hunter2", &salt, 4096);
        let b = scram_sha256_hash_with("hunter2", &salt, 4096);
        assert_eq!(a, b, "same inputs must produce the same hash");
    }

    #[test]
    fn salt_is_base64_decodable_and_16_bytes() {
        let salt = [0x42; 16];
        let hash = scram_sha256_hash_with("any", &salt, 4096);
        let b64 = base64::engine::general_purpose::STANDARD;
        // Extract the base64 salt segment between "SCRAM-SHA-256$<iter>:" and "$".
        let after_iter = hash.split_once(':').unwrap().1;
        let salt_b64 = after_iter.split_once('$').unwrap().0;
        let decoded = b64.decode(salt_b64).unwrap();
        assert_eq!(decoded.len(), 16);
        assert_eq!(decoded, salt);
    }

    #[test]
    fn different_iterations_produce_different_hashes() {
        let salt = [0u8; 16];
        let a = scram_sha256_hash_with("pw", &salt, 4096);
        let b = scram_sha256_hash_with("pw", &salt, 8192);
        assert_ne!(a, b);
    }

    #[test]
    fn different_salts_produce_different_hashes() {
        let a = scram_sha256_hash_with("pw", &[0u8; 16], 4096);
        let b = scram_sha256_hash_with("pw", &[1u8; 16], 4096);
        assert_ne!(a, b);
    }

    #[test]
    fn empty_password_still_produces_a_valid_hash() {
        let salt = [0u8; 16];
        let hash = scram_sha256_hash_with("", &salt, 4096);
        assert!(hash.starts_with("SCRAM-SHA-256$4096:"));
    }

    #[test]
    fn unicode_password_uses_utf8_bytes() {
        let salt = [0u8; 16];
        // Same bytes via raw vs. char source — utf-8 path is single.
        let h1 = scram_sha256_hash_with("p\u{00E9}ch\u{00E9}", &salt, 4096);
        let h2 = scram_sha256_hash_with("péché", &salt, 4096);
        assert_eq!(h1, h2);
    }

    #[test]
    fn known_vector_lock() {
        // Locked vector: password="password", salt=16 zero bytes,
        // iterations=4096. Cross-checked once against an independent
        // SCRAM-SHA-256 implementation (Python `passlib.crypto`) to
        // confirm the algorithm matches RFC 5802 + PG storage format;
        // pinned here so any future refactor that drifts the output
        // fails this test immediately.
        let salt = [0u8; 16];
        let hash = scram_sha256_hash_with("password", &salt, 4096);
        assert_eq!(
            hash,
            "SCRAM-SHA-256$4096:AAAAAAAAAAAAAAAAAAAAAA==$\
             D8ITpkywEF0Cmziv1x7miJwf+RAjwZmy1t/IrJ/6T74=:\
             W94Lu5QoXTNxFjBGrdBDUZWt0oGAzLsA243wkIheU18=",
            "captured vector drifted — verify algorithm before re-locking",
        );
    }
}
