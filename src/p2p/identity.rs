//! PGP identity: key generation, persistent load-or-generate, armoured export.
//!
//! Adapted from pgp-chat-core/src/crypto/identity.rs.
//! Changes vs. original:
//!   - User ID format: `"<nickname@cmp-p2p>"` instead of `"<nickname@pgp-chat>"`
//!   - Added `load_or_generate()` for config-backed persistence
//!   - Uses `anyhow::Error` instead of `crate::error::Error`

use std::io::Cursor;

use pgp::{
    composed::{
        key::{SecretKeyParamsBuilder, SubkeyParamsBuilder},
        Deserializable, KeyType, SignedPublicKey, SignedSecretKey,
    },
    crypto::{ecc_curve::ECCCurve, hash::HashAlgorithm, sym::SymmetricKeyAlgorithm},
    types::{CompressionAlgorithm, KeyTrait, SecretKeyTrait},
    ArmorOptions,
};
use smallvec::smallvec;
use zeroize::{Zeroize, Zeroizing};

// ---------------------------------------------------------------------------
// Public type
// ---------------------------------------------------------------------------

/// A node's long-term PGP identity.
///
/// Primary key: EdDSA (sign + certify).  Subkey: ECDH Curve25519 (encrypt).
/// The passphrase is wrapped in `Zeroizing` and zeroed on drop.
pub struct PgpIdentity {
    secret_key: SignedSecretKey,
    public_key: SignedPublicKey,
    user_id:    String,
    nickname:   String,
    passphrase: Zeroizing<String>,
}

impl Drop for PgpIdentity {
    fn drop(&mut self) {
        self.passphrase.zeroize();
    }
}

impl PgpIdentity {
    // -----------------------------------------------------------------------
    // load_or_generate  — config-backed entry point
    // -----------------------------------------------------------------------

    /// Load an existing identity from armoured secret key + passphrase, or
    /// generate a fresh keypair and return the armoured secret key + passphrase
    /// so the caller can persist them.
    ///
    /// Returns `(identity, Option<(armored_secret, passphrase)>)`:
    /// - `None` → loaded from existing config (nothing new to persist)
    /// - `Some(…)` → freshly generated; caller must write these to config
    pub fn load_or_generate(
        nickname: &str,
        stored_armored: Option<&str>,
        stored_passphrase: Option<&str>,
    ) -> anyhow::Result<(Self, Option<(String, String)>)> {
        if let (Some(armored), Some(pw)) = (stored_armored, stored_passphrase) {
            let identity = Self::from_armored_secret_key(
                nickname,
                armored,
                Zeroizing::new(pw.to_string()),
            )?;
            return Ok((identity, None));
        }

        // Generate fresh keypair + random passphrase
        let passphrase = generate_passphrase();
        let identity = Self::generate(nickname, Zeroizing::new(passphrase.clone()))?;
        let armored = identity.secret_key_armored()?;
        Ok((identity, Some((armored, passphrase))))
    }

    // -----------------------------------------------------------------------
    // Construction — generate a fresh keypair
    // -----------------------------------------------------------------------

    pub fn generate(nickname: &str, passphrase: Zeroizing<String>) -> anyhow::Result<Self> {
        let user_id = format!("{} <{}@cmp-p2p>", nickname, nickname.to_lowercase());

        let pw_for_sign = passphrase.clone();
        let pw_for_pub  = passphrase.clone();

        let params = SecretKeyParamsBuilder::default()
            .key_type(KeyType::EdDSA)
            .can_certify(true)
            .can_sign(true)
            .primary_user_id(user_id.clone())
            .preferred_symmetric_algorithms(smallvec![
                SymmetricKeyAlgorithm::AES256,
                SymmetricKeyAlgorithm::AES128,
            ])
            .preferred_hash_algorithms(smallvec![
                HashAlgorithm::SHA2_256,
                HashAlgorithm::SHA2_512,
            ])
            .preferred_compression_algorithms(smallvec![
                CompressionAlgorithm::ZLIB,
                CompressionAlgorithm::ZIP,
            ])
            .subkeys(vec![
                SubkeyParamsBuilder::default()
                    .key_type(KeyType::ECDH(ECCCurve::Curve25519))
                    .can_encrypt(true)
                    .build()
                    .map_err(|e| anyhow::anyhow!("subkey build: {e}"))?,
            ])
            .build()
            .map_err(|e| anyhow::anyhow!("key params build: {e}"))?;

        let secret_key = params
            .generate()
            .map_err(|e| anyhow::anyhow!("key generate: {e}"))?;

        let signed_secret = secret_key
            .sign(move || pw_for_sign.as_str().to_owned())
            .map_err(|e| anyhow::anyhow!("self-sign: {e}"))?;

        let pub_key = signed_secret.public_key();
        let signed_public = pub_key
            .sign(&signed_secret, move || pw_for_pub.as_str().to_owned())
            .map_err(|e| anyhow::anyhow!("pub-sign: {e}"))?;

        Ok(Self {
            secret_key: signed_secret,
            public_key: signed_public,
            user_id,
            nickname: nickname.to_string(),
            passphrase,
        })
    }

    // -----------------------------------------------------------------------
    // Construction — import from ASCII armour
    // -----------------------------------------------------------------------

    pub fn from_armored_secret_key(
        nickname: &str,
        armored: &str,
        passphrase: Zeroizing<String>,
    ) -> anyhow::Result<Self> {
        let pw_for_pub = passphrase.clone();

        let (signed_secret, _headers) =
            SignedSecretKey::from_armor_single(Cursor::new(armored.as_bytes()))
                .map_err(|e| anyhow::anyhow!("key parse: {e}"))?;

        let pub_key = signed_secret.public_key();
        let signed_public = pub_key
            .sign(&signed_secret, move || pw_for_pub.as_str().to_owned())
            .map_err(|e| anyhow::anyhow!("pub-sign: {e}"))?;

        let user_id = signed_secret
            .details
            .users
            .first()
            .map(|u| u.id.id().to_string())
            .unwrap_or_else(|| format!("{} <{}@cmp-p2p>", nickname, nickname.to_lowercase()));

        Ok(Self {
            secret_key: signed_secret,
            public_key: signed_public,
            user_id,
            nickname: nickname.to_string(),
            passphrase,
        })
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    pub fn nickname(&self)    -> &str             { &self.nickname }
    pub fn user_id(&self)     -> &str             { &self.user_id }
    pub fn public_key(&self)  -> &SignedPublicKey { &self.public_key }
    pub fn secret_key(&self)  -> &SignedSecretKey { &self.secret_key }

    /// Hex-encoded fingerprint (lowercase, no spaces).
    pub fn fingerprint(&self) -> String {
        hex::encode(self.public_key.fingerprint())
    }

    /// Export the public key as ASCII armour.
    pub fn public_key_armored(&self) -> anyhow::Result<String> {
        self.public_key
            .to_armored_string(ArmorOptions::default())
            .map_err(|e| anyhow::anyhow!("armor export: {e}"))
    }

    /// Export the secret key as ASCII armour (protected by stored passphrase).
    pub fn secret_key_armored(&self) -> anyhow::Result<String> {
        self.secret_key
            .to_armored_string(ArmorOptions::default())
            .map_err(|e| anyhow::anyhow!("armor export: {e}"))
    }

    /// Passphrase closure for rPGP sign/decrypt calls.
    pub(crate) fn passphrase_fn(&self) -> impl Fn() -> String + Clone + '_ {
        let pw = self.passphrase.clone();
        move || pw.as_str().to_owned()
    }
}

impl std::fmt::Debug for PgpIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgpIdentity")
            .field("user_id",     &self.user_id)
            .field("fingerprint", &self.fingerprint())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Generate a 24-character random alphanumeric passphrase.
/// Beta convenience — TODO: move to OS keychain before stable release.
fn generate_passphrase() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..24)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}
