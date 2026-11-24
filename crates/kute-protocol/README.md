# kute-protocol

A small protocol by which two KUTE peers agree on a vocabulary before
exchanging encoded data: handshake, capability exchange, vocabulary
announcement and transfer with digest verification, proposals that extend a
shared vocabulary, and encoded messages acknowledged with a hash of what the
receiver decoded.

The peer logic is a plain state machine (`Peer::handle`) with no I/O.
`Framed` carries messages over any byte stream as length-prefixed JSON.

It provides no encryption and no authentication. Run it over a channel you
already trust, or one that provides both.

Part of [KUTE](https://github.com/kannaka-labs/kute), the Kannaka Universal Tokenization Engine.

Experimental: the message set and framing may change before 1.0.

Licensed under MIT or Apache-2.0, at your option.
