//! PGP crypto primitives: asymmetric encrypt/decrypt, detached sign/verify,
//! and symmetric seal/open for gossipsub payloads.
//!
//! Merged and adapted from pgp-chat-core/src/crypto/{encrypt,sign,room_cipher}.rs.
//! Error handling uses anyhow instead of the chat crate's custom Error type.

use std::io::Cursor;

use pgp::{
    composed::{message::Message, Deserializable, SignedPublicKey, SignedSecretKey, StandaloneSignature},
    crypto::{hash::HashAlgorithm, sym::SymmetricKeyAlgorithm},
    packet::{SignatureConfig, SignatureType, SignatureVersion},
    ser::Serialize,
    types::{KeyTrait, StringToKey},
};

// ---------------------------------------------------------------------------
// Asymmetric encryption / decryption
// ---------------------------------------------------------------------------

/// Encrypt `plaintext` to one or more recipients' public keys (AES-256).
///
/// Uses each recipient's ECDH encryption subkey.  The EdDSA primary key is
/// sign-only and cannot be used for encryption.
pub fn encrypt_for_recipients(
    plaintext: &[u8],
    recipients: &[&SignedPublicKey],
) -> anyhow::Result<Vec<u8>> {
    if recipients.is_empty() {
        anyhow::bail!("no recipients specified");
    }

    let enc_subkeys: Vec<_> = recipients
        .iter()
        .flat_map(|pk| pk.public_subkeys.iter())
        .collect();

    if enc_subkeys.is_empty() {
        anyhow::bail!("recipients have no encryption subkeys");
    }

    let literal = Message::new_literal_bytes("msg", plaintext);
    let encrypted = literal
        .encrypt_to_keys(
            &mut rand::thread_rng(),
            SymmetricKeyAlgorithm::AES256,
            &enc_subkeys,
        )
        .map_err(|e| anyhow::anyhow!("pgp encrypt: {e}"))?;

    let mut buf = Vec::new();
    encrypted
        .to_writer(&mut buf)
        .map_err(|e| anyhow::anyhow!("pgp serialize: {e}"))?;
    Ok(buf)
}

/// Decrypt a PGP-encrypted blob using `secret_key`.
///
/// `passphrase` is called by rPGP to unlock the secret key.
/// Pass `|| String::new()` for unprotected keys.
pub fn decrypt_message(
    ciphertext: &[u8],
    secret_key: &SignedSecretKey,
    passphrase: impl FnOnce() -> String + Clone,
) -> anyhow::Result<Vec<u8>> {
    let msg = Message::from_bytes(Cursor::new(ciphertext))
        .map_err(|e| anyhow::anyhow!("pgp parse: {e}"))?;

    let (decrypted, _key_ids) = msg
        .decrypt(passphrase, &[secret_key])
        .map_err(|_| anyhow::anyhow!("decryption failed — wrong key or corrupt data"))?;

    decrypted
        .get_content()
        .map_err(|e| anyhow::anyhow!("pgp content: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("decrypted message has no content"))
}

// ---------------------------------------------------------------------------
// Detached signatures
// ---------------------------------------------------------------------------

/// Create a detached PGP signature (v4, EdDSA) over `data`.
///
/// Returns a serialised `StandaloneSignature` packet.
pub fn sign_data(
    data: &[u8],
    secret_key: &SignedSecretKey,
    passphrase: impl Fn() -> String,
) -> anyhow::Result<Vec<u8>> {
    let config = SignatureConfig::new_v4(
        SignatureVersion::V4,
        SignatureType::Binary,
        secret_key.algorithm(),
        HashAlgorithm::SHA2_256,
        vec![],
        vec![],
    );

    let sig_packet = config
        .sign(secret_key, passphrase, Cursor::new(data))
        .map_err(|e| anyhow::anyhow!("pgp sign: {e}"))?;

    let standalone = StandaloneSignature::new(sig_packet);
    let mut buf = Vec::new();
    standalone
        .to_writer(&mut buf)
        .map_err(|e| anyhow::anyhow!("pgp serialize: {e}"))?;
    Ok(buf)
}

/// Verify a detached signature produced by [`sign_data`].
///
/// Returns `true` if valid.  Never returns `Err` for an invalid signature —
/// callers can display a warning rather than abort.
pub fn verify_data(
    data: &[u8],
    signature_bytes: &[u8],
    public_key: &SignedPublicKey,
) -> anyhow::Result<bool> {
    let standalone = StandaloneSignature::from_bytes(Cursor::new(signature_bytes))
        .map_err(|e| anyhow::anyhow!("sig parse: {e}"))?;
    Ok(standalone.verify(public_key, data).is_ok())
}

// ---------------------------------------------------------------------------
// Symmetric seal / open (gossipsub transport layer)
// ---------------------------------------------------------------------------

/// Symmetrically encrypt `plaintext` with a `passphrase` (AES-256, Argon2 S2K).
///
/// Used to wrap all gossipsub payloads so eavesdroppers see opaque OpenPGP
/// packets rather than application-recognisable wire format.
pub fn seal(plaintext: &[u8], passphrase: &str) -> anyhow::Result<Vec<u8>> {
    let mut rng = rand::thread_rng();
    let s2k = StringToKey::new_default(&mut rng);
    let pw = passphrase.to_string();

    let msg = Message::new_literal_bytes("", plaintext);
    let encrypted = msg
        .encrypt_with_password(&mut rng, s2k, SymmetricKeyAlgorithm::AES256, || {
            pw.clone()
        })
        .map_err(|e| anyhow::anyhow!("sym encrypt: {e}"))?;

    let mut buf = Vec::new();
    encrypted
        .to_writer(&mut buf)
        .map_err(|e| anyhow::anyhow!("sym serialize: {e}"))?;
    Ok(buf)
}

/// Decrypt a symmetrically-encrypted gossipsub payload.
pub fn open(ciphertext: &[u8], passphrase: &str) -> anyhow::Result<Vec<u8>> {
    let pw = passphrase.to_string();
    let msg = Message::from_bytes(Cursor::new(ciphertext))
        .map_err(|e| anyhow::anyhow!("sym parse: {e}"))?;

    let decrypted = msg
        .decrypt_with_password(|| pw)
        .map_err(|_| anyhow::anyhow!("sym decryption failed"))?;

    decrypted
        .get_content()
        .map_err(|e| anyhow::anyhow!("sym content: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("decrypted message has no content"))
}
