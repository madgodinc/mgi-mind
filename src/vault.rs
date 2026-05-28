use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use argon2::Argon2;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

/// Secure vault for secrets (passwords, SSH keys, API tokens).
/// Encrypted with AES-256-GCM, key derived from master password via Argon2.
/// NOT stored in Qdrant - no vector search on secrets.

const VAULT_FILE: &str = "vault.enc";
const SALT_FILE: &str = "vault.salt";

#[derive(Debug, Serialize, Deserialize)]
pub struct VaultEntry {
    pub key: String,
    pub value: String,
    pub category: String,
    pub description: String,
    pub created_at: String,
    pub last_accessed: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct VaultData {
    pub entries: HashMap<String, VaultEntry>,
}

fn vault_path() -> PathBuf {
    crate::config::mind_home().join(VAULT_FILE)
}

fn salt_path() -> PathBuf {
    crate::config::mind_home().join(SALT_FILE)
}

fn get_or_create_salt() -> Result<[u8; 32]> {
    let path = salt_path();
    if path.exists() {
        let data = std::fs::read(&path).context("Failed to read vault salt")?;
        let mut salt = [0u8; 32];
        if data.len() >= 32 {
            salt.copy_from_slice(&data[..32]);
        } else {
            anyhow::bail!("Corrupt vault salt file");
        }
        Ok(salt)
    } else {
        let mut salt = [0u8; 32];
        OsRng.fill_bytes(&mut salt);
        std::fs::write(&path, &salt).context("Failed to write vault salt")?;
        Ok(salt)
    }
}

fn derive_key(password: &str, salt: &[u8; 32]) -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow::anyhow!("Key derivation failed: {e}"))?;
    Ok(key)
}

fn encrypt(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| anyhow::anyhow!("Cipher init failed: {e}"))?;

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, data)
        .map_err(|e| anyhow::anyhow!("Encryption failed: {e}"))?;

    // Format: nonce (12 bytes) + ciphertext
    let mut output = Vec::with_capacity(12 + ciphertext.len());
    output.extend_from_slice(&nonce_bytes);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

fn decrypt(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>> {
    if data.len() < 12 {
        anyhow::bail!("Vault data too short");
    }

    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| anyhow::anyhow!("Cipher init failed: {e}"))?;

    let nonce = Nonce::from_slice(&data[..12]);
    let ciphertext = &data[12..];

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!("Decryption failed - wrong master password?"))
}

fn prompt_password(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let mut password = String::new();
    std::io::stdin().read_line(&mut password)?;
    Ok(password.trim().to_string())
}

fn load_vault(password: &str) -> Result<VaultData> {
    let path = vault_path();
    if !path.exists() {
        return Ok(VaultData::default());
    }

    let salt = get_or_create_salt()?;
    let key = derive_key(password, &salt)?;
    let encrypted = std::fs::read(&path).context("Failed to read vault")?;
    let decrypted = decrypt(&encrypted, &key)?;
    let vault: VaultData = serde_json::from_slice(&decrypted)?;
    Ok(vault)
}

fn save_vault(vault: &VaultData, password: &str) -> Result<()> {
    let salt = get_or_create_salt()?;
    let key = derive_key(password, &salt)?;
    let json = serde_json::to_vec(vault)?;
    let encrypted = encrypt(&json, &key)?;
    std::fs::write(vault_path(), encrypted)?;
    Ok(())
}

pub fn is_vault_initialized() -> bool {
    salt_path().exists()
}

/// Store a secret.
pub fn store(key: &str, value: &str, category: &str, description: &str) -> Result<()> {
    let password = if is_vault_initialized() {
        prompt_password("Master password: ")?
    } else {
        let p = prompt_password("Set master password for vault: ")?;
        let confirm = prompt_password("Confirm master password: ")?;
        if p != confirm {
            anyhow::bail!("Passwords don't match");
        }
        p
    };

    let mut vault = load_vault(&password)?;

    vault.entries.insert(
        key.to_string(),
        VaultEntry {
            key: key.to_string(),
            value: value.to_string(),
            category: category.to_string(),
            description: description.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            last_accessed: None,
        },
    );

    save_vault(&vault, &password)?;
    update_count(&vault);
    Ok(())
}

/// Retrieve a secret. Requires master password + user confirmation.
pub fn retrieve(key: &str, skip_confirm: bool) -> Result<Option<String>> {
    let password = prompt_password("Master password: ")?;
    let mut vault = load_vault(&password)?;

    let entry = match vault.entries.get_mut(key) {
        Some(e) => e,
        None => return Ok(None),
    };

    if !skip_confirm {
        eprintln!("=== VAULT ACCESS REQUEST ===");
        eprintln!("Key:         {}", entry.key);
        eprintln!("Category:    {}", entry.category);
        eprintln!("Description: {}", entry.description);
        eprintln!("============================");
        let answer = prompt_password("Allow access? [y/N]: ")?;

        if !answer.eq_ignore_ascii_case("y") {
            eprintln!("Access denied by user.");
            return Ok(None);
        }
    }

    entry.last_accessed = Some(chrono::Utc::now().to_rfc3339());
    let value = entry.value.clone();
    save_vault(&vault, &password)?;

    Ok(Some(value))
}

/// List all keys (values never shown).
pub fn list_keys() -> Result<Vec<(String, String, String)>> {
    // List doesn't need decryption if vault doesn't exist
    if !vault_path().exists() {
        return Ok(Vec::new());
    }

    let password = prompt_password("Master password: ")?;
    let vault = load_vault(&password)?;

    let mut keys: Vec<(String, String, String)> = vault
        .entries
        .values()
        .map(|e| (e.key.clone(), e.category.clone(), e.description.clone()))
        .collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(keys)
}

/// Delete a secret.
pub fn delete(key: &str) -> Result<bool> {
    let password = prompt_password("Master password: ")?;
    let mut vault = load_vault(&password)?;
    let removed = vault.entries.remove(key).is_some();
    if removed {
        save_vault(&vault, &password)?;
        update_count(&vault);
    }
    Ok(removed)
}

/// Non-interactive list for stats (just returns count without decryption).
pub fn count() -> usize {
    if !vault_path().exists() {
        return 0;
    }
    // Can't count without decryption, return "unknown"
    // For stats, we store count in a separate unencrypted file
    let count_path = crate::config::mind_home().join("vault.count");
    std::fs::read_to_string(count_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Update the unencrypted entry count (called after store/delete).
fn update_count(vault: &VaultData) {
    let count_path = crate::config::mind_home().join("vault.count");
    let _ = std::fs::write(count_path, vault.entries.len().to_string());
}
