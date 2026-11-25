# Signing a vocabulary

    kute keygen --output publisher.pem
    kute sign --vocab events.kvf --key publisher.pem --output events-signed.kvf
    kute verify --vocab events-signed.kvf --trusted-key <the public key keygen printed>

Stability: **experimental**.

A digest says what a vocabulary's content is. A signature says that the
holder of a key vouches for that content. Use it where a vocabulary
travels by a way you do not control: a download, a shared folder, a
message.

## What each command does

`kute keygen` makes an Ed25519 key. The private half is written to the
file named, as PKCS#8 in PEM, **not encrypted**; the file must not
already exist. On Unix it is readable by its owner only. On Windows it
has the permissions of the folder it is in. The public half and the
key's id (`ed25519:` and the SHA-256 of the public key) are printed.
Give the public half to whoever is to verify, by a way they have reason
to believe.

`kute sign` adds the key's signature to a vocabulary and writes it out.
The digest does not change: signatures are kept beside the digest and
outside what it covers, so a signed vocabulary is the same vocabulary to
everything that names it by digest, and encodes identically. A
vocabulary can carry up to 16 signatures. Signing twice with one key
adds nothing. An entry already in the file under the signer's key is not
trusted: the signature is made again and replaces it, so a file that
arrives with a false entry under your key leaves with a true one.

`kute verify` checks the signatures against the public keys you name
with `--trusted-key` (repeatable). It exits 0 only if at least one of
those keys has a valid signature of this exact content, and prints each
signature as `valid`, `INVALID`, `not from a trusted key` or
`unsupported algorithm`.

## What a signature covers

The signature is over a fixed line, `kute-vocabulary-signature-v1`, and
the digest, which the verifier computes from the content and does not
take from the file. So:

* change the content and the file no longer loads (the digest check),
  let alone verifies;
* a signature copied onto another vocabulary is `INVALID` there;
* a vocabulary made from a signed one (`kute evolve`) is new content
  and starts with no signatures;
* converting between the text and compact forms keeps the signatures.

## What it does not do

* **It does not say the vocabulary is good**, only that the key holder
  signed it. It does not say who holds the key.
* **Nothing checks signatures unless you run `kute verify`.** Loading,
  encoding, `kute peer` and `kute serve` read and keep signatures and
  check none. A peer accepting vocabularies does not yet require one.
* **No registry, no chain of keys, no expiry, no revocation.** The list
  of keys you trust is the `--trusted-key` options you pass. To stop
  trusting a key, stop passing it.
* **No protection for the private key** beyond file permissions.
* A signature can be removed from a file by anyone; that leaves an
  unsigned vocabulary, which verifies against nothing.

This is the first step of the specification's B8. A registry and
federation are not built.
