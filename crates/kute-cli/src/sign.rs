//! Signatures on vocabularies: `kute keygen`, `kute sign`, `kute verify`.
//!
//! A signature says that the holder of a key vouches for a vocabulary's
//! content. It is Ed25519 over a fixed prefix and the vocabulary's
//! digest, kept in the file beside the digest and outside what the digest
//! covers, so signing changes neither the digest nor anything that
//! refers to the vocabulary by it.
//!
//! Verifying needs the signer's public key from somewhere the file is
//! not: a signature in a file proves nothing to someone who takes the key
//! from the same file. There is no registry and no chain of keys here;
//! whoever verifies says which keys they trust.

use base64::Engine as _;
use kute::{vocab, KuteError};
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use serde_json::{json, Value};
use std::path::Path;

pub const ALGORITHM: &str = "ed25519";
/// Most signatures one vocabulary may carry.
pub const MAX_SIGNATURES: usize = 16;
/// What is signed is this line followed by the digest. The prefix keeps a
/// signature made here from being a valid signature of anything else the
/// same key signs.
const CONTEXT: &str = "kute-vocabulary-signature-v1\n";
const PEM_HEAD: &str = "-----BEGIN PRIVATE KEY-----";
const PEM_TAIL: &str = "-----END PRIVATE KEY-----";

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn message(digest: &str) -> Vec<u8> {
    format!("{CONTEXT}{digest}").into_bytes()
}

/// A key is named by the hash of its public half.
pub fn key_id(public: &[u8]) -> String {
    format!("{ALGORITHM}:{}", kute::sha256_tagged(public).trim_start_matches("sha256:"))
}

fn bad_key(why: &str) -> KuteError {
    KuteError::InvalidInput(format!("not a usable signing key: {why}"))
}

fn load_key(path: &Path) -> kute::Result<Ed25519KeyPair> {
    if std::fs::metadata(path)?.len() > 16 * 1024 {
        return Err(bad_key("the file is larger than a key"));
    }
    let text = std::fs::read_to_string(path).map_err(|_| bad_key("the file is not text"))?;
    let body = text.trim().strip_prefix(PEM_HEAD).and_then(|rest| rest.strip_suffix(PEM_TAIL)).ok_or_else(|| bad_key("expected one PEM block labelled PRIVATE KEY"))?;
    let der = b64().decode(body.split_whitespace().collect::<String>()).map_err(|_| bad_key("the PEM body is not base64"))?;
    Ed25519KeyPair::from_pkcs8(&der).map_err(|_| bad_key("not an Ed25519 key in PKCS#8"))
}

fn public_of(text: &str) -> kute::Result<Vec<u8>> {
    let key = b64().decode(text.trim()).map_err(|_| KuteError::InvalidInput(format!("a trusted key is not base64: {text:?}")))?;
    if key.len() != 32 {
        return Err(KuteError::InvalidInput(format!("a trusted key is {} bytes; an Ed25519 public key is 32", key.len())));
    }
    Ok(key)
}

/// Make a key and write its private half to `output`, which must not exist.
pub fn keygen(output: &Path) -> kute::Result<Value> {
    let random = ring::rand::SystemRandom::new();
    let document = Ed25519KeyPair::generate_pkcs8(&random).map_err(|_| KuteError::Backend("the system gave no randomness".into()))?;
    let pair = Ed25519KeyPair::from_pkcs8(document.as_ref()).map_err(|_| KuteError::Backend("the key just made cannot be read back".into()))?;
    let encoded = b64().encode(document.as_ref());
    let mut pem = String::from(PEM_HEAD);
    pem.push('\n');
    for line in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str(PEM_TAIL);
    pem.push('\n');
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options.open(output)?.write_all(pem.as_bytes())?;
    let public = pair.public_key().as_ref();
    Ok(json!({
        "private_key_file": output,
        "public_key": b64().encode(public),
        "key_id": key_id(public),
        "algorithm": ALGORITHM,
        "keep": "the private key file is not encrypted. On Unix it is readable by its owner only; on Windows it has the permissions of its folder.",
    }))
}

/// Add this key's signature to the vocabulary and write it to `output`.
pub fn sign(vocab_path: &Path, key_path: &Path, output: &Path, force: bool) -> kute::Result<Value> {
    let pair = load_key(key_path)?;
    let mut vocabulary = vocab::load(vocab_path)?;
    // The digest of what is there, not the digest the file states.
    let digest = vocabulary.compute_digest()?;
    vocabulary.seal()?;
    let id = key_id(pair.public_key().as_ref());
    let integrity = vocabulary.integrity.as_mut().expect("sealed");
    // An entry under this key's name is only a claim until it is checked:
    // anyone can write one. The signature is made again and takes the place
    // of whatever was there, so a file that arrives with a false entry
    // under the signer's name leaves with a true one. Ed25519 gives the
    // same bytes for the same key and message, so a true entry is unchanged.
    let signature = b64().encode(pair.sign(&message(&digest)).as_ref());
    let before = integrity.signatures.len();
    let already = integrity.signatures.iter().any(|s| s.key_id == id && s.algorithm == ALGORITHM && s.signature == signature);
    integrity.signatures.retain(|s| s.key_id != id);
    let replaced = before - integrity.signatures.len() - usize::from(already);
    if integrity.signatures.len() >= MAX_SIGNATURES {
        return Err(KuteError::LimitExceeded(format!("the vocabulary already carries {MAX_SIGNATURES} signatures")));
    }
    integrity.signatures.push(kute::Signature { key_id: id.clone(), algorithm: ALGORITHM.into(), signature });
    vocab::save(&vocabulary, output, force)?;
    let written = vocab::load(output)?;
    Ok(json!({
        "output": output,
        "digest": digest,
        "digest_unchanged": written.digest()? == digest,
        "key_id": id,
        "already_signed_by_this_key": already,
        "entries_under_this_key_replaced": replaced,
        "signatures": written.integrity.map_or(0, |i| i.signatures.len()),
    }))
}

/// Check the vocabulary's signatures against the keys the caller trusts.
/// Returns the report and whether at least one trusted key's signature holds.
pub fn verify(vocab_path: &Path, trusted: &[String]) -> kute::Result<(Value, bool)> {
    if trusted.is_empty() {
        return Err(KuteError::InvalidConfig("verify needs at least one --trusted-key: a signature is only worth the key it is checked against".into()));
    }
    let keys = trusted.iter().map(|t| public_of(t)).collect::<kute::Result<Vec<_>>>()?;
    let vocabulary = vocab::load(vocab_path)?;
    let (digest, rows, held) = check(&vocabulary, &keys)?;
    let verified = held > 0;
    Ok((
        json!({
            "vocabulary": vocab_path,
            "digest": digest,
            "signatures": rows,
            "trusted_keys_given": keys.iter().map(|k| key_id(k)).collect::<Vec<_>>(),
            "trusted_keys_with_a_valid_signature": held,
            "verified": verified,
            "means": "a valid signature says the holder of that key signed this exact content. It says nothing about whether the content is good, and nothing about who holds the key.",
        }),
        verified,
    ))
}

/// A test for `kute peer serve --require-signature-from`: passes a
/// vocabulary that carries a valid signature from one of `trusted`.
pub fn gate(trusted: &[String]) -> kute::Result<impl Fn(&kute::Vocabulary) -> Result<(), String> + Send + 'static> {
    let keys = trusted.iter().map(|t| public_of(t)).collect::<kute::Result<Vec<_>>>()?;
    Ok(move |vocabulary: &kute::Vocabulary| match check(vocabulary, &keys) {
        Ok((_, _, held)) if held > 0 => Ok(()),
        Ok((_, rows, _)) if rows.is_empty() => Err("it carries no signature, and this peer requires one from a key it trusts".to_string()),
        Ok(_) => Err("none of its signatures is a valid one from a key this peer trusts".to_string()),
        Err(e) => Err(e.to_string()),
    })
}

/// The digest computed from content, each signature's standing against
/// `keys`, and how many of the keys have a valid signature.
fn check(vocabulary: &kute::Vocabulary, keys: &[Vec<u8>]) -> kute::Result<(String, Vec<Value>, usize)> {
    let digest = vocabulary.compute_digest()?;
    let signatures = vocabulary.integrity.as_ref().map(|i| i.signatures.clone()).unwrap_or_default();
    if signatures.len() > MAX_SIGNATURES {
        return Err(KuteError::LimitExceeded(format!("the vocabulary carries {} signatures; at most {MAX_SIGNATURES} are read", signatures.len())));
    }
    let mut rows = Vec::new();
    // Keys, not entries: the same signature written sixteen times is one
    // key vouching once.
    let mut holding: Vec<&Vec<u8>> = Vec::new();
    for signature in &signatures {
        let status = if signature.algorithm != ALGORITHM {
            "unsupported algorithm"
        } else {
            match keys.iter().find(|k| key_id(k) == signature.key_id) {
                None => "not from a trusted key",
                Some(key) => match b64().decode(&signature.signature) {
                    Ok(bytes) if UnparsedPublicKey::new(&ED25519, key).verify(&message(&digest), &bytes).is_ok() => {
                        if holding.contains(&key) {
                            "valid, repeats an earlier entry"
                        } else {
                            holding.push(key);
                            "valid"
                        }
                    }
                    _ => "INVALID",
                },
            }
        };
        rows.push(json!({ "key_id": signature.key_id, "algorithm": signature.algorithm, "status": status }));
    }
    let held = holding.len();
    Ok((digest, rows, held))
}
