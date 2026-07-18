use aes_gcm::aead::{Aead, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use mongodb::bson::{self, doc, DateTime, Document};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{AdminSettings, ConfigHistoryConfig};
use crate::error::{AppError, ConfigError};

const ALG: &str = "AES-256-GCM";
const DATA_KEY_AAD: &[u8] = b"logflayer-config-history-data-key";
const SECRET_AAD: &[u8] = b"logflayer-config-history-secret";
pub const SECRET_FIELDS: &[&str] = &[
    "anthropic_api_key",
    "slack_webhook_url",
    "webhook_url",
    "webhook_secret",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrappedDataKey {
    pub alg: String,
    pub key_id: String,
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedSecret {
    pub encrypted: bool,
    pub alg: String,
    pub key_id: String,
    pub nonce: String,
    pub ciphertext: String,
    pub wrapped_key: WrappedDataKey,
    pub fingerprint: String,
}

pub fn build_history_document(
    settings: &AdminSettings,
    config: &ConfigHistoryConfig,
    version: i64,
    reason: &str,
) -> Result<Document, AppError> {
    let mut settings_doc = bson::to_document(settings)
        .map_err(|e| AppError::Validation(format!("failed to serialize admin settings: {e}")))?;

    // These fields are never stored in history — they contain credentials that
    // would create circular dependency (master key encrypting itself) or are
    // better kept out of the audit log entirely.
    settings_doc.remove("config_history_master_key");
    settings_doc.remove("mongodb_uri");

    encrypt_secret_field(
        &mut settings_doc,
        "anthropic_api_key",
        settings.anthropic_api_key.as_deref(),
        config,
    )?;
    encrypt_secret_field(
        &mut settings_doc,
        "slack_webhook_url",
        settings.slack_webhook_url.as_deref(),
        config,
    )?;
    encrypt_secret_field(
        &mut settings_doc,
        "webhook_url",
        settings.webhook_url.as_deref(),
        config,
    )?;
    encrypt_secret_field(
        &mut settings_doc,
        "webhook_secret",
        settings.webhook_secret.as_deref(),
        config,
    )?;

    Ok(doc! {
        "version": version,
        "created_at": DateTime::now(),
        "created_by": "admin-ui",
        "source": "api",
        "reason": reason,
        "settings": settings_doc,
        "secret_fields": SECRET_FIELDS,
        "encryption": {
            "alg": ALG,
            "key_id": &config.key_id,
        },
    })
}

pub fn encrypt_secret(
    plaintext: &str,
    config: &ConfigHistoryConfig,
) -> Result<EncryptedSecret, AppError> {
    let master_key = parse_master_key(config)?;

    let mut data_key = [0_u8; 32];
    OsRng.fill_bytes(&mut data_key);

    let (secret_nonce, secret_ciphertext) =
        encrypt_with_key(&data_key, plaintext.as_bytes(), SECRET_AAD)?;
    let (wrapped_nonce, wrapped_ciphertext) =
        encrypt_with_key(&master_key, &data_key, DATA_KEY_AAD)?;

    Ok(EncryptedSecret {
        encrypted: true,
        alg: ALG.to_string(),
        key_id: config.key_id.clone(),
        nonce: secret_nonce,
        ciphertext: secret_ciphertext,
        wrapped_key: WrappedDataKey {
            alg: ALG.to_string(),
            key_id: config.key_id.clone(),
            nonce: wrapped_nonce,
            ciphertext: wrapped_ciphertext,
        },
        fingerprint: fingerprint_secret(plaintext),
    })
}

pub fn decrypt_secret(envelope: &EncryptedSecret, master_key: &str) -> Result<String, AppError> {
    let master_key = parse_master_key_value(master_key)?;
    let data_key = decrypt_with_key(
        &master_key,
        &envelope.wrapped_key.nonce,
        &envelope.wrapped_key.ciphertext,
        DATA_KEY_AAD,
    )?;

    if data_key.len() != 32 {
        return Err(AppError::Validation(
            "decrypted config history data key had invalid length".to_string(),
        ));
    }

    let mut data_key_bytes = [0_u8; 32];
    data_key_bytes.copy_from_slice(&data_key);

    let plaintext = decrypt_with_key(
        &data_key_bytes,
        &envelope.nonce,
        &envelope.ciphertext,
        SECRET_AAD,
    )?;

    String::from_utf8(plaintext).map_err(|e| {
        AppError::Validation(format!(
            "decrypted config history secret was not UTF-8: {e}"
        ))
    })
}

fn encrypt_secret_field(
    settings_doc: &mut Document,
    field: &str,
    value: Option<&str>,
    config: &ConfigHistoryConfig,
) -> Result<(), AppError> {
    if let Some(value) = value {
        let encrypted = encrypt_secret(value, config)?;
        let encrypted = bson::to_bson(&encrypted).map_err(|e| {
            AppError::Validation(format!("failed to serialize encrypted secret: {e}"))
        })?;
        settings_doc.insert(field, encrypted);
    }
    Ok(())
}

fn parse_master_key(config: &ConfigHistoryConfig) -> Result<[u8; 32], AppError> {
    let value = config.master_key.as_deref().ok_or_else(|| {
        AppError::Config(ConfigError::MissingVar(
            "CONFIG_HISTORY_MASTER_KEY".to_string(),
        ))
    })?;
    parse_master_key_value(value)
}

fn parse_master_key_value(value: &str) -> Result<[u8; 32], AppError> {
    let trimmed = value.trim();

    if let Ok(decoded) = BASE64.decode(trimmed) {
        if decoded.len() == 32 {
            let mut key = [0_u8; 32];
            key.copy_from_slice(&decoded);
            return Ok(key);
        }
    }

    if trimmed.as_bytes().len() == 32 {
        let mut key = [0_u8; 32];
        key.copy_from_slice(trimmed.as_bytes());
        return Ok(key);
    }

    Err(AppError::Config(ConfigError::InvalidVar(
        "CONFIG_HISTORY_MASTER_KEY".to_string(),
        "expected a base64-encoded 32-byte key or exactly 32 raw bytes".to_string(),
    )))
}

fn encrypt_with_key(
    key: &[u8; 32],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<(String, String), AppError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| {
        AppError::Validation(format!("failed to initialize config history cipher: {e}"))
    })?;

    let mut nonce = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce);

    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| AppError::Validation("failed to encrypt config history secret".to_string()))?;

    Ok((BASE64.encode(nonce), BASE64.encode(ciphertext)))
}

fn decrypt_with_key(
    key: &[u8; 32],
    nonce: &str,
    ciphertext: &str,
    aad: &[u8],
) -> Result<Vec<u8>, AppError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| {
        AppError::Validation(format!("failed to initialize config history cipher: {e}"))
    })?;
    let nonce = BASE64
        .decode(nonce)
        .map_err(|e| AppError::Validation(format!("invalid encrypted secret nonce: {e}")))?;
    let ciphertext = BASE64
        .decode(ciphertext)
        .map_err(|e| AppError::Validation(format!("invalid encrypted secret ciphertext: {e}")))?;

    cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad,
            },
        )
        .map_err(|_| AppError::Validation("failed to decrypt config history secret".to_string()))
}

fn fingerprint_secret(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("sha256:{}", &digest[..12])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history_config(master_key: &str) -> ConfigHistoryConfig {
        ConfigHistoryConfig {
            enabled: true,
            master_key: Some(master_key.to_string()),
            key_id: "test-key".to_string(),
            collection_name: "app_settings_history".to_string(),
        }
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = "0123456789abcdef0123456789abcdef";
        let config = history_config(key);
        let encrypted = encrypt_secret("sk-test-secret", &config).expect("encrypt");
        let decrypted = decrypt_secret(&encrypted, key).expect("decrypt");

        assert_eq!(decrypted, "sk-test-secret");
        assert_ne!(encrypted.ciphertext, "sk-test-secret");
        assert_eq!(encrypted.key_id, "test-key");
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let config = history_config("0123456789abcdef0123456789abcdef");
        let encrypted = encrypt_secret("sk-test-secret", &config).expect("encrypt");
        let result = decrypt_secret(&encrypted, "abcdef0123456789abcdef0123456789");

        assert!(result.is_err());
    }

    #[test]
    fn history_document_does_not_store_plaintext_secret() {
        let config = history_config("0123456789abcdef0123456789abcdef");
        let settings = AdminSettings {
            anthropic_api_key: Some("sk-test-secret".to_string()),
            slack_webhook_url: Some("https://hooks.slack.com/services/test-secret".to_string()),
            webhook_url: Some("https://example.com/hook/test-secret".to_string()),
            webhook_secret: Some("webhook-secret".to_string()),
            ..Default::default()
        };

        let document =
            build_history_document(&settings, &config, 1, "test save").expect("history doc");
        let rendered = format!("{document:?}");
        assert!(!rendered.contains("sk-test-secret"));
        assert!(!rendered.contains("hooks.slack.com/services/test-secret"));
        assert!(!rendered.contains("example.com/hook/test-secret"));
        assert!(!rendered.contains("webhook-secret"));

        let settings_doc = document.get_document("settings").expect("settings doc");
        let api_key = settings_doc
            .get_document("anthropic_api_key")
            .expect("encrypted api key");
        assert_eq!(api_key.get_bool("encrypted"), Ok(true));
    }

    #[test]
    fn invalid_master_key_is_rejected() {
        let config = history_config("too-short");
        let result = encrypt_secret("secret", &config);

        assert!(result.is_err());
    }
}
