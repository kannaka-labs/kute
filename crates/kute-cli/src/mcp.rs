//! A Model Context Protocol server over standard input and output: one
//! JSON-RPC 2.0 message per line.
//!
//! It offers five tools, all of them read-only with respect to this
//! machine: `encode`, `decode`, `inspect`, `compare` and `discover`. It
//! works on the vocabularies it was started with and on data passed in the
//! call. It reads and writes no file, opens no connection, trains nothing,
//! publishes nothing and keeps nothing between calls. Every input and
//! output is bounded by [`Limits`].

use base64::Engine as _;
use kute::{vocab, Engine, KuteError, TokenSequence, Tokenizer, UnknownPolicy, Vocabulary};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};

/// The protocol revision this server was written against. A client that
/// asks for another is answered with this one, as the protocol prescribes.
pub const MCP_PROTOCOL: &str = "2025-06-18";

/// What one call may cost.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Longest request line, in bytes. A longer one ends the session.
    pub max_request_bytes: u64,
    /// Most bytes of text or data in one `encode` or `discover` call.
    pub max_input_bytes: u64,
    /// Most bytes `decode` gives back, and most tokens it reads.
    pub max_output_bytes: u64,
    /// `encode` lists ids only up to this many; the container always holds all.
    pub max_ids_listed: usize,
    /// Most patterns `discover` reports of each kind.
    pub max_patterns: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_request_bytes: 8 << 20, max_input_bytes: 1 << 20, max_output_bytes: 4 << 20, max_ids_listed: 4096, max_patterns: 200 }
    }
}

struct Held {
    digest: String,
    vocabulary: Vocabulary,
    tokenizer: Box<dyn Tokenizer>,
}

pub struct Server {
    engine: Engine,
    held: Vec<Held>,
    limits: Limits,
}

type Failure = String;

fn fail(e: KuteError) -> Failure {
    e.to_string()
}

fn text_arg<'a>(args: &'a Value, name: &str) -> Result<Option<&'a str>, Failure> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{name} must be a string")),
    }
}

fn number_arg(args: &Value, name: &str) -> Result<Option<u64>, Failure> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| format!("{name} must be a whole number, zero or more")),
    }
}

impl Server {
    pub fn new(engine: Engine, vocabularies: Vec<Vocabulary>, limits: Limits) -> kute::Result<Self> {
        let mut held: Vec<Held> = Vec::new();
        for vocabulary in vocabularies {
            let digest = vocabulary.compute_digest()?;
            if held.iter().any(|h| h.digest == digest) {
                continue;
            }
            let tokenizer = engine.tokenizer(vocabulary.clone())?;
            held.push(Held { digest, vocabulary, tokenizer });
        }
        Ok(Self { engine, held, limits })
    }

    /// A held vocabulary by digest, by `id version`, or by id alone when
    /// only one version of it is held.
    fn find(&self, name: &str) -> Result<&Held, Failure> {
        let matches: Vec<&Held> = self
            .held
            .iter()
            .filter(|h| {
                let (id, version) = (h.vocabulary.id().as_str(), h.vocabulary.version().to_string());
                h.digest == name || id == name || format!("{id} {version}") == name || format!("{id}@{version}") == name
            })
            .collect();
        match matches.as_slice() {
            [one] => Ok(one),
            [] => Err(format!("no vocabulary {name:?} is held; call inspect without arguments to list them")),
            _ => Err(format!("{name:?} names {} held vocabularies; give the version or the digest", matches.len())),
        }
    }

    /// The engine vocabularies are checked with.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// For the registry routes of the HTTP service: what is held, with
    /// the key names of the signatures each file carries. The signatures
    /// are listed as the files state them and are not checked here; the
    /// one who fetches checks them against keys of their own choosing.
    pub fn registry_list(&self) -> Value {
        let list: Vec<Value> = self
            .held
            .iter()
            .map(|h| {
                let mut entry = Self::reference(h);
                entry["algorithm"] = json!(h.vocabulary.manifest.algorithm);
                entry["tokens"] = json!(h.vocabulary.len());
                entry["signed_by"] = json!(h.vocabulary.integrity.as_ref().map(|i| i.signatures.iter().map(|s| s.key_id.clone()).collect::<Vec<_>>()).unwrap_or_default());
                entry
            })
            .collect();
        json!({ "vocabularies": list })
    }

    /// The file of the held vocabulary with exactly this digest, with
    /// the signatures it was loaded with.
    pub fn registry_file(&self, digest: &str) -> Option<kute::Result<Vec<u8>>> {
        self.held.iter().find(|h| h.digest == digest).map(|h| vocab::to_vec(&h.vocabulary))
    }

    fn named(&self, args: &Value, field: &str) -> Result<&Held, Failure> {
        self.find(text_arg(args, field)?.ok_or_else(|| format!("{field} is required"))?)
    }

    fn reference(held: &Held) -> Value {
        json!({ "id": held.vocabulary.id().as_str(), "version": held.vocabulary.version().to_string(), "digest": held.digest })
    }

    fn encode(&self, args: &Value) -> Result<Value, Failure> {
        let held = self.named(args, "vocabulary")?;
        let raw = match (text_arg(args, "text")?, text_arg(args, "data_base64")?) {
            (Some(text), None) => text.as_bytes().to_vec(),
            (None, Some(data)) => {
                if data.len() as u64 > self.limits.max_input_bytes.saturating_mul(4) / 3 + 4 {
                    return Err(format!("the data is larger than the limit of {} bytes", self.limits.max_input_bytes));
                }
                base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| format!("data_base64 is not base64: {e}"))?
            }
            _ => return Err("give exactly one of text and data_base64".into()),
        };
        if raw.len() as u64 > self.limits.max_input_bytes {
            return Err(format!("the input is {} bytes; the limit is {}", raw.len(), self.limits.max_input_bytes));
        }
        let policy = UnknownPolicy::parse(text_arg(args, "unknown")?.unwrap_or("strict")).map_err(fail)?;
        let sequence = self.engine.encode_raw_open(held.tokenizer.as_ref(), &raw, policy).map_err(fail)?;
        let container = sequence.to_bytes_compressed().map_err(fail)?;
        let listed = sequence.ids.len() <= self.limits.max_ids_listed;
        Ok(json!({
            "vocabulary": Self::reference(held),
            "input_bytes": raw.len(),
            "tokens": sequence.ids.len(),
            "ids": if listed { json!(sequence.ids) } else { Value::Null },
            "ids_omitted": !listed,
            "escaped_symbols": sequence.escapes,
            "container_bytes": container.len(),
            "container_base64": base64::engine::general_purpose::STANDARD.encode(&container),
        }))
    }

    fn decode(&self, args: &Value) -> Result<Value, Failure> {
        let container = text_arg(args, "container_base64")?.ok_or("container_base64 is required")?;
        let container = base64::engine::general_purpose::STANDARD.decode(container).map_err(|e| format!("container_base64 is not base64: {e}"))?;
        // Every token decodes to at least one byte or symbol, so the
        // output limit also bounds the tokens worth reading.
        let sequence = TokenSequence::from_bytes_bounded(&container, self.limits.max_output_bytes).map_err(fail)?;
        let held = self.find(&sequence.vocabulary_digest).map_err(|_| {
            format!("the container was encoded under {} {} ({}), which is not held", sequence.vocabulary_id, sequence.vocabulary_version, sequence.vocabulary_digest)
        })?;
        let bytes = self.engine.decode_raw_within(held.tokenizer.as_ref(), &sequence, self.limits.max_output_bytes, self.limits.max_output_bytes).map_err(fail)?;
        if bytes.len() as u64 > self.limits.max_output_bytes {
            return Err(format!("the container decodes to {} bytes; the limit is {}", bytes.len(), self.limits.max_output_bytes));
        }
        let mut out = json!({ "vocabulary": Self::reference(held), "tokens": sequence.ids.len(), "bytes": bytes.len() });
        match String::from_utf8(bytes) {
            Ok(text) => out["text"] = json!(text),
            Err(e) => out["data_base64"] = json!(base64::engine::general_purpose::STANDARD.encode(e.as_bytes())),
        }
        Ok(out)
    }

    fn inspect(&self, args: &Value) -> Result<Value, Failure> {
        if text_arg(args, "vocabulary")?.is_none() {
            let list: Vec<Value> = self
                .held
                .iter()
                .map(|h| {
                    let mut entry = Self::reference(h);
                    entry["algorithm"] = json!(h.vocabulary.manifest.algorithm);
                    entry["tokens"] = json!(h.vocabulary.len());
                    entry
                })
                .collect();
            return Ok(json!({ "vocabularies": list, "limits": self.limits_json() }));
        }
        let held = self.named(args, "vocabulary")?;
        if let Some(id) = number_arg(args, "token")? {
            let id = u32::try_from(id).map_err(|_| "token is not a token id".to_string())?;
            return serde_json::to_value(held.vocabulary.record(id).map_err(fail)?).map_err(|e| e.to_string());
        }
        let mut report = json!({ "summary": vocab::summarize(&held.vocabulary).map_err(fail)? });
        let longest = number_arg(args, "longest")?.unwrap_or(0).min(100) as usize;
        if longest > 0 {
            let mut learned: Vec<&kute::TokenEntry> = held.vocabulary.tokens.iter().filter(|t| t.kind == kute::TokenKind::Merge).collect();
            learned.sort_by(|a, b| b.payload.len().cmp(&a.payload.len()).then(a.id.cmp(&b.id)));
            let rows: Vec<kute::TokenRecord> = learned.into_iter().take(longest).map(|t| held.vocabulary.record(t.id)).collect::<kute::Result<_>>().map_err(fail)?;
            report["longest_learned_tokens"] = serde_json::to_value(rows).map_err(|e| e.to_string())?;
        }
        Ok(report)
    }

    fn compare(&self, args: &Value) -> Result<Value, Failure> {
        let (a, b) = (self.named(args, "a")?, self.named(args, "b")?);
        serde_json::to_value(vocab::compare(&a.vocabulary, &b.vocabulary).map_err(fail)?).map_err(|e| e.to_string())
    }

    fn discover(&self, args: &Value) -> Result<Value, Failure> {
        let records: Vec<Vec<String>> = match (args.get("records"), text_arg(args, "lines")?) {
            (Some(records), None) => serde_json::from_value(records.clone()).map_err(|_| "records must be a list of lists of strings".to_string())?,
            (None, Some(lines)) => lines.lines().map(|l| l.split_whitespace().map(str::to_string).collect()).filter(|r: &Vec<String>| !r.is_empty()).collect(),
            _ => return Err("give exactly one of records and lines".into()),
        };
        // Times, when given, are one whole number for each symbol.
        let times: Option<Vec<Vec<i64>>> = match args.get("times").filter(|v| !v.is_null()) {
            None => None,
            Some(_) if args.get("records").is_none() => return Err("times go with records, one number for each symbol".into()),
            Some(times) => Some(serde_json::from_value(times.clone()).map_err(|_| "times must be a list of lists of whole numbers".to_string())?),
        };
        let size: u64 = records.iter().flatten().map(|s| s.len() as u64 + 1).sum::<u64>() + times.iter().flatten().map(|t| 8 * t.len() as u64).sum::<u64>();
        if size > self.limits.max_input_bytes {
            return Err(format!("the records hold {size} bytes of symbols; the limit is {}", self.limits.max_input_bytes));
        }
        let mut params = kute_discover::Params { max_candidates: 200_000, ..Default::default() };
        if let Some(n) = number_arg(args, "min_support")? {
            params.min_support = n;
        }
        if let Some(n) = number_arg(args, "max_length")? {
            params.max_length = n as usize;
        }
        if let Some(n) = number_arg(args, "window")? {
            params.window = n as usize;
        }
        if let Some(bits) = args.get("min_cohesion_bits").filter(|v| !v.is_null()) {
            params.min_cohesion_bits = bits.as_f64().ok_or("min_cohesion_bits must be a number")?;
        }
        params.max_patterns = (number_arg(args, "max_patterns")?.unwrap_or(50) as usize).min(self.limits.max_patterns);
        let discovery = kute_discover::discover(&records, &params).map_err(fail)?;
        let mut answer = serde_json::to_value(&discovery).map_err(|e| e.to_string())?;
        if let Some(times) = times {
            if times.len() != records.len() {
                return Err(format!("{} lists of times for {} records", times.len(), records.len()));
            }
            // The unit is the caller's: durations come back in the ticks
            // that were given. The schema needs one to be a timed schema.
            let schema = kute::EventSchema {
                id: "call".into(),
                version: 1,
                symbol_field: "symbol".into(),
                time_field: Some("time".into()),
                time_unit: Some(kute::TimeUnit::Milliseconds),
                group_field: None,
            };
            let mut stream = kute::EventStream::new(schema).map_err(fail)?;
            stream.records = records.into_iter().zip(times).map(|(symbols, times)| kute::EventRecord { key: None, symbols, times }).collect();
            // Refuses times not aligned with their symbols, or going back.
            stream.validate().map_err(fail)?;
            answer["timing"] = serde_json::to_value(kute_discover::timing(&discovery, &stream).map_err(fail)?).map_err(|e| e.to_string())?;
            answer["timing_what"] = "per pattern (index into patterns), in the ticks the times were given in: time from the first event of an occurrence to its last; for a gapped pair, from the first symbol to the nearest second symbol within the window. Measured on the records given, the same ones the patterns were found in.".into();
        }
        Ok(answer)
    }

    fn limits_json(&self) -> Value {
        json!({
            "max_request_bytes": self.limits.max_request_bytes,
            "max_input_bytes": self.limits.max_input_bytes,
            "max_output_bytes": self.limits.max_output_bytes,
            "max_ids_listed": self.limits.max_ids_listed,
            "max_patterns": self.limits.max_patterns,
        })
    }

    /// The tools, with the JSON Schema of each one's arguments.
    pub fn tools() -> Value {
        let vocabulary = json!({ "type": "string", "description": "A held vocabulary: its digest, its id, or \"id version\"." });
        let read_only = json!({ "readOnlyHint": true, "openWorldHint": false });
        json!([
            {
                "name": "encode",
                "description": "Encode text or bytes under a held vocabulary. Returns the token ids and a .kute container (base64) that decode accepts. Lossless: decoding the container gives the input back.",
                "inputSchema": { "type": "object", "required": ["vocabulary"], "additionalProperties": false, "properties": {
                    "vocabulary": vocabulary,
                    "text": { "type": "string", "description": "The input as text. Give this or data_base64." },
                    "data_base64": { "type": "string", "description": "The input as base64 bytes." },
                    "unknown": { "type": "string", "enum": ["strict", "escape"], "description": "For symbol vocabularies: refuse a symbol the vocabulary lacks (strict, the default) or carry it literally (escape)." }
                } },
                "annotations": read_only,
            },
            {
                "name": "decode",
                "description": "Decode a .kute container produced by encode. The vocabulary is the one the container names, and must be held.",
                "inputSchema": { "type": "object", "required": ["container_base64"], "additionalProperties": false, "properties": {
                    "container_base64": { "type": "string" }
                } },
                "annotations": read_only,
            },
            {
                "name": "inspect",
                "description": "Without arguments, list the held vocabularies and this server's limits. With a vocabulary, summarize it; with token, show one token; with longest, the N longest learned tokens.",
                "inputSchema": { "type": "object", "additionalProperties": false, "properties": {
                    "vocabulary": vocabulary,
                    "token": { "type": "integer", "minimum": 0 },
                    "longest": { "type": "integer", "minimum": 0, "maximum": 100 }
                } },
                "annotations": read_only,
            },
            {
                "name": "compare",
                "description": "Compare two held vocabularies by token content: shared and distinct payloads. Token ids are not compared; the same id in two vocabularies means nothing.",
                "inputSchema": { "type": "object", "required": ["a", "b"], "additionalProperties": false, "properties": { "a": vocabulary, "b": vocabulary } },
                "annotations": read_only,
            },
            {
                "name": "discover",
                "description": "Find recurring patterns in records of symbols passed in the call: contiguous sequences and gapped pairs, with support and cohesion. A statistical description of the input, not a claim about meaning. Nothing is stored.",
                "inputSchema": { "type": "object", "additionalProperties": false, "properties": {
                    "records": { "type": "array", "items": { "type": "array", "items": { "type": "string" } }, "description": "One list of symbols per record. Give this or lines." },
                    "lines": { "type": "string", "description": "One record per line, symbols separated by whitespace." },
                    "times": { "type": "array", "items": { "type": "array", "items": { "type": "integer" } }, "description": "With records: one whole-number timestamp for each symbol, never going back within a record. The answer then has timing: how long each pattern took, in the same ticks." },
                    "min_support": { "type": "integer", "minimum": 2 },
                    "max_length": { "type": "integer", "minimum": 2 },
                    "window": { "type": "integer", "minimum": 0 },
                    "min_cohesion_bits": { "type": "number" },
                    "max_patterns": { "type": "integer", "minimum": 1 }
                } },
                "annotations": read_only,
            }
        ])
    }

    /// Run one tool by name. The HTTP service calls the same function.
    pub fn call(&self, name: &str, args: &Value) -> Result<Value, String> {
        if !args.is_object() {
            return Err("arguments must be an object".into());
        }
        match name {
            "encode" => self.encode(args),
            "decode" => self.decode(args),
            "inspect" => self.inspect(args),
            "compare" => self.compare(args),
            "discover" => self.discover(args),
            other => Err(format!("there is no tool {other:?}")),
        }
    }

    /// Answer one request. `None` for a notification, which gets no answer.
    pub fn handle(&self, request: &Value) -> Option<Value> {
        let id = request.get("id").cloned();
        let error_id = id.clone().unwrap_or(Value::Null);
        let error = |code: i64, message: String| Some(json!({ "jsonrpc": "2.0", "id": error_id.clone(), "error": { "code": code, "message": message } }));
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            return error(-32600, "not a JSON-RPC request: no method".into());
        };
        let id = id?; // without an id it is a notification
        let empty = json!({});
        let params = request.get("params").unwrap_or(&empty);
        let result = match method {
            "initialize" => json!({
                "protocolVersion": MCP_PROTOCOL,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "kute", "version": kute::KUTE_VERSION },
                "instructions": "Tokenization tools over the vocabularies this server was started with. Call inspect without arguments first. Nothing is trained, stored, published or read from disk.",
            }),
            "ping" => json!({}),
            "tools/list" => json!({ "tools": Self::tools() }),
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return error(-32602, "tools/call needs a name".into());
                };
                if !Self::tools().as_array().is_some_and(|tools| tools.iter().any(|t| t["name"] == name)) {
                    return error(-32602, format!("there is no tool {name:?}"));
                }
                // A tool that fails is a result with isError, so the model
                // sees why; protocol errors are for malformed requests.
                match self.call(name, params.get("arguments").unwrap_or(&empty)) {
                    Ok(value) => json!({ "content": [{ "type": "text", "text": value.to_string() }], "structuredContent": value, "isError": false }),
                    Err(why) => json!({ "content": [{ "type": "text", "text": why }], "isError": true }),
                }
            }
            other => return error(-32601, format!("method {other:?} is not supported")),
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    /// Serve until the input ends. Returns the number of requests answered.
    pub fn serve(&self, mut input: impl BufRead, mut output: impl Write) -> kute::Result<u64> {
        let mut answered = 0;
        loop {
            let mut line = Vec::new();
            let read = (&mut input).take(self.limits.max_request_bytes + 1).read_until(b'\n', &mut line)?;
            if read == 0 {
                return Ok(answered);
            }
            let reply = if line.len() as u64 > self.limits.max_request_bytes && !line.ends_with(b"\n") {
                // The rest of the line cannot be skipped without reading it,
                // so say why and stop.
                let message = format!("the request is longer than {} bytes", self.limits.max_request_bytes);
                serde_json::to_writer(&mut output, &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": message } }))?;
                output.write_all(b"\n")?;
                output.flush()?;
                return Err(KuteError::LimitExceeded("an MCP request exceeded the request limit".into()));
            } else if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            } else {
                match serde_json::from_slice::<Value>(&line) {
                    Ok(request) if request.is_object() => self.handle(&request),
                    Ok(_) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "a request must be one JSON object; batches are not supported" } })),
                    Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("not JSON: {e}") } })),
                }
            };
            if let Some(reply) = reply {
                serde_json::to_writer(&mut output, &reply)?;
                output.write_all(b"\n")?;
                output.flush()?;
                answered += 1;
            }
        }
    }
}
