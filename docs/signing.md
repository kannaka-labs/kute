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
* **Nothing checks signatures unless you ask.** Loading, encoding and
  `kute serve` read and keep signatures and check none. `kute verify`
  checks a file; `kute peer serve --require-signature-from` checks what
  the other side sends (below).
* **No registry, no chain of keys, no expiry, no revocation.** The list
  of keys you trust is the `--trusted-key` options you pass. To stop
  trusting a key, stop passing it.
* **No protection for the private key** beyond file permissions.
* A signature can be removed from a file by anyone; that leaves an
  unsigned vocabulary, which verifies against nothing.

## A peer that takes only signed vocabularies

    kute peer serve --listen 127.0.0.1:7000 --accept-vocabularies \
        --require-signature-from <public key> ...

With this option (repeatable), a vocabulary the other side sends is
taken only if it carries a valid signature from one of the keys named.
One that is unsigned, signed by another key, or carrying a signature
made for other content is refused, nothing is kept, and the sender is
told why. The check is made on the content received, after its digest
has been recomputed.

What it leaves alone:

* Vocabularies you give the peer yourself with `--vocab` are not
  checked. You chose them.
* It cannot be combined with `--accept-proposals`. A proposal makes the
  peer derive a new vocabulary from one it holds, and nobody has signed
  the result; a peer that requires signatures refuses proposals. To
  extend a vocabulary between such peers, evolve it, sign it and send
  it whole.
* It is a second lock, not a replacement for the first. Who may connect
  at all is still the TLS certificate and `--allow`. The signature says
  who vouches for the vocabulary, which need not be who delivers it.
* A malformed key is refused when the command starts, before it listens.
* The refusal says whether the vocabulary was unsigned or signed by a
  key the peer does not trust. Signed vocabularies are public files, so
  anyone allowed to connect can learn which signers a peer trusts by
  offering them. The texts are kept because the sender needs them; do
  not treat the list of trusted keys as secret.
* A signature does not expire. Trusting a key trusts everything it has
  ever signed, including what it signed before you would have trusted it.
* That includes an earlier run's output: a vocabulary this peer received
  and you later pass back with `--vocab` is yours by then, unchecked.

Reviewed by reading every place a received vocabulary is kept or used;
no way past the check was found. That is one reader, not a proof.

These are the first two steps of the specification's B8. A registry and
federation are not built.
