//! OS credential store integration for the P2P identity passphrase.
//!
//! # Why this exists
//!
//! The P2P identity is a PGP keypair.  The secret key is stored armoured in
//! `config.json`, protected by a passphrase.  Storing the passphrase *in the
//! same file* as the encrypted key defeats the protection entirely — anyone
//! who can read `config.json` gets both pieces and can reconstruct the raw
//! secret key.
//!
//! This module keeps the passphrase in the platform's native credential store:
//!
//! | Platform | Backend |
//! |----------|---------|
//! | Windows  | Credential Manager (`wincred`) |
//! | macOS    | Keychain |
//! | Linux    | Secret Service (GNOME Keyring / KDE Wallet via D-Bus) |
//!
//! With the passphrase in the credential store, an attacker needs to
//! compromise two distinct system boundaries (filesystem *and* credential
//! store) to reconstruct the private key.
//!
//! # Fallback behaviour
//!
//! Two situations cause a graceful fallback to `config.json` storage:
//!
//! 1. **Android / Termux** — no credential store is accessible from a terminal
//!    process.  The `keyring` dependency is excluded entirely at compile time
//!    via `[target.'cfg(not(target_os = "android"))'.dependencies]`.
//!
//! 2. **Headless Linux** — a secret service daemon may not be running (common
//!    on servers, some WSL setups, or minimal desktop installs).  The keyring
//!    call fails at runtime and we fall back with a warning toast.
//!
//! In both cases `StoreOutcome::ConfigFallback` is returned so the caller can
//! write the passphrase to `config.json` and show an appropriate warning.

const SERVICE: &str = "console-music-player";
const ACCOUNT: &str = "p2p-identity-passphrase";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of a [`store_passphrase`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOutcome {
    /// Passphrase is now secured in the OS credential store.
    /// The caller should ensure it is *not* written to `config.json`.
    Keychain,
    /// Credential store unavailable for the given reason.
    /// The caller must write the passphrase to `config.json` as a fallback.
    ConfigFallback(String),
}

// ---------------------------------------------------------------------------
// Public API  (always present, compiles to no-ops on Android)
// ---------------------------------------------------------------------------

/// Store `passphrase` in the OS credential store.
///
/// Returns [`StoreOutcome::Keychain`] on success.  Returns
/// [`StoreOutcome::ConfigFallback`] with a reason string when the credential
/// store is unavailable — the caller is then responsible for storing the
/// passphrase in `config.json`.
pub fn store_passphrase(passphrase: &str) -> StoreOutcome {
    #[cfg(not(target_os = "android"))]
    {
        match imp::store(passphrase) {
            Ok(()) => return StoreOutcome::Keychain,
            Err(reason) => return StoreOutcome::ConfigFallback(reason),
        }
    }
    #[cfg(target_os = "android")]
    {
        let _ = passphrase;
        StoreOutcome::ConfigFallback(
            "OS credential store not available on Android/Termux".into(),
        )
    }
}

/// Load the passphrase from the OS credential store.
///
/// Returns `None` if the entry does not exist or the credential store is
/// unavailable.  The caller should then try `config.p2p_identity_passphrase`
/// as a migration fallback.
pub fn load_passphrase() -> Option<String> {
    #[cfg(not(target_os = "android"))]
    {
        imp::load().ok()
    }
    #[cfg(target_os = "android")]
    {
        None
    }
}

/// Delete the passphrase from the OS credential store.
///
/// Called when the P2P identity is reset.  Errors are silently ignored —
/// the entry may not exist if the user was on a fallback platform.
pub fn delete_passphrase() {
    #[cfg(not(target_os = "android"))]
    {
        let _ = imp::delete();
    }
}

// ---------------------------------------------------------------------------
// Platform implementation (excluded on Android)
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "android"))]
mod imp {
    use super::{ACCOUNT, SERVICE};

    pub fn store(passphrase: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT)
            .map_err(|e| format!("credential store unavailable: {e}"))?;
        entry
            .set_password(passphrase)
            .map_err(|e| format!("could not write to credential store: {e}"))
    }

    pub fn load() -> Result<String, keyring::Error> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT)?;
        entry.get_password()
    }

    pub fn delete() -> Result<(), keyring::Error> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT)?;
        entry.delete_credential()
    }
}
