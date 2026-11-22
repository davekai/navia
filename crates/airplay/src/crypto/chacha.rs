//! ChaCha20-Poly1305 AEAD encryption for control channel and audio.

use crate::base::error::CryptoError;
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use rand::{rngs::OsRng, RngCore};
use zeroize::ZeroizeOnDrop;

/// control channel cipher with an auto-incrementing nonce; both directions' ciphers are cached.
#[derive(ZeroizeOnDrop)]
pub struct ControlCipher {
    write_key: [u8; 32],
    read_key: [u8; 32],
    #[zeroize(skip)]
    write_cipher: ChaCha20Poly1305,
    #[zeroize(skip)]
    read_cipher: ChaCha20Poly1305,
    #[zeroize(skip)]
    encrypt_counter: u64,
    #[zeroize(skip)]
    decrypt_counter: u64,
}

/// audio packet cipher with an explicit nonce, cached rather than rebuilt per packet.
#[derive(ZeroizeOnDrop)]
pub struct AudioCipher {
    key: [u8; 32],
    #[zeroize(skip)]
    cipher: ChaCha20Poly1305,
}

impl ControlCipher {
    pub fn new(write_key: [u8; 32], read_key: [u8; 32]) -> Self {
        let write_cipher = ChaCha20Poly1305::new(&write_key.into());
        let read_cipher = ChaCha20Poly1305::new(&read_key.into());
        Self {
            write_key,
            read_key,
            write_cipher,
            read_cipher,
            encrypt_counter: 0,
            decrypt_counter: 0,
        }
    }

    pub fn new_unidirectional(key: [u8; 32]) -> Self {
        Self::new(key, key)
    }

    /// HomeKit framing: each block is [u16_le len][ciphertext][16-byte tag], with AAD=len.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        const MAX_BLOCK: usize = 0x400;
        if plaintext.is_empty() {
            return Err(CryptoError::Encryption("Empty plaintext".to_string()));
        }

        let mut out = Vec::with_capacity(plaintext.len() + (plaintext.len() / MAX_BLOCK + 1) * 18);
        let mut offset = 0;
        while offset < plaintext.len() {
            let remaining = plaintext.len() - offset;
            let block_len = remaining.min(MAX_BLOCK) as u16;
            let block = &plaintext[offset..offset + block_len as usize];
            let aad = block_len.to_le_bytes();

            let nonce = build_nonce_from_counter(self.encrypt_counter);
            let nonce = Nonce::from_slice(&nonce);
            let payload = Payload { msg: block, aad: &aad };

            let ciphertext_with_tag = self.write_cipher
                .encrypt(nonce, payload)
                .map_err(|e| CryptoError::Encryption(format!("Encryption failed: {}", e)))?;

            out.extend_from_slice(&aad);
            out.extend_from_slice(&ciphertext_with_tag);

            self.encrypt_counter += 1;
            offset += block_len as usize;
        }

        Ok(out)
    }

    /// Decrypt a single HomeKit-framed block.
    pub fn decrypt_block(&mut self, ciphertext_with_tag: &[u8], block_len: u16) -> Result<Vec<u8>, CryptoError> {
        if ciphertext_with_tag.len() < block_len as usize + 16 {
            return Err(CryptoError::Decryption(
                "Ciphertext block too short".to_string(),
            ));
        }

        let aad = block_len.to_le_bytes();
        let nonce = build_nonce_from_counter(self.decrypt_counter);
        let nonce = Nonce::from_slice(&nonce);
        let payload = Payload {
            msg: ciphertext_with_tag,
            aad: &aad,
        };

        let plaintext = self.read_cipher
            .decrypt(nonce, payload)
            .map_err(|_| CryptoError::Decryption("Decryption/authentication failed".to_string()))?;

        self.decrypt_counter += 1;
        Ok(plaintext)
    }

    /// Decrypt a raw ciphertext block (ciphertext+tag) using AAD=len.
    pub fn decrypt_raw(&mut self, ciphertext_with_tag: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if ciphertext_with_tag.len() < 16 {
            return Err(CryptoError::Decryption(
                "Ciphertext too short (missing tag)".to_string(),
            ));
        }

        let block_len = (ciphertext_with_tag.len() - 16) as u16;
        self.decrypt_block(ciphertext_with_tag, block_len)
    }

    /// decrypt HomeKit-framed data: [u16_le len][ciphertext][16-byte tag] per block.
    pub fn decrypt(&mut self, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if data.len() < 18 {
            return Err(CryptoError::Decryption(
                "Data too short for HomeKit frame".to_string(),
            ));
        }

        let mut out = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            if offset + 2 > data.len() {
                return Err(CryptoError::Decryption(
                    "Incomplete length prefix".to_string(),
                ));
            }

            let block_len = u16::from_le_bytes([data[offset], data[offset + 1]]);
            offset += 2;

            let block_end = offset + block_len as usize + 16;
            if block_end > data.len() {
                return Err(CryptoError::Decryption(
                    "Incomplete ciphertext block".to_string(),
                ));
            }

            let ciphertext_with_tag = &data[offset..block_end];
            let plaintext = self.decrypt_block(ciphertext_with_tag, block_len)?;
            out.extend_from_slice(&plaintext);

            offset = block_end;
        }

        Ok(out)
    }

    pub fn encrypt_counter(&self) -> u64 {
        self.encrypt_counter
    }

    pub fn decrypt_counter(&self) -> u64 {
        self.decrypt_counter
    }

    pub fn reset_counters(&mut self) {
        self.encrypt_counter = 0;
        self.decrypt_counter = 0;
    }
}

impl AudioCipher {
    /// Create cipher with 32-byte shared key (from SETUP 'shk').
    pub fn new(key: [u8; 32]) -> Self {
        let cipher = ChaCha20Poly1305::new(&key.into());
        Self { key, cipher }
    }

    /// encrypt with a random nonce; AAD is the RTP timestamp and SSRC.
    /// Returns (ciphertext, 8-byte nonce, 16-byte tag).
    pub fn encrypt(
        &self,
        audio_data: &[u8],
        rtp_timestamp: u32,
        ssrc: u32,
    ) -> Result<(Vec<u8>, [u8; 8], [u8; 16]), CryptoError> {
        let mut nonce_8 = [0u8; 8];
        OsRng.fill_bytes(&mut nonce_8);
        self.encrypt_with_nonce(audio_data, rtp_timestamp, ssrc, nonce_8)
    }

    /// encrypt with the RTP sequence number as the nonce: [0, 0, 0, 0, seq_lo, seq_hi, 0, 0, 0, 0, 0, 0],
    /// the u16 in host (LE) order. only nonce[4..12] is sent
    /// in the packet trailer. returns (ciphertext, 8-byte nonce, 16-byte tag).
    pub fn encrypt_with_seq(
        &self,
        audio_data: &[u8],
        rtp_timestamp: u32,
        ssrc: u32,
        seqnum: u16,
    ) -> Result<(Vec<u8>, [u8; 8], [u8; 16]), CryptoError> {
        let mut nonce_12 = [0u8; 12];
        nonce_12[4..6].copy_from_slice(&seqnum.to_le_bytes());
        let nonce = Nonce::from_slice(&nonce_12);

        let aad = build_aad(rtp_timestamp, ssrc);

        let payload = Payload {
            msg: audio_data,
            aad: &aad,
        };

        let mut ciphertext_with_tag = self.cipher
            .encrypt(nonce, payload)
            .map_err(|e| CryptoError::Encryption(format!("Encryption failed: {}", e)))?;

        let tag_start = ciphertext_with_tag.len() - 16;
        let tag = ciphertext_with_tag.split_off(tag_start);
        let mut tag_array = [0u8; 16];
        tag_array.copy_from_slice(&tag);

        // Packet nonce is bytes 4-11 of the 12-byte nonce
        let mut nonce_8 = [0u8; 8];
        nonce_8.copy_from_slice(&nonce_12[4..12]);

        Ok((ciphertext_with_tag, nonce_8, tag_array))
    }

    fn encrypt_with_nonce(
        &self,
        audio_data: &[u8],
        rtp_timestamp: u32,
        ssrc: u32,
        nonce_8: [u8; 8],
    ) -> Result<(Vec<u8>, [u8; 8], [u8; 16]), CryptoError> {
        let nonce_12 = build_nonce_12(&nonce_8);
        let nonce = Nonce::from_slice(&nonce_12);

        let aad = build_aad(rtp_timestamp, ssrc);

        let payload = Payload {
            msg: audio_data,
            aad: &aad,
        };

        let mut ciphertext_with_tag = self.cipher
            .encrypt(nonce, payload)
            .map_err(|e| CryptoError::Encryption(format!("Encryption failed: {}", e)))?;

        let tag_start = ciphertext_with_tag.len() - 16;
        let tag = ciphertext_with_tag.split_off(tag_start);
        let mut tag_array = [0u8; 16];
        tag_array.copy_from_slice(&tag);

        Ok((ciphertext_with_tag, nonce_8, tag_array))
    }

    /// Decrypt audio data with explicit nonce and AAD.
    pub fn decrypt(
        &self,
        ciphertext: &[u8],
        nonce: &[u8; 8],
        tag: &[u8; 16],
        rtp_timestamp: u32,
        ssrc: u32,
    ) -> Result<Vec<u8>, CryptoError> {
        let nonce_12 = build_nonce_12(nonce);
        let nonce = Nonce::from_slice(&nonce_12);

        let aad = build_aad(rtp_timestamp, ssrc);

        let mut ciphertext_with_tag = ciphertext.to_vec();
        ciphertext_with_tag.extend_from_slice(tag);

        let payload = Payload {
            msg: &ciphertext_with_tag,
            aad: &aad,
        };

        self.cipher
            .decrypt(nonce, payload)
            .map_err(|_| CryptoError::Decryption("Decryption/authentication failed".to_string()))
    }
}

/// the 8-byte packet nonce goes at bytes 4..12; bytes 0..4 are zero.
fn build_nonce_12(nonce_8: &[u8; 8]) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[4..12].copy_from_slice(nonce_8);
    nonce
}

/// the counter goes at bytes 4..12, little-endian; bytes 0..4 are zero.
fn build_nonce_from_counter(counter: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[4..12].copy_from_slice(&counter.to_le_bytes());
    nonce
}

/// RTP timestamp then SSRC, big-endian.
fn build_aad(rtp_timestamp: u32, ssrc: u32) -> [u8; 8] {
    let mut aad = [0u8; 8];
    aad[0..4].copy_from_slice(&rtp_timestamp.to_be_bytes());
    aad[4..8].copy_from_slice(&ssrc.to_be_bytes());
    aad
}

/// encrypt with an explicit 12-byte nonce (pairing); the 16-byte tag is appended.
pub fn encrypt_with_nonce(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|e| CryptoError::Encryption(format!("Invalid key: {}", e)))?;

    let nonce = Nonce::from_slice(nonce);

    cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| CryptoError::Encryption(format!("Encryption failed: {}", e)))
}

/// decrypt with an explicit 12-byte nonce (pairing); expects the 16-byte tag appended.
pub fn decrypt_with_nonce(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    if ciphertext.len() < 16 {
        return Err(CryptoError::Decryption(
            "Ciphertext too short (missing tag)".to_string(),
        ));
    }

    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|e| CryptoError::Decryption(format!("Invalid key: {}", e)))?;

    let nonce = Nonce::from_slice(nonce);

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| CryptoError::Decryption("Decryption/authentication failed".to_string()))
}

/// `s` right-aligned in 12 zero bytes: "PV-Msg02" becomes `\x00\x00\x00\x00PV-Msg02`.
pub fn nonce_from_string(s: &[u8]) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    let len = s.len().min(12);
    let start = 12 - len;
    nonce[start..].copy_from_slice(&s[..len]);
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    mod control_cipher {
        use super::*;

        #[test]
        fn new_starts_counters_at_zero() {
            let cipher = ControlCipher::new_unidirectional([0u8; 32]);
            assert_eq!(cipher.encrypt_counter(), 0);
            assert_eq!(cipher.decrypt_counter(), 0);
        }

        #[test]
        fn encrypt_increments_counter() {
            let mut cipher = ControlCipher::new_unidirectional([0u8; 32]);
            assert_eq!(cipher.encrypt_counter(), 0);

            let _ = cipher.encrypt(b"test").unwrap();
            assert_eq!(cipher.encrypt_counter(), 1);

            let _ = cipher.encrypt(b"test2").unwrap();
            assert_eq!(cipher.encrypt_counter(), 2);
        }

        #[test]
        fn decrypt_increments_counter() {
            let key = [0x42u8; 32];
            let mut encrypt_cipher = ControlCipher::new_unidirectional(key);
            let mut decrypt_cipher = ControlCipher::new_unidirectional(key);

            assert_eq!(decrypt_cipher.decrypt_counter(), 0);

            let ciphertext = encrypt_cipher.encrypt(b"test").unwrap();
            let _ = decrypt_cipher.decrypt(&ciphertext).unwrap();
            assert_eq!(decrypt_cipher.decrypt_counter(), 1);

            let ciphertext2 = encrypt_cipher.encrypt(b"test2").unwrap();
            let _ = decrypt_cipher.decrypt(&ciphertext2).unwrap();
            assert_eq!(decrypt_cipher.decrypt_counter(), 2);
        }

        #[test]
        fn encrypt_produces_homekit_framed_output() {
            let mut cipher = ControlCipher::new_unidirectional([0u8; 32]);
            let plaintext = b"hello";
            let ciphertext = cipher.encrypt(plaintext).unwrap();
            // HomeKit frame: 2-byte length prefix + ciphertext + 16-byte tag
            assert_eq!(ciphertext.len(), 2 + plaintext.len() + 16);
            let len = u16::from_le_bytes([ciphertext[0], ciphertext[1]]);
            assert_eq!(len, plaintext.len() as u16);
        }

        #[test]
        fn decrypt_roundtrip() {
            let key = [0x42u8; 32];
            let mut encrypt_cipher = ControlCipher::new_unidirectional(key);
            let mut decrypt_cipher = ControlCipher::new_unidirectional(key);

            let plaintext = b"Hello, AirPlay!";
            let ciphertext = encrypt_cipher.encrypt(plaintext).unwrap();
            let decrypted = decrypt_cipher.decrypt(&ciphertext).unwrap();

            assert_eq!(decrypted, plaintext);
        }

        #[test]
        fn decrypt_fails_with_wrong_key() {
            let mut encrypt_cipher = ControlCipher::new_unidirectional([0x42u8; 32]);
            let mut decrypt_cipher = ControlCipher::new_unidirectional([0x43u8; 32]); // Different key

            let ciphertext = encrypt_cipher.encrypt(b"secret").unwrap();
            let result = decrypt_cipher.decrypt(&ciphertext);

            assert!(result.is_err());
        }

        #[test]
        fn decrypt_fails_with_tampered_ciphertext() {
            let key = [0x42u8; 32];
            let mut encrypt_cipher = ControlCipher::new_unidirectional(key);
            let mut decrypt_cipher = ControlCipher::new_unidirectional(key);

            let mut ciphertext = encrypt_cipher.encrypt(b"secret").unwrap();
            // Tamper with the ciphertext (not the tag)
            ciphertext[0] ^= 0xFF;

            let result = decrypt_cipher.decrypt(&ciphertext);
            assert!(result.is_err());
        }

        #[test]
        fn decrypt_fails_with_tampered_tag() {
            let key = [0x42u8; 32];
            let mut encrypt_cipher = ControlCipher::new_unidirectional(key);
            let mut decrypt_cipher = ControlCipher::new_unidirectional(key);

            let mut ciphertext = encrypt_cipher.encrypt(b"secret").unwrap();
            // Tamper with the last byte (in the tag)
            let len = ciphertext.len();
            ciphertext[len - 1] ^= 0xFF;

            let result = decrypt_cipher.decrypt(&ciphertext);
            assert!(result.is_err());
        }

        #[test]
        fn different_counters_produce_different_ciphertext() {
            let key = [0x42u8; 32];
            let mut cipher1 = ControlCipher::new_unidirectional(key);
            let mut cipher2 = ControlCipher::new_unidirectional(key);

            let ct1 = cipher1.encrypt(b"same").unwrap();

            // Advance cipher2's counter
            let _ = cipher2.encrypt(b"dummy").unwrap();
            let ct2 = cipher2.encrypt(b"same").unwrap();

            // Same plaintext, different nonces = different ciphertext
            assert_ne!(ct1, ct2);
        }

        #[test]
        fn reset_counters() {
            let mut cipher = ControlCipher::new_unidirectional([0u8; 32]);
            let _ = cipher.encrypt(b"test").unwrap();
            let _ = cipher.encrypt(b"test").unwrap();

            assert_eq!(cipher.encrypt_counter(), 2);

            cipher.reset_counters();
            assert_eq!(cipher.encrypt_counter(), 0);
            assert_eq!(cipher.decrypt_counter(), 0);
        }
    }

    mod audio_cipher {
        use super::*;

        #[test]
        fn encrypt_returns_ciphertext_nonce_tag() {
            let cipher = AudioCipher::new([0x42u8; 32]);
            let audio_data = vec![0xABu8; 1024];
            let timestamp = 12345u32;
            let ssrc = 0xDEADBEEFu32;

            let (ciphertext, nonce, tag) = cipher.encrypt(&audio_data, timestamp, ssrc).unwrap();

            assert_eq!(ciphertext.len(), audio_data.len());
            assert_eq!(nonce.len(), 8);
            assert_eq!(tag.len(), 16);
        }

        #[test]
        fn encrypt_generates_random_nonce() {
            let cipher = AudioCipher::new([0x42u8; 32]);
            let audio_data = vec![0xABu8; 100];

            let (_, nonce1, _) = cipher.encrypt(&audio_data, 1, 1).unwrap();
            let (_, nonce2, _) = cipher.encrypt(&audio_data, 1, 1).unwrap();

            // Nonces should be different (random)
            assert_ne!(nonce1, nonce2);
        }

        #[test]
        fn decrypt_roundtrip() {
            let cipher = AudioCipher::new([0x42u8; 32]);
            let audio_data = vec![0xABu8; 1024];
            let timestamp = 12345u32;
            let ssrc = 0xDEADBEEFu32;

            let (ciphertext, nonce, tag) = cipher.encrypt(&audio_data, timestamp, ssrc).unwrap();
            let decrypted = cipher
                .decrypt(&ciphertext, &nonce, &tag, timestamp, ssrc)
                .unwrap();

            assert_eq!(decrypted, audio_data);
        }

        #[test]
        fn decrypt_fails_with_wrong_nonce() {
            let cipher = AudioCipher::new([0x42u8; 32]);
            let audio_data = vec![0xABu8; 100];
            let timestamp = 12345u32;
            let ssrc = 0xDEADBEEFu32;

            let (ciphertext, _nonce, tag) = cipher.encrypt(&audio_data, timestamp, ssrc).unwrap();

            let wrong_nonce = [0xFFu8; 8];
            let result = cipher.decrypt(&ciphertext, &wrong_nonce, &tag, timestamp, ssrc);
            assert!(result.is_err());
        }

        #[test]
        fn decrypt_fails_with_wrong_timestamp() {
            let cipher = AudioCipher::new([0x42u8; 32]);
            let audio_data = vec![0xABu8; 100];
            let timestamp = 12345u32;
            let ssrc = 0xDEADBEEFu32;

            let (ciphertext, nonce, tag) = cipher.encrypt(&audio_data, timestamp, ssrc).unwrap();

            let wrong_timestamp = 99999u32;
            let result = cipher.decrypt(&ciphertext, &nonce, &tag, wrong_timestamp, ssrc);
            assert!(result.is_err());
        }

        #[test]
        fn decrypt_fails_with_wrong_ssrc() {
            let cipher = AudioCipher::new([0x42u8; 32]);
            let audio_data = vec![0xABu8; 100];
            let timestamp = 12345u32;
            let ssrc = 0xDEADBEEFu32;

            let (ciphertext, nonce, tag) = cipher.encrypt(&audio_data, timestamp, ssrc).unwrap();

            let wrong_ssrc = 0x12345678u32;
            let result = cipher.decrypt(&ciphertext, &nonce, &tag, timestamp, wrong_ssrc);
            assert!(result.is_err());
        }

        #[test]
        fn aad_is_timestamp_ssrc_big_endian() {
            let timestamp = 0x12345678u32;
            let ssrc = 0xDEADBEEFu32;

            let aad = build_aad(timestamp, ssrc);

            // Should be big-endian
            assert_eq!(aad[0..4], [0x12, 0x34, 0x56, 0x78]);
            assert_eq!(aad[4..8], [0xDE, 0xAD, 0xBE, 0xEF]);
        }
    }

    mod nonce_construction {
        use super::*;

        #[test]
        fn build_nonce_12_places_nonce_at_bytes_4_to_11() {
            let nonce_8 = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
            let nonce_12 = build_nonce_12(&nonce_8);

            // First 4 bytes should be zero (matching owntone format)
            assert_eq!(nonce_12[0..4], [0, 0, 0, 0]);
            assert_eq!(nonce_12[4..12], nonce_8);
        }

        #[test]
        fn build_nonce_from_counter_little_endian() {
            let counter = 0x0102030405060708u64;
            let nonce = build_nonce_from_counter(counter);

            // Counter in little-endian at bytes 4-11
            let expected_le = counter.to_le_bytes();
            assert_eq!(nonce[4..12], expected_le);
        }

        #[test]
        fn counter_in_bytes_4_through_11() {
            let counter = 1u64;
            let nonce = build_nonce_from_counter(counter);

            assert_eq!(nonce[0..4], [0, 0, 0, 0]);
            // Counter value (1) in little-endian at bytes 4-11
            assert_eq!(nonce[4], 1); // LSB
            assert_eq!(nonce[5..12], [0, 0, 0, 0, 0, 0, 0]);
        }

        #[test]
        fn nonce_from_string_right_aligned() {
            // HomeKit pair-verify nonces are right-aligned with zero padding
            let nonce = nonce_from_string(b"PV-Msg02");

            assert_eq!(&nonce[0..4], &[0, 0, 0, 0]);
            assert_eq!(&nonce[4..12], b"PV-Msg02");
        }

        #[test]
        fn nonce_from_string_pv_msg03() {
            let nonce = nonce_from_string(b"PV-Msg03");

            assert_eq!(&nonce[0..4], &[0, 0, 0, 0]);
            assert_eq!(&nonce[4..12], b"PV-Msg03");
        }

        #[test]
        fn nonce_from_string_ps_msg05() {
            // Pair-Setup M5/M6 nonces
            let nonce = nonce_from_string(b"PS-Msg05");

            assert_eq!(&nonce[0..4], &[0, 0, 0, 0]);
            assert_eq!(&nonce[4..12], b"PS-Msg05");
        }

        #[test]
        fn nonce_from_string_short_input() {
            // Short inputs get more zero padding
            let nonce = nonce_from_string(b"test");

            assert_eq!(&nonce[0..8], &[0, 0, 0, 0, 0, 0, 0, 0]);
            assert_eq!(&nonce[8..12], b"test");
        }

        #[test]
        fn nonce_from_string_full_12_bytes() {
            // Full 12-byte input has no padding
            let nonce = nonce_from_string(b"123456789012");
            assert_eq!(&nonce, b"123456789012");
        }
    }

    mod security {
        use super::*;

        #[test]
        fn key_zeroized_on_drop() {
            // ZeroizeOnDrop clears the key; this only checks that dropping works.
            let cipher = ControlCipher::new_unidirectional([0x42u8; 32]);
            drop(cipher);
        }

        #[test]
        fn constant_time_tag_comparison() {
            // the tag comparison is constant-time inside ChaCha20Poly1305; this checks authentication.
            let key = [0x42u8; 32];
            let mut encrypt_cipher = ControlCipher::new_unidirectional(key);
            let mut decrypt_cipher = ControlCipher::new_unidirectional(key);

            let ciphertext = encrypt_cipher.encrypt(b"test").unwrap();
            assert!(decrypt_cipher.decrypt(&ciphertext).is_ok());
        }
    }

    mod known_vectors {
        use super::*;

        #[test]
        fn rfc8439_aead_test_vector() {
            // RFC 8439 Section 2.8.2 - AEAD test vector
            let key =
                hex::decode("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f")
                    .unwrap();
            let nonce = hex::decode("070000004041424344454647").unwrap(); // 12 bytes
            let aad = hex::decode("50515253c0c1c2c3c4c5c6c7").unwrap();
            let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";

            let expected_ciphertext = hex::decode(
                "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3ff4def08e4b7a9de576d26586cec64b6116",
            ).unwrap();
            let expected_tag = hex::decode("1ae10b594f09e26a7e902ecbd0600691").unwrap();

            // Use ChaCha20Poly1305 directly for known vector test
            let cipher = ChaCha20Poly1305::new_from_slice(&key).unwrap();
            let nonce_arr = Nonce::from_slice(&nonce);

            let payload = Payload {
                msg: plaintext.as_slice(),
                aad: &aad,
            };

            let result = cipher.encrypt(nonce_arr, payload).unwrap();

            let ciphertext = &result[..result.len() - 16];
            let tag = &result[result.len() - 16..];

            assert_eq!(ciphertext, expected_ciphertext.as_slice());
            assert_eq!(tag, expected_tag.as_slice());
        }

        #[test]
        fn decrypt_rfc8439_vector() {
            // Verify we can decrypt the RFC 8439 test vector
            let key =
                hex::decode("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f")
                    .unwrap();
            let nonce = hex::decode("070000004041424344454647").unwrap();
            let aad = hex::decode("50515253c0c1c2c3c4c5c6c7").unwrap();
            let ciphertext = hex::decode(
                "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3ff4def08e4b7a9de576d26586cec64b6116",
            ).unwrap();
            let tag = hex::decode("1ae10b594f09e26a7e902ecbd0600691").unwrap();

            let expected_plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";

            let cipher = ChaCha20Poly1305::new_from_slice(&key).unwrap();
            let nonce_arr = Nonce::from_slice(&nonce);

            let mut ciphertext_with_tag = ciphertext.clone();
            ciphertext_with_tag.extend_from_slice(&tag);

            let payload = Payload {
                msg: &ciphertext_with_tag,
                aad: &aad,
            };

            let result = cipher.decrypt(nonce_arr, payload).unwrap();
            assert_eq!(result, expected_plaintext);
        }
    }
}
