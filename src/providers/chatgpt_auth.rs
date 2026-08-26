//! Finch-owned ChatGPT subscription authorization.
//!
//! The device and subscription transport endpoints in this module are not a
//! public OpenAI API contract. They are pinned to OpenAI Codex source commit
//! `3e4707b34b16e139fcb7ad11ab8445993b62bba1` (2026-08-25). Keep fixtures
//! synchronized with that source and fail closed when response shapes drift.

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use fs2::FileExt;
use futures::StreamExt;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const CHATGPT_PROTOCOL_REVISION: &str = "openai-codex@3e4707b34b16e139fcb7ad11ab8445993b62bba1";
const DEFAULT_AUTH_BASE_URL: &str = "https://auth.openai.com";
const DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEVICE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const REFRESH_SKEW_SECS: u64 = 120;
const MAX_AUTH_BODY_BYTES: usize = 64 * 1024;
const MAX_POLL_INTERVAL_SECS: u64 = 60;
const MAX_ACCOUNT_ID_BYTES: usize = 256;

/// Opaque config reference for Finch-owned ChatGPT credentials.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialRef(String);

impl CredentialRef {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() > 128
            || value.is_empty()
            || !value.starts_with("chatgpt:")
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_'))
        {
            bail!("Invalid ChatGPT credential reference");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CredentialRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialRef(<opaque>)")
    }
}

impl fmt::Display for CredentialRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<opaque ChatGPT credential>")
    }
}

/// Persisted OAuth credential. Debug intentionally reveals no claims or tokens.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub(crate) struct ChatGptTokens {
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) id_token: Option<String>,
    pub(crate) account_id: String,
    /// Normalized credential-sharing boundary. Model/profile names are never
    /// part of this identity.
    pub(crate) identity: CredentialIdentity,
    pub(crate) expires_at: u64,
    /// Durable random generation used for compare-and-swap. Unlike a counter,
    /// this cannot ABA when logout leaves a tombstone and the account is later
    /// recreated.
    pub(crate) generation: String,
    #[serde(default)]
    tombstone: bool,
    /// Durable crash marker set before a rotating remote issuance starts.
    #[serde(default)]
    mutation_pending: bool,
}

/// Account-level sharing identity for a named credential record.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct CredentialIdentity {
    /// Locally enforced endpoint/client boundary, not a JWT claim.
    pub authorization_endpoint: String,
    pub client_id: String,
    pub subscription_endpoint: String,
    /// Unverified observations used only for refresh continuity diagnostics.
    pub observed_subject: Option<String>,
    pub observed_issuer: Option<String>,
    pub observed_audiences: Vec<String>,
    pub observed_scopes: Vec<String>,
}

impl CredentialIdentity {
    fn tombstone() -> Self {
        Self {
            authorization_endpoint: String::new(),
            client_id: String::new(),
            subscription_endpoint: String::new(),
            observed_subject: None,
            observed_issuer: None,
            observed_audiences: Vec::new(),
            observed_scopes: Vec::new(),
        }
    }
}

impl fmt::Debug for CredentialIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialIdentity")
            .field("authorization_endpoint", &self.authorization_endpoint)
            .field("client_id", &"<redacted public client binding>")
            .field("subscription_endpoint", &self.subscription_endpoint)
            .field("observed_subject", &"<unverified redacted>")
            .field("observed_issuer", &"<unverified redacted>")
            .field("observed_audiences", &"<unverified redacted>")
            .field("observed_scopes", &"<unverified redacted>")
            .finish()
    }
}

impl fmt::Debug for ChatGptTokens {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatGptTokens")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .field("account_id", &"<redacted>")
            .field("identity", &self.identity)
            .field("expires_at", &self.expires_at)
            .field("generation", &"<redacted random generation>")
            .field("tombstone", &self.tombstone)
            .field("mutation_pending", &self.mutation_pending)
            .finish()
    }
}

impl ChatGptTokens {
    fn needs_refresh(&self) -> bool {
        self.expires_at <= unix_time().saturating_add(REFRESH_SKEW_SECS)
    }

    fn tombstone() -> Self {
        Self {
            access_token: String::new(),
            refresh_token: String::new(),
            id_token: None,
            account_id: String::new(),
            identity: CredentialIdentity::tombstone(),
            expires_at: 0,
            generation: new_generation(),
            tombstone: true,
            mutation_pending: false,
        }
    }
}

/// Injected secret persistence. Implementations must not log `secret`.
pub trait CredentialStore: Send + Sync {
    fn load(&self, reference: &CredentialRef) -> Result<Option<Vec<u8>>>;
    /// Acquire the durable per-record mutation lease. Callers hold this across
    /// remote refresh/revoke/issuance and the following local commit.
    fn acquire_mutation_lease(&self, reference: &CredentialRef) -> Result<CredentialMutationLease>;
    fn compare_and_swap(
        &self,
        reference: &CredentialRef,
        expected_generation: Option<&str>,
        secret: &[u8],
    ) -> Result<()>;
    /// Replace only an existing malformed record with a valid tombstone while
    /// the caller holds the mutation lease.
    fn replace_corrupt_with_tombstone(
        &self,
        reference: &CredentialRef,
        tombstone: &[u8],
    ) -> Result<()>;
}

/// Held cross-process credential mutation lock.
pub struct CredentialMutationLease {
    _file: File,
}

impl fmt::Debug for CredentialMutationLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialMutationLease(<locked>)")
    }
}

/// Fail-closed, atomic 0600 file credential store.
///
/// This is the portable fallback where no supported OS credential store is
/// available. Each opaque reference maps to a private file under `root`.
#[derive(Debug, Clone)]
pub struct FileCredentialStore {
    root: PathBuf,
}

impl FileCredentialStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, reference: &CredentialRef) -> PathBuf {
        self.root
            .join(format!("{}.json", reference.as_str().replace(':', "_")))
    }

    fn ensure_private_directory(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root).context("Failed to create credential directory")?;
        let metadata = std::fs::symlink_metadata(&self.root)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("Credential directory is not a real directory");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let parent = self
                .root
                .parent()
                .context("Credential directory has no parent")?;
            if metadata.uid() != std::fs::metadata(parent)?.uid() {
                bail!("Credential directory owner does not match its parent");
            }
            std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o700))
                .context("Failed to secure credential directory")?;
            let mode = std::fs::metadata(&self.root)?.permissions().mode() & 0o777;
            if mode != 0o700 {
                bail!("Credential directory permissions are not private");
            }
        }
        Ok(())
    }

    fn lock(&self) -> Result<File> {
        self.ensure_private_directory()?;
        let path = self.root.join(".lock");
        let file = open_private_lock_file(&path, &self.root)?;
        file.lock_exclusive()
            .context("Failed to lock credential store")?;
        Ok(file)
    }

    fn mutation_lock(&self, reference: &CredentialRef) -> Result<File> {
        self.ensure_private_directory()?;
        let path = self.root.join(format!(
            ".{}.mutation.lock",
            reference.as_str().replace(':', "_")
        ));
        let file = open_private_lock_file(&path, &self.root)?;
        file.lock_exclusive()
            .context("Failed to acquire credential mutation lease")?;
        Ok(file)
    }
}

impl CredentialStore for FileCredentialStore {
    fn load(&self, reference: &CredentialRef) -> Result<Option<Vec<u8>>> {
        let lock = self.lock()?;
        let path = self.path(reference);
        let result = read_private_file(&path)?;
        FileExt::unlock(&lock)?;
        Ok(result)
    }

    fn acquire_mutation_lease(&self, reference: &CredentialRef) -> Result<CredentialMutationLease> {
        Ok(CredentialMutationLease {
            _file: self.mutation_lock(reference)?,
        })
    }

    fn compare_and_swap(
        &self,
        reference: &CredentialRef,
        expected_generation: Option<&str>,
        secret: &[u8],
    ) -> Result<()> {
        if secret.len() > MAX_AUTH_BODY_BYTES {
            bail!("ChatGPT credential record exceeds the size limit");
        }
        let lock = self.lock()?;
        let path = self.path(reference);
        let actual_generation = match read_private_file(&path)? {
            Some(bytes) => {
                let bytes = zeroize::Zeroizing::new(bytes);
                Some(
                    serde_json::from_slice::<ChatGptTokens>(&bytes)
                        .context("Stored ChatGPT credential record is invalid")?
                        .generation
                        .clone(),
                )
            }
            None => None,
        };
        if actual_generation.as_deref() != expected_generation {
            bail!("ChatGPT credentials changed during mutation; retry safely");
        }
        write_private_file(&path, secret)?;
        FileExt::unlock(&lock)?;
        Ok(())
    }

    fn replace_corrupt_with_tombstone(
        &self,
        reference: &CredentialRef,
        tombstone: &[u8],
    ) -> Result<()> {
        let lock = self.lock()?;
        let path = self.path(reference);
        let current = zeroize::Zeroizing::new(
            read_private_file(&path)?.context("No corrupt credential record exists")?,
        );
        if serde_json::from_slice::<ChatGptTokens>(&current).is_ok() {
            bail!("Refusing corrupt-record replacement because the credential record is valid");
        }
        serde_json::from_slice::<ChatGptTokens>(tombstone)
            .context("Replacement tombstone is invalid")?;
        write_private_file(&path, tombstone)?;
        FileExt::unlock(&lock)?;
        Ok(())
    }
}

/// Explicit Keychain policy used by production and platform-gated tests.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeychainPolicy {
    accessibility: &'static str,
    synchronizable: bool,
    widens_access_group: bool,
    uses_data_protection_keychain: bool,
}

#[cfg(target_os = "macos")]
const KEYCHAIN_POLICY: KeychainPolicy = KeychainPolicy {
    accessibility: "after-first-unlock-this-device-only",
    synchronizable: false,
    widens_access_group: false,
    uses_data_protection_keychain: true,
};

/// macOS Keychain credential store using modern SecItem data-protection
/// attributes. A private Finch lock file serializes CAS across processes;
/// policy errors never silently fall back to a plaintext file.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
pub struct KeychainCredentialStore {
    lock_store: FileCredentialStore,
}

#[cfg(target_os = "macos")]
impl KeychainCredentialStore {
    const SERVICE: &'static str = "dev.darwin-finch.chatgpt";
    const ERR_ITEM_NOT_FOUND: i32 = -25300;

    pub fn new(lock_root: PathBuf) -> Self {
        Self {
            lock_store: FileCredentialStore::new(lock_root),
        }
    }

    fn load_unlocked(&self, reference: &CredentialRef) -> Result<Option<Vec<u8>>> {
        let query = SecItemDictionary::query(reference, true)?;
        let mut result = std::ptr::null();
        let status = unsafe { SecItemCopyMatching(query.as_ptr(), &mut result) };
        if status == Self::ERR_ITEM_NOT_FOUND {
            return Ok(None);
        }
        if status != 0 || result.is_null() {
            bail!("macOS Keychain could not read Finch ChatGPT credentials (OSStatus {status})");
        }
        let result = OwnedCf(result);
        let is_data = unsafe { CFGetTypeID(result.0) == CFDataGetTypeID() };
        if !is_data {
            bail!("macOS Keychain returned a non-data ChatGPT credential record");
        }
        let length = unsafe { CFDataGetLength(result.0) };
        if length < 0 || length as usize > MAX_AUTH_BODY_BYTES {
            bail!("macOS Keychain returned an oversized ChatGPT credential record");
        }
        if length == 0 {
            return Ok(Some(Vec::new()));
        }
        let pointer = unsafe { CFDataGetBytePtr(result.0) };
        if pointer.is_null() {
            bail!("macOS Keychain returned an invalid ChatGPT credential record");
        }
        Ok(Some(
            unsafe { std::slice::from_raw_parts(pointer, length as usize) }.to_vec(),
        ))
    }

    fn store_unlocked(
        &self,
        reference: &CredentialRef,
        secret: &[u8],
        item_exists: bool,
    ) -> Result<()> {
        let status = if item_exists {
            let query = SecItemDictionary::query(reference, false)?;
            let attributes = SecItemDictionary::update(secret)?;
            unsafe { SecItemUpdate(query.as_ptr(), attributes.as_ptr()) }
        } else {
            let attributes = SecItemDictionary::insert(reference, secret)?;
            unsafe { SecItemAdd(attributes.as_ptr(), std::ptr::null_mut()) }
        };
        if status != 0 {
            bail!("macOS Keychain could not save Finch ChatGPT credentials (OSStatus {status})");
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl CredentialStore for KeychainCredentialStore {
    fn load(&self, reference: &CredentialRef) -> Result<Option<Vec<u8>>> {
        let lock = self.lock_store.lock()?;
        let result = self.load_unlocked(reference);
        FileExt::unlock(&lock)?;
        result
    }

    fn acquire_mutation_lease(&self, reference: &CredentialRef) -> Result<CredentialMutationLease> {
        Ok(CredentialMutationLease {
            _file: self.lock_store.mutation_lock(reference)?,
        })
    }

    fn compare_and_swap(
        &self,
        reference: &CredentialRef,
        expected_generation: Option<&str>,
        secret: &[u8],
    ) -> Result<()> {
        if secret.len() > MAX_AUTH_BODY_BYTES {
            bail!("ChatGPT credential record exceeds the size limit");
        }
        let lock = self.lock_store.lock()?;
        let current = self.load_unlocked(reference)?.map(zeroize::Zeroizing::new);
        let actual_generation = current
            .as_deref()
            .map(serde_json::from_slice::<ChatGptTokens>)
            .transpose()
            .context("Stored ChatGPT credential record is invalid")?
            .map(|tokens| tokens.generation.clone());
        if actual_generation.as_deref() != expected_generation {
            FileExt::unlock(&lock)?;
            bail!("ChatGPT credentials changed during mutation; retry safely");
        }
        self.store_unlocked(reference, secret, current.is_some())?;
        FileExt::unlock(&lock)?;
        Ok(())
    }

    fn replace_corrupt_with_tombstone(
        &self,
        reference: &CredentialRef,
        tombstone: &[u8],
    ) -> Result<()> {
        let lock = self.lock_store.lock()?;
        let current = zeroize::Zeroizing::new(
            self.load_unlocked(reference)?
                .context("No corrupt credential record exists")?,
        );
        if serde_json::from_slice::<ChatGptTokens>(&current).is_ok() {
            bail!("Refusing corrupt-record replacement because the credential record is valid");
        }
        serde_json::from_slice::<ChatGptTokens>(tombstone)
            .context("Replacement tombstone is invalid")?;
        self.store_unlocked(reference, tombstone, true)?;
        FileExt::unlock(&lock)?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
struct OwnedCf(*const std::ffi::c_void);

#[cfg(target_os = "macos")]
impl Drop for OwnedCf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

#[cfg(target_os = "macos")]
struct SecretCfData(*const std::ffi::c_void);

#[cfg(target_os = "macos")]
impl SecretCfData {
    fn new(secret: &[u8]) -> Result<Self> {
        let data = unsafe { CFDataCreateMutable(std::ptr::null(), secret.len() as isize) };
        if data.is_null() {
            bail!("CoreFoundation could not allocate protected credential data");
        }
        unsafe {
            CFDataSetLength(data, secret.len() as isize);
            if !secret.is_empty() {
                let destination = CFDataGetMutableBytePtr(data);
                if destination.is_null() {
                    CFRelease(data);
                    bail!("CoreFoundation returned invalid protected credential data");
                }
                std::ptr::copy_nonoverlapping(secret.as_ptr(), destination, secret.len());
            }
        }
        Ok(Self(data))
    }
}

#[cfg(target_os = "macos")]
impl Drop for SecretCfData {
    fn drop(&mut self) {
        if !self.0.is_null() {
            let length = unsafe { CFDataGetLength(self.0) };
            let pointer = unsafe { CFDataGetMutableBytePtr(self.0) };
            if !pointer.is_null() && length > 0 {
                unsafe { std::ptr::write_bytes(pointer, 0, length as usize) };
            }
            unsafe { CFRelease(self.0) };
        }
    }
}

#[cfg(target_os = "macos")]
struct SecItemDictionary {
    dictionary: OwnedCf,
    _strings: Vec<OwnedCf>,
    _secret: Option<SecretCfData>,
    shape: KeychainDictionaryShape,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Default, PartialEq, Eq)]
struct KeychainDictionaryShape {
    exact_generic_password_identity: bool,
    returns_data: bool,
    match_limit_one: bool,
    device_only_accessibility: bool,
    synchronizable_false: bool,
    data_protection_keychain: bool,
    includes_access_group: bool,
    includes_secret_data: bool,
}

#[cfg(target_os = "macos")]
impl SecItemDictionary {
    fn empty() -> Result<Self> {
        let dictionary = unsafe {
            CFDictionaryCreateMutable(std::ptr::null(), 0, std::ptr::null(), std::ptr::null())
        };
        if dictionary.is_null() {
            bail!("CoreFoundation could not allocate a Keychain query");
        }
        Ok(Self {
            dictionary: OwnedCf(dictionary),
            _strings: Vec::new(),
            _secret: None,
            shape: KeychainDictionaryShape::default(),
        })
    }

    fn query(reference: &CredentialRef, return_data: bool) -> Result<Self> {
        let mut value = Self::empty()?;
        value.set_exact_identity(reference)?;
        if return_data {
            value.set(unsafe { kSecReturnData }, unsafe { kCFBooleanTrue });
            value.set(unsafe { kSecMatchLimit }, unsafe { kSecMatchLimitOne });
            value.shape.returns_data = true;
            value.shape.match_limit_one = true;
        }
        Ok(value)
    }

    fn insert(reference: &CredentialRef, secret: &[u8]) -> Result<Self> {
        let mut value = Self::empty()?;
        value.set_exact_identity(reference)?;
        value.set_policy(true);
        value.set_secret(secret)?;
        Ok(value)
    }

    fn update(secret: &[u8]) -> Result<Self> {
        let mut value = Self::empty()?;
        value.set_policy(false);
        value.set_secret(secret)?;
        Ok(value)
    }

    fn set_exact_identity(&mut self, reference: &CredentialRef) -> Result<()> {
        self.set(unsafe { kSecClass }, unsafe { kSecClassGenericPassword });
        let service = cf_string(KeychainCredentialStore::SERVICE)?;
        self.set(unsafe { kSecAttrService }, service.0);
        self._strings.push(service);
        let account = cf_string(reference.as_str())?;
        self.set(unsafe { kSecAttrAccount }, account.0);
        self._strings.push(account);
        self.set(unsafe { kSecAttrSynchronizable }, unsafe {
            kCFBooleanFalse
        });
        self.set(unsafe { kSecUseDataProtectionKeychain }, unsafe {
            kCFBooleanTrue
        });
        self.shape.exact_generic_password_identity = true;
        self.shape.synchronizable_false = true;
        self.shape.data_protection_keychain = true;
        Ok(())
    }

    fn set_policy(&mut self, include_synchronizable_attribute: bool) {
        debug_assert_eq!(
            KEYCHAIN_POLICY.accessibility,
            "after-first-unlock-this-device-only"
        );
        debug_assert!(!KEYCHAIN_POLICY.synchronizable);
        debug_assert!(!KEYCHAIN_POLICY.widens_access_group);
        debug_assert!(KEYCHAIN_POLICY.uses_data_protection_keychain);
        self.set(unsafe { kSecAttrAccessible }, unsafe {
            kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        });
        self.shape.device_only_accessibility = true;
        if include_synchronizable_attribute {
            self.set(unsafe { kSecAttrSynchronizable }, unsafe {
                kCFBooleanFalse
            });
            self.shape.synchronizable_false = true;
        }
        // Deliberately omit kSecAttrAccessGroup: Finch does not widen access
        // beyond the current executable's default Keychain access group.
    }

    fn set_secret(&mut self, secret: &[u8]) -> Result<()> {
        let data = SecretCfData::new(secret)?;
        self.set(unsafe { kSecValueData }, data.0);
        self._secret = Some(data);
        self.shape.includes_secret_data = true;
        Ok(())
    }

    fn set(&mut self, key: *const std::ffi::c_void, value: *const std::ffi::c_void) {
        unsafe { CFDictionarySetValue(self.dictionary.0, key, value) };
    }

    fn as_ptr(&self) -> *const std::ffi::c_void {
        self.dictionary.0
    }
}

#[cfg(target_os = "macos")]
fn cf_string(value: &str) -> Result<OwnedCf> {
    const UTF8: u32 = 0x0800_0100;
    let string = unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            value.as_ptr(),
            value.len() as isize,
            UTF8,
            0,
        )
    };
    if string.is_null() {
        bail!("CoreFoundation could not encode a Keychain attribute");
    }
    Ok(OwnedCf(string))
}

#[cfg(target_os = "macos")]
#[link(name = "Security", kind = "framework")]
extern "C" {
    fn SecItemCopyMatching(
        query: *const std::ffi::c_void,
        result: *mut *const std::ffi::c_void,
    ) -> i32;
    fn SecItemAdd(attributes: *const std::ffi::c_void, result: *mut *const std::ffi::c_void)
        -> i32;
    fn SecItemUpdate(
        query: *const std::ffi::c_void,
        attributes_to_update: *const std::ffi::c_void,
    ) -> i32;
    static kSecClass: *const std::ffi::c_void;
    static kSecClassGenericPassword: *const std::ffi::c_void;
    static kSecAttrService: *const std::ffi::c_void;
    static kSecAttrAccount: *const std::ffi::c_void;
    static kSecAttrSynchronizable: *const std::ffi::c_void;
    static kSecAttrAccessible: *const std::ffi::c_void;
    static kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly: *const std::ffi::c_void;
    static kSecValueData: *const std::ffi::c_void;
    static kSecReturnData: *const std::ffi::c_void;
    static kSecMatchLimit: *const std::ffi::c_void;
    static kSecMatchLimitOne: *const std::ffi::c_void;
    static kSecUseDataProtectionKeychain: *const std::ffi::c_void;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: *const std::ffi::c_void);
    fn CFGetTypeID(value: *const std::ffi::c_void) -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFDataGetLength(data: *const std::ffi::c_void) -> isize;
    fn CFDataGetBytePtr(data: *const std::ffi::c_void) -> *const u8;
    fn CFDataCreateMutable(
        allocator: *const std::ffi::c_void,
        capacity: isize,
    ) -> *const std::ffi::c_void;
    fn CFDataSetLength(data: *const std::ffi::c_void, length: isize);
    fn CFDataGetMutableBytePtr(data: *const std::ffi::c_void) -> *mut u8;
    fn CFDictionaryCreateMutable(
        allocator: *const std::ffi::c_void,
        capacity: isize,
        key_callbacks: *const std::ffi::c_void,
        value_callbacks: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;
    fn CFDictionarySetValue(
        dictionary: *const std::ffi::c_void,
        key: *const std::ffi::c_void,
        value: *const std::ffi::c_void,
    );
    fn CFStringCreateWithBytes(
        allocator: *const std::ffi::c_void,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        is_external_representation: u8,
    ) -> *const std::ffi::c_void;
    static kCFBooleanTrue: *const std::ffi::c_void;
    static kCFBooleanFalse: *const std::ffi::c_void;
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct UserCodeResponse {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(deserialize_with = "deserialize_poll_interval")]
    interval: String,
}

fn deserialize_poll_interval<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value {
        String(String),
        Number(u64),
    }
    Ok(match Value::deserialize(deserializer)? {
        Value::String(value) => value,
        Value::Number(value) => value.to_string(),
    })
}

/// Non-secret state displayed and resumed by CLI or setup wizard.
#[derive(Clone)]
pub struct PendingDeviceLogin {
    pub user_code: String,
    pub verification_url: String,
    device_auth_id: String,
    poll_interval: Duration,
    started_at: tokio::time::Instant,
    expected_generation: Option<String>,
    auth_base_url: String,
    client_id: String,
    credential_ref: String,
}

impl Drop for PendingDeviceLogin {
    fn drop(&mut self) {
        self.user_code.zeroize();
        self.device_auth_id.zeroize();
        self.client_id.zeroize();
        self.credential_ref.zeroize();
    }
}

impl fmt::Debug for PendingDeviceLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingDeviceLogin")
            .field("verification_url", &self.verification_url)
            .field("user_code", &"<redacted one-time code>")
            .field("device_auth_id", &"<redacted>")
            .field("bindings", &"<redacted>")
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct DeviceTokenResponse {
    authorization_code: String,
    code_challenge: String,
    code_verifier: String,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct OAuthTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct LegacyChatGptTokens {
    access_token: String,
    refresh_token: String,
    account_id: String,
    expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptAccountStatus {
    pub credential_ref: CredentialRef,
    pub account_id_suffix: String,
    pub expires_at: u64,
    pub needs_refresh: bool,
}

/// Native authorization client. Clones share refresh singleflight state.
#[derive(Clone)]
pub struct ChatGptAuth {
    client: Client,
    auth_base_url: String,
    client_id: String,
    credential_ref: CredentialRef,
    store: Arc<dyn CredentialStore>,
    refresh_lock: Arc<Mutex<()>>,
    legacy_path: Option<PathBuf>,
}

type SharedRefreshLock = Arc<Mutex<()>>;

fn refresh_lock_for(auth_base_url: &str, reference: &CredentialRef) -> SharedRefreshLock {
    static LOCKS: once_cell::sync::Lazy<
        std::sync::Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>,
    > = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashMap::new()));
    let key = format!(
        "{}|{}",
        auth_base_url.trim_end_matches('/'),
        reference.as_str()
    );
    let mut locks = LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(lock) = locks.get(&key).and_then(std::sync::Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

impl fmt::Debug for ChatGptAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatGptAuth")
            .field("auth_base_url", &self.auth_base_url)
            .field("credential_ref", &self.credential_ref)
            .field("client_id", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl ChatGptAuth {
    /// Construct native auth with Finch's private portable credential fallback.
    pub fn new(credential_ref: impl Into<String>) -> Result<Self> {
        let home = dirs::home_dir().context("Cannot determine Finch credential directory")?;
        let root = home.join(".finch").join("credentials");
        let legacy_path = home.join(".finch").join("auth").join("chatgpt.json");
        let credential_ref = CredentialRef::parse(credential_ref)?;
        #[cfg(target_os = "macos")]
        let store: Arc<dyn CredentialStore> = Arc::new(KeychainCredentialStore::new(root));
        #[cfg(not(target_os = "macos"))]
        let store: Arc<dyn CredentialStore> = Arc::new(FileCredentialStore::new(root));
        let mut auth = Self::with_options(
            DEFAULT_AUTH_BASE_URL,
            DEFAULT_CLIENT_ID,
            credential_ref,
            store,
        )?;
        auth.legacy_path = Some(legacy_path);
        Ok(auth)
    }

    /// Inject endpoints and storage for setup integration and conformance tests.
    pub fn with_options(
        auth_base_url: impl Into<String>,
        client_id: impl Into<String>,
        credential_ref: CredentialRef,
        store: Arc<dyn CredentialStore>,
    ) -> Result<Self> {
        let auth_base_url = auth_base_url.into().trim_end_matches('/').to_string();
        validate_endpoint(&auth_base_url, "auth.openai.com", "ChatGPT authorization")?;
        let refresh_lock = refresh_lock_for(&auth_base_url, &credential_ref);
        Ok(Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(HTTP_TIMEOUT)
                .build()
                .context("Failed to create ChatGPT authentication client")?,
            auth_base_url,
            client_id: client_id.into(),
            credential_ref,
            store,
            refresh_lock,
            legacy_path: None,
        })
    }

    pub fn credential_ref(&self) -> &CredentialRef {
        &self.credential_ref
    }

    fn migrate_legacy_file(&self, legacy_path: &Path) -> Result<()> {
        let Some(bytes) = read_private_file(legacy_path)? else {
            return Ok(());
        };
        let bytes = zeroize::Zeroizing::new(bytes);
        if self.credential_ref.as_str() != "chatgpt:default" {
            bail!(
                "Legacy Finch ChatGPT credentials still exist; run auth status for chatgpt:default to migrate them before using another named account"
            );
        }
        let mut legacy: LegacyChatGptTokens = serde_json::from_slice(&bytes).context(
            "Legacy Finch ChatGPT credentials are malformed; remove or recover ~/.finch/auth/chatgpt.json explicitly",
        )?;
        validate_token(&legacy.access_token, "legacy access token")?;
        validate_token(&legacy.refresh_token, "legacy refresh token")?;
        validate_account_id(&legacy.account_id)?;
        let _mutation = self.store.acquire_mutation_lease(&self.credential_ref)?;
        let existing = self
            .store
            .load(&self.credential_ref)?
            .map(|bytes| {
                let bytes = zeroize::Zeroizing::new(bytes);
                serde_json::from_slice::<ChatGptTokens>(&bytes)
                    .context("Finch ChatGPT credential record is invalid")
            })
            .transpose()?;
        if let Some(existing) = existing.as_ref() {
            if !existing.tombstone && existing.account_id != legacy.account_id {
                bail!("Legacy ChatGPT account differs from the named account record; refusing implicit replacement");
            }
        } else {
            let migrated = ChatGptTokens {
                access_token: std::mem::take(&mut legacy.access_token),
                refresh_token: std::mem::take(&mut legacy.refresh_token),
                id_token: None,
                account_id: std::mem::take(&mut legacy.account_id),
                identity: CredentialIdentity {
                    authorization_endpoint: self.auth_base_url.clone(),
                    client_id: self.client_id.clone(),
                    subscription_endpoint: "https://chatgpt.com/backend-api/codex".to_string(),
                    observed_subject: None,
                    observed_issuer: None,
                    observed_audiences: Vec::new(),
                    observed_scopes: Vec::new(),
                },
                expires_at: legacy.expires_at,
                generation: new_generation(),
                tombstone: false,
                mutation_pending: false,
            };
            self.persist(&migrated, None)?;
        }
        match std::fs::remove_file(legacy_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context(
                    "Legacy ChatGPT credentials were migrated but the plaintext file could not be removed",
                )
            }
        }
        Ok(())
    }

    pub async fn begin_device_login(&self) -> Result<PendingDeviceLogin> {
        let expected_generation = self.load_record()?.map(|tokens| tokens.generation.clone());
        let response = self
            .client
            .post(format!(
                "{}/api/accounts/deviceauth/usercode",
                self.auth_base_url
            ))
            .json(&serde_json::json!({ "client_id": self.client_id }))
            .send()
            .await
            .context("Failed to request ChatGPT device authorization")?;
        let status = response.status();
        if !status.is_success() {
            drain_bounded(response).await?;
            bail!("ChatGPT device authorization is unavailable (HTTP {status})");
        }
        let bytes = zeroize::Zeroizing::new(read_bounded(response, MAX_AUTH_BODY_BYTES).await?);
        let mut code: UserCodeResponse = serde_json::from_slice(&bytes)
            .context("ChatGPT device authorization contract changed")?;
        validate_short_field(&code.device_auth_id, "device authorization id")?;
        validate_short_field(&code.user_code, "device user code")?;
        let interval = code
            .interval
            .trim()
            .parse::<u64>()
            .context("ChatGPT device polling interval is invalid")?;
        if !(1..=MAX_POLL_INTERVAL_SECS).contains(&interval) {
            bail!("ChatGPT device polling interval is outside protocol bounds");
        }
        Ok(PendingDeviceLogin {
            user_code: std::mem::take(&mut code.user_code),
            verification_url: format!("{}/codex/device", self.auth_base_url),
            device_auth_id: std::mem::take(&mut code.device_auth_id),
            poll_interval: Duration::from_secs(interval),
            started_at: tokio::time::Instant::now(),
            expected_generation,
            auth_base_url: self.auth_base_url.clone(),
            client_id: self.client_id.clone(),
            credential_ref: self.credential_ref.as_str().to_string(),
        })
    }

    /// Complete login without blocking the event loop; cancellation is prompt.
    pub async fn finish_device_login(
        &self,
        pending: &PendingDeviceLogin,
        cancel: CancellationToken,
    ) -> Result<ChatGptAccountStatus> {
        if pending.auth_base_url != self.auth_base_url
            || pending.client_id != self.client_id
            || pending.credential_ref != self.credential_ref.as_str()
        {
            bail!("ChatGPT device authorization belongs to a different Finch auth context");
        }
        let deadline = pending.started_at + DEVICE_TIMEOUT;
        let mut interval = pending.poll_interval;
        loop {
            if tokio::time::Instant::now() >= deadline {
                bail!("ChatGPT device authorization expired");
            }
            let response = tokio::select! {
                _ = cancel.cancelled() => bail!("ChatGPT device authorization cancelled"),
                response = self.client
                    .post(format!("{}/api/accounts/deviceauth/token", self.auth_base_url))
                    .json(&serde_json::json!({
                        "device_auth_id": pending.device_auth_id,
                        "user_code": pending.user_code,
                    }))
                    .send() => response.context("Failed while waiting for ChatGPT device authorization")?,
            };
            let status = response.status();
            if status.is_success() {
                let bytes =
                    zeroize::Zeroizing::new(read_bounded(response, MAX_AUTH_BODY_BYTES).await?);
                let device: DeviceTokenResponse = serde_json::from_slice(&bytes)
                    .context("ChatGPT device authorization contract changed")?;
                validate_short_field(&device.authorization_code, "authorization code")?;
                validate_short_field(&device.code_challenge, "PKCE challenge")?;
                validate_short_field(&device.code_verifier, "PKCE verifier")?;
                let expected_challenge =
                    URL_SAFE_NO_PAD.encode(Sha256::digest(device.code_verifier.as_bytes()));
                if expected_challenge != device.code_challenge {
                    bail!("ChatGPT device authorization returned an invalid PKCE pair");
                }
                let _mutation = tokio::select! {
                    _ = cancel.cancelled() => bail!("ChatGPT device authorization cancelled"),
                    lease = self.mutation_lease() => lease?,
                };
                self.require_generation(pending.expected_generation.as_deref())?;
                let mut issuance_marker =
                    self.load_record()?.unwrap_or_else(ChatGptTokens::tombstone);
                issuance_marker.generation = new_generation();
                issuance_marker.mutation_pending = true;
                self.persist(&issuance_marker, pending.expected_generation.as_deref())?;
                let mut tokens = tokio::select! {
                    _ = cancel.cancelled() => bail!("ChatGPT device authorization was cancelled during token issuance; the named record is marked incomplete and must be logged out before retrying"),
                    tokens = self.exchange_authorization_code(device) => tokens?,
                };
                tokens.generation = new_generation();
                if let Err(error) = self.persist(&tokens, Some(&issuance_marker.generation)) {
                    let _ = self.revoke_token(&tokens.refresh_token).await;
                    return Err(error.context(
                        "ChatGPT login was issued remotely but lost its local generation commit; the issued refresh token was revoked best-effort",
                    ));
                }
                return self
                    .account_status()?
                    .context("ChatGPT login completed without stored account status");
            }

            let body = read_bounded(response, MAX_AUTH_BODY_BYTES).await?;
            let code = oauth_error_code(&body);
            let pending_status = matches!(status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN)
                && (body.iter().all(u8::is_ascii_whitespace)
                    || matches!(code.as_deref(), Some("authorization_pending" | "pending")));
            if code.as_deref() == Some("slow_down") || status == StatusCode::TOO_MANY_REQUESTS {
                interval = (interval + Duration::from_secs(5))
                    .min(Duration::from_secs(MAX_POLL_INTERVAL_SECS));
            } else if matches!(
                code.as_deref(),
                Some("access_denied" | "authorization_declined")
            ) {
                bail!("ChatGPT device authorization was denied");
            } else if matches!(
                code.as_deref(),
                Some("expired_token" | "authorization_expired")
            ) {
                bail!("ChatGPT device authorization expired");
            } else if !pending_status {
                bail!("ChatGPT device authorization failed (HTTP {status})");
            }

            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let wait = interval.min(remaining);
            tokio::select! {
                _ = cancel.cancelled() => bail!("ChatGPT device authorization cancelled"),
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }

    async fn exchange_authorization_code(
        &self,
        device: DeviceTokenResponse,
    ) -> Result<ChatGptTokens> {
        let redirect_uri = format!("{}/deviceauth/callback", self.auth_base_url);
        let body = form_body(&[
            ("grant_type", "authorization_code"),
            ("code", &device.authorization_code),
            ("redirect_uri", &redirect_uri),
            ("client_id", &self.client_id),
            ("code_verifier", &device.code_verifier),
        ]);
        let response = self
            .client
            .post(format!("{}/oauth/token", self.auth_base_url))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .context("Failed to exchange ChatGPT device authorization")?;
        self.decode_token_response(response, None).await
    }

    /// Return a fresh access credential using singleflight and revision CAS.
    pub(crate) async fn tokens(&self) -> Result<ChatGptTokens> {
        let current = self
            .load()?
            .context("No Finch ChatGPT login found; run `finch auth login chatgpt`")?;
        if !current.needs_refresh() {
            return Ok(current);
        }
        let _guard = self.refresh_lock.lock().await;
        let _mutation = self.mutation_lease().await?;
        let current = self
            .load()?
            .context("No Finch ChatGPT login found; run `finch auth login chatgpt`")?;
        if !current.needs_refresh() {
            return Ok(current);
        }
        self.refresh_current(current).await
    }

    /// Refresh exactly once after a backend 401. If another process already
    /// rotated the generation, reuse its committed credential instead.
    pub(crate) async fn tokens_after_unauthorized(
        &self,
        rejected_generation: &str,
    ) -> Result<ChatGptTokens> {
        let _guard = self.refresh_lock.lock().await;
        let _mutation = self.mutation_lease().await?;
        let current = self
            .load()?
            .context("No Finch ChatGPT login found; run `finch auth login chatgpt`")?;
        if current.generation != rejected_generation {
            return Ok(current);
        }
        self.refresh_current(current).await
    }

    async fn refresh_current(&self, current: ChatGptTokens) -> Result<ChatGptTokens> {
        let mut refresh_marker = current.clone();
        refresh_marker.generation = new_generation();
        refresh_marker.mutation_pending = true;
        self.persist(&refresh_marker, Some(&current.generation))?;
        let response = self
            .client
            .post(format!("{}/oauth/token", self.auth_base_url))
            .json(&serde_json::json!({
                "client_id": self.client_id,
                "grant_type": "refresh_token",
                "refresh_token": &current.refresh_token,
            }))
            .send()
            .await
            .context("Failed to refresh ChatGPT credentials")?;
        let refreshed = self.decode_token_response(response, Some(&current)).await?;
        if let Err(error) = validate_refresh_continuity(&current, &refreshed) {
            let _ = self.revoke_token(&refreshed.refresh_token).await;
            let tombstone = ChatGptTokens::tombstone();
            let _ = self.persist(&tombstone, Some(&refresh_marker.generation));
            return Err(error.context(
                "ChatGPT refresh changed account identity; the issued refresh token was revoked best-effort",
            ));
        }
        if let Err(error) = self.persist(&refreshed, Some(&refresh_marker.generation)) {
            let _ = self.revoke_token(&refreshed.refresh_token).await;
            return Err(error.context(
                "ChatGPT refresh was issued remotely but lost its local generation commit; the issued refresh token was revoked best-effort",
            ));
        }
        Ok(refreshed)
    }

    pub fn account_status(&self) -> Result<Option<ChatGptAccountStatus>> {
        let Some(tokens) = self.load()? else {
            return Ok(None);
        };
        let suffix: String = tokens
            .account_id
            .chars()
            .rev()
            .take(6)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        Ok(Some(ChatGptAccountStatus {
            credential_ref: self.credential_ref.clone(),
            account_id_suffix: suffix,
            expires_at: tokens.expires_at,
            needs_refresh: tokens.needs_refresh(),
        }))
    }

    /// Best-effort remote revocation followed by unconditional local deletion.
    pub async fn logout(&self) -> Result<()> {
        if let Some(legacy_path) = self.legacy_path.as_deref() {
            self.migrate_legacy_file(legacy_path)?;
        }
        let _guard = self.refresh_lock.lock().await;
        let _mutation = self.mutation_lease().await?;
        let record = self.load_record();
        let (expected_generation, revoke_result) = match record {
            Ok(Some(tokens)) if !tokens.tombstone => {
                let result = self.revoke_token(&tokens.refresh_token).await;
                (Some(tokens.generation.clone()), result)
            }
            Ok(Some(tokens)) => (Some(tokens.generation.clone()), Ok(StatusCode::OK)),
            Ok(None) => (None, Ok(StatusCode::OK)),
            Err(error) => {
                let tombstone = ChatGptTokens::tombstone();
                let bytes = serde_json::to_vec(&tombstone)
                    .context("Failed to encode corrupt-record tombstone")?;
                self.store
                    .replace_corrupt_with_tombstone(&self.credential_ref, &bytes)?;
                return Err(error.context(
                    "Malformed ChatGPT credentials were tombstoned locally; remote revocation was impossible",
                ));
            }
        };
        let tombstone = ChatGptTokens::tombstone();
        self.persist(&tombstone, expected_generation.as_deref())?;
        let status = revoke_result?;
        if !status.is_success() {
            bail!("ChatGPT credentials were tombstoned locally, but remote revocation failed (HTTP {status})");
        }
        Ok(())
    }

    async fn revoke_token(&self, refresh_token: &str) -> Result<StatusCode> {
        self.client
            .post(format!("{}/oauth/revoke", self.auth_base_url))
            .timeout(Duration::from_secs(10))
            .json(&serde_json::json!({
                "token": refresh_token,
                "token_type_hint": "refresh_token",
                "client_id": self.client_id,
            }))
            .send()
            .await
            .map(|response| response.status())
            .context("Failed to revoke ChatGPT credentials")
    }

    async fn mutation_lease(&self) -> Result<CredentialMutationLease> {
        let store = self.store.clone();
        let reference = self.credential_ref.clone();
        tokio::task::spawn_blocking(move || store.acquire_mutation_lease(&reference))
            .await
            .context("Credential mutation lease task stopped")?
    }

    async fn decode_token_response(
        &self,
        response: Response,
        previous: Option<&ChatGptTokens>,
    ) -> Result<ChatGptTokens> {
        let status = response.status();
        let bytes = zeroize::Zeroizing::new(read_bounded(response, MAX_AUTH_BODY_BYTES).await?);
        if !status.is_success() {
            let code = oauth_error_code(&bytes).unwrap_or_else(|| "unknown".to_string());
            bail!("ChatGPT token request failed (HTTP {status}, code {code})");
        }
        let mut response: OAuthTokenResponse =
            serde_json::from_slice(&bytes).context("ChatGPT token response contract changed")?;
        validate_token(&response.access_token, "access token")?;
        if let Some(refresh) = response.refresh_token.as_deref() {
            validate_token(refresh, "refresh token")?;
        }
        let claims = response
            .id_token
            .as_deref()
            .and_then(jwt_claims)
            .or_else(|| jwt_claims(&response.access_token));
        let account_id = claims
            .as_ref()
            .and_then(account_id_from_claims)
            .or_else(|| previous.map(|tokens| tokens.account_id.clone()))
            .context("ChatGPT token did not contain an account identifier")?;
        validate_account_id(&account_id)?;
        let subject = claims
            .as_ref()
            .and_then(|value| value.get("sub"))
            .and_then(ValueExt::short_string);
        let mut scopes = claims
            .as_ref()
            .and_then(|value| value.get("scope").or_else(|| value.get("scp")))
            .map(extract_scopes)
            .unwrap_or_default();
        scopes.sort();
        scopes.dedup();
        let observed_issuer = claims
            .as_ref()
            .and_then(|value| value.get("iss"))
            .and_then(ValueExt::short_string);
        let observed_audiences = claims
            .as_ref()
            .and_then(|value| value.get("aud"))
            .map(extract_scopes)
            .unwrap_or_default();
        let identity = CredentialIdentity {
            authorization_endpoint: self.auth_base_url.clone(),
            client_id: self.client_id.clone(),
            subscription_endpoint: "https://chatgpt.com/backend-api/codex".to_string(),
            observed_subject: subject,
            observed_issuer,
            observed_audiences,
            observed_scopes: scopes,
        };
        let refresh_token = response
            .refresh_token
            .take()
            .or_else(|| previous.map(|tokens| tokens.refresh_token.clone()))
            .context("ChatGPT token response did not contain a refresh token")?;
        let now = unix_time();
        // JWT payloads are deliberately parsed without claiming verification;
        // expiry therefore comes only from the authenticated token endpoint.
        let expiry =
            now.saturating_add(response.expires_in.unwrap_or(3600).clamp(60, 24 * 60 * 60));
        Ok(ChatGptTokens {
            access_token: std::mem::take(&mut response.access_token),
            refresh_token,
            id_token: response
                .id_token
                .take()
                .or_else(|| previous.and_then(|p| p.id_token.clone())),
            account_id,
            identity: identity.clone(),
            expires_at: expiry,
            generation: new_generation(),
            tombstone: false,
            mutation_pending: false,
        })
    }

    fn load(&self) -> Result<Option<ChatGptTokens>> {
        let Some(tokens) = self.load_record()? else {
            return Ok(None);
        };
        if tokens.mutation_pending {
            bail!("ChatGPT credential has an incomplete remote mutation; logout and sign in again");
        }
        if tokens.tombstone {
            return Ok(None);
        }
        validate_token(&tokens.access_token, "access token")?;
        validate_token(&tokens.refresh_token, "refresh token")?;
        validate_account_id(&tokens.account_id)?;
        self.validate_identity(&tokens.identity)?;
        Ok(Some(tokens))
    }

    fn load_record(&self) -> Result<Option<ChatGptTokens>> {
        if let Some(legacy_path) = self.legacy_path.as_deref() {
            self.migrate_legacy_file(legacy_path)?;
        }
        let Some(bytes) = self.store.load(&self.credential_ref)? else {
            return Ok(None);
        };
        let bytes = zeroize::Zeroizing::new(bytes);
        let tokens = serde_json::from_slice::<ChatGptTokens>(&bytes)
            .context("Finch ChatGPT credential record is invalid")?;
        validate_generation(&tokens.generation)?;
        Ok(Some(tokens))
    }

    fn persist(&self, tokens: &ChatGptTokens, expected: Option<&str>) -> Result<()> {
        let bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(tokens).context("Failed to encode ChatGPT credentials")?,
        );
        self.store
            .compare_and_swap(&self.credential_ref, expected, &bytes)
    }

    fn require_generation(&self, expected: Option<&str>) -> Result<()> {
        let actual = self.load_record()?.map(|tokens| tokens.generation.clone());
        if actual.as_deref() != expected {
            bail!("ChatGPT credential generation changed while authorization was pending");
        }
        Ok(())
    }

    fn validate_identity(&self, identity: &CredentialIdentity) -> Result<()> {
        if identity.authorization_endpoint != self.auth_base_url
            || identity.client_id != self.client_id
            || identity.subscription_endpoint != "https://chatgpt.com/backend-api/codex"
        {
            bail!("ChatGPT credential endpoint or client binding does not match this provider");
        }
        Ok(())
    }
}

fn validate_endpoint(value: &str, production_host: &str, label: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).with_context(|| format!("Invalid {label} endpoint"))?;
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
        bail!("Invalid {label} endpoint");
    }
    let host = url.host_str().context("Endpoint omitted host")?;
    let production = url.scheme() == "https" && host.eq_ignore_ascii_case(production_host);
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !production && !loopback {
        bail!("Refusing to send ChatGPT credentials to an untrusted {label} endpoint");
    }
    Ok(())
}

trait ValueExt {
    fn short_string(&self) -> Option<String>;
}

impl ValueExt for serde_json::Value {
    fn short_string(&self) -> Option<String> {
        let value = self.as_str()?;
        (value.len() <= 1024 && !value.chars().any(char::is_control)).then(|| value.to_string())
    }
}

fn extract_scopes(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::String(scopes) => scopes
            .split_whitespace()
            .filter(|scope| scope.len() <= 128)
            .map(str::to_string)
            .collect(),
        serde_json::Value::Array(scopes) => scopes
            .iter()
            .filter_map(ValueExt::short_string)
            .take(128)
            .collect(),
        _ => Vec::new(),
    }
}

async fn read_bounded(response: Response, maximum: usize) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Failed to read ChatGPT response")?;
        if bytes.len().saturating_add(chunk.len()) > maximum {
            bail!("ChatGPT response exceeded the size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn drain_bounded(response: Response) -> Result<()> {
    let _ = read_bounded(response, MAX_AUTH_BODY_BYTES).await?;
    Ok(())
}

fn oauth_error_code(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value
        .get("error")
        .and_then(|error| error.as_str().or_else(|| error.get("code")?.as_str()))
        .or_else(|| value.get("code")?.as_str())
        .and_then(|value| {
            matches!(
                value,
                "authorization_pending"
                    | "pending"
                    | "slow_down"
                    | "access_denied"
                    | "authorization_declined"
                    | "expired_token"
                    | "authorization_expired"
                    | "invalid_grant"
                    | "refresh_token_expired"
                    | "refresh_token_reused"
                    | "refresh_token_invalidated"
            )
            .then(|| value.to_string())
        })
}

fn validate_short_field(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        bail!("ChatGPT {label} is invalid");
    }
    Ok(())
}

fn validate_token(value: &str, label: &str) -> Result<()> {
    if value.len() < 8 || value.len() > 32 * 1024 || value.chars().any(char::is_whitespace) {
        bail!("ChatGPT {label} is invalid");
    }
    Ok(())
}

fn validate_account_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_ACCOUNT_ID_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        bail!("ChatGPT account identifier is invalid");
    }
    Ok(())
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn new_generation() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn validate_generation(value: &str) -> Result<()> {
    let parsed =
        uuid::Uuid::parse_str(value).context("ChatGPT credential generation is invalid")?;
    if parsed.get_version_num() != 4 {
        bail!("ChatGPT credential generation is not random");
    }
    Ok(())
}

fn validate_refresh_continuity(previous: &ChatGptTokens, refreshed: &ChatGptTokens) -> Result<()> {
    if previous.account_id != refreshed.account_id
        || matches!(
            (
                &previous.identity.observed_subject,
                &refreshed.identity.observed_subject
            ),
            (Some(old), Some(new)) if old != new
        )
    {
        bail!("ChatGPT refresh changed the observed account identity");
    }
    if previous.identity.authorization_endpoint != refreshed.identity.authorization_endpoint
        || previous.identity.client_id != refreshed.identity.client_id
        || previous.identity.subscription_endpoint != refreshed.identity.subscription_endpoint
    {
        bail!("ChatGPT refresh changed the locally enforced credential boundary");
    }
    Ok(())
}

fn form_body(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() || payload.len() > 24 * 1024 {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    if bytes.len() > 16 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn account_id_from_claims(claims: &serde_json::Value) -> Option<String> {
    claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .or_else(|| claims.get("chatgpt_account_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn open_private_lock_file(path: &Path, owner_directory: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .context("Failed to open private credential lock")?;
    #[cfg(unix)]
    validate_private_descriptor(&file, owner_directory, true)?;
    Ok(file)
}

#[cfg(unix)]
fn validate_private_descriptor(
    file: &File,
    owner_directory: &Path,
    set_private_permissions: bool,
) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = file.metadata()?;
    let owner = std::fs::metadata(owner_directory)?.uid();
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != owner {
        bail!("Credential file is not a singly-linked file owned by Finch's credential directory owner");
    }
    if set_private_permissions {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
        bail!("Credential file permissions are not private");
    }
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Credential path has no parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .context("Failed to create temporary credential file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(bytes)?;
    temp.as_file_mut().sync_all()?;
    temp.persist(path)
        .map_err(|error| error.error)
        .context("Failed to atomically save ChatGPT credentials")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(path)?;
        validate_private_descriptor(&file, parent, false)?;
    }
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn read_private_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Failed to open ChatGPT credentials safely"),
    };
    #[cfg(unix)]
    validate_private_descriptor(
        &file,
        path.parent().context("Credential path has no parent")?,
        false,
    )?;
    #[cfg(not(unix))]
    if !file.metadata()?.is_file() {
        bail!("ChatGPT credential path is not a regular file");
    }
    let mut bytes = Vec::new();
    file.take((MAX_AUTH_BODY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_AUTH_BODY_BYTES {
        bail!("ChatGPT credential record exceeds the size limit");
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn jwt(claims: serde_json::Value) -> String {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        format!("header.{payload}.signature")
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_keychain_secitem_policy_is_device_only_non_sync_and_exact() {
        assert_eq!(
            KEYCHAIN_POLICY,
            KeychainPolicy {
                accessibility: "after-first-unlock-this-device-only",
                synchronizable: false,
                widens_access_group: false,
                uses_data_protection_keychain: true,
            }
        );
        let reference = CredentialRef::parse("chatgpt:policy-test").unwrap();
        let query = SecItemDictionary::query(&reference, true).unwrap();
        assert!(query.shape.exact_generic_password_identity);
        assert!(query.shape.returns_data);
        assert!(query.shape.match_limit_one);
        assert!(query.shape.synchronizable_false);
        assert!(query.shape.data_protection_keychain);
        assert!(!query.shape.includes_access_group);
        assert!(!query.shape.includes_secret_data);

        let insert = SecItemDictionary::insert(&reference, b"dummy-secret").unwrap();
        assert!(insert.shape.exact_generic_password_identity);
        assert!(insert.shape.device_only_accessibility);
        assert!(insert.shape.synchronizable_false);
        assert!(insert.shape.data_protection_keychain);
        assert!(!insert.shape.includes_access_group);
        assert!(insert.shape.includes_secret_data);

        let update = SecItemDictionary::update(b"rotated-dummy-secret").unwrap();
        assert!(update.shape.device_only_accessibility);
        assert!(!update.shape.includes_access_group);
        assert!(update.shape.includes_secret_data);
    }

    #[test]
    fn test_chatgpt_tokens_debug_redacts_all_credentials() {
        let tokens = ChatGptTokens {
            access_token: "secret-access".into(),
            refresh_token: "secret-refresh".into(),
            id_token: Some("secret-id".into()),
            account_id: "secret-account".into(),
            identity: CredentialIdentity {
                authorization_endpoint: "https://auth.openai.com".into(),
                client_id: "client".into(),
                subscription_endpoint: "https://chatgpt.com/backend-api/codex".into(),
                observed_subject: Some("secret-subject".into()),
                observed_issuer: Some("secret-issuer".into()),
                observed_audiences: vec!["secret-audience".into()],
                observed_scopes: vec!["secret-scope".into()],
            },
            expires_at: 42,
            generation: new_generation(),
            tombstone: false,
            mutation_pending: false,
        };
        let debug = format!("{tokens:?}");
        for secret in [
            "secret-access",
            "secret-refresh",
            "secret-id",
            "secret-account",
            "secret-subject",
            "secret-issuer",
            "secret-audience",
            "secret-scope",
        ] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn test_extracts_nested_chatgpt_account_id() {
        let claims = jwt_claims(&jwt(json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct-123" }
        })))
        .unwrap();
        assert_eq!(account_id_from_claims(&claims).as_deref(), Some("acct-123"));
    }

    #[test]
    fn test_malformed_jwt_fails_closed_without_panicking() {
        for malformed in ["", "x", "x.y", "x.%%%.z", "x.e30.z.extra"] {
            assert!(jwt_claims(malformed).is_none());
        }
    }

    #[test]
    fn test_form_encoding_preserves_refresh_tokens() {
        assert_eq!(
            form_body(&[("refresh_token", "a+b/c=")]),
            "refresh_token=a%2Bb%2Fc%3D"
        );
    }

    #[test]
    fn test_file_store_is_private_atomic_and_cas_protected() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("credentials"));
        let reference = CredentialRef::parse("chatgpt:test").unwrap();
        let first = ChatGptTokens {
            access_token: "access-one".into(),
            refresh_token: "refresh-one".into(),
            id_token: None,
            account_id: "account-one".into(),
            identity: CredentialIdentity {
                authorization_endpoint: "https://auth.openai.com".into(),
                client_id: "client".into(),
                subscription_endpoint: "https://chatgpt.com/backend-api/codex".into(),
                observed_subject: None,
                observed_issuer: None,
                observed_audiences: Vec::new(),
                observed_scopes: Vec::new(),
            },
            expires_at: 100,
            generation: new_generation(),
            tombstone: false,
            mutation_pending: false,
        };
        let bytes = serde_json::to_vec(&first).unwrap();
        store.compare_and_swap(&reference, None, &bytes).unwrap();
        assert_eq!(store.load(&reference).unwrap().unwrap(), bytes);
        assert!(store
            .compare_and_swap(
                &reference,
                Some("00000000-0000-4000-8000-000000000000"),
                &bytes
            )
            .is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = store.path(&reference);
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn test_tombstone_generation_prevents_aba_after_logout_and_recreate() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("credentials"));
        let reference = CredentialRef::parse("chatgpt:aba").unwrap();
        let active = ChatGptTokens {
            access_token: "access-active".into(),
            refresh_token: "refresh-active".into(),
            id_token: None,
            account_id: "account-active".into(),
            identity: CredentialIdentity {
                authorization_endpoint: "https://auth.openai.com".into(),
                client_id: "client".into(),
                subscription_endpoint: "https://chatgpt.com/backend-api/codex".into(),
                observed_subject: None,
                observed_issuer: None,
                observed_audiences: Vec::new(),
                observed_scopes: Vec::new(),
            },
            expires_at: 100,
            generation: new_generation(),
            tombstone: false,
            mutation_pending: false,
        };
        let stale_generation = active.generation.clone();
        store
            .compare_and_swap(&reference, None, &serde_json::to_vec(&active).unwrap())
            .unwrap();
        let tombstone = ChatGptTokens::tombstone();
        store
            .compare_and_swap(
                &reference,
                Some(&stale_generation),
                &serde_json::to_vec(&tombstone).unwrap(),
            )
            .unwrap();
        assert!(store
            .compare_and_swap(
                &reference,
                Some(&stale_generation),
                &serde_json::to_vec(&active).unwrap(),
            )
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_file_store_rejects_symlinks_and_hardlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("credentials"));
        let hard_ref = CredentialRef::parse("chatgpt:hardlink").unwrap();
        let record = ChatGptTokens::tombstone();
        store
            .compare_and_swap(&hard_ref, None, &serde_json::to_vec(&record).unwrap())
            .unwrap();
        std::fs::hard_link(store.path(&hard_ref), dir.path().join("alias")).unwrap();
        assert!(store.load(&hard_ref).is_err());

        let symlink_ref = CredentialRef::parse("chatgpt:symlink").unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, serde_json::to_vec(&record).unwrap()).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, store.path(&symlink_ref)).unwrap();
        assert!(store.load(&symlink_ref).is_err());
    }

    #[tokio::test]
    async fn test_pending_device_login_is_bound_to_auth_context() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileCredentialStore::new(dir.path().join("credentials")));
        let first = ChatGptAuth::with_options(
            "http://127.0.0.1:1",
            "client-one",
            CredentialRef::parse("chatgpt:first").unwrap(),
            store.clone(),
        )
        .unwrap();
        let second = ChatGptAuth::with_options(
            "http://127.0.0.1:1",
            "client-two",
            CredentialRef::parse("chatgpt:second").unwrap(),
            store,
        )
        .unwrap();
        let pending = PendingDeviceLogin {
            user_code: "ABCD-EFGH".into(),
            verification_url: "http://127.0.0.1:1/codex/device".into(),
            device_auth_id: "device-auth".into(),
            poll_interval: Duration::from_secs(5),
            started_at: tokio::time::Instant::now(),
            expected_generation: None,
            auth_base_url: first.auth_base_url.clone(),
            client_id: first.client_id.clone(),
            credential_ref: first.credential_ref.as_str().to_string(),
        };
        let error = second
            .finish_device_login(&pending, CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("different Finch auth context"));
    }

    #[tokio::test]
    async fn test_logout_tombstone_prevents_late_login_resurrection() {
        let dir = tempfile::tempdir().unwrap();
        let auth = ChatGptAuth::with_options(
            "http://127.0.0.1:1",
            "client",
            CredentialRef::parse("chatgpt:late-login").unwrap(),
            Arc::new(FileCredentialStore::new(dir.path().join("credentials"))),
        )
        .unwrap();
        auth.require_generation(None).unwrap();
        auth.logout().await.unwrap();
        assert!(auth.require_generation(None).is_err());
        assert!(auth.account_status().unwrap().is_none());
    }

    #[tokio::test]
    async fn test_device_flow_rejects_server_poll_interval_over_bound() {
        let mut server = mockito::Server::new_async().await;
        let _user_code = server
            .mock("POST", "/api/accounts/deviceauth/usercode")
            .with_status(200)
            .with_body(
                json!({
                    "device_auth_id": "device-auth",
                    "user_code": "ABCD-EFGH",
                    "interval": 61
                })
                .to_string(),
            )
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let auth = ChatGptAuth::with_options(
            server.url(),
            "client",
            CredentialRef::parse("chatgpt:interval").unwrap(),
            Arc::new(FileCredentialStore::new(dir.path().join("credentials"))),
        )
        .unwrap();
        assert!(auth.begin_device_login().await.is_err());
    }

    #[test]
    fn test_legacy_plaintext_record_migrates_and_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let legacy_path = dir.path().join("auth").join("chatgpt.json");
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        write_private_file(
            &legacy_path,
            &serde_json::to_vec(&json!({
                "access_token": "legacy-access-token",
                "refresh_token": "legacy-refresh-token",
                "account_id": "legacy-account",
                "expires_at": unix_time() + 3600,
            }))
            .unwrap(),
        )
        .unwrap();
        let store = Arc::new(FileCredentialStore::new(dir.path().join("credentials")));
        let mut auth = ChatGptAuth::with_options(
            "http://127.0.0.1:1",
            "client",
            CredentialRef::parse("chatgpt:default").unwrap(),
            store,
        )
        .unwrap();
        auth.legacy_path = Some(legacy_path.clone());
        let status = auth.account_status().unwrap().unwrap();
        assert_eq!(status.account_id_suffix, "ccount");
        assert!(!legacy_path.exists());
    }

    #[tokio::test]
    async fn test_restart_detects_incomplete_remote_mutation_and_logout_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileCredentialStore::new(dir.path().join("credentials")));
        let reference = CredentialRef::parse("chatgpt:crash").unwrap();
        let marker = ChatGptTokens {
            access_token: String::new(),
            refresh_token: String::new(),
            id_token: None,
            account_id: String::new(),
            identity: CredentialIdentity::tombstone(),
            expires_at: 0,
            generation: new_generation(),
            tombstone: true,
            mutation_pending: true,
        };
        store
            .compare_and_swap(&reference, None, &serde_json::to_vec(&marker).unwrap())
            .unwrap();
        let restarted =
            ChatGptAuth::with_options("http://127.0.0.1:1", "client", reference, store).unwrap();
        assert!(restarted.account_status().is_err());
        restarted.logout().await.unwrap();
        assert!(restarted.account_status().unwrap().is_none());
    }

    #[tokio::test]
    async fn test_device_flow_exchanges_pkce_and_persists_tokens() {
        let mut server = mockito::Server::new_async().await;
        let access = jwt(json!({ "chatgpt_account_id": "acct-test", "exp": unix_time() + 3600 }));
        let _user_code = server
            .mock("POST", "/api/accounts/deviceauth/usercode")
            .match_body(mockito::Matcher::PartialJson(
                json!({"client_id": "client"}),
            ))
            .with_status(200)
            .with_body(
                json!({
                    "device_auth_id": "device",
                    "user_code": "ABCD-EFGH",
                    "interval": "1"
                })
                .to_string(),
            )
            .create_async()
            .await;
        let _poll = server
            .mock("POST", "/api/accounts/deviceauth/token")
            .with_status(200)
            .with_body(
                json!({
                    "authorization_code": "authorization",
                    "code_challenge": URL_SAFE_NO_PAD.encode(Sha256::digest(b"verifier")),
                    "code_verifier": "verifier"
                })
                .to_string(),
            )
            .create_async()
            .await;
        let _exchange = server
            .mock("POST", "/oauth/token")
            .match_body(mockito::Matcher::Regex(
                "grant_type=authorization_code.*code=authorization.*redirect_uri=.*code_verifier=verifier".into(),
            ))
            .with_status(200)
            .with_body(json!({
                "access_token": access,
                "refresh_token": "refresh-token",
                "id_token": jwt(json!({"chatgpt_account_id": "acct-test"}))
            }).to_string())
            .create_async().await;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileCredentialStore::new(dir.path().join("credentials")));
        let reference = CredentialRef::parse("chatgpt:test").unwrap();
        let auth = ChatGptAuth::with_options(server.url(), "client", reference, store).unwrap();
        let pending = auth.begin_device_login().await.unwrap();
        assert_eq!(pending.user_code, "ABCD-EFGH");
        let status = auth
            .finish_device_login(&pending, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(status.account_id_suffix, "t-test");
        assert!(auth.account_status().unwrap().is_some());
    }

    #[tokio::test]
    async fn test_device_flow_honors_cancellation_while_pending() {
        let mut server = mockito::Server::new_async().await;
        let _user_code = server
            .mock("POST", "/api/accounts/deviceauth/usercode")
            .with_status(200)
            .with_body(
                json!({
                    "device_auth_id": "device",
                    "user_code": "ABCD-EFGH",
                    "interval": "60"
                })
                .to_string(),
            )
            .create_async()
            .await;
        let _poll = server
            .mock("POST", "/api/accounts/deviceauth/token")
            .with_status(404)
            .with_body(json!({"error": "authorization_pending"}).to_string())
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let auth = ChatGptAuth::with_options(
            server.url(),
            "client",
            CredentialRef::parse("chatgpt:test").unwrap(),
            Arc::new(FileCredentialStore::new(dir.path().join("credentials"))),
        )
        .unwrap();
        let pending = auth.begin_device_login().await.unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = auth
            .finish_device_login(&pending, cancel)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }

    #[tokio::test]
    async fn test_device_flow_reports_denial_without_exposing_response_body() {
        let mut server = mockito::Server::new_async().await;
        let _user_code = server
            .mock("POST", "/api/accounts/deviceauth/usercode")
            .with_status(200)
            .with_body(
                json!({
                    "device_auth_id": "device",
                    "user_code": "ABCD-EFGH",
                    "interval": "1"
                })
                .to_string(),
            )
            .create_async()
            .await;
        let _poll = server
            .mock("POST", "/api/accounts/deviceauth/token")
            .with_status(403)
            .with_body(
                json!({
                    "error": "access_denied",
                    "message": "secret-server-detail"
                })
                .to_string(),
            )
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let auth = ChatGptAuth::with_options(
            server.url(),
            "client",
            CredentialRef::parse("chatgpt:denied").unwrap(),
            Arc::new(FileCredentialStore::new(dir.path().join("credentials"))),
        )
        .unwrap();
        let pending = auth.begin_device_login().await.unwrap();
        let error = auth
            .finish_device_login(&pending, CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("denied"));
        assert!(!error.contains("secret-server-detail"));
    }

    #[tokio::test]
    async fn test_refresh_rotation_is_singleflight_across_shared_profiles() {
        let mut server = mockito::Server::new_async().await;
        let access = jwt(json!({
            "chatgpt_account_id": "account-shared",
            "sub": "subject-shared",
            "exp": unix_time() + 3600
        }));
        let refresh = server
            .mock("POST", "/oauth/token")
            .match_body(mockito::Matcher::PartialJson(json!({
                "grant_type": "refresh_token",
                "refresh_token": "old-refresh",
                "client_id": "client"
            })))
            .with_status(200)
            .with_body(
                json!({
                    "access_token": access,
                    "refresh_token": "rotated-refresh",
                    "expires_in": 3600
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileCredentialStore::new(dir.path().join("credentials")));
        let reference = CredentialRef::parse("chatgpt:shared").unwrap();
        let old = ChatGptTokens {
            access_token: "old-access".into(),
            refresh_token: "old-refresh".into(),
            id_token: None,
            account_id: "account-shared".into(),
            identity: CredentialIdentity {
                authorization_endpoint: server.url(),
                client_id: "client".into(),
                subscription_endpoint: "https://chatgpt.com/backend-api/codex".into(),
                observed_subject: Some("subject-shared".into()),
                observed_issuer: None,
                observed_audiences: Vec::new(),
                observed_scopes: Vec::new(),
            },
            expires_at: 1,
            generation: new_generation(),
            tombstone: false,
            mutation_pending: false,
        };
        store
            .compare_and_swap(&reference, None, &serde_json::to_vec(&old).unwrap())
            .unwrap();
        let first =
            ChatGptAuth::with_options(server.url(), "client", reference.clone(), store.clone())
                .unwrap();
        let second = ChatGptAuth::with_options(server.url(), "client", reference, store).unwrap();
        let (one, two) = tokio::join!(first.tokens(), second.tokens());
        assert_eq!(one.unwrap().refresh_token.as_str(), "rotated-refresh");
        assert_eq!(two.unwrap().refresh_token.as_str(), "rotated-refresh");
        refresh.assert_async().await;
    }

    #[tokio::test]
    async fn test_logout_revokes_shared_account_record_then_removes_it() {
        let mut server = mockito::Server::new_async().await;
        let revoke = server
            .mock("POST", "/oauth/revoke")
            .match_body(mockito::Matcher::PartialJson(json!({
                "token": "refresh-token",
                "token_type_hint": "refresh_token",
                "client_id": "client"
            })))
            .with_status(200)
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileCredentialStore::new(dir.path().join("credentials")));
        let reference = CredentialRef::parse("chatgpt:logout").unwrap();
        let tokens = ChatGptTokens {
            access_token: "access-token".into(),
            refresh_token: "refresh-token".into(),
            id_token: None,
            account_id: "account-logout".into(),
            identity: CredentialIdentity {
                authorization_endpoint: server.url(),
                client_id: "client".into(),
                subscription_endpoint: "https://chatgpt.com/backend-api/codex".into(),
                observed_subject: None,
                observed_issuer: None,
                observed_audiences: Vec::new(),
                observed_scopes: Vec::new(),
            },
            expires_at: unix_time() + 3600,
            generation: new_generation(),
            tombstone: false,
            mutation_pending: false,
        };
        store
            .compare_and_swap(&reference, None, &serde_json::to_vec(&tokens).unwrap())
            .unwrap();
        let auth = ChatGptAuth::with_options(server.url(), "client", reference, store).unwrap();
        auth.logout().await.unwrap();
        assert!(auth.account_status().unwrap().is_none());
        revoke.assert_async().await;
    }
}
