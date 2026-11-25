//! `kute`: train, encode, decode, inspect, validate, compare, export, benchmark.
//!
//! Results go to stdout as JSON (or raw bytes for `decode`), messages to
//! stderr. Exit status is 0 on success and 1 on any error.

pub mod http;
pub mod live;
pub mod shadow;
pub mod mcp;

use clap::{Parser, Subcommand, ValueEnum};
use kute::{vocab, AdapterConfig, Engine, KuteError, Pretokenizer, TokenSequence, TrainingConfig, Version, VocabularyId};
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "kute", version, about = "Kannaka Universal Tokenization Engine", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Learn a vocabulary from files and write it as KVF.
    Train(Box<TrainArgs>),
    /// Encode a file to a .kute token sequence.
    Encode {
        #[arg(long)]
        vocab: PathBuf,
        #[arg(long)]
        input: PathBuf,
        /// Where to write the .kute container. Without it, token ids are printed as JSON.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Write the compressed container (format 2) instead of plain varint ids.
        #[arg(long, requires = "output")]
        compress: bool,
        /// What to do with a symbol the vocabulary does not contain:
        /// `strict` refuses the input, `escape` carries the symbol literally
        /// in the sequence so it still decodes exactly.
        #[arg(long, default_value = "strict")]
        unknown: String,
    },
    /// Decode a .kute token sequence back to bytes.
    Decode {
        #[arg(long)]
        vocab: PathBuf,
        #[arg(long)]
        input: PathBuf,
        /// Where to write the decoded bytes. Without it, they go to stdout.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Refuse a sequence that would decode to more than this many bytes
        /// or symbols. Use it for sequences from an untrusted source.
        #[arg(long)]
        max_units: Option<u64>,
    },
    /// Summarize a vocabulary, or show tokens from it.
    Inspect {
        vocab: PathBuf,
        /// Show the full envelope of one token id.
        #[arg(long)]
        token: Option<u32>,
        /// Show the N longest learned tokens.
        #[arg(long, default_value_t = 0)]
        longest: usize,
    },
    /// Check structure, integrity and the embedded test vectors.
    Validate { vocab: PathBuf },
    /// Compare two vocabularies by token content.
    Compare { a: PathBuf, b: PathBuf },
    /// Rewrite a vocabulary in the other file form: compact binary when the
    /// output ends in .kvfb, JSON otherwise. The digest does not change.
    Convert {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// Replace an existing output file.
        #[arg(long)]
        force: bool,
    },
    /// Convert a vocabulary to another tool's format.
    Export {
        #[arg(long)]
        vocab: PathBuf,
        #[arg(long, value_enum, default_value_t = ExportFormat::Tiktoken)]
        format: ExportFormat,
        /// Output file, e.g. general.tiktoken. A `.json` sidecar with the
        /// pattern and special tokens is written next to it.
        #[arg(long)]
        output: PathBuf,
    },
    /// Measure a vocabulary on a dataset, or run the benchmark suite.
    Benchmark {
        #[arg(long, required_unless_present = "suite")]
        vocab: Option<PathBuf>,
        /// File or directory to evaluate on. Keep it separate from training data.
        #[arg(long, required_unless_present = "suite")]
        dataset: Option<PathBuf>,
        /// Run the built-in benchmark program instead.
        #[arg(long, conflicts_with_all = ["vocab", "dataset"])]
        suite: bool,
        /// Repository root for the suite's in-repo corpora.
        #[arg(long)]
        repo_root: Option<PathBuf>,
        /// A large UTF-8 text file for the suite's real-data case (for example enwik8).
        /// The first 90% trains a 32K vocabulary; the rest is the evaluation set.
        #[arg(long, requires = "suite")]
        large_text: Option<PathBuf>,
        #[arg(long, default_value_t = 5)]
        iterations: usize,
        /// Also write the JSON report to this file.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Find recurring patterns in symbol streams, without training a tokenizer.
    Discover(Box<DiscoverArgs>),
    /// Derive the next version of an event vocabulary by adding symbols or
    /// symbol sequences. Existing token ids do not change.
    Evolve {
        #[arg(long)]
        vocab: PathBuf,
        /// A symbol, or several separated by spaces for a sequence. Repeatable.
        #[arg(long = "add", required = true, value_name = "SYMBOLS")]
        additions: Vec<String>,
        /// Version of the new vocabulary; must be greater than the current one.
        #[arg(long)]
        new_version: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Exchange vocabularies and encoded messages with another kute process,
    /// over TLS with a certificate on both sides.
    Peer {
        #[command(subcommand)]
        action: PeerAction,
    },
    /// Serve encode, decode, inspect, compare and discover as Model Context
    /// Protocol tools on standard input and output. Works only on the
    /// vocabularies named here and on data passed in each call: no file is
    /// read or written by a tool, nothing is trained, stored or published.
    Mcp {
        /// Vocabularies the tools may use. Repeatable.
        #[arg(long)]
        vocab: Vec<PathBuf>,
        /// Most bytes of input in one encode or discover call.
        #[arg(long, default_value_t = 1_048_576)]
        max_input_bytes: u64,
        /// Most bytes one decode call gives back.
        #[arg(long, default_value_t = 4_194_304)]
        max_output_bytes: u64,
    },
    /// Measure how well an entity's recent events predict its next one,
    /// on time that was held out, against two baselines. A measurement,
    /// not a predictor to deploy; association, not cause.
    Predict {
        /// JSON Lines files or directories, one event per line.
        #[arg(long, required = true, num_args = 1..)]
        input: Vec<PathBuf>,
        /// Field holding the event name.
        #[arg(long, value_name = "FIELD")]
        symbol_field: String,
        /// Field holding the event time, a whole number.
        #[arg(long, value_name = "FIELD")]
        time_field: String,
        /// Unit of the time field: s, ms, us or ns.
        #[arg(long, default_value = "s")]
        time_unit: String,
        /// Field naming the entity an event belongs to.
        #[arg(long, value_name = "FIELD")]
        group_field: Option<String>,
        /// Share of the events, latest in time, held out and predicted.
        #[arg(long, default_value_t = 0.25)]
        holdout_fraction: f64,
        /// Most past events the context predictor looks at (1 to 8).
        #[arg(long, default_value_t = 3)]
        max_context: usize,
        /// A run of past events is used only if seen this often before the cut.
        #[arg(long, default_value_t = 5)]
        min_count: u64,
        /// Parts the held-out time is cut into, to show drift.
        #[arg(long, default_value_t = 4)]
        windows: usize,
        /// Resamples of the held-out entities for the intervals.
        #[arg(long, default_value_t = 1000)]
        resamples: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Also write the JSON report to this file.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Try a candidate vocabulary beside the one in use, on data neither
    /// was made from, and decide whether it has earned its place. Encodes
    /// every input with both; publishes and replaces nothing.
    Shadow {
        /// The vocabulary in use.
        #[arg(long)]
        parent: PathBuf,
        /// The vocabulary proposed in its place.
        #[arg(long)]
        candidate: PathBuf,
        /// Held-out inputs, each encoded as one message.
        #[arg(long, required = true, num_args = 1..)]
        input: Vec<PathBuf>,
        /// What to do with a symbol a vocabulary lacks: `escape` or `strict`.
        #[arg(long, default_value = "escape")]
        unknown: String,
        /// Least net saving to approve, as a share of the parent's bytes.
        #[arg(long, default_value_t = 0.01)]
        min_improvement: f64,
        /// Traffic the candidate will carry, as a multiple of the inputs.
        #[arg(long, default_value_t = 1.0)]
        horizon: f64,
        /// Times the candidate vocabulary file has to be sent.
        #[arg(long, default_value_t = 1)]
        distributions: u64,
        /// Most the candidate may slow encoding, as a ratio.
        #[arg(long, default_value_t = 2.0)]
        max_slowdown: f64,
        /// Timed passes over the inputs; the median is used.
        #[arg(long, default_value_t = 3)]
        passes: usize,
        /// Exit with status 1 when the candidate is rejected.
        #[arg(long)]
        fail_on_reject: bool,
    },
    /// Discovery over events as they arrive, by event time, in windows
    /// that forget: JSON Lines in, one report per closed window out.
    /// Memory is bounded by the options. Experimental.
    Live {
        /// JSON Lines to read; standard input when absent.
        input: Option<PathBuf>,
        /// Field holding the event name.
        #[arg(long, value_name = "FIELD")]
        symbol_field: String,
        /// Field holding the time, a whole number of any unit.
        #[arg(long, value_name = "FIELD")]
        time_field: String,
        /// Field naming the entity an event belongs to.
        #[arg(long, value_name = "FIELD")]
        group_field: Option<String>,
        /// Length of a pane, in the unit of the time field.
        #[arg(long)]
        pane: i64,
        /// Panes in a window: 1 for tumbling windows, more for a sliding one.
        #[arg(long, default_value_t = 1)]
        panes: usize,
        /// How far behind the latest time an event may arrive and still count.
        #[arg(long, default_value_t = 0)]
        lateness: i64,
        /// Events held waiting to be put in time order.
        #[arg(long, default_value_t = 100_000)]
        max_buffered: usize,
        /// Longest sequence counted.
        #[arg(long, default_value_t = 6)]
        max_length: usize,
        /// Distinct event names known; later ones are folded together.
        #[arg(long, default_value_t = 65_536)]
        max_symbols: usize,
        /// Sequences counted at each length in each pane.
        #[arg(long, default_value_t = 200_000)]
        max_candidates: usize,
        /// Entities whose recent events are remembered.
        #[arg(long, default_value_t = 100_000)]
        max_entities: usize,
        #[arg(long, default_value_t = 5)]
        min_support: u64,
        #[arg(long, default_value_t = 1.0)]
        min_cohesion_bits: f64,
        /// Patterns in each report.
        #[arg(long, default_value_t = 100)]
        max_patterns: usize,
        /// File the state is saved to as windows close.
        #[arg(long)]
        checkpoint: Option<PathBuf>,
        /// Save after this many window reports.
        #[arg(long, default_value_t = 1)]
        checkpoint_every: u64,
        /// Start from the checkpoint, passing over the input it had read.
        #[arg(long)]
        resume: bool,
        /// Count lines that are not events and go on, instead of stopping.
        #[arg(long)]
        skip_bad_lines: bool,
    },
    /// Complete event records (entity, time and every other field) packed
    /// by column into one container, and back. Experimental.
    Events {
        #[command(subcommand)]
        action: EventsAction,
    },
    /// Serve encode, decode, inspect, compare and discover over HTTP, under
    /// /v1/. Read-only, like `kute mcp`, and through the same code. No TLS
    /// and no authentication: it listens on loopback unless told otherwise.
    Serve {
        /// Vocabularies the operations may use. Repeatable.
        #[arg(long)]
        vocab: Vec<PathBuf>,
        /// Address to listen on. Port 0 picks a free one; the address in
        /// use is printed as JSON on standard output.
        #[arg(long, default_value = "127.0.0.1:8787")]
        listen: String,
        /// Allow an address that is not loopback. Anyone who can reach it
        /// can use it: there is no TLS and no authentication.
        #[arg(long)]
        insecure_remote: bool,
        /// Most bytes of input in one encode or discover call.
        #[arg(long, default_value_t = 1_048_576)]
        max_input_bytes: u64,
        /// Most bytes one decode call gives back.
        #[arg(long, default_value_t = 4_194_304)]
        max_output_bytes: u64,
        /// Requests handled at once.
        #[arg(long, default_value_t = 4)]
        workers: usize,
        /// Seconds a request may take to arrive.
        #[arg(long, default_value_t = 10)]
        timeout: u64,
    },
    /// List registered algorithms and adapters and what each declares.
    Capabilities,
    /// Write the seeded example datasets used by the documentation.
    SampleData {
        /// Directory to create the files in.
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ExportFormat {
    Tiktoken,
}

#[derive(Subcommand)]
enum EventsAction {
    /// Pack JSON Lines, one object per event, keeping every field.
    Pack {
        /// JSON Lines to read.
        input: PathBuf,
        /// Container to write.
        #[arg(long)]
        output: PathBuf,
        /// Field holding the event name.
        #[arg(long, value_name = "FIELD")]
        symbol_field: String,
        /// Field holding the time, a whole number.
        #[arg(long, value_name = "FIELD")]
        time_field: Option<String>,
        /// Field naming the entity an event belongs to.
        #[arg(long, value_name = "FIELD")]
        group_field: Option<String>,
        /// How columns are compressed: zstd, or zlib for a container that
        /// needs only a zlib reader.
        #[arg(long, default_value = "zstd")]
        codec: String,
    },
    /// Write the records back as canonical JSON Lines, in source order.
    Unpack {
        input: PathBuf,
        /// File to write; standard output when absent.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Refuse a container that unpacks to more bytes than this.
        #[arg(long, default_value_t = 1 << 32)]
        max_bytes: u64,
    },
    /// Show a container's header: layout, counts, digest and columns.
    Inspect { input: PathBuf },
}

#[derive(Subcommand)]
enum PeerAction {
    /// Wait for one connection and answer it until it closes.
    Serve {
        /// Address to listen on, e.g. 127.0.0.1:7411.
        #[arg(long)]
        listen: String,
        /// Vocabularies this side already holds. Repeatable.
        #[arg(long)]
        vocab: Vec<PathBuf>,
        /// Take vocabularies this side does not already hold. Off by
        /// default: the other side can add nothing unless told it may.
        #[arg(long)]
        accept_vocabularies: bool,
        /// Let the other side extend vocabularies held here.
        #[arg(long)]
        accept_proposals: bool,
        /// Send a held vocabulary's file to the other side when it asks.
        #[arg(long)]
        give_vocabularies: bool,
        /// Answer whether a held vocabulary has a token for given symbols.
        #[arg(long)]
        answer_queries: bool,
        /// Re-encode the other side's sequences under another held vocabulary.
        #[arg(long)]
        translate: bool,
        /// Give up on a connection that sends nothing for this many
        /// seconds, during the handshake or after. 0 waits forever.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        /// Serve over a plain socket: no encryption, and whoever connects
        /// is served. Required when no TLS options are given.
        #[arg(long, conflicts_with = "tls_cert")]
        insecure_plain: bool,
        /// Refuse a message that decodes to more than this many units.
        #[arg(long, default_value_t = 268_435_456)]
        max_units: u64,
        /// Write each decoded message and each vocabulary received here.
        #[arg(long)]
        output_dir: Option<PathBuf>,
        /// This side's certificate chain (PEM). With --tls-key,
        /// --tls-client-ca and --allow, the connection is TLS 1.3 with a
        /// certificate on both sides. A connection that fails the
        /// handshake or is not allowed is refused and the next one awaited.
        #[arg(long, requires_all = ["tls_key", "tls_client_ca"])]
        tls_cert: Option<PathBuf>,
        /// This side's private key (PEM).
        #[arg(long, requires = "tls_cert")]
        tls_key: Option<PathBuf>,
        /// Authorities whose client certificates are accepted (PEM).
        #[arg(long, requires = "tls_cert")]
        tls_client_ca: Option<PathBuf>,
        /// A peer that may connect, by certificate fingerprint as printed
        /// by `kute peer fingerprint`. Repeatable. Anyone else is refused,
        /// even with a certificate from an accepted authority. The
        /// fingerprint is of the certificate, not the key: a renewed
        /// certificate has to be allowed again, and removing a fingerprint
        /// is the only revocation.
        #[arg(long, value_name = "SHA256", requires = "tls_cert")]
        allow: Vec<String>,
    },
    /// Connect, make sure the other side holds the vocabulary, and send files.
    Send {
        /// Address to connect to.
        #[arg(long)]
        connect: String,
        #[arg(long)]
        vocab: PathBuf,
        /// Files to encode and send. Repeatable.
        #[arg(long, num_args = 0..)]
        input: Vec<PathBuf>,
        /// strict or escape; see `kute encode --unknown`.
        #[arg(long, default_value = "strict")]
        unknown: String,
        /// Before sending, propose these additions (as in `kute evolve --add`).
        #[arg(long = "propose", value_name = "SYMBOLS", requires = "new_version")]
        additions: Vec<String>,
        #[arg(long)]
        new_version: Option<String>,
        /// This side's certificate chain (PEM); with --tls-key and --tls-ca
        /// the connection is TLS 1.3 with a certificate on both sides.
        #[arg(long, requires_all = ["tls_key", "tls_ca"])]
        tls_cert: Option<PathBuf>,
        /// This side's private key (PEM).
        #[arg(long, requires = "tls_cert")]
        tls_key: Option<PathBuf>,
        /// Authorities whose server certificates are accepted (PEM).
        #[arg(long, requires = "tls_cert")]
        tls_ca: Option<PathBuf>,
        /// The name the server's certificate must carry. Default: the host
        /// part of --connect.
        #[arg(long, requires = "tls_cert")]
        server_name: Option<String>,
        /// Talk only to the server with this certificate fingerprint.
        /// Without it, any server the authority issued a certificate for
        /// the name is accepted, which is less than the server asks of you.
        #[arg(long, value_name = "SHA256", requires = "tls_cert")]
        server_fingerprint: Option<String>,
        /// Connect over a plain socket: no encryption and no check of who
        /// answers. Required when no TLS options are given.
        #[arg(long, conflicts_with = "tls_cert")]
        insecure_plain: bool,
    },
    /// Print the fingerprint that identifies a certificate to `--allow`.
    Fingerprint {
        /// A certificate (PEM). The first one in the file is used.
        #[arg(long)]
        cert: PathBuf,
    },
}

const NO_TRANSPORT: &str = "give the TLS options (--tls-cert, --tls-key and an authority file), or --insecure-plain for a socket with no encryption and no authentication";

/// A plain or an encrypted connection.
trait Stream: std::io::Read + std::io::Write {}
impl<T: std::io::Read + std::io::Write> Stream for T {}

fn split_additions(additions: &[String]) -> Vec<Vec<String>> {
    additions.iter().map(|a| a.split_whitespace().map(str::to_string).collect()).collect()
}

fn peer(engine: Engine, action: PeerAction) -> kute::Result<()> {
    use kute_protocol::{tls, Framed, Peer, Policy, Session, DEFAULT_MAX_FRAME};
    match action {
        PeerAction::Fingerprint { cert } => {
            let chain = tls::load_certificates(&cert)?;
            print(&json!({ "certificate": cert, "fingerprint": tls::fingerprint(&chain[0]) }))
        }
        PeerAction::Serve {
            listen,
            vocab: held,
            accept_vocabularies,
            accept_proposals,
            give_vocabularies,
            answer_queries,
            translate,
            timeout,
            insecure_plain,
            max_units,
            output_dir,
            tls_cert,
            tls_key,
            tls_client_ca,
            allow,
        } => {
            let policy = Policy { accept_vocabularies, accept_proposals, give_vocabularies, answer_queries, translate, max_decoded_units: max_units, ..Policy::default() };
            if tls_cert.is_none() && !insecure_plain {
                return Err(KuteError::InvalidConfig(NO_TRANSPORT.into()));
            }
            let timeout = (timeout > 0).then(|| std::time::Duration::from_secs(timeout));
            let secured = match (&tls_cert, &tls_key, &tls_client_ca) {
                (Some(cert), Some(key), Some(ca)) => {
                    if allow.is_empty() {
                        return Err(KuteError::InvalidConfig("with TLS, name at least one peer with --allow; nobody is allowed otherwise".into()));
                    }
                    let mut access = tls::Access::new();
                    for fingerprint in &allow {
                        access.allow(fingerprint, policy.clone())?;
                    }
                    Some((tls::server_config(tls::load_certificates(cert)?, tls::load_private_key(key)?, tls::load_certificates(ca)?)?, access))
                }
                _ => None,
            };
            let listener = std::net::TcpListener::bind(&listen)?;
            eprintln!("listening on {}", listener.local_addr()?);
            if secured.is_none() {
                eprintln!("warning: no TLS. The connection is not encrypted and whoever connects is served.");
            }
            // With TLS, a connection that fails the handshake, stalls in
            // it, or is not allowed does not end the listener: whoever can
            // reach the port could otherwise stop it before the allowed peer
            // arrives.
            let (stream, identity, policy, from): (Box<dyn Stream>, Option<String>, Policy, std::net::SocketAddr) = loop {
                let (tcp, from) = listener.accept()?;
                tcp.set_read_timeout(timeout)?;
                tcp.set_write_timeout(timeout)?;
                let Some((config, access)) = &secured else { break (Box::new(tcp), None, policy, from) };
                match tls::accept(config, tcp).and_then(|(stream, who)| Ok((stream, access.policy_for(&who)?, who))) {
                    Ok((stream, policy, who)) => break (Box::new(stream), Some(who.fingerprint), policy, from),
                    Err(e) => eprintln!("refused {from}: {e}"),
                }
            };
            let mut peer = Peer::new("kute", engine, policy);
            for path in &held {
                peer.add_vocabulary(vocab::load(path)?)?;
            }
            let before = peer.digests();
            let mut session = Session::new(peer, Framed::new(stream, DEFAULT_MAX_FRAME));
            let handled = session.serve()?;
            let inbox = session.peer.take_inbox();
            let new: Vec<String> = session.peer.digests().into_iter().filter(|d| !before.contains(d)).collect();
            // Nothing received, nothing created.
            if let Some(dir) = output_dir.as_ref().filter(|_| !inbox.is_empty() || !new.is_empty()) {
                std::fs::create_dir_all(dir)?;
                for message in &inbox {
                    std::fs::write(dir.join(format!("message-{}.bin", message.id)), &message.bytes)?;
                }
                for digest in &new {
                    if let Some(v) = session.peer.vocabulary(digest) {
                        let name = format!("{}-{}.kvf", v.id().as_str().replace(':', "_"), v.version());
                        vocab::save(v, dir.join(name), true)?;
                    }
                }
            }
            print(&json!({
                "from": from.to_string(),
                "peer_identity": identity,
                "messages_handled": handled,
                "vocabularies_received": new,
                "received": inbox.iter().map(|m| json!({
                    "id": m.id,
                    "bytes": m.bytes.len(),
                    "tokens": m.sequence.ids.len(),
                    "escaped_symbols": m.sequence.escapes.len(),
                    "vocabulary": format!("{} {}", m.sequence.vocabulary_id, m.sequence.vocabulary_version),
                })).collect::<Vec<_>>(),
            }))
        }
        PeerAction::Send { connect, vocab: vocab_path, input, unknown, additions, new_version, tls_cert, tls_key, tls_ca, server_name, server_fingerprint, insecure_plain } => {
            if tls_cert.is_none() && !insecure_plain {
                return Err(KuteError::InvalidConfig(NO_TRANSPORT.into()));
            }
            let policy = kute::UnknownPolicy::parse(&unknown)?;
            let mut peer = Peer::new("kute", engine, Policy::default());
            let mut digest = peer.add_vocabulary(vocab::load(&vocab_path)?)?;
            let tcp = std::net::TcpStream::connect(&connect)?;
            let (stream, server_identity): (Box<dyn Stream>, Option<String>) = match (&tls_cert, &tls_key, &tls_ca) {
                (Some(cert), Some(key), Some(ca)) => {
                    let config = tls::client_config(tls::load_certificates(cert)?, tls::load_private_key(key)?, tls::load_certificates(ca)?)?;
                    let host = connect.rsplit_once(':').map_or(connect.as_str(), |(host, _)| host);
                    let (stream, who) = tls::connect(&config, server_name.as_deref().unwrap_or(host), tcp)?;
                    if server_fingerprint.as_ref().is_some_and(|wanted| *wanted != who.fingerprint) {
                        return Err(KuteError::Integrity(format!("the server is {}, not the one named by --server-fingerprint", who.fingerprint)));
                    }
                    (Box::new(stream), Some(who.fingerprint))
                }
                _ => (Box::new(tcp), None),
            };
            let mut session = Session::new(peer, Framed::new(stream, DEFAULT_MAX_FRAME));
            let remote = session.connect()?;
            let transferred = session.share(&digest)?;
            let mut proposed = None;
            if let Some(version) = &new_version {
                digest = session.propose(&digest, &split_additions(&additions), version)?;
                proposed = Some(digest.clone());
            }
            let mut sent = Vec::new();
            for path in &input {
                let raw = std::fs::read(path)?;
                let units = session.send(&digest, &raw, policy)?;
                sent.push(json!({ "input": path, "bytes": raw.len(), "units_decoded_by_peer": units, "receipt_matches": true }));
            }
            print(&json!({
                "peer": remote.peer,
                "peer_software": remote.software,
                "server_identity": server_identity,
                "vocabulary_transferred": transferred,
                "vocabulary_after_proposal": proposed,
                "sent": sent,
            }))
        }
    }
}

#[derive(clap::Args)]
struct DiscoverArgs {
    /// Files or directories to look for patterns in.
    #[arg(long, required = true, num_args = 1..)]
    input: Vec<PathBuf>,
    /// Adapter that turns each file into one record of symbols.
    #[arg(long, default_value = "jsonl-events")]
    adapter: String,
    /// Adapter option as key=value. Repeatable.
    #[arg(long = "adapter-option", value_name = "KEY=VALUE")]
    adapter_options: Vec<String>,
    /// Read plain text instead of using an adapter: every line is one
    /// record, symbols separated by whitespace.
    #[arg(long, conflicts_with_all = ["adapter", "adapter_options"])]
    lines: bool,
    /// Read typed events instead: JSON Lines with one event per line, the
    /// event name in this field. Keeps time and grouping; see the other
    /// --*-field options.
    #[arg(long, value_name = "FIELD", conflicts_with_all = ["adapter", "adapter_options", "lines"])]
    symbol_field: Option<String>,
    /// Field with the event time, a whole number of --time-unit. With it,
    /// the report says how long each pattern takes.
    #[arg(long, value_name = "FIELD", requires = "symbol_field")]
    time_field: Option<String>,
    /// Unit of the time field: s, ms, us or ns.
    #[arg(long, default_value = "s", requires = "time_field")]
    time_unit: String,
    /// Field naming what an event belongs to (a device, a block, a session).
    /// Events with the same value form one record.
    #[arg(long, value_name = "FIELD", requires = "symbol_field")]
    group_field: Option<String>,
    /// Name recorded for this way of reading the events.
    #[arg(long, default_value = "kute:events:unnamed", requires = "symbol_field")]
    schema_id: String,
    /// Files with held-out data to check the patterns against.
    #[arg(long, num_args = 1.., conflicts_with = "holdout_fraction")]
    holdout: Vec<PathBuf>,
    /// Hold out this share of the input instead. With --lines the last
    /// records are held out; otherwise the end of each record.
    #[arg(long)]
    holdout_fraction: Option<f64>,
    #[arg(long, default_value_t = 5)]
    min_support: u64,
    /// Longest contiguous pattern, in symbols.
    #[arg(long, default_value_t = 8)]
    max_length: usize,
    /// Minimum cohesion in bits: how much more often than chance.
    #[arg(long, default_value_t = 1.0)]
    min_cohesion: f64,
    /// Gapped pairs look this many symbols ahead; 0 turns them off.
    #[arg(long, default_value_t = 8)]
    window: usize,
    /// Report at most this many patterns of each kind.
    #[arg(long, default_value_t = 50)]
    top: usize,
    /// Also run on a shuffled copy of the input (same symbol counts, no
    /// order) and report how much the method finds there.
    #[arg(long)]
    control: bool,
    /// Also write the JSON report to this file.
    #[arg(long)]
    output: Option<PathBuf>,
}

fn read_records(engine: &Engine, args: &DiscoverArgs, paths: &[PathBuf]) -> kute::Result<Vec<Vec<String>>> {
    let mut adapter_config = AdapterConfig::default();
    for option in &args.adapter_options {
        let (key, value) = option
            .split_once('=')
            .ok_or_else(|| KuteError::InvalidConfig(format!("adapter option {option:?}: expected key=value")))?;
        adapter_config = adapter_config.with(key, value);
    }
    let mut records = Vec::new();
    for path in paths {
        for file in kute::collect_files(path)? {
            let raw = std::fs::read(&file)?;
            if args.lines {
                let text = String::from_utf8(raw).map_err(|_| KuteError::InvalidInput(format!("{}: not UTF-8", file.display())))?;
                records.extend(text.lines().map(|l| l.split_whitespace().map(str::to_string).collect::<Vec<_>>()).filter(|r| !r.is_empty()));
            } else {
                let input = engine.registry().adapter(&args.adapter)?.ingest(&raw, &adapter_config)?;
                records.extend(kute_discover::records_of(&[input])?);
            }
        }
    }
    if records.is_empty() {
        return Err(KuteError::InvalidInput("no records found in the input".into()));
    }
    Ok(records)
}

fn read_events(args: &DiscoverArgs, symbol_field: &str, paths: &[PathBuf]) -> kute::Result<kute::EventStream> {
    let schema = kute::EventSchema {
        id: args.schema_id.clone(),
        version: 1,
        symbol_field: symbol_field.to_string(),
        time_unit: args.time_field.as_ref().map(|_| kute::TimeUnit::parse(&args.time_unit)).transpose()?,
        time_field: args.time_field.clone(),
        group_field: args.group_field.clone(),
    };
    let mut stream = kute::EventStream::new(schema)?;
    for path in paths {
        for file in kute::collect_files(path)? {
            let reader = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(&file)?);
            stream.read_jsonl(reader, &file.display().to_string())?;
        }
    }
    if stream.records.is_empty() {
        return Err(KuteError::InvalidInput("no events found in the input".into()));
    }
    Ok(stream)
}

/// Lend a stream's symbols out as plain records, and give them back.
fn take_symbols(stream: &mut kute::EventStream) -> Vec<Vec<String>> {
    stream.records.iter_mut().map(|r| std::mem::take(&mut r.symbols)).collect()
}

fn give_symbols(stream: &mut kute::EventStream, records: Vec<Vec<String>>) {
    for (r, symbols) in stream.records.iter_mut().zip(records) {
        r.symbols = symbols;
    }
}

fn describe(stream: &kute::EventStream) -> kute::Result<serde_json::Value> {
    Ok(json!({ "records": stream.records.len(), "events": stream.events(), "fingerprint": stream.fingerprint()? }))
}

fn discover(engine: &Engine, args: DiscoverArgs) -> kute::Result<()> {
    use kute_discover as kd;
    let params = kd::Params {
        min_support: args.min_support,
        max_length: args.max_length,
        min_cohesion_bits: args.min_cohesion,
        window: args.window,
        max_patterns: args.top,
        ..kd::Params::default()
    };
    // Typed events: the symbols go through discovery like any others, and
    // the streams keep their times for the timing pass afterwards.
    let mut streams: Option<(kute::EventStream, Option<kute::EventStream>)> = match &args.symbol_field {
        None => None,
        Some(field) => {
            let stream = read_events(&args, field, &args.input)?;
            Some(if !args.holdout.is_empty() {
                (stream, Some(read_events(&args, field, &args.holdout)?))
            } else if let Some(fraction) = args.holdout_fraction {
                let (head, tail) = stream.split_last(fraction)?;
                (head, Some(tail))
            } else {
                (stream, None)
            })
        }
    };
    let typed = streams.is_some();
    let mut records = match &mut streams {
        Some((stream, _)) => take_symbols(stream),
        None => read_records(engine, &args, &args.input)?,
    };
    let mut held_out = match &mut streams {
        Some((_, held)) => held.as_mut().map(take_symbols),
        None if args.holdout.is_empty() => None,
        None => Some(read_records(engine, &args, &args.holdout)?),
    };
    if let Some(fraction) = args.holdout_fraction.filter(|_| !typed) {
        if args.lines {
            if !(fraction > 0.0 && fraction < 1.0) {
                return Err(KuteError::InvalidConfig("the held-out fraction must be between 0 and 1".into()));
            }
            let cut = ((records.len() as f64) * (1.0 - fraction)).round() as usize;
            held_out = Some(records.split_off(cut.clamp(1, records.len())));
        } else {
            let (head, tail) = kd::split_holdout(&records, fraction)?;
            records = head;
            held_out = Some(tail);
        }
    }
    let start = std::time::Instant::now();
    let found = kd::discover(&records, &params)?;
    let seconds = start.elapsed().as_secs_f64();
    let evaluation = held_out.as_ref().map(|h| kd::evaluate(&found, h)).transpose()?;
    let control = if args.control {
        let found = kd::discover(&kd::shuffled(&records, 0), &params)?;
        let evaluation = held_out.as_ref().map(|h| kd::evaluate(&found, h)).transpose()?;
        Some(json!({
            "what": "the same run on a shuffled copy of the input: same symbol counts, no order",
            "patterns": found.patterns.len(),
            "strongest_evidence_bits": found.patterns.iter().map(|p| p.evidence_bits).fold(0.0, f64::max),
            "coverage": found.coverage,
            "held_out_confirmed": evaluation.as_ref().map(|e| e.confirmed),
            "held_out_coverage": evaluation.as_ref().map(|e| e.coverage),
        }))
    } else {
        None
    };
    let events = match streams {
        None => None,
        Some((mut stream, mut held)) => {
            give_symbols(&mut stream, records);
            if let (Some(h), Some(symbols)) = (held.as_mut(), held_out) {
                give_symbols(h, symbols);
            }
            let timing = if stream.timed() { Some(kd::timing(&found, &stream)?) } else { None };
            let held_timing = match &held {
                Some(h) if h.timed() => Some(kd::timing(&found, h)?),
                _ => None,
            };
            Some(json!({
                "schema": stream.schema,
                "searched": describe(&stream)?,
                "held_out": held.as_ref().map(describe).transpose()?,
                "timing_what": "per pattern, in the schema's time unit: time from the first event of an occurrence to its last; for a gapped pair, from the first symbol to the nearest second symbol within the window",
                "timing": timing,
                "held_out_timing": held_timing,
            }))
        }
    };
    let report = json!({
        "kute_version": kute::KUTE_VERSION,
        "discover_seconds": seconds,
        "peak_memory_bytes": kute_bench::peak_memory_bytes(),
        "discovery": found,
        "held_out": evaluation,
        "control": control,
        "events": events,
    });
    if let Some(path) = &args.output {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    print(&report)
}

#[derive(clap::Args)]
struct TrainArgs {
    /// Files or directories to train on. Directories are read recursively.
    #[arg(long, required = true, num_args = 1..)]
    input: Vec<PathBuf>,
    /// Vocabulary identifier, e.g. kute:industrial:events.
    #[arg(long)]
    id: String,
    /// Vocabulary version (semantic versioning).
    #[arg(long, default_value = "0.1.0")]
    vocab_version: String,
    #[arg(long, default_value = "bpe")]
    algorithm: String,
    #[arg(long, default_value = "text")]
    adapter: String,
    /// Adapter option as key=value. Repeatable.
    #[arg(long = "adapter-option", value_name = "KEY=VALUE")]
    adapter_options: Vec<String>,
    #[arg(long, default_value_t = 4096)]
    vocab_size: u32,
    /// none, gpt4, or regex:<pattern>. Default: gpt4 for the text adapter, none otherwise.
    #[arg(long)]
    pretokenizer: Option<String>,
    #[arg(long, default_value_t = 1)]
    min_frequency: u64,
    #[arg(long)]
    max_token_length: Option<usize>,
    /// Reserved (special) token. Repeatable.
    #[arg(long = "reserved")]
    reserved: Vec<String>,
    /// File with one dictionary entry per line, for the dictionary algorithm.
    #[arg(long)]
    dictionary_file: Option<PathBuf>,
    #[arg(long)]
    description: Option<String>,
    /// License of the vocabulary, e.g. an SPDX identifier.
    #[arg(long)]
    license: Option<String>,
    /// Read input files in pieces of about this many bytes, cut at line
    /// breaks, so memory does not grow with file size. Recorded in the
    /// vocabulary, because it decides where records end.
    #[arg(long, value_name = "BYTES")]
    stream_bytes: Option<usize>,
    /// Continue from a vocabulary trained earlier on the same input with the
    /// same settings and a smaller --vocab-size. The result is the same as
    /// one run to the new size. bpe and sequence-bpe. Its merges are taken
    /// as given: resume only from a vocabulary you trained or trust.
    #[arg(long, value_name = "VOCAB")]
    resume_from: Option<PathBuf>,
    /// Store the start of the first training records in the vocabulary as
    /// self-test vectors, so anyone holding the file can check it. They are
    /// verbatim excerpts: leave this off when the corpus is private.
    #[arg(long, conflicts_with = "no_test_vectors")]
    embed_test_vectors: bool,
    /// Accepted for compatibility; this is the default since v0.4.0.
    #[arg(long, hide = true)]
    no_test_vectors: bool,
    #[arg(long)]
    output: PathBuf,
    /// Replace an existing output file even if its content differs.
    #[arg(long)]
    force: bool,
}

fn print(value: &impl serde::Serialize) -> kute::Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{text}")?;
    Ok(())
}

fn parse_pretokenizer(spec: Option<&str>, adapter: &str) -> kute::Result<Pretokenizer> {
    match spec {
        None => Ok(if adapter == "text" { Pretokenizer::Gpt4 } else { Pretokenizer::None }),
        Some("none") => Ok(Pretokenizer::None),
        Some("gpt4") => Ok(Pretokenizer::Gpt4),
        Some(s) => match s.strip_prefix("regex:") {
            Some(p) if !p.is_empty() => Ok(Pretokenizer::Regex(p.to_string())),
            _ => Err(KuteError::InvalidConfig(format!("pretokenizer {s:?}: expected none, gpt4 or regex:<pattern>"))),
        },
    }
}

fn train(engine: &Engine, args: TrainArgs) -> kute::Result<()> {
    let mut config = TrainingConfig::new(VocabularyId::parse(&args.id)?, args.vocab_size);
    config.vocabulary_version = Version::parse(&args.vocab_version)
        .map_err(|e| KuteError::InvalidIdentifier(format!("version {:?}: {e}", args.vocab_version)))?;
    let symbol_algorithm = args.algorithm == "sequence-bpe";
    config.pretokenizer = if symbol_algorithm && args.pretokenizer.is_none() {
        Pretokenizer::None
    } else {
        parse_pretokenizer(args.pretokenizer.as_deref(), &args.adapter)?
    };
    config.min_frequency = args.min_frequency;
    config.max_token_length = args.max_token_length;
    config.reserved_tokens = args.reserved;
    config.description = args.description;
    config.license = args.license;
    config.embed_test_vectors = args.embed_test_vectors;
    config.stream_bytes = args.stream_bytes;
    if let Some(path) = &args.resume_from {
        config.resume_from = Some(Box::new(vocab::load(path)?));
    }
    if let Some(path) = &args.dictionary_file {
        let text = std::fs::read_to_string(path)?;
        config.dictionary = text.lines().map(|l| l.trim_end_matches('\r')).filter(|l| !l.is_empty()).map(str::to_string).collect();
    }
    let mut adapter_config = AdapterConfig::default();
    for option in &args.adapter_options {
        let (key, value) = option
            .split_once('=')
            .ok_or_else(|| KuteError::InvalidConfig(format!("adapter option {option:?}: expected key=value")))?;
        adapter_config = adapter_config.with(key, value);
    }
    let start = std::time::Instant::now();
    let vocabulary = engine.train_paths(&args.algorithm, &args.adapter, &adapter_config, &args.input, &config)?;
    let seconds = start.elapsed().as_secs_f64();
    vocab::save(&vocabulary, &args.output, args.force)?;
    eprintln!("trained {} {} in {seconds:.2} s -> {}", vocabulary.id(), vocabulary.version(), args.output.display());
    print(&json!({
        "output": args.output,
        "train_seconds": seconds,
        "peak_memory_bytes": kute_bench::peak_memory_bytes(),
        "resumed_from": args.resume_from,
        "summary": vocab::summarize(&vocabulary)?,
    }))
}

fn run(engine: Engine, cli: Cli) -> kute::Result<()> {
    match cli.command {
        Command::Train(args) => train(&engine, *args),
        Command::Encode { vocab: vocab_path, input, output, compress, unknown } => {
            let policy = kute::UnknownPolicy::parse(&unknown)?;
            let tokenizer = engine.tokenizer(vocab::load(&vocab_path)?)?;
            let raw = std::fs::read(&input)?;
            let sequence = engine.encode_raw_open(tokenizer.as_ref(), &raw, policy)?;
            match output {
                Some(path) => {
                    let bytes = if compress { sequence.to_bytes_compressed()? } else { sequence.to_bytes()? };
                    std::fs::write(&path, &bytes)?;
                    print(&json!({
                        "output": path,
                        "input_bytes": raw.len(),
                        "tokens": sequence.ids.len(),
                        "escaped_symbols": sequence.escapes.len(),
                        "serialized_bytes": bytes.len(),
                        "vocabulary": format!("{} {}", sequence.vocabulary_id, sequence.vocabulary_version),
                    }))
                }
                None => print(&sequence),
            }
        }
        Command::Decode { vocab: vocab_path, input, output, max_units } => {
            let tokenizer = engine.tokenizer(vocab::load(&vocab_path)?)?;
            let sequence = TokenSequence::from_bytes(&std::fs::read(&input)?)?;
            let bytes = engine.decode_raw_bounded(tokenizer.as_ref(), &sequence, max_units.unwrap_or(u64::MAX))?;
            match output {
                Some(path) => {
                    std::fs::write(&path, &bytes)?;
                    print(&json!({ "output": path, "bytes": bytes.len(), "tokens": sequence.ids.len() }))
                }
                None => {
                    let mut out = std::io::stdout().lock();
                    out.write_all(&bytes)?;
                    out.flush()?;
                    Ok(())
                }
            }
        }
        Command::Inspect { vocab: vocab_path, token, longest } => {
            let vocabulary = vocab::load(&vocab_path)?;
            if let Some(id) = token {
                return print(&vocabulary.record(id)?);
            }
            let mut report = json!({ "summary": vocab::summarize(&vocabulary)? });
            if longest > 0 {
                let mut learned: Vec<&kute::TokenEntry> = vocabulary.tokens.iter().filter(|t| t.kind == kute::TokenKind::Merge).collect();
                learned.sort_by(|a, b| b.payload.len().cmp(&a.payload.len()).then(a.id.cmp(&b.id)));
                let rows: Vec<kute::TokenRecord> = learned.into_iter().take(longest).map(|t| vocabulary.record(t.id)).collect::<kute::Result<_>>()?;
                report["longest_learned_tokens"] = serde_json::to_value(rows)?;
            }
            print(&report)
        }
        Command::Validate { vocab: vocab_path } => {
            let vocabulary = vocab::load(&vocab_path)?;
            let report = engine.verify(&vocabulary)?;
            print(&report)?;
            if report.ok() {
                Ok(())
            } else {
                Err(KuteError::InvalidVocabulary(format!("{} of {} test vectors failed", report.failures.len(), report.vectors)))
            }
        }
        Command::Compare { a, b } => print(&vocab::compare(&vocab::load(&a)?, &vocab::load(&b)?)?),
        Command::Convert { input, output, force } => {
            let vocabulary = vocab::load(&input)?;
            vocab::save(&vocabulary, &output, force)?;
            // Read the new file back: the report describes what is on disk.
            let written = vocab::load(&output)?;
            if written != vocabulary {
                return Err(KuteError::Integrity("the converted file does not read back as the same vocabulary".into()));
            }
            let median_ms = |path: &PathBuf| -> kute::Result<f64> {
                let bytes = std::fs::read(path)?;
                let mut times = Vec::new();
                for _ in 0..5 {
                    let start = std::time::Instant::now();
                    vocab::from_slice(&bytes)?;
                    times.push(start.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(|a, b| a.total_cmp(b));
                Ok((times[2] * 1000.0).round() / 1000.0)
            };
            print(&json!({
                "input": { "path": input, "bytes": std::fs::metadata(&input)?.len(), "load_ms_median": median_ms(&input)? },
                "output": { "path": output, "bytes": std::fs::metadata(&output)?.len(), "load_ms_median": median_ms(&output)? },
                "digest": written.digest()?,
                "tokens": written.len(),
                "identical": true,
            }))
        }
        Command::Export { vocab: vocab_path, format: ExportFormat::Tiktoken, output } => {
            let vocabulary = vocab::load(&vocab_path)?;
            let export = vocab::export_tiktoken(&vocabulary)?;
            std::fs::write(&output, &export.bpe_file)?;
            let mut sidecar = output.clone().into_os_string();
            sidecar.push(".json");
            let sidecar = PathBuf::from(sidecar);
            let meta = json!({
                "name": vocabulary.id().as_str().replace(':', "_"),
                "pat_str": export.pat_str,
                "special_tokens": export.special_tokens,
                "explicit_n_vocab": export.n_vocab,
                "source_vocabulary": format!("{} {}", vocabulary.id(), vocabulary.version()),
                "source_digest": vocabulary.digest()?,
            });
            std::fs::write(&sidecar, serde_json::to_vec_pretty(&meta)?)?;
            print(&json!({ "bpe_file": output, "metadata": sidecar, "tokens": export.n_vocab }))
        }
        Command::Benchmark { vocab: vocab_path, dataset, suite, repo_root, large_text, iterations, output } => {
            let report = if suite {
                serde_json::to_value(kute_bench::suite::run(&engine, &kute_bench::suite::Options { repo_root, iterations, large_text })?)?
            } else {
                let (Some(vocab_path), Some(dataset)) = (vocab_path, dataset) else {
                    return Err(KuteError::InvalidConfig("benchmark needs --vocab and --dataset, or --suite".into()));
                };
                let vocabulary = vocab::load(&vocab_path)?;
                let documents = kute_bench::read_dataset(&dataset)?;
                json!({
                    "environment": kute_bench::Environment::detect(),
                    "dataset": dataset,
                    "evaluation": kute_bench::evaluate(&engine, &vocabulary, &documents, iterations)?,
                })
            };
            if let Some(path) = output {
                let mut text = serde_json::to_string_pretty(&report)?;
                text.push('\n');
                std::fs::write(&path, text)?;
                eprintln!("report written to {}", path.display());
            }
            print(&report)
        }
        Command::SampleData { output } => {
            use kute_bench::data;
            std::fs::create_dir_all(&output)?;
            let files: Vec<(&str, Vec<u8>, &str)> = vec![
                ("alarms-train.jsonl", data::alarm_log(51, 4000), "plant alarm log, 4000 episodes"),
                ("alarms-eval.jsonl", data::alarm_log(52, 1000), "plant alarm log, 1000 episodes, different seed"),
                ("messages-train.jsonl", data::agent_messages(61, 4000), "agent messages, 4000 records"),
                ("messages-eval.jsonl", data::agent_messages(62, 1000), "agent messages, 1000 records, different seed"),
                ("frames-train.bin", data::binary_records(31, 6000), "binary telemetry frames, 6000 records"),
                ("frames-eval.bin", data::binary_records(32, 2000), "binary telemetry frames, 2000 records, different seed"),
                ("words-train.txt", data::invented_words(81, 60_000), "invented-word text, 60000 words"),
                ("words-eval.txt", data::invented_words(82, 15_000), "invented-word text, 15000 words, different seed"),
            ];
            let mut written = Vec::new();
            for (name, bytes, what) in files {
                std::fs::write(output.join(name), &bytes)?;
                written.push(json!({ "file": name, "bytes": bytes.len(), "sha256": kute::sha256_tagged(&bytes), "content": what }));
            }
            print(&json!({ "output": output, "synthetic": true, "files": written }))
        }
        Command::Discover(args) => discover(&engine, *args),
        Command::Evolve { vocab: vocab_path, additions, new_version, output, force } => {
            let version = Version::parse(&new_version).map_err(|e| KuteError::InvalidIdentifier(format!("version {new_version:?}: {e}")))?;
            let parent = vocab::load(&vocab_path)?;
            let child = kute::bpe::sequence::evolve(&parent, &split_additions(&additions), version)?;
            let report = engine.verify(&child)?;
            if !report.ok() {
                return Err(KuteError::Backend(format!("the evolved vocabulary fails its test vectors: {}", report.failures.join("; "))));
            }
            vocab::save(&child, &output, force)?;
            // A container writes every id in as many bytes as the largest
            // id of the vocabulary needs. A step across one of these sizes
            // makes every token cost more, which can outweigh what the new
            // tokens save (measured: benchmarks/README.md, drifting data).
            let mut warnings = Vec::new();
            for (limit, bytes) in [(256usize, "one byte"), (65_536, "two bytes")] {
                if parent.len() <= limit && child.len() > limit {
                    warnings.push(format!(
                        "the vocabulary grew from {} to {} tokens, past {limit}: a token id no longer fits {bytes}, so sequences can come out larger than under the parent even with fewer tokens. Measure on your data before adopting it.",
                        parent.len(),
                        child.len()
                    ));
                }
            }
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }
            print(&json!({
                "output": output,
                "parent": { "version": parent.version().to_string(), "digest": parent.digest()?, "tokens": parent.len() },
                "summary": vocab::summarize(&child)?,
                "warnings": warnings,
            }))
        }
        Command::Peer { action } => peer(engine, action),
        Command::Mcp { vocab: paths, max_input_bytes, max_output_bytes } => {
            let vocabularies = paths.iter().map(vocab::load).collect::<kute::Result<Vec<_>>>()?;
            let limits = mcp::Limits { max_input_bytes, max_output_bytes, max_request_bytes: max_input_bytes.saturating_mul(2).saturating_add(1 << 20), ..Default::default() };
            let server = mcp::Server::new(engine, vocabularies, limits)?;
            server.serve(std::io::stdin().lock(), std::io::stdout().lock())?;
            Ok(())
        }
        Command::Live {
            input,
            symbol_field,
            time_field,
            group_field,
            pane,
            panes,
            lateness,
            max_buffered,
            max_length,
            max_symbols,
            max_candidates,
            max_entities,
            min_support,
            min_cohesion_bits,
            max_patterns,
            checkpoint,
            checkpoint_every,
            resume,
            skip_bad_lines,
        } => {
            let bounds = kute_discover::live::LiveParams { max_length, max_symbols, max_candidates, max_entities, ..Default::default() };
            let params = kute_discover::watch::WatchParams { live: bounds, pane_ticks: pane, panes, lateness, max_buffered, min_support, min_cohesion_bits, max_patterns };
            live::run(live::Options { input, symbol_field, time_field, group_field, params, checkpoint, checkpoint_every, resume, skip_bad_lines })
        }
        Command::Predict { input, symbol_field, time_field, time_unit, group_field, holdout_fraction, max_context, min_count, windows, resamples, seed, output } => {
            let schema = kute::EventSchema {
                id: "kute:events:unnamed".into(),
                version: 1,
                symbol_field,
                time_unit: Some(kute::TimeUnit::parse(&time_unit)?),
                time_field: Some(time_field),
                group_field,
            };
            let mut stream = kute::EventStream::new(schema)?;
            for path in &input {
                for file in kute::collect_files(path)? {
                    let reader = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(&file)?);
                    stream.read_jsonl(reader, &file.display().to_string())?;
                }
            }
            let params = kute_discover::predict::PredictParams { holdout_fraction, max_context, min_count, windows, resamples, seed };
            let report = kute_discover::predict::evaluate(&stream, &params)?;
            if let Some(path) = output {
                std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
            }
            print(&report)
        }
        Command::Shadow { parent, candidate, input, unknown, min_improvement, horizon, distributions, max_slowdown, passes, fail_on_reject } => {
            let (report, approved) = shadow::run(&engine, &shadow::Options { parent, candidate, inputs: input, unknown, min_improvement, horizon, distributions, max_slowdown, passes })?;
            print(&report)?;
            if fail_on_reject && !approved {
                return Err(KuteError::Backend("the candidate was rejected".into()));
            }
            Ok(())
        }
        Command::Events { action } => match action {
            EventsAction::Pack { input, output, symbol_field, time_field, group_field, codec } => {
                let layout = kute_events::Layout { symbol_field, time_field, group_field };
                let (container, report) = kute_events::pack_with(&std::fs::read(&input)?, &layout, kute_events::Codec::parse(&codec)?)?;
                std::fs::write(&output, &container)?;
                print(&json!({ "output": output, "report": report }))
            }
            EventsAction::Unpack { input, output, max_bytes } => {
                let bytes = kute_events::unpack(&std::fs::read(&input)?, max_bytes)?;
                match output {
                    Some(path) => {
                        std::fs::write(&path, &bytes)?;
                        print(&json!({ "output": path, "bytes": bytes.len() }))
                    }
                    None => {
                        let mut out = std::io::stdout().lock();
                        out.write_all(&bytes)?;
                        out.flush()?;
                        Ok(())
                    }
                }
            }
            EventsAction::Inspect { input } => print(&kute_events::header(&std::fs::read(&input)?)?.0),
        },
        Command::Serve { vocab: paths, listen, insecure_remote, max_input_bytes, max_output_bytes, workers, timeout } => {
            let vocabularies = paths.iter().map(vocab::load).collect::<kute::Result<Vec<_>>>()?;
            let max_body_bytes = max_input_bytes.saturating_mul(2).saturating_add(1 << 20);
            let limits = mcp::Limits { max_input_bytes, max_output_bytes, max_request_bytes: max_body_bytes, ..Default::default() };
            let server = mcp::Server::new(engine, vocabularies, limits)?;
            let listener = std::net::TcpListener::bind(&listen)?;
            let address = listener.local_addr()?;
            if !http::is_loopback(&address) && !insecure_remote {
                return Err(KuteError::InvalidConfig(format!(
                    "{address} is not a loopback address, and this service has no TLS and no authentication; pass --insecure-remote to serve it anyway"
                )));
            }
            print(&json!({ "listening": address.to_string(), "api": "v1", "loopback_only": http::is_loopback(&address) }))?;
            let options = http::Options { workers, timeout: std::time::Duration::from_secs(timeout.max(1)), max_body_bytes, any_host: !http::is_loopback(&address), ..Default::default() };
            http::serve(server, listener, options)
        }
        Command::Capabilities => {
            let registry = engine.registry();
            let algorithms: Vec<kute::TokenizerCapabilities> =
                registry.algorithm_names().iter().map(|n| registry.tokenizer(n).map(|t| t.capabilities())).collect::<kute::Result<_>>()?;
            let adapters: Vec<serde_json::Value> = registry
                .adapter_names()
                .iter()
                .map(|n| {
                    registry.adapter(n).map(|a| {
                        let c = a.capabilities();
                        json!({ "name": c.name, "modality": c.modality, "lossless": c.lossless, "notes": c.notes })
                    })
                })
                .collect::<kute::Result<_>>()?;
            print(&json!({
                "kute_version": kute::KUTE_VERSION,
                "kvf_version": kute::KVF_VERSION,
                "algorithms": algorithms,
                "adapters": adapters,
            }))
        }
    }
}

/// Parse the command line and run it on `engine`. The `kute` binary passes
/// the default engine; other front ends can register more algorithms first.
pub fn main_with(engine: Engine) -> ExitCode {
    match run(engine, Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kute: {e}");
            ExitCode::FAILURE
        }
    }
}
