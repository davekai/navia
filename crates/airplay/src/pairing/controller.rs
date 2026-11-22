//! the controller's identity: an Ed25519 key pair and the UUID that pair-setup M5 and
//! pair-verify M3 both send.

use crate::crypto::ed25519::IdentityKeyPair;
use std::path::Path;

/// the same identifier must be sent in pair-setup M5 and pair-verify M3, or the device rejects
/// the pairing.
#[derive(Clone)]
pub struct ControllerIdentity {
    /// Stable UUID identifier (e.g., "12345678-ABCD-1234-ABCD-123456789ABC")
    id: String,
    /// Ed25519 keypair (LTSK/LTPK)
    keypair: IdentityKeyPair,
}

impl ControllerIdentity {
    /// a new key pair, with the UUID taken from the first 16 bytes of its public key.
    pub fn generate() -> Self {
        let keypair = IdentityKeyPair::generate();
        Self::from_keypair(keypair)
    }

    /// the UUID comes from the public key's bytes, as `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`.
    pub fn from_keypair(keypair: IdentityKeyPair) -> Self {
        let pk = keypair.public_key();
        let id = format!(
            "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            pk[0], pk[1], pk[2], pk[3],
            pk[4], pk[5],
            pk[6], pk[7],
            pk[8], pk[9],
            pk[10], pk[11], pk[12], pk[13], pk[14], pk[15]
        );
        Self { id, keypair }
    }

    /// with a given identifier, for interoperating with other implementations.
    pub fn with_id(keypair: IdentityKeyPair, id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            keypair,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// for TLV encoding.
    pub fn id_bytes(&self) -> Vec<u8> {
        self.id.as_bytes().to_vec()
    }

    pub fn keypair(&self) -> &IdentityKeyPair {
        &self.keypair
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.keypair.public_key()
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.keypair.sign(message)
    }

    /// file format: line 1 the UUID, line 2 the hex-encoded 32-byte Ed25519 seed.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let mut lines = contents.lines();

        let id = lines
            .next()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "Missing UUID"))?
            .to_string();

        let seed_hex = lines
            .next()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "Missing seed"))?;

        let seed_bytes = hex_decode(seed_hex).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("Invalid hex: {}", e))
        })?;

        if seed_bytes.len() != 32 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Seed must be 32 bytes, got {}", seed_bytes.len()),
            ));
        }

        let mut seed = [0u8; 32];
        seed.copy_from_slice(&seed_bytes);

        let keypair = IdentityKeyPair::from_seed(&seed);
        Ok(Self { id, keypair })
    }

    /// same format as [`Self::load`].
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let seed_hex = hex_encode(&self.keypair.seed());
        let contents = format!("{}\n{}\n", self.id, seed_hex);
        std::fs::write(path, contents)
    }

    pub fn load_or_generate(path: &Path) -> std::io::Result<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            let identity = Self::generate();
            identity.save(path)?;
            Ok(identity)
        }
    }
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("Odd number of hex characters".to_string());
    }

    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| format!("Invalid hex at position {}: {}", i, e))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn generate_creates_valid_identity() {
        let identity = ControllerIdentity::generate();

        // UUID should be 36 characters (8-4-4-4-12)
        assert_eq!(identity.id().len(), 36);
        assert!(identity.id().contains('-'));

        assert_eq!(identity.public_key().len(), 32);
    }

    #[test]
    fn from_keypair_generates_consistent_id() {
        let keypair = IdentityKeyPair::generate();
        let pk = keypair.public_key();

        let identity1 = ControllerIdentity::from_keypair(keypair.clone());
        let identity2 = ControllerIdentity::from_keypair(keypair);

        assert_eq!(identity1.id(), identity2.id());

        assert!(identity1.id().starts_with(&format!("{:02X}{:02X}", pk[0], pk[1])));
    }

    #[test]
    fn with_id_uses_custom_identifier() {
        let keypair = IdentityKeyPair::generate();
        let custom_id = "Custom-Controller-ID";

        let identity = ControllerIdentity::with_id(keypair, custom_id);

        assert_eq!(identity.id(), custom_id);
    }

    #[test]
    fn id_bytes_returns_utf8_bytes() {
        let identity = ControllerIdentity::generate();

        let id_bytes = identity.id_bytes();

        assert_eq!(id_bytes, identity.id().as_bytes());
    }

    #[test]
    fn sign_produces_valid_signature() {
        let identity = ControllerIdentity::generate();
        let message = b"test message";

        let signature = identity.sign(message);

        assert_eq!(signature.len(), 64);

        crate::crypto::ed25519::verify(&identity.public_key(), message, &signature).unwrap();
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("identity.txt");

        let original = ControllerIdentity::generate();
        original.save(&path).unwrap();

        let loaded = ControllerIdentity::load(&path).unwrap();

        assert_eq!(original.id(), loaded.id());
        assert_eq!(original.public_key(), loaded.public_key());
    }

    #[test]
    fn load_or_generate_creates_new_if_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("new_identity.txt");

        assert!(!path.exists());

        let identity = ControllerIdentity::load_or_generate(&path).unwrap();

        assert!(path.exists());
        assert_eq!(identity.id().len(), 36);
    }

    #[test]
    fn load_or_generate_loads_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("existing.txt");

        let original = ControllerIdentity::generate();
        original.save(&path).unwrap();

        let loaded = ControllerIdentity::load_or_generate(&path).unwrap();

        assert_eq!(original.id(), loaded.id());
        assert_eq!(original.public_key(), loaded.public_key());
    }
}
