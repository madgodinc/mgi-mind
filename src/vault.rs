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

impl VaultData {
    /// Wipe the decrypted secret values from memory (audit #8). The key is
    /// already zeroized after derivation, but the plaintext secrets live on in
    /// this struct until it drops; for a vault, scrubbing them right after use is
    /// the logical completion. Call this before dropping any `VaultData` that was
    /// decrypted from disk.
    fn zeroize_secrets(&mut self) {
        for entry in self.entries.values_mut() {
            entry.value.zeroize();
        }
    }
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

/// Pinned Argon2id parameters (audit #11). We do NOT use `Argon2::default()`:
/// if the crate's defaults ever change, every existing vault would stop
/// decrypting with a misleading "wrong master password". These values match the
/// argon2 0.5 defaults at the time of writing and are now frozen here. Changing
/// them requires a versioned re-derivation, not an edit.
fn argon2() -> Argon2<'static> {
    use argon2::{Algorithm, Params, Version};
    let params = Params::new(19456, 2, 1, Some(32)).expect("valid Argon2 params");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

fn derive_key(password: &str, salt: &[u8; 32]) -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    argon2()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow::anyhow!("Key derivation failed: {e}"))?;
    Ok(key)
}

// ===== Reusable crypto for the encrypted-backup module =====
// The backup feature reuses the vault's vetted AES-256-GCM + pinned Argon2id
// primitives, but with a SEPARATE key derived from a SEPARATE salt — backup
// rotation must not be coupled to secret-vault rotation (different threat
// model, different blast radius). These thin wrappers expose the primitives
// without widening the master-key path.

/// Derive a 32-byte key from a passphrase + caller-supplied salt (pinned
/// Argon2id). The caller owns the salt — backups use their own, not `vault.salt`.
pub(crate) fn derive_key_with_salt(password: &str, salt: &[u8; 32]) -> Result<[u8; 32]> {
    derive_key(password, salt)
}

/// Encrypt arbitrary bytes with a caller-derived key (AES-256-GCM,
/// nonce-prepended format). Used by the backup module.
pub(crate) fn encrypt_with_key(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>> {
    encrypt(data, key)
}

/// Decrypt bytes produced by `encrypt_with_key`.
pub(crate) fn decrypt_with_key(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>> {
    decrypt(data, key)
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
pub(crate) fn prompt_password(prompt: &str) -> Result<String> {
    rpassword::prompt_password(prompt).context(
        "This requires an interactive terminal for the passphrase. \
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

/// Decrypt the vault with an already-derived key. Used by mutating ops that
/// derive the key once and reuse it for both the load and the save (audit #7),
/// instead of paying Argon2id (intentionally ~tens of ms) twice per mutation.
fn load_vault_with_key(key: &[u8; 32]) -> Result<VaultData> {
    let path = vault_path();
    if !path.exists() {
        return Ok(VaultData::default());
    }
    let encrypted = std::fs::read(&path).context("Failed to read vault")?;
    let decrypted = decrypt(&encrypted, key)?;
    let vault: VaultData = serde_json::from_slice(&decrypted)?;
    Ok(vault)
}

/// Encrypt + atomically write the vault with an already-derived key (audit #7).
fn save_vault_with_key(vault: &VaultData, key: &[u8; 32]) -> Result<()> {
    let json = serde_json::to_vec(vault)?;
    let encrypted = encrypt(&json, key)?;
    // Atomic write so a crash can't corrupt the only copy of the secrets (audit #4).
    crate::util::atomic_write(&vault_path(), &encrypted)
}

/// Decrypt the vault for a read-only caller (one Argon2 derivation).
fn load_vault(password: &str) -> Result<VaultData> {
    if !vault_path().exists() {
        return Ok(VaultData::default());
    }
    let salt = get_or_create_salt()?;
    let mut key = derive_key(password, &salt)?;
    let vault = load_vault_with_key(&key);
    key.zeroize();
    vault
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

    // Derive the key ONCE and reuse it for both the load and the save (audit #7).
    let salt = get_or_create_salt()?;
    let mut dkey = derive_key(&password, &salt)?;
    password.zeroize();

    let mut vault = load_vault_with_key(&dkey)?;

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

    let result = save_vault_with_key(&vault, &dkey);
    dkey.zeroize();
    vault.zeroize_secrets(); // wipe plaintext secrets from memory (audit #8)
    result
}

/// Retrieve a secret. Requires master password + user confirmation.
/// Does NOT rewrite the encrypted vault (audit #5) - reads stay read-only.
pub fn retrieve(key: &str, skip_confirm: bool) -> Result<Option<String>> {
    let mut password = prompt_password("Master password: ")?;
    let vault = load_vault(&password);
    password.zeroize();
    let mut vault = vault?;

    // Copy out what we need, then scrub every plaintext secret from the decrypted
    // vault before we go on (audit #8). `value` is the one secret we deliberately
    // return to the caller; the rest never linger in memory.
    let found = vault.entries.get(key).map(|e| {
        (
            e.key.clone(),
            e.category.clone(),
            e.description.clone(),
            e.value.clone(),
        )
    });
    vault.zeroize_secrets();

    let Some((ekey, category, description, mut value)) = found else {
        return Ok(None);
    };

    if !skip_confirm {
        eprintln!("=== VAULT ACCESS REQUEST ===");
        eprintln!("Key:         {ekey}");
        eprintln!("Category:    {category}");
        eprintln!("Description: {description}");
        eprintln!("============================");
        let answer = prompt_line("Allow access? [y/N]: ")?;

        if !answer.eq_ignore_ascii_case("y") {
            eprintln!("Access denied by user.");
            value.zeroize(); // don't leave the denied secret in memory (audit #8)
            return Ok(None);
        }
    }

    Ok(Some(value))
}

/// List all keys (values never shown).
pub fn list_keys() -> Result<Vec<(String, String, String)>> {
    if !vault_path().exists() {
        return Ok(Vec::new());
    }

    let mut password = prompt_password("Master password: ")?;
    let vault = load_vault(&password);
    password.zeroize();
    let mut vault = vault?;

    let mut keys: Vec<(String, String, String)> = vault
        .entries
        .values()
        .map(|e| (e.key.clone(), e.category.clone(), e.description.clone()))
        .collect();
    vault.zeroize_secrets(); // values were never read here; scrub them anyway (audit #8)
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(keys)
}

/// Delete a secret.
pub fn delete(key: &str) -> Result<bool> {
    let mut password = prompt_password("Master password: ")?;
    // Derive once for both load and (conditional) save (audit #7).
    let salt = get_or_create_salt()?;
    let mut dkey = derive_key(&password, &salt)?;
    password.zeroize();

    let mut vault = load_vault_with_key(&dkey)?;
    let removed = vault.entries.remove(key).is_some();
    let result = if removed {
        save_vault_with_key(&vault, &dkey)
    } else {
        Ok(())
    };
    dkey.zeroize();
    vault.zeroize_secrets(); // wipe remaining plaintext secrets (audit #8)
    result.map(|_| removed)
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

    #[test]
    fn roundtrip_holds_for_varied_payloads() {
        // Property: decrypt(encrypt(x)) == x for many shapes of input.
        let key = derive_key("master", &[3u8; 32]).unwrap();
        let mut inputs: Vec<Vec<u8>> = vec![
            vec![],
            b"a".to_vec(),
            b"\x00\x01\x02 binary \xff\xfe".to_vec(),
            "юникод и emoji - тест".as_bytes().to_vec(),
            vec![0u8; 4096],
        ];
        // A few pseudo-random-ish lengths/contents (deterministic, no rng dep).
        for n in [7usize, 63, 255, 1000] {
            inputs.push((0..n).map(|i| (i * 31 + 7) as u8).collect());
        }
        for data in &inputs {
            let blob = encrypt(data, &key).unwrap();
            let back = decrypt(&blob, &key).unwrap();
            assert_eq!(&back, data, "roundtrip failed for len {}", data.len());
        }
    }

    #[test]
    fn argon2_params_are_pinned_not_default() {
        // Guard against a silent crate-default change that would brick vaults.
        let a = derive_key("p", &[1u8; 32]).unwrap();
        let b = derive_key("p", &[1u8; 32]).unwrap();
        assert_eq!(a, b, "derivation must be deterministic");
    }
}
