mod encryption {
    use crate::core::security::crypto::{decrypt, encrypt};
    use base64::{Engine, engine::general_purpose};

    fn test_key() -> [u8; 32] {
        [0x42; 32] // Test key - all bytes set to 0x42
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = test_key();
        let original = "my_secret_password";

        let encrypted = encrypt(original, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();

        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_encrypt_produces_different_ciphertext() {
        let key = test_key();
        let value = "same_password";

        let encrypted1 = encrypt(value, &key).unwrap();
        let encrypted2 = encrypt(value, &key).unwrap();

        // Due to random nonce, ciphertexts should be different
        assert_ne!(encrypted1, encrypted2);

        // But both should decrypt to the same value
        assert_eq!(decrypt(&encrypted1, &key).unwrap(), value);
        assert_eq!(decrypt(&encrypted2, &key).unwrap(), value);
    }

    #[test]
    fn test_decrypt_with_wrong_key_fails() {
        let key1 = test_key();
        let mut key2 = test_key();
        key2[0] = 0xFF; // Change one byte

        let encrypted = encrypt("secret", &key1).unwrap();
        let result = decrypt(&encrypted, &key2);

        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_invalid_base64_fails() {
        let key = test_key();
        let result = decrypt("not_valid_base64!!!", &key);

        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_too_short_data_fails() {
        let key = test_key();
        // Create a base64 string that's too short (less than 12 bytes when decoded)
        let short_data = general_purpose::STANDARD.encode([0u8; 5]);
        let result = decrypt(&short_data, &key);

        assert!(result.is_err());
    }

    #[test]
    fn test_encrypt_empty_string() {
        let key = test_key();
        let encrypted = encrypt("", &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();

        assert_eq!("", decrypted);
    }

    #[test]
    fn test_encrypt_unicode_string() {
        let key = test_key();
        let original = "密码123!@#";

        let encrypted = encrypt(original, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();

        assert_eq!(original, decrypted);
    }
}

mod signatures {
    use crate::core::security::signing::signature_has_expired;

    #[test]
    fn zero_signature_expiry_never_expires() {
        assert!(!signature_has_expired(0));
    }

    #[test]
    fn past_positive_signature_expiry_expires() {
        assert!(signature_has_expired(chrono::Utc::now().timestamp() - 60));
    }
}
