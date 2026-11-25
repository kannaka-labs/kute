// Drives KUTE Studio in a real browser, headless, over the DevTools
// protocol. No packages: Node's own fetch and WebSocket (Node 22 or
// later). Started by tests/studio_browser.rs, which runs the service and
// writes the fixture files.
//
//   node studio_browser.mjs <studio url> <browser executable> <fixture dir>
//
// Prints one JSON object naming what was checked and exits 0, or prints
// the failure and exits 1.

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const [studioUrl, browserPath, fixtures] = process.argv.slice(2);
const origin = new URL(studioUrl).origin;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const checked = [];

function must(condition, what) {
  if (!condition) throw new Error("failed: " + what);
  checked.push(what);
}

const profile = fs.mkdtempSync(path.join(os.tmpdir(), "kute-studio-browser-"));
const flags = ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu", "--disable-extensions", "--disable-background-networking", "about:blank"];
if (process.env.KUTE_STUDIO_BROWSER_NO_SANDBOX === "1") flags.unshift("--no-sandbox");
const browser = spawn(browserPath, flags, { stdio: "ignore" });

async function connect() {
  // The browser writes the port it chose into its profile.
  const portFile = path.join(profile, "DevToolsActivePort");
  for (let i = 0; i < 300 && !fs.existsSync(portFile); i += 1) await sleep(100);
  if (!fs.existsSync(portFile)) throw new Error("the browser did not open a debugging port within 30 s");
  const port = fs.readFileSync(portFile, "utf8").split("\n")[0].trim();
  let target;
  for (let i = 0; i < 100 && !target; i += 1) {
    const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    target = list.find((t) => t.type === "page");
    if (!target) await sleep(100);
  }
  if (!target) throw new Error("the browser has no page to drive");
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve);
    socket.addEventListener("error", () => reject(new Error("could not connect to the page")));
  });
  return socket;
}

let nextId = 1;
const waiting = new Map();
const requests = [];
const exceptions = [];
const blocked = [];
let socket;

function send(method, params = {}) {
  const id = nextId;
  nextId += 1;
  socket.send(JSON.stringify({ id, method, params }));
  return new Promise((resolve, reject) => waiting.set(id, { resolve, reject, method }));
}

function listen() {
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    if (message.id) {
      const pending = waiting.get(message.id);
      waiting.delete(message.id);
      if (message.error) pending.reject(new Error(`${pending.method}: ${message.error.message}`));
      else pending.resolve(message.result);
    } else if (message.method === "Network.requestWillBeSent") {
      requests.push(message.params.request.url);
    } else if (message.method === "Runtime.exceptionThrown") {
      exceptions.push(message.params.exceptionDetails.exception?.description || message.params.exceptionDetails.text);
    } else if (message.method === "Log.entryAdded" && message.params.entry.source === "security") {
      blocked.push(message.params.entry.text);
    }
  });
}

// Run an expression in the page and give back its value.
async function page(expression) {
  const result = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (result.exceptionDetails) throw new Error("in the page: " + (result.exceptionDetails.exception?.description || result.exceptionDetails.text));
  return result.result.value;
}

async function until(expression, what, seconds = 20) {
  for (let i = 0; i < seconds * 10; i += 1) {
    if (await page(expression)) return;
    await sleep(100);
  }
  throw new Error(`timed out waiting for: ${what}. The view says: ${(await page('document.getElementById("view").innerText')).slice(0, 600)}`);
}

// Helpers that live in the page for the length of the run.
const HELPERS = `
  window.__t = {
    tab(name) { [...document.querySelectorAll("#tabs button")].find((b) => b.textContent === name).click(); },
    current() { const b = document.querySelector('#tabs button[aria-current="page"]'); return b ? b.textContent : null; },
    set(id, value) { const e = document.getElementById(id); e.value = value; e.dispatchEvent(new Event("input", { bubbles: true })); e.dispatchEvent(new Event("change", { bubbles: true })); },
    press(label) { [...document.querySelectorAll("#view button")].find((b) => b.textContent === label).click(); },
    // The text itself, not as styled: headings are drawn in capitals.
    text() { return document.getElementById("view").textContent; },
    alerts() { return [...document.querySelectorAll('#view [role="alert"]')].map((a) => a.textContent); },
    asking() { return document.getElementById("view").textContent.includes("Asking…"); },
    rowsAfter(heading) {
      const h = [...document.querySelectorAll("#view h3")].find((x) => x.textContent.startsWith(heading));
      if (!h) return null;
      const panel = h.nextElementSibling;
      return [...panel.querySelectorAll("tbody tr")].map((tr) => [...tr.children].map((td) => td.textContent));
    },
  };
  true`;

async function open(name) {
  await page(`window.__t.tab(${JSON.stringify(name)})`);
  await until(`window.__t.current() === ${JSON.stringify(name)} && document.getElementById("view").children.length > 0 && !window.__t.asking()`, `the ${name} view`);
}

async function choose(inputId, file) {
  const { root } = await send("DOM.getDocument", {});
  const { nodeId } = await send("DOM.querySelector", { nodeId: root.nodeId, selector: "#" + inputId });
  await send("DOM.setFileInputFiles", { nodeId, files: [path.join(fixtures, file)] });
}

async function run() {
  socket = await connect();
  listen();
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Network.enable");
  await send("Log.enable");
  await send("Page.navigate", { url: studioUrl });
  await until('document.querySelectorAll("#tabs button").length > 0 && document.getElementById("view").children.length > 0', "the page to start");
  await page(HELPERS);

  // The frame, and the first view.
  const tabs = await page('[...document.querySelectorAll("#tabs button")].map((b) => b.textContent)');
  must(JSON.stringify(tabs) === JSON.stringify(["Runtime", "Vocabulary", "Encode", "Compare", "Patterns", "Reports", "Not here"]), "the seven views are offered: " + tabs.join(", "));
  const line = await page('document.getElementById("runtime-line").textContent');
  must(/^kute \d+\.\d+\.\d+ · interface v1 · 2 vocabularies/.test(line), "the header names the software and what it holds: " + line);
  const runtime = await page("window.__t.text()");
  must(runtime.includes("test:studio:events") && runtime.includes("test:studio:text") && runtime.includes("sha256:"), "Runtime lists both vocabularies with digests");

  // Every view draws without an error.
  for (const name of tabs) {
    await open(name);
    const alerts = await page("window.__t.alerts()");
    must(alerts.length === 0, `${name} draws without an error${alerts.length ? ": " + alerts.join("; ") : ""}`);
  }
  must((await page("location.hash")) === "#Not%20here", "the address names the view shown");

  // Vocabulary: a summary, the longest tokens, one token.
  await open("Vocabulary");
  must((await page("window.__t.text()")).includes("corpus fingerprints"), "Vocabulary shows a summary");
  await page('window.__t.press("Show longest tokens")');
  await until('document.querySelectorAll("#view table thead").length > 0 && !window.__t.asking()', "the longest tokens");
  await page('window.__t.set("token-id", "3"); window.__t.press("Look up token")');
  await until('window.__t.text().includes("content hash") && !window.__t.asking()', "one token");
  must((await page("window.__t.alerts()")).length === 0, "Vocabulary answers both questions");
  // A token that is not there is an error shown in the page, not a broken page.
  await page('window.__t.set("token-id", "999999"); window.__t.press("Look up token")');
  await until("window.__t.alerts().length === 1", "the refusal of a token id that does not exist");
  must(true, "a refusal from the service is shown as an alert: " + (await page("window.__t.alerts()[0]")).slice(0, 80));

  // Encode with the text vocabulary, and decode again.
  await open("Encode");
  await page(`
    const pick = document.getElementById("encode-vocab");
    pick.value = [...pick.options].find((o) => o.textContent.startsWith("test:studio:text")).value;
    window.__t.set("encode-text", "The engine reads the event log.");
    window.__t.press("Encode, then decode")`);
  await until('window.__t.text().includes("Decoded again") && !window.__t.asking()', "an encoding");
  const encoded = await page("window.__t.text()");
  must(encoded.includes("Decoding the container gave back exactly the text above."), "Encode decodes back to the text typed");
  must(encoded.includes("POST /v1/encode") && encoded.includes("POST /v1/decode"), "each result names the request that produced it");

  // Compare the two vocabularies.
  await open("Compare");
  await page('window.__t.press("Compare")');
  await until('window.__t.text().includes("POST /v1/compare") && !window.__t.asking()', "a comparison");
  must((await page("window.__t.alerts()")).length === 0, "Compare answers");

  // Patterns, with a time field: thirty entities, each A B C twice, the
  // three events 0, 2 and 7 ticks into a cycle of 10. Two lines are
  // left out: one that is not JSON and one whose time is past 2^53.
  const lines = [];
  for (let r = 0; r < 30; r += 1) for (let i = 0; i < 6; i += 1) lines.push(JSON.stringify({ event: "ABC"[i % 3], unit: r, t: r * 1000 + Math.floor(i / 3) * 10 + [0, 2, 7][i % 3] }));
  lines.push("not json");
  lines.push('{"event":"A","unit":0,"t":9007199254740994}');
  // An event name that would be markup if the page ever treated text as markup.
  const hostile = '<img src=x onerror="window.__pwned=1">';
  for (let i = 0; i < 8; i += 1) lines.push(JSON.stringify({ event: hostile, unit: "h", t: i }));
  await open("Patterns");
  await page(`window.__t.set("corpus-text", ${JSON.stringify(lines.join("\n"))}); window.__t.set("symbol-field", "event"); window.__t.set("group-field", "unit"); window.__t.set("time-field", "t"); window.__t.press("Look for recurring sequences")`);
  await until('window.__t.text().includes("POST /v1/discover") && !window.__t.asking()', "patterns");
  const patterns = await page("window.__t.text()");
  must((await page("window.__t.alerts()")).length === 0, "Patterns answers");
  must(patterns.includes("2 line(s) were not JSON objects with that field and were left out."), "lines that cannot be read are counted, not guessed at");
  const found = await page('window.__t.rowsAfter("Recurring sequences")');
  must(found && found.some((row) => row[0] === "ABC"), "the planted sequence A B C is found");
  const timing = await page('window.__t.rowsAfter("How long each took")');
  const abc = timing && timing.find((row) => row[0] === "ABC");
  must(abc && abc.slice(1).join(",") === "60,7,7,7,7,7", "the timing table gives A B C sixty occurrences of 7 ticks: " + JSON.stringify(abc));
  must(patterns.includes("predicts nothing"), "the timing table says what it does not show");
  must((await page("window.__pwned")) === undefined && (await page('document.querySelector("#view img")')) === null && patterns.includes("<img src=x"), "an event name that looks like markup is shown as text and runs nothing");
  // Without a time field there is no timing table.
  await page('window.__t.set("time-field", ""); window.__t.press("Look for recurring sequences")');
  await until('window.__t.text().includes("POST /v1/discover") && !window.__t.asking()', "patterns without times");
  must((await page('window.__t.rowsAfter("How long each took")')) === null, "no time field, no timing table");

  // Reports: what the command line printed, one exchange and several.
  await open("Reports");
  for (const [file, kind, says] of [
    ["serve.json", "Read as kute peer serve, one exchange", "1 delivered"],
    ["send.json", "Read as kute peer send, one exchange", "every receipt matching"],
    ["both.log", "Read as kute peer, a log of exchanges", "2 exchange(s): 1 served, 1 sent"],
    ["shadow.json", "Read as kute shadow", "Decision:"],
    ["other.json", "Read as a JSON file", "Not a report this page recognises"],
  ]) {
    await choose("report-file", file);
    await until(`window.__t.text().includes(${JSON.stringify(kind)}) || window.__t.alerts().length > 0`, `the report ${file}`);
    const report = await page("window.__t.text()");
    must(report.includes(kind) && report.includes(says) && report.includes(`file ${file}`), `Reports reads ${file} as: ${kind.slice(8)}`);
  }
  await choose("report-file", "broken.json");
  await until("window.__t.alerts().length === 1", "the refusal of a broken file");
  must((await page("window.__t.alerts()[0]")).includes("not JSON"), "a file that is not JSON is refused in the page");

  // What the browser enforces. The page may reach its own origin only,
  // and may run only the script it was served.
  const outside = await page('fetch("http://127.0.0.1:9/").then(() => "reached", () => "refused")');
  must(outside === "refused", "a request to another origin is refused");
  await page('const s = document.createElement("script"); s.textContent = "window.__inline = 1"; document.body.append(s); true');
  must((await page("window.__inline")) === undefined, "a script written into the page does not run");
  must(blocked.some((text) => text.includes("Content Security Policy")), "and the browser says its policy is why: " + blocked.length + " refusal(s) logged");
  const foreign = requests.filter((url) => !url.startsWith(origin + "/") && !url.startsWith("about:") && !url.startsWith("http://127.0.0.1:9/"));
  must(foreign.length === 0, `every request the page made went to ${origin} (${requests.length} requests)${foreign.length ? ": " + foreign.join(", ") : ""}`);
  must(exceptions.length === 0, "no script error in the whole run" + (exceptions.length ? ": " + exceptions.join("; ") : ""));
}

let failed = null;
try {
  await run();
} catch (error) {
  failed = error;
}
try { socket?.close(); } catch {}
browser.kill();
await sleep(300);
try { fs.rmSync(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 }); } catch {}
if (failed) {
  console.error(failed.message);
  console.error(JSON.stringify({ passed_before_failing: checked }, null, 1));
  process.exit(1);
}
console.log(JSON.stringify({ browser: browserPath, checks: checked.length, checked }, null, 1));
process.exit(0);
