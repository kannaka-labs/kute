"""Turn the Loghub-2.0 OpenSSH log into complete event records, one JSON object per line.

    python openssh_events.py OpenSSH_full.log OpenSSH_full.log_structured.csv OUT.jsonl

Each record: t (seconds since the first line, year assumed constant), host,
pid, event (the template id) and params (the text that fills the
template's <*> slots). Lines whose content does not fit their template are
kept with the whole content as one parameter and counted.
"""
import csv
import json
import re
import sys
import time

log, structured, out = sys.argv[1:4]
MONTHS = {m: i + 1 for i, m in enumerate("Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec".split())}
HEAD = re.compile(r"^(\w{3}) +(\d+) (\d\d):(\d\d):(\d\d) (\S+) (\w+)\[(\d+)\]: (.*)$")
compiled = {}
first = None
misfit = bad_head = n = 0
with open(log, encoding="utf-8", newline="") as lf, open(structured, encoding="utf-8", newline="") as sf, open(out, "w", encoding="utf-8", newline="\n") as of:
    for line, row in zip(lf, csv.DictReader(sf)):
        n += 1
        m = HEAD.match(line.rstrip("\r\n"))
        if not m:
            bad_head += 1
            continue
        mon, day, hh, mm, ss, host, proc, pid, content = m.groups()
        t = int(time.mktime((2023, MONTHS[mon], int(day), int(hh), int(mm), int(ss), 0, 0, 0)))
        first = t if first is None else first
        template = row["EventTemplate"]
        rx = compiled.get(template)
        if rx is None:
            rx = compiled[template] = re.compile("^" + "(.*?)".join(re.escape(p) for p in template.split("<*>")) + "$")
        fit = rx.match(content)
        if fit:
            params = list(fit.groups())
        else:
            misfit += 1
            params = [content]
        record = {"t": t - first, "host": host, "proc": proc, "pid": int(pid), "event": row["EventId"], "fits": bool(fit), "params": params}
        of.write(json.dumps(record, separators=(",", ":"), ensure_ascii=False) + "\n")
print(f"{n} lines; {bad_head} without the usual head (dropped); {misfit} whose content does not fit its template")
