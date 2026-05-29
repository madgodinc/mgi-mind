use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, OsRng},
};
use anyhow::{Context, Result};
use argon2::Argon2;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use zeroize::Zeroize;

// Secure vault for secrets (passwords, SSH keys, API tokens).
// Encrypted with AES-256-GCM, key derived from master password via Argon2.
// NOT stored in Qdrant - no vector search on secrets.
//
// Reads (`retrieve`) never rewrite the encrypted blob (audit #5), the master
// password is read without echo (audit #3), all writes are atomic (audit #4),
// and there is no plaintext secret count on disk (audit #26).

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
        crate::util::atomic_write(&path, &salt).context("Failed to write vault salt")?;
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
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| anyhow::anyhow!("Cipher init failed: {e}"))?;

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

    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| anyhow::anyhow!("Cipher init failed: {e}"))?;

    let nonce = Nonce::from_slice(&data[..12]);
    let ciphertext = &data[12..];

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!("Decryption failed - wrong master password?"))
}

/// Read the master password without echoing it to the terminal (audit #3).
/// Requires a TTY; over a non-interactive channel (e.g. raw MCP) this errors
/// instead of silently using an empty password (audit #2).
fn prompt_password(prompt: &str) -> Result<String> {
    rpassword::prompt_password(prompt).context(
        "Vault requires an interactive terminal for the master password. \
         Run this command directly in a terminal, not through an automated channel.",
    )
}

/// Visible y/N confirmation (not a secret).
fn prompt_line(prompt: &str) -> Result<String> {
    use std::io::Write;
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(answer.trim().to_string())
}

fn load_vault(password: &str) -> Result<VaultData> {
    let path = vault_path();
    if !path.exists() {
        return Ok(VaultData::default());
    }

    let salt = get_or_create_salt()?;
    let mut key = derive_key(password, &salt)?;
    let encrypted = std::fs::read(&path).context("Failed to read vault")?;
    let decrypted = decrypt(&encrypted, &key);
    key.zeroize();
    let decrypted = decrypted?;
    let vault: VaultData = serde_json::from_slice(&decrypted)?;
    Ok(vault)
}

fn save_vault(vault: &VaultData, password: &str) -> Result<()> {
    let salt = get_or_create_salt()?;
    let mut key = derive_key(password, &salt)?;
    let json = serde_json::to_vec(vault)?;
    let encrypted = encrypt(&json, &key);
    key.zeroize();
    let encrypted = encrypted?;
    // Atomic write so a crash can't corrupt the only copy of the secrets (audit #4).
    crate::util::atomic_write(&vault_path(), &encrypted)
}

pub fn is_vault_initialized() -> bool {
    salt_path().exists()
}

/// Store a secret.
pub fn store(key: &str, value: &str, category: &str, description: &str) -> Result<()> {
    let mut password = if is_vault_initialized() {
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

    let result = save_vault(&vault, &password);
    password.zeroize();
    result
}

/// Retrieve a secret. Requires master password + user confirmation.
/// Does NOT rewrite the encrypted vault (audit #5) - reads stay read-only.
pub fn retrieve(key: &str, skip_confirm: bool) -> Result<Option<String>> {
    let mut password = prompt_password("Master password: ")?;
    let vault = load_vault(&password);
    password.zeroize();
    let vault = vault?;

    let entry = match vault.entries.get(key) {
        Some(e) => e,
        None => return Ok(None),
    };

    if !skip_confirm {
        eprintln!("=== VAULT ACCESS REQUEST ===");
        eprintln!("Key:         {}", entry.key);
        eprintln!("Category:    {}", entry.category);
        eprintln!("Description: {}", entry.description);
        eprintln!("============================");
        let answer = prompt_line("Allow access? [y/N]: ")?;

        if !answer.eq_ignore_ascii_case("y") {
            eprintln!("Access denied by user.");
            return Ok(None);
        }
    }

    Ok(Some(entry.value.clone()))
}

/// List all keys (values never shown).
pub fn list_keys() -> Result<Vec<(String, String, String)>> {
    if !vault_path().exists() {
        return Ok(Vec::new());
    }

    let mut password = prompt_password("Master password: ")?;
    let vault = load_vault(&password);
    password.zeroize();
    let vault = vault?;

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
    let mut password = prompt_password("Master password: ")?;
    let mut vault = load_vault(&password)?;
    let removed = vault.entries.remove(key).is_some();
    if removed {
        save_vault(&vault, &password)?;
    }
    password.zeroize();
    Ok(removed)
}

/// Lock state for display. We cannot know the secret count without the master
/// password, and we no longer keep a plaintext counter on disk (audit #26).
pub fn summary() -> &'static str {
    if is_vault_initialized() {
        "initialized (locked)"
    } else {
        "empty"
    }
}

#[cfg(test)]
mod tests {
    use super::{decrypt, derive_key, encrypt};

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let salt = [7u8; 32];
        let key = derive_key("correct horse battery staple", &salt).unwrap();
        let secret = b"super-secret-ssh-key";
        let blob = encrypt(secret, &key).unwrap();
        assert_ne!(
            &blob[12..],
            &secret[..],
            "ciphertext must differ from plaintext"
        );
        let back = decrypt(&blob, &key).unwrap();
        assert_eq!(back, secret);
    }

    #[test]
    fn wrong_password_fails_to_decrypt() {
        let salt = [7u8; 32];
        let good = derive_key("right", &salt).unwrap();
        let bad = derive_key("wrong", &salt).unwrap();
        let blob = encrypt(b"data", &good).unwrap();
        assert!(decrypt(&blob, &bad).is_err(), "wrong key must not decrypt");
    }
}
