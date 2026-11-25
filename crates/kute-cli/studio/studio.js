// KUTE Studio. Everything shown comes from /v1/ on this origin or from a
// file the person chose. Text from either is only ever set as text,
// never as markup.
"use strict";

const state = { capabilities: null, view: "Runtime" };

function el(tag, attrs, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs || {})) {
    if (key === "class") node.className = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else if (key === "w") node.style.width = value + "px";
    else if (value !== null && value !== undefined && value !== false) node.setAttribute(key, value === true ? "" : String(value));
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

const fmt = (n) => (typeof n === "number" ? n.toLocaleString("en-US", { maximumFractionDigits: 3 }) : String(n));

async function call(operation, body) {
  const started = performance.now();
  const init = body === undefined ? {} : { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) };
  let response;
  try {
    response = await fetch("/v1/" + operation, init);
  } catch (error) {
    throw new Error("the service did not answer: " + error.message);
  }
  let data = null;
  try {
    data = await response.json();
  } catch (error) {
    throw new Error("the service answered " + response.status + " with something that is not JSON");
  }
  if (!response.ok) {
    const problem = data && data.error ? data.error.code + ": " + data.error.message : "status " + response.status;
    throw new Error(problem);
  }
  return { data, operation, at: new Date(), ms: performance.now() - started };
}

// Where a result came from, and the answer itself for anyone who wants it.
function provenance(result, extra) {
  const software = state.capabilities ? state.capabilities.software : "unknown";
  return el("div", {},
    el("div", { class: "prov" }, `${result.operation === "capabilities" ? "GET" : "POST"} /v1/${result.operation} · ${software} · asked ${result.at.toLocaleTimeString()} · answered in ${fmt(Math.round(result.ms))} ms${extra ? " · " + extra : ""}`),
    el("details", {}, el("summary", {}, "The answer as the service gave it"), el("pre", {}, JSON.stringify(result.data, null, 1))));
}

function pairs(object, keys) {
  const body = el("tbody", {});
  for (const key of keys || Object.keys(object)) {
    let value = object[key];
    if (value === undefined) continue;
    if (value === null) value = "none";
    else if (typeof value === "object") value = JSON.stringify(value);
    else if (typeof value === "number") value = fmt(value);
    body.append(el("tr", {}, el("td", { class: "k" }, key.replaceAll("_", " ")), el("td", { class: "v" }, String(value))));
  }
  return el("div", { class: "scroll" }, el("table", {}, body));
}

function table(headers, rows) {
  return el("div", { class: "scroll" }, el("table", {},
    el("thead", {}, el("tr", {}, headers.map((h) => el("th", { class: h.n ? "n" : null }, h.name)))),
    el("tbody", {}, rows.map((row) => el("tr", {}, row.map((cell, i) => el("td", { class: headers[i].n ? "n" : null }, cell)))))));
}

function failure(error) {
  return el("div", { class: "limit", role: "alert" }, error.message);
}

function vocabularySelect(id) {
  const held = state.capabilities ? state.capabilities.vocabularies : [];
  return el("select", { id }, held.map((v) => el("option", { value: v.digest }, `${v.id} ${v.version} (${v.algorithm}, ${fmt(v.tokens)} tokens)`)));
}

function noVocabulary() {
  return el("p", { class: "limit" }, "The service holds no vocabulary. Start it with one: kute serve --studio --vocab file.kvf");
}

function runner(button, output, work) {
  return async () => {
    button.disabled = true;
    output.replaceChildren(el("p", { class: "note" }, "Asking…"));
    try {
      output.replaceChildren(...(await work()));
    } catch (error) {
      output.replaceChildren(failure(error));
    }
    button.disabled = false;
  };
}

// ------------------------------------------------------------------ views

const views = {};

views["Runtime"] = async (root) => {
  const result = await call("capabilities");
  state.capabilities = result.data;
  const c = result.data;
  root.append(
    el("h2", {}, "What this service is"),
    el("div", { class: "panel" }, pairs({ software: c.software, interface: c.api, operations: c.operations.join(", ") })),
    el("h3", {}, "Vocabularies held"),
    c.vocabularies.length
      ? el("div", { class: "panel" }, table(
          [{ name: "id" }, { name: "version" }, { name: "algorithm" }, { name: "tokens", n: true }, { name: "digest" }],
          c.vocabularies.map((v) => [v.id, v.version, v.algorithm, fmt(v.tokens), el("span", { class: "mono" }, v.digest)])))
      : noVocabulary(),
    el("h3", {}, "Limits in force"),
    el("div", { class: "panel" }, pairs(c.limits)),
    el("p", { class: "note" }, "The vocabularies are the files the service was started with. This page cannot add, change or remove one, and the service keeps nothing that is sent to it."),
    provenance(result));
};

views["Vocabulary"] = async (root) => {
  if (!state.capabilities.vocabularies.length) return root.append(noVocabulary());
  const select = vocabularySelect("vocab-pick");
  const summary = el("div", {});
  const longest = el("div", {});
  const one = el("div", {});
  const count = el("input", { type: "number", id: "longest-n", value: 12, min: 1, max: 200 });
  const tokenId = el("input", { type: "number", id: "token-id", value: 300, min: 0 });

  const showToken = (t) => [t.token_id, el("span", { class: "sym" }, typeof t.payload === "string" ? t.payload : JSON.stringify(t.payload)), t.symbol_type, t.metadata && t.metadata.length !== undefined ? t.metadata.length : "", t.frequency === null || t.frequency === undefined ? "not recorded" : fmt(t.frequency), t.metadata && t.metadata.merge ? t.metadata.merge.join(" + ") : ""];
  const tokenHeaders = [{ name: "id", n: true }, { name: "payload" }, { name: "kind" }, { name: "length", n: true }, { name: "frequency in training", n: true }, { name: "made by merging ids" }];

  const load = async () => {
    summary.replaceChildren(el("p", { class: "note" }, "Asking…"));
    try {
      const result = await call("inspect", { vocabulary: select.value });
      const s = result.data.summary;
      summary.replaceChildren(...[
        el("div", { class: "panel" }, pairs(s, ["id", "version", "digest", "algorithm", "adapter", "modality", "tokens", "merges", "longest_token_units", "lossless", "adapter_lossless", "parent", "generator", "kvf_version", "license", "test_vectors"])),
        el("h3", {}, "Tokens by kind"), el("div", { class: "panel" }, pairs(s.tokens_by_kind || {})),
        el("h3", {}, "How it was made"), el("div", { class: "panel" }, pairs({ ...(s.statistics || {}), corpus_fingerprints: (s.corpus_fingerprints || []).join(", ") || "none recorded" })),
        s.adapter_lossless === false ? el("p", { class: "limit" }, "This vocabulary's adapter is not lossless: decoding gives back the symbols, not the original bytes of each record.") : null,
        el("p", { class: "note" }, "The corpus fingerprints identify the training data without holding it. The frequency of a token is what training recorded, on that corpus; it says nothing about other data."),
        provenance(result)].filter(Boolean));
    } catch (error) {
      summary.replaceChildren(failure(error));
    }
    longest.replaceChildren();
    one.replaceChildren();
  };
  select.addEventListener("change", load);

  const longestButton = el("button", { class: "go" }, "Show longest tokens");
  longestButton.addEventListener("click", runner(longestButton, longest, async () => {
    const result = await call("inspect", { vocabulary: select.value, longest: Number(count.value) });
    const tokens = result.data.longest_learned_tokens || [];
    return [el("div", { class: "panel" }, tokens.length ? table(tokenHeaders, tokens.map(showToken)) : el("p", {}, "This vocabulary has no learned tokens.")), provenance(result)];
  }));
  const tokenButton = el("button", { class: "go" }, "Look up token");
  tokenButton.addEventListener("click", runner(tokenButton, one, async () => {
    const result = await call("inspect", { vocabulary: select.value, token: Number(tokenId.value) });
    return [el("div", { class: "panel" }, table(tokenHeaders, [showToken(result.data)]), pairs({ content_hash: result.data.content_hash, reversible: result.data.metadata ? result.data.metadata.reversible : undefined })), provenance(result)];
  }));

  root.append(
    el("h2", {}, "Vocabulary and merges"),
    el("label", { for: "vocab-pick" }, "Vocabulary"), select, summary,
    el("h3", {}, "Longest learned tokens"),
    el("label", { for: "longest-n" }, "How many"), count, longestButton, longest,
    el("h3", {}, "One token"),
    el("label", { for: "token-id" }, "Token id"), tokenId, tokenButton, one);
  await load();
};

views["Encode"] = async (root) => {
  if (!state.capabilities.vocabularies.length) return root.append(noVocabulary());
  const select = vocabularySelect("encode-vocab");
  const text = el("textarea", { id: "encode-text" }, "the quick brown fox");
  const unknown = el("select", { id: "encode-unknown" }, el("option", { value: "strict" }, "strict: refuse a symbol the vocabulary lacks"), el("option", { value: "escape" }, "escape: carry it as it is"));
  const output = el("div", {});
  const button = el("button", { class: "go" }, "Encode, then decode");
  button.addEventListener("click", runner(button, output, async () => {
    const encoded = await call("encode", { vocabulary: select.value, text: text.value, unknown: unknown.value });
    const e = encoded.data;
    const decoded = await call("decode", { container_base64: e.container_base64 });
    const back = decoded.data.text;
    const same = back === text.value;
    const parts = [
      el("div", { class: "panel" }, pairs({
        input_bytes: e.input_bytes, tokens: e.tokens, escaped_symbols: e.escaped_symbols.length, container_bytes: e.container_bytes,
        container_over_input: e.input_bytes ? (e.container_bytes / e.input_bytes).toFixed(3) : "n/a",
      })),
      e.container_bytes > e.input_bytes ? el("p", { class: "limit" }, "The container is larger than the input. A container names its vocabulary and digest, which costs a couple of hundred bytes whatever the input; on a short input that is most of it.") : null,
      el("h3", {}, "Token ids"),
      el("div", { class: "panel tokens" }, e.ids_omitted ? `Not listed: more than ${fmt(state.capabilities.limits.max_ids_listed)} ids.` : e.ids.map((id) => el("span", {}, id))),
      e.escaped_symbols.length ? el("p", { class: "limit" }, `${e.escaped_symbols.length} symbol(s) were not in the vocabulary and were carried by escape: ${JSON.stringify(e.escaped_symbols).slice(0, 400)}`) : null,
      el("h3", {}, "Decoded again"),
      el("div", { class: same ? "note" : "limit" }, back === undefined
        ? "The decoded bytes are not UTF-8 text, so they are not compared here."
        : same ? "Decoding the container gave back exactly the text above." : "Decoding gave back different text. For an adapter that is not lossless that is expected: " + JSON.stringify(back).slice(0, 400)),
      provenance(encoded, `vocabulary ${e.vocabulary.id} ${e.vocabulary.version} ${e.vocabulary.digest}`),
      provenance(decoded),
    ];
    return parts.filter(Boolean);
  }));
  root.append(
    el("h2", {}, "Encode and decode"),
    el("label", { for: "encode-vocab" }, "Vocabulary"), select,
    el("label", { for: "encode-text" }, "Text, in the form the vocabulary's adapter reads"), text,
    el("label", { for: "encode-unknown" }, "A symbol the vocabulary lacks"), unknown,
    button, output,
    el("p", { class: "note" }, "A token count is not a size. The container bytes are what would be stored or sent."));
};

views["Compare"] = async (root) => {
  if (state.capabilities.vocabularies.length < 2) return root.append(el("p", { class: "limit" }, "Comparing needs two vocabularies. Start the service with a parent and its child: kute serve --studio --vocab parent.kvf --vocab child.kvf"));
  const a = vocabularySelect("compare-a");
  const b = vocabularySelect("compare-b");
  b.selectedIndex = 1;
  const output = el("div", {});
  const button = el("button", { class: "go" }, "Compare");
  button.addEventListener("click", runner(button, output, async () => {
    const result = await call("compare", { a: a.value, b: b.value });
    const d = result.data;
    const lists = Object.keys(d).filter((k) => Array.isArray(d[k]));
    const scalars = Object.keys(d).filter((k) => !Array.isArray(d[k]));
    return [
      el("div", { class: "panel" }, pairs(d, scalars)),
      ...lists.map((k) => el("div", {}, el("h3", {}, k.replaceAll("_", " ")), el("div", { class: "panel" }, d[k].length ? d[k].map((x) => el("span", { class: "sym" }, x)) : "none"))),
      el("p", { class: "note" }, "This compares what tokens the two vocabularies hold. It does not say which encodes better: for that, run kute shadow on held-out data and open its report under Reports."),
      provenance(result),
    ];
  }));
  root.append(el("h2", {}, "Evolution comparison"),
    el("div", { class: "row" }, el("div", {}, el("label", { for: "compare-a" }, "First"), a), el("div", {}, el("label", { for: "compare-b" }, "Second"), b)),
    button, output);
};

views["Patterns"] = async (root) => {
  const file = el("input", { type: "file", id: "corpus-file" });
  const text = el("textarea", { id: "corpus-text", placeholder: "One event per line, or JSON Lines" });
  const symbolField = el("input", { type: "text", id: "symbol-field", placeholder: "event" });
  const groupField = el("input", { type: "text", id: "group-field", placeholder: "none: the whole corpus is one record" });
  const minSupport = el("input", { type: "number", id: "min-support", value: 5, min: 2 });
  const maxLength = el("input", { type: "number", id: "max-length", value: 8, min: 2 });
  const output = el("div", {});
  let source = "typed into this page";
  file.addEventListener("change", async () => {
    const chosen = file.files[0];
    if (!chosen) return;
    text.value = await chosen.text();
    source = `file ${chosen.name}, ${fmt(chosen.size)} bytes, last changed ${new Date(chosen.lastModified).toISOString()}`;
  });
  text.addEventListener("input", () => { source = "typed into this page"; });

  // The corpus as records of symbols. With a symbol field each line is a
  // JSON object; otherwise each line is one symbol.
  const records = () => {
    const lines = text.value.split(/\r?\n/).filter((line) => line.trim() !== "");
    if (!lines.length) throw new Error("the corpus is empty");
    const field = symbolField.value.trim();
    const group = groupField.value.trim();
    if (!field) {
      if (group) throw new Error("an entity field needs a symbol field: both are read from JSON Lines");
      return { records: [lines.map((line) => line.trim())], skipped: 0 };
    }
    const byEntity = new Map();
    let skipped = 0;
    for (const line of lines) {
      let object;
      try { object = JSON.parse(line); } catch (error) { skipped += 1; continue; }
      if (object === null || typeof object !== "object" || typeof object[field] !== "string") { skipped += 1; continue; }
      const key = group ? String(object[group]) : "";
      if (!byEntity.has(key)) byEntity.set(key, []);
      byEntity.get(key).push(object[field]);
    }
    if (!byEntity.size) throw new Error(`no line is a JSON object with a text field ${JSON.stringify(field)}`);
    return { records: [...byEntity.values()], skipped };
  };

  const button = el("button", { class: "go" }, "Look for recurring sequences");
  button.addEventListener("click", runner(button, output, async () => {
    const corpus = records();
    const result = await call("discover", { records: corpus.records, min_support: Number(minSupport.value), max_length: Number(maxLength.value) });
    const d = result.data;
    const most = Math.max(1, ...d.patterns.map((p) => p.support));
    return [
      corpus.skipped ? el("p", { class: "limit" }, `${fmt(corpus.skipped)} line(s) were not JSON objects with that field and were left out.`) : null,
      el("h3", {}, "What was read"),
      el("div", { class: "panel" }, pairs({ ...d.observation, most_frequent: d.observation.most_frequent.map((m) => `${m.symbol} ×${m.count}`).join(", "), coverage: d.coverage })),
      el("h3", {}, `Recurring sequences (${d.patterns.length})`),
      el("div", { class: "panel" }, d.patterns.length ? table(
        [{ name: "sequence" }, { name: "kind" }, { name: "support", n: true }, { name: "records", n: true }, { name: "cohesion, bits", n: true }, { name: "evidence, bits", n: true }],
        d.patterns.map((p) => [
          el("span", {}, p.symbols.map((s) => el("span", { class: "sym" }, s))), p.kind,
          el("span", {}, el("span", { class: "bar", w: Math.round(60 * p.support / most) }), fmt(p.support)),
          fmt(p.records), p.cohesion_bits.toFixed(2), p.evidence_bits.toFixed(1)])) : el("p", {}, "Nothing recurred often enough, and together enough, to be listed.")),
      d.patterns.length >= d.params.max_patterns ? el("p", { class: "limit" }, `The list stops at ${d.params.max_patterns} sequences; there may be more.`) : null,
      el("p", { class: "note" }, "Support is how often a sequence occurred in this corpus. Cohesion is how much more often than its parts would meet by chance. Both describe this corpus only: nothing here was checked on held-out data, and a sequence that recurs is not shown to mean anything or to cause anything. Timing between events is not available through the service; kute discover on the command line reports it."),
      provenance(result, `corpus: ${source} · read as ${fmt(corpus.records.length)} record(s) · parameters ${JSON.stringify(d.params)}`),
    ].filter(Boolean);
  }));
  root.append(
    el("h2", {}, "Corpus import and pattern evidence"),
    el("p", {}, "Choose a file or paste a corpus. It is read by this page and sent to the service on this machine, which keeps nothing."),
    el("label", { for: "corpus-file" }, "Corpus file"), file,
    el("label", { for: "corpus-text" }, "Corpus"), text,
    el("div", { class: "row" },
      el("div", {}, el("label", { for: "symbol-field" }, "Field holding the event name (JSON Lines); empty for one event per line"), symbolField),
      el("div", {}, el("label", { for: "group-field" }, "Field naming the entity (optional)"), groupField)),
    el("div", { class: "row" },
      el("div", {}, el("label", { for: "min-support" }, "Least support"), minSupport),
      el("div", {}, el("label", { for: "max-length" }, "Longest sequence"), maxLength)),
    button, output,
    state.capabilities ? el("p", { class: "note" }, `The service takes at most ${fmt(state.capabilities.limits.max_input_bytes)} bytes of symbols in one call. For more, use kute discover or kute live on the command line.`) : null);
};

// A report file from the command line, shown as it is.
function tree(value, depth) {
  if (value === null || typeof value !== "object") return el("span", { class: "mono" }, value === null ? "none" : typeof value === "number" ? fmt(value) : String(value));
  const entries = Array.isArray(value) ? value.map((v, i) => [String(i + 1), v]) : Object.entries(value);
  if (!entries.length) return el("span", { class: "mono" }, Array.isArray(value) ? "none" : "empty");
  if (Array.isArray(value) && value.every((v) => v === null || typeof v !== "object")) return el("span", { class: "mono" }, value.map((v) => (typeof v === "number" ? fmt(v) : String(v))).join(", "));
  const rows = entries.slice(0, 200).map(([k, v]) => el("tr", {}, el("td", { class: "k" }, k.replaceAll("_", " ")), el("td", {}, tree(v, depth + 1))));
  if (entries.length > 200) rows.push(el("tr", {}, el("td", { class: "k" }, "…"), el("td", {}, `${fmt(entries.length - 200)} more not shown`)));
  const body = el("div", { class: "scroll" }, el("table", {}, el("tbody", {}, rows)));
  return depth > 1 ? el("details", {}, el("summary", {}, `${fmt(entries.length)} ${Array.isArray(value) ? "items" : "fields"}`), body) : body;
}

function recognise(report) {
  if (report && report.decision && report.comparison) return ["kute shadow", `Decision: ${report.decision}. ${(Array.isArray(report.reasons) ? report.reasons : []).join("; ") || "No constraint was broken."}`, report.assumes || report.not_measured];
  if (report && report.context_over_previous && typeof report.context_over_previous === "object") return ["kute predict", `Looking further back saved ${fmt(report.context_over_previous.bits_per_event)} bits an event over one event of past, on ${fmt(report.held_out_events)} held-out events.`, report.note];
  if (report && Array.isArray(report.lines)) return ["kute live", `${fmt(report.lines.length)} lines: window reports and a summary.`, "Counts under bounds are lower limits; each window says whether it was exact."];
  if (report && report.synthetic === true) return ["a benchmark or example report", "The report says its data is synthetic.", null];
  return ["a JSON file", "Not a report this page recognises; shown as it is.", null];
}

views["Reports"] = async (root) => {
  const file = el("input", { type: "file", id: "report-file", accept: ".json,.jsonl" });
  const output = el("div", {});
  file.addEventListener("change", async () => {
    const chosen = file.files[0];
    if (!chosen) return;
    try {
      const text = await chosen.text();
      let report;
      try {
        report = JSON.parse(text);
      } catch (error) {
        report = { lines: text.split(/\r?\n/).filter((l) => l.trim()).map((l) => JSON.parse(l)) };
      }
      const [kind, headline, caution] = recognise(report);
      output.replaceChildren(...[
        el("h3", {}, `Read as ${kind}`),
        el("p", {}, headline),
        caution ? el("p", { class: "limit" }, String(caution)) : null,
        el("div", { class: "panel" }, tree(report, 0)),
        el("div", { class: "prov" }, `file ${chosen.name} · ${fmt(chosen.size)} bytes · last changed ${new Date(chosen.lastModified).toISOString()} · read by this page, not sent anywhere, and not run again${report.kute_version ? " · made by " + report.kute_version : ""}`)].filter(Boolean));
    } catch (error) {
      output.replaceChildren(failure(new Error("that file is not JSON or JSON Lines: " + error.message)));
    }
  });
  root.append(
    el("h2", {}, "Benchmark and decision reports"),
    el("p", {}, "Open a report written by the command line: kute shadow, kute predict, the output of kute live, or a file from benchmarks/results."),
    el("label", { for: "report-file" }, "Report file"), file, output,
    el("p", { class: "note" }, "This shows a measurement that was made earlier. It does not make it again, and it cannot tell whether the file was edited after it was written."));
};

views["Not here"] = async (root) => {
  root.append(
    el("h2", {}, "What this page does not show"),
    el("p", {}, "The specification lists these for Studio. The service has no operation for them, so there is nothing real to show, and this page shows nothing rather than a mock-up."),
    el("div", { class: "panel" }, table([{ name: "view" }, { name: "why not" }, { name: "where it is today" }], [
      ["Peer status", "kute peer is a separate process, one connection at a time, and reports when it ends.", "The JSON that kute peer serve and kute peer send print."],
      ["Pattern timing", "The discover operation of the service takes symbols without times.", "kute discover with --time-field; kute live."],
      ["Running a benchmark again", "The service is read-only and runs nothing but its five operations.", "The scripts in benchmarks/; open their reports under Reports."],
      ["Representation experiments", "No representation research is built yet (specification F1).", "Nowhere."],
      ["Training and evolving", "The service cannot create or change a vocabulary.", "kute train, kute evolve, kute shadow."],
    ])));
};

// ------------------------------------------------------------------ frame

async function show(name) {
  state.view = name;
  history.replaceState(null, "", "#" + encodeURIComponent(name));
  for (const button of document.querySelectorAll("#tabs button")) {
    if (button.textContent === name) button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  }
  const root = document.getElementById("view");
  root.replaceChildren();
  try {
    await views[name](root);
  } catch (error) {
    root.replaceChildren(failure(error));
  }
}

async function start() {
  const tabs = document.getElementById("tabs");
  for (const name of Object.keys(views)) tabs.append(el("button", { type: "button", onclick: () => show(name) }, name));
  const line = document.getElementById("runtime-line");
  try {
    state.capabilities = (await call("capabilities")).data;
    line.textContent = `${state.capabilities.software} · interface ${state.capabilities.api} · ${state.capabilities.vocabularies.length} vocabular${state.capabilities.vocabularies.length === 1 ? "y" : "ies"} held · served from ${location.host}`;
  } catch (error) {
    line.textContent = "The service did not answer: " + error.message;
    state.capabilities = { software: "unknown", api: "?", vocabularies: [], limits: {}, operations: [] };
  }
  // A view can be linked to: /studio#Patterns. The fragment is whatever
  // a link says, so it may not decode, and it may name something every
  // object has ("constructor") and not a view.
  let wanted = "";
  try {
    wanted = decodeURIComponent(location.hash.slice(1));
  } catch (error) {
    wanted = "";
  }
  await show(Object.hasOwn(views, wanted) ? wanted : "Runtime");
}

start();
