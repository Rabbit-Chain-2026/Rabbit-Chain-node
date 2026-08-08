//! Wallet command implementation (native ed25519 only)
//! with encrypted private keys and short-lived unlock sessions.

use crate::Result;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use chrono::Utc;
use ed25519_dalek::{Signer as _, Verifier as _};
use pbkdf2::pbkdf2_hmac;
use rabbitcore::crypto::keccak256;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sha3::{Digest, Keccak256};
use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const PBKDF2_ITERATIONS: u32 = 120_000;
const KEY_LEN: usize = 32;

#[derive(Debug, Clone)]
pub enum WalletCommand {
    New {
        name: Option<String>,
        scheme: WalletScheme,
        passphrase: Option<String>,
    },
    BatchNew {
        name_prefix: String,
        count: usize,
        scheme: WalletScheme,
        passphrase: Option<String>,
        addresses_out: Option<String>,
    },
    List,
    Show {
        name: String,
    },
    Sign {
        name: String,
        message: String,
        passphrase: Option<String>,
    },
    Verify {
        name: String,
        message: String,
        signature_hex: String,
    },
    Delete {
        name: String,
    },
    RotatePassphrase {
        name: String,
        old_passphrase: Option<String>,
        new_passphrase: Option<String>,
    },
    Unlock {
        name: String,
        passphrase: Option<String>,
        ttl_secs: u64,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WalletScheme {
    Ed25519,
}

#[derive(Debug, Serialize, Deserialize)]
struct WalletFile {
    version: u32,
    created_at: String,
    default: Option<String>,
    accounts: Vec<WalletAccount>,
}

impl Default for WalletFile {
    fn default() -> Self {
        Self {
            version: 2,
            created_at: Utc::now().to_rfc3339(),
            default: None,
            accounts: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct WalletAccount {
    id: String,
    name: String,
    scheme: WalletScheme,
    created_at: String,
    public_key_hex: String,
    encrypted_private_key: EncryptedSecret,
    address: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct EncryptedSecret {
    kdf: String,
    iterations: u32,
    salt_hex: String,
    nonce_hex: String,
    ciphertext_hex: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
struct SessionFile {
    sessions: Vec<UnlockedSession>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct UnlockedSession {
    account_name: String,
    expires_unix_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_token_hash_hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encrypted_secret: Option<EncryptedSecret>,
}

#[derive(Debug, Clone)]
pub(crate) struct WalletSigningContext {
    pub account_name: String,
    pub scheme: WalletScheme,
    pub address: Option<String>,
    pub secret: [u8; 32],
}

pub async fn handle_wallet(data_dir: &str, cmd: WalletCommand) -> Result<()> {
    let path = wallet_file_path(data_dir);
    let _wallet_lock = if wallet_command_mutates_wallet_file(&cmd) {
        Some(acquire_wallet_lock(&path)?)
    } else {
        None
    };

    let mut wallet = load_wallet_file(&path)?;

    match cmd {
        WalletCommand::New {
            name,
            scheme,
            passphrase,
        } => {
            let passphrase = resolve_new_passphrase(passphrase, "Wallet passphrase")?;
            let account_name = name.unwrap_or_else(|| default_name(scheme, wallet.accounts.len()));
            if wallet.accounts.iter().any(|a| a.name == account_name) {
                anyhow::bail!("wallet account already exists: {}", account_name);
            }

            let account = match scheme {
                WalletScheme::Ed25519 => new_ed25519_account(account_name, &passphrase)?,
            };

            if wallet.default.is_none() {
                wallet.default = Some(account.name.clone());
            }
            print_account(&account);
            wallet.accounts.push(account);
            save_wallet_file(&path, &wallet)?;
            println!("✅ wallet saved: {}", path.display());
        }
        WalletCommand::BatchNew {
            name_prefix,
            count,
            scheme,
            passphrase,
            addresses_out,
        } => {
            if count == 0 {
                anyhow::bail!("count must be greater than 0");
            }
            let passphrase = resolve_new_passphrase(passphrase, "Wallet passphrase")?;
            let mut existing_names: std::collections::HashSet<String> =
                wallet.accounts.iter().map(|a| a.name.clone()).collect();
            let mut next_suffix = 1usize;
            let mut new_accounts = Vec::with_capacity(count);

            while new_accounts.len() < count {
                let candidate = format!("{name_prefix}-{next_suffix}");
                next_suffix = next_suffix.saturating_add(1);
                if !existing_names.insert(candidate.clone()) {
                    continue;
                }

                let account = match scheme {
                    WalletScheme::Ed25519 => new_ed25519_account(candidate, &passphrase)?,
                };
                new_accounts.push(account);
            }

            if wallet.default.is_none() {
                wallet.default = new_accounts.first().map(|a| a.name.clone());
            }
            let new_addresses: Vec<String> = new_accounts
                .iter()
                .filter_map(|account| account.address.clone())
                .collect();
            wallet.accounts.extend(new_accounts);
            save_wallet_file(&path, &wallet)?;

            if let Some(addresses_out) = addresses_out {
                let addresses_path = PathBuf::from(addresses_out);
                let mut content = new_addresses.join("\n");
                content.push('\n');
                write_secret_file(&addresses_path, content.as_bytes()).map_err(|e| {
                    anyhow::anyhow!(
                        "failed to write addresses file {}: {}",
                        addresses_path.display(),
                        e
                    )
                })?;
                println!("✅ address list saved: {}", addresses_path.display());
            }

            println!(
                "✅ created {} wallet accounts with prefix '{}' in {}",
                count,
                name_prefix,
                path.display()
            );
        }
        WalletCommand::List => {
            if wallet.accounts.is_empty() {
                println!("No wallet accounts found.");
                return Ok(());
            }
            println!("Wallet file: {}", path.display());
            for a in &wallet.accounts {
                let default_mark = if wallet.default.as_deref() == Some(a.name.as_str()) {
                    " (default)"
                } else {
                    ""
                };
                println!(
                    "- {} [{}]{}\n  pubkey: {}\n  address: {}",
                    a.name,
                    scheme_name(a.scheme),
                    default_mark,
                    a.public_key_hex,
                    a.address.clone().unwrap_or_else(|| "n/a".to_string())
                );
            }
        }
        WalletCommand::Show { name } => {
            let account = wallet
                .accounts
                .iter()
                .find(|a| a.name == name)
                .ok_or_else(|| anyhow::anyhow!("wallet account not found: {}", name))?;
            print_account(account);
        }
        WalletCommand::Sign {
            name,
            message,
            passphrase,
        } => {
            let account = find_account(&wallet, &name)?;
            let secret = decrypt_or_session(data_dir, account, passphrase.as_deref())?;
            let msg_bytes = message.as_bytes();

            match account.scheme {
                WalletScheme::Ed25519 => {
                    let signing = ed25519_dalek::SigningKey::from_bytes(&secret);
                    let sig = signing.sign(msg_bytes);
                    println!("scheme: ed25519");
                    println!("message: {}", message);
                    println!("signature_hex: 0x{}", hex::encode(sig.to_bytes()));
                }
            }
        }
        WalletCommand::Verify {
            name,
            message,
            signature_hex,
        } => {
            let account = find_account(&wallet, &name)?;
            let msg_bytes = message.as_bytes();
            let sig_bytes = parse_hex(signature_hex)?;

            match account.scheme {
                WalletScheme::Ed25519 => {
                    if sig_bytes.len() != 64 {
                        anyhow::bail!("ed25519 signature must be 64 bytes");
                    }
                    let pub_bytes = parse_fixed_32_hex(&account.public_key_hex)?;
                    let pubkey = ed25519_dalek::VerifyingKey::from_bytes(&pub_bytes)
                        .map_err(|e| anyhow::anyhow!("invalid ed25519 public key: {e}"))?;
                    let sig = ed25519_dalek::Signature::from_slice(&sig_bytes)
                        .map_err(|e| anyhow::anyhow!("invalid ed25519 signature: {e}"))?;
                    match pubkey.verify(msg_bytes, &sig) {
                        Ok(_) => println!("✅ verify ok (ed25519)"),
                        Err(_) => println!("❌ verify failed (ed25519)"),
                    }
                }
            }
        }
        WalletCommand::Delete { name } => {
            let before = wallet.accounts.len();
            wallet.accounts.retain(|a| a.name != name);
            if wallet.accounts.len() == before {
                anyhow::bail!("wallet account not found: {}", name);
            }
            if wallet.default.as_deref() == Some(name.as_str()) {
                wallet.default = wallet.accounts.first().map(|a| a.name.clone());
            }
            save_wallet_file(&path, &wallet)?;
            let mut sessions = load_session_file(&session_file_path(data_dir))?;
            sessions.sessions.retain(|s| s.account_name != name);
            save_session_file(&session_file_path(data_dir), &sessions)?;
            println!("✅ deleted wallet account: {}", name);
        }
        WalletCommand::RotatePassphrase {
            name,
            old_passphrase,
            new_passphrase,
        } => {
            let old_passphrase =
                resolve_existing_passphrase(old_passphrase, "Current wallet passphrase")?;
            let new_passphrase = resolve_new_passphrase(new_passphrase, "New wallet passphrase")?;
            let account = wallet
                .accounts
                .iter_mut()
                .find(|a| a.name == name)
                .ok_or_else(|| anyhow::anyhow!("wallet account not found: {}", name))?;
            let secret = decrypt_secret(&account.encrypted_private_key, &old_passphrase)?;
            account.encrypted_private_key = encrypt_secret(&secret, &new_passphrase)?;
            save_wallet_file(&path, &wallet)?;
            println!("✅ passphrase rotated for account: {}", name);
        }
        WalletCommand::Unlock {
            name,
            passphrase,
            ttl_secs,
        } => {
            let passphrase = resolve_existing_passphrase(passphrase, "Wallet passphrase")?;
            let account = find_account(&wallet, &name)?;
            let secret = decrypt_secret(&account.encrypted_private_key, &passphrase)?;
            let session_token = save_unlocked_session(data_dir, &name, &secret, ttl_secs)?;
            let env_name = wallet_unlock_env_name(&name);
            println!("✅ account unlocked: {} (ttl={}s)", name, ttl_secs);
            println!(
                "Set shell session token to enable signing without passphrase:\n  export {}={}",
                env_name, session_token
            );
        }
    }

    Ok(())
}

pub(crate) fn load_signing_context(
    data_dir: &str,
    account_name: Option<&str>,
    address: Option<&str>,
    passphrase: Option<&str>,
) -> Result<WalletSigningContext> {
    let wallet = load_wallet_file(&wallet_file_path(data_dir))?;
    let account = resolve_account_for_signing(&wallet, account_name, address)?;
    let secret = decrypt_or_session(data_dir, account, passphrase)?;
    Ok(WalletSigningContext {
        account_name: account.name.clone(),
        scheme: account.scheme,
        address: account.address.clone(),
        secret,
    })
}

fn find_account<'a>(wallet: &'a WalletFile, name: &str) -> Result<&'a WalletAccount> {
    wallet
        .accounts
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| anyhow::anyhow!("wallet account not found: {}", name))
}

fn resolve_account_for_signing<'a>(
    wallet: &'a WalletFile,
    account_name: Option<&str>,
    address: Option<&str>,
) -> Result<&'a WalletAccount> {
    if let Some(name) = account_name {
        return find_account(wallet, name);
    }

    if let Some(address) = address {
        let normalized = normalize_address(address);
        return wallet
            .accounts
            .iter()
            .find(|account| {
                account.address.as_deref().map(normalize_address).as_deref()
                    == Some(normalized.as_str())
            })
            .ok_or_else(|| anyhow::anyhow!("wallet account not found by address: {}", address));
    }

    if let Some(default_name) = wallet.default.as_deref() {
        if let Ok(account) = find_account(wallet, default_name) {
            return Ok(account);
        }
    }

    wallet
        .accounts
        .first()
        .ok_or_else(|| anyhow::anyhow!("wallet account not found"))
}

fn ensure_passphrase_strength(passphrase: &str) -> Result<()> {
    if passphrase.len() < 10 {
        anyhow::bail!("passphrase too short: at least 10 characters required");
    }
    Ok(())
}

fn resolve_new_passphrase(passphrase: Option<String>, prompt: &str) -> Result<String> {
    if let Some(passphrase) = passphrase {
        ensure_passphrase_strength(&passphrase)?;
        return Ok(passphrase);
    }

    ensure_interactive_stdin(prompt)?;
    let passphrase = rpassword::prompt_password(format!("{prompt}: "))
        .map_err(|e| anyhow::anyhow!("failed to read passphrase: {e}"))?;
    ensure_passphrase_strength(&passphrase)?;
    let confirmation = rpassword::prompt_password(format!("Confirm {prompt}: "))
        .map_err(|e| anyhow::anyhow!("failed to read passphrase confirmation: {e}"))?;
    if passphrase != confirmation {
        anyhow::bail!("passphrase confirmation does not match");
    }
    Ok(passphrase)
}

fn resolve_existing_passphrase(passphrase: Option<String>, prompt: &str) -> Result<String> {
    if let Some(passphrase) = passphrase {
        return Ok(passphrase);
    }

    prompt_existing_passphrase(prompt)
}

fn prompt_existing_passphrase(prompt: &str) -> Result<String> {
    ensure_interactive_stdin(prompt)?;
    rpassword::prompt_password(format!("{prompt}: "))
        .map_err(|e| anyhow::anyhow!("failed to read passphrase: {e}"))
}

fn ensure_interactive_stdin(action: &str) -> Result<()> {
    if !io::stdin().is_terminal() {
        anyhow::bail!(
            "{} requires an interactive terminal when --passphrase is omitted",
            action
        );
    }
    Ok(())
}

fn new_ed25519_account(name: String, passphrase: &str) -> Result<WalletAccount> {
    let signing = ed25519_dalek::SigningKey::generate(&mut OsRng);
    let verify = signing.verifying_key();

    let public = verify.to_bytes();
    let address = native_address_from_public_key(&public);
    let encrypted_private_key = encrypt_secret(&signing.to_bytes(), passphrase)?;

    Ok(WalletAccount {
        id: Uuid::new_v4().to_string(),
        name,
        scheme: WalletScheme::Ed25519,
        created_at: Utc::now().to_rfc3339(),
        public_key_hex: hex::encode(public),
        encrypted_private_key,
        address: Some(format_rabbit_native_address(address)),
    })
}

fn derive_key(passphrase: &str, salt: &[u8], iterations: u32) -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), salt, iterations, &mut key);
    key
}

fn encrypt_secret(secret: &[u8; 32], passphrase: &str) -> Result<EncryptedSecret> {
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);

    let key = derive_key(passphrase, &salt, PBKDF2_ITERATIONS);
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| anyhow::anyhow!("failed to initialize cipher: {e}"))?;
    let nonce_ga = Nonce::from(nonce);
    let ciphertext = cipher
        .encrypt(&nonce_ga, secret.as_ref())
        .map_err(|_| anyhow::anyhow!("encryption failed"))?;

    Ok(EncryptedSecret {
        kdf: "pbkdf2-sha256".to_string(),
        iterations: PBKDF2_ITERATIONS,
        salt_hex: hex::encode(salt),
        nonce_hex: hex::encode(nonce),
        ciphertext_hex: hex::encode(ciphertext),
    })
}

fn decrypt_secret(secret: &EncryptedSecret, passphrase: &str) -> Result<[u8; 32]> {
    if secret.kdf != "pbkdf2-sha256" {
        anyhow::bail!("unsupported kdf: {}", secret.kdf);
    }
    let salt = parse_hex(secret.salt_hex.clone())?;
    let nonce = parse_hex(secret.nonce_hex.clone())?;
    let ciphertext = parse_hex(secret.ciphertext_hex.clone())?;
    if nonce.len() != 12 {
        anyhow::bail!("invalid nonce length");
    }

    let key = derive_key(passphrase, &salt, secret.iterations);
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| anyhow::anyhow!("failed to initialize cipher: {e}"))?;
    let nonce_arr: [u8; 12] = nonce
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid nonce length"))?;
    let nonce_ga = Nonce::from(nonce_arr);
    let plaintext = cipher
        .decrypt(&nonce_ga, ciphertext.as_ref())
        .map_err(|_| anyhow::anyhow!("invalid passphrase or corrupted wallet entry"))?;
    if plaintext.len() != 32 {
        anyhow::bail!("invalid decrypted key length");
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&plaintext);
    Ok(out)
}

fn decrypt_or_session(
    data_dir: &str,
    account: &WalletAccount,
    passphrase: Option<&str>,
) -> Result<[u8; 32]> {
    if let Some(p) = passphrase {
        return decrypt_secret(&account.encrypted_private_key, p);
    }
    match load_unlocked_session(data_dir, &account.name) {
        Ok(secret) => Ok(secret),
        Err(session_err) => {
            if !io::stdin().is_terminal() {
                anyhow::bail!(
                    "{}; pass --passphrase in non-interactive mode or run wallet unlock first",
                    session_err
                );
            }
            let passphrase =
                prompt_existing_passphrase(&format!("Wallet passphrase for {}", account.name))?;
            decrypt_secret(&account.encrypted_private_key, &passphrase)
        }
    }
}

fn native_address_from_public_key(pubkey: &[u8; 32]) -> [u8; 20] {
    let mut hasher = Keccak256::new();
    hasher.update(pubkey);
    let out: [u8; 32] = hasher.finalize().into();
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&out[12..]);
    addr
}

fn format_rabbit_native_address(address: [u8; 20]) -> String {
    let lower_hex = hex::encode(address);
    let hash = keccak256(lower_hex.as_bytes());

    let mut checksummed = String::with_capacity(40);
    for (idx, ch) in lower_hex.chars().enumerate() {
        let nibble = if idx % 2 == 0 {
            (hash[idx / 2] >> 4) & 0x0f
        } else {
            hash[idx / 2] & 0x0f
        };

        if ch.is_ascii_hexdigit() && ch.is_ascii_lowercase() && nibble >= 8 {
            checksummed.push(ch.to_ascii_uppercase());
        } else {
            checksummed.push(ch);
        }
    }

    format!("0x{}", checksummed)
}

fn wallet_file_path(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("wallet.json")
}

fn session_file_path(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("wallet_sessions.json")
}

fn load_wallet_file(path: &Path) -> Result<WalletFile> {
    if !path.exists() {
        return Ok(WalletFile::default());
    }
    let content = fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read wallet file {}: {}", path.display(), e))?;
    let mut v: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse wallet file {}: {}", path.display(), e))?;

    let wallet: WalletFile = serde_json::from_value(v)
        .map_err(|e| anyhow::anyhow!("failed to decode wallet file {}: {}", path.display(), e))?;
    Ok(wallet)
}

fn save_wallet_file(path: &Path, wallet: &WalletFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            anyhow::anyhow!("failed to create wallet dir {}: {}", parent.display(), e)
        })?;
    }
    let json = serde_json::to_string_pretty(wallet)
        .map_err(|e| anyhow::anyhow!("failed to encode wallet file: {}", e))?;
    write_secret_file(path, json.as_bytes())
        .map_err(|e| anyhow::anyhow!("failed to write wallet file {}: {}", path.display(), e))?;
    Ok(())
}

fn load_session_file(path: &Path) -> Result<SessionFile> {
    if !path.exists() {
        return Ok(SessionFile::default());
    }
    let content = fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read session file {}: {}", path.display(), e))?;
    let mut session: SessionFile = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse session file {}: {}", path.display(), e))?;
    prune_expired_sessions(&mut session);
    Ok(session)
}

fn save_session_file(path: &Path, session: &SessionFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            anyhow::anyhow!("failed to create session dir {}: {}", parent.display(), e)
        })?;
    }
    let json = serde_json::to_string_pretty(session)
        .map_err(|e| anyhow::anyhow!("failed to encode session file: {}", e))?;
    write_secret_file(path, json.as_bytes())
        .map_err(|e| anyhow::anyhow!("failed to write session file {}: {}", path.display(), e))?;
    Ok(())
}

fn write_secret_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "secret".into());
    let tmp_path = parent.join(format!(
        ".{}.tmp.{}.{}",
        file_name,
        std::process::id(),
        Uuid::new_v4()
    ));

    let result = write_secret_file_atomic(path, &tmp_path, bytes);
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

fn write_secret_file_atomic(path: &Path, tmp_path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);

    #[cfg(unix)]
    {
        options.mode(0o600);
    }

    let mut file = options.open(tmp_path)?;
    #[cfg(unix)]
    {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);

    fs::rename(tmp_path, path)?;

    #[cfg(unix)]
    {
        if let Some(parent) = path.parent() {
            if let Ok(dir) = OpenOptions::new().read(true).open(parent) {
                let _ = dir.sync_all();
            }
        }
    }

    Ok(())
}

fn wallet_command_mutates_wallet_file(cmd: &WalletCommand) -> bool {
    matches!(
        cmd,
        WalletCommand::New { .. }
            | WalletCommand::BatchNew { .. }
            | WalletCommand::Delete { .. }
            | WalletCommand::RotatePassphrase { .. }
    )
}

#[derive(Debug)]
struct WalletFileLock {
    path: PathBuf,
}

impl Drop for WalletFileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn acquire_wallet_lock(wallet_path: &Path) -> Result<WalletFileLock> {
    if let Some(parent) = wallet_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            anyhow::anyhow!("failed to create wallet dir {}: {}", parent.display(), e)
        })?;
    }

    let lock_path = wallet_lock_path(wallet_path);
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);

    #[cfg(unix)]
    {
        options.mode(0o600);
    }

    match options.open(&lock_path) {
        Ok(mut file) => {
            writeln!(
                file,
                "pid={}\ncreated_unix_secs={}",
                std::process::id(),
                now_unix_secs()
            )
            .map_err(|e| anyhow::anyhow!("failed to write wallet lock: {e}"))?;
            file.sync_all()
                .map_err(|e| anyhow::anyhow!("failed to sync wallet lock: {e}"))?;
            Ok(WalletFileLock { path: lock_path })
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            anyhow::bail!(
                "wallet file is locked by another process: {}. Retry later, or remove the lock only after confirming no wallet command is running.",
                lock_path.display()
            )
        }
        Err(e) => Err(anyhow::anyhow!(
            "failed to acquire wallet lock {}: {}",
            lock_path.display(),
            e
        )),
    }
}

fn wallet_lock_path(wallet_path: &Path) -> PathBuf {
    let parent = wallet_path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = wallet_path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "wallet.json".into());
    parent.join(format!("{file_name}.lock"))
}

fn save_unlocked_session(
    data_dir: &str,
    name: &str,
    secret: &[u8; 32],
    ttl_secs: u64,
) -> Result<String> {
    let path = session_file_path(data_dir);
    let mut session_file = load_session_file(&path)?;
    prune_expired_sessions(&mut session_file);

    let session_token = generate_session_token();
    let token_bytes = parse_fixed_32_hex(&session_token)?;
    let token_hash_hex = hex::encode(keccak256(&token_bytes));
    let encrypted_secret = encrypt_secret(secret, &session_token)?;

    session_file.sessions.retain(|s| s.account_name != name);
    session_file.sessions.push(UnlockedSession {
        account_name: name.to_string(),
        expires_unix_secs: now_unix_secs().saturating_add(ttl_secs),
        session_token_hash_hex: Some(token_hash_hex),
        encrypted_secret: Some(encrypted_secret),
    });
    save_session_file(&path, &session_file)?;
    Ok(session_token)
}

fn load_unlocked_session(data_dir: &str, name: &str) -> Result<[u8; 32]> {
    let env_name = wallet_unlock_env_name(name);
    let env_secret = std::env::var(&env_name).map_err(|_| {
        anyhow::anyhow!(
            "account is locked; pass --passphrase, run wallet unlock in current shell, or export {}",
            env_name
        )
    })?;
    let token = parse_fixed_32_hex(&env_secret)
        .map_err(|_| anyhow::anyhow!("invalid unlocked session token in env: {}", env_name))?;

    let path = session_file_path(data_dir);
    let mut session_file = load_session_file(&path)?;
    prune_expired_sessions(&mut session_file);

    let sess = session_file
        .sessions
        .iter()
        .find(|s| s.account_name == name)
        .ok_or_else(|| {
            anyhow::anyhow!("account is locked; pass --passphrase or run wallet unlock")
        })?
        .clone();

    if let Some(token_hash_hex) = sess.session_token_hash_hex.as_deref() {
        let actual_token_hash = hex::encode(keccak256(&token));
        if actual_token_hash != token_hash_hex {
            anyhow::bail!("unlocked session token mismatch or stale env token");
        }
        let encrypted_secret = sess
            .encrypted_secret
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("corrupted unlock session: encrypted secret missing"))?;
        let secret = decrypt_secret(encrypted_secret, &env_secret).map_err(|_| {
            anyhow::anyhow!("failed to decrypt unlocked session secret, run wallet unlock again")
        })?;
        save_session_file(&path, &session_file)?;
        return Ok(secret);
    }

    anyhow::bail!("invalid unlock session; run wallet unlock again");
}

fn wallet_unlock_env_name(name: &str) -> String {
    format!(
        "RABBITCHAIN_WALLET_UNLOCK_{}",
        name.to_uppercase().replace('-', "_")
    )
}

fn generate_session_token() -> String {
    let mut token = [0u8; 32];
    OsRng.fill_bytes(&mut token);
    format!("0x{}", hex::encode(token))
}

fn prune_expired_sessions(session_file: &mut SessionFile) {
    let now = now_unix_secs();
    session_file.sessions.retain(|s| s.expires_unix_secs > now);
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn default_name(scheme: WalletScheme, idx: usize) -> String {
    match scheme {
        WalletScheme::Ed25519 => format!("native-{}", idx + 1),
    }
}

fn parse_hex(s: String) -> Result<Vec<u8>> {
    let raw = s.trim().strip_prefix("0x").unwrap_or(s.trim());
    hex::decode(raw).map_err(|e| anyhow::anyhow!("invalid hex: {}", e))
}

fn parse_fixed_32_hex(s: &str) -> Result<[u8; 32]> {
    let raw = s.trim().strip_prefix("0x").unwrap_or(s.trim());
    let bytes = hex::decode(raw).map_err(|e| anyhow::anyhow!("invalid hex: {}", e))?;
    if bytes.len() != 32 {
        anyhow::bail!("expected 32-byte key, got {} bytes", bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn normalize_address(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn print_account(a: &WalletAccount) {
    println!("name: {}", a.name);
    println!("scheme: {}", scheme_name(a.scheme));
    println!("public_key: 0x{}", a.public_key_hex);
    if let Some(addr) = &a.address {
        println!("address: {}", addr);
    }
    println!(
        "private_key: encrypted ({} iterations)",
        a.encrypted_private_key.iterations
    );
}

fn scheme_name(s: WalletScheme) -> &'static str {
    match s {
        WalletScheme::Ed25519 => "ed25519",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn env_name_for(account_name: &str) -> String {
        wallet_unlock_env_name(account_name)
    }

    #[test]
    fn unlock_session_token_roundtrip() {
        let temp = TempDir::new().expect("create tempdir");
        let data_dir = temp.path().to_string_lossy().to_string();
        let account_name = "native-token-test";
        let secret = [7u8; 32];

        let token = save_unlocked_session(&data_dir, account_name, &secret, 60).expect("save");
        let env_name = env_name_for(account_name);
        std::env::set_var(&env_name, token);

        let loaded = load_unlocked_session(&data_dir, account_name).expect("load");
        std::env::remove_var(&env_name);
        assert_eq!(loaded, secret);

        let session = load_session_file(&session_file_path(&data_dir)).expect("load session file");
        let entry = session
            .sessions
            .iter()
            .find(|item| item.account_name == account_name)
            .expect("session entry");
        assert!(entry.session_token_hash_hex.is_some());
        assert!(entry.encrypted_secret.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn saved_secret_files_are_owner_only() {
        let temp = TempDir::new().expect("create tempdir");
        let path = temp.path().join("secret.json");

        write_secret_file(&path, br#"{"secret":true}"#).expect("write secret file");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn wallet_new_preserves_accounts_and_rejects_duplicate_name() {
        let temp = TempDir::new().expect("create tempdir");
        let data_dir = temp.path().to_string_lossy().to_string();

        handle_wallet(
            &data_dir,
            WalletCommand::New {
                name: Some("miner-1".to_string()),
                scheme: WalletScheme::Ed25519,
                passphrase: Some("StrongPassphrase123!".to_string()),
            },
        )
        .await
        .expect("create first account");
        handle_wallet(
            &data_dir,
            WalletCommand::New {
                name: Some("miner-2".to_string()),
                scheme: WalletScheme::Ed25519,
                passphrase: Some("StrongPassphrase456!".to_string()),
            },
        )
        .await
        .expect("create second account");

        let path = wallet_file_path(&data_dir);
        let wallet = load_wallet_file(&path).expect("load wallet");
        assert_eq!(wallet.accounts.len(), 2);
        assert!(wallet.accounts.iter().any(|a| a.name == "miner-1"));
        assert!(wallet.accounts.iter().any(|a| a.name == "miner-2"));

        let before = fs::read_to_string(&path).expect("read before duplicate");
        let err = handle_wallet(
            &data_dir,
            WalletCommand::New {
                name: Some("miner-1".to_string()),
                scheme: WalletScheme::Ed25519,
                passphrase: Some("StrongPassphrase789!".to_string()),
            },
        )
        .await
        .expect_err("duplicate account should fail");
        assert!(err.to_string().contains("wallet account already exists"));
        let after = fs::read_to_string(&path).expect("read after duplicate");
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn wallet_batch_new_writes_addresses_file() {
        let temp = TempDir::new().expect("create tempdir");
        let data_dir = temp.path().to_string_lossy().to_string();
        let addresses_path = temp.path().join("coinbases.txt");

        handle_wallet(
            &data_dir,
            WalletCommand::BatchNew {
                name_prefix: "miner".to_string(),
                count: 3,
                scheme: WalletScheme::Ed25519,
                passphrase: Some("StrongPassphrase123!".to_string()),
                addresses_out: Some(addresses_path.to_string_lossy().to_string()),
            },
        )
        .await
        .expect("batch create accounts");

        let wallet = load_wallet_file(&wallet_file_path(&data_dir)).expect("load wallet");
        assert_eq!(wallet.accounts.len(), 3);
        assert!(wallet.accounts.iter().any(|a| a.name == "miner-1"));
        assert!(wallet.accounts.iter().any(|a| a.name == "miner-2"));
        assert!(wallet.accounts.iter().any(|a| a.name == "miner-3"));

        let addresses = fs::read_to_string(&addresses_path).expect("read addresses");
        let lines: Vec<_> = addresses.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.starts_with("0x")));
    }

    #[test]
    fn wallet_lock_rejects_concurrent_mutation() {
        let temp = TempDir::new().expect("create tempdir");
        let wallet_path = temp.path().join("wallet.json");
        let lock_path = wallet_lock_path(&wallet_path);

        let first_lock = acquire_wallet_lock(&wallet_path).expect("acquire first lock");
        let err = acquire_wallet_lock(&wallet_path).expect_err("second lock should fail");
        assert!(err.to_string().contains("wallet file is locked"));
        assert!(lock_path.exists());

        drop(first_lock);
        assert!(!lock_path.exists());
    }
}
