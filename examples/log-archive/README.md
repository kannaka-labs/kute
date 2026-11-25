# A log archive built on KUTE

An application that is not part of KUTE and needs nothing from this
repository except this one file: `log_archive.py` imports the published
`kute` package and the Python standard library. It talks to no service. It
only reads a log; nothing it learns is acted on.

It takes a system log and does two things with it: keeps the held-out part
as tokens and proves the bytes come back exactly, and finds event sequences
that recur within a session and checks them on sessions it had not seen.

## Run it

Python 3.9 or later.

```
python -m venv venv
venv/bin/pip install kute-engine==0.6.0        # venv\Scripts\pip on Windows

curl -L -o OpenSSH.zip "https://zenodo.org/records/8275861/files/OpenSSH.zip?download=1"
unzip OpenSSH.zip

venv/bin/python log_archive.py \
    --log OpenSSH/OpenSSH_full.log \
    --structured OpenSSH/OpenSSH_full.log_structured.csv \
    --work work
```

The data is the OpenSSH log from Loghub-2.0 (Zenodo record 8275861, CC BY
4.0; MD5 of the archive `fef920579c366462ea7d341af8f533d8`): 638,947 lines
from one SSH server, 70.5 MB, with a companion file that names the template
of every line. The download is 8 MB. Any other Loghub-2.0 system should
work the same way if its lines carry `name[pid]:`; only OpenSSH has been
run.

The program exits 0 when the round trip is exact and writes
`work/report.json`. It took 14 seconds on a laptop.

## What it printed

Recorded in `report-openssh-kute-0.6.0.json`, from the package as published
on PyPI, one run:

```
log: 638,947 lines; learned from 511,157, held out 127,790
archive: 14,273,135 bytes -> 4,870,481 tokens (2.931 bytes per token); round trip exact: True
  packed ids 9,740,962 bytes, then xz 501,064; vocabulary file 490,685
  xz -9 of the same bytes, nothing shared: 712,268
patterns: 217 reported from 32,362 sessions; 20 of the top 20 seen again in 19,337 held-out sessions, 8 at half their learned rate or more
```

Run again with `kute-engine==0.7.0` from PyPI, on another machine
(`report-openssh-kute-0.7.0.json`, one run, 21 seconds): every count and
size above is the same. Two things differ: the timings, and the
vocabulary's digest, because the generator's version is part of what the
digest covers. A vocabulary made by 0.6.0 and one made by 0.7.0 from the
same data hold the same tokens and have different digests.

## How to read it

**The round trip is the result.** The last fifth of the log, which the
vocabulary never saw, went to token ids and came back byte for byte.

**The sizes are not a compression claim.** The token ids, packed two bytes
each and then compressed with `xz`, are 30% smaller than `xz` on the same
bytes. But the vocabulary learned from the first four fifths has to be on
both sides, and it is 490,685 bytes as a text file: counted together they
are larger than plain `xz`. Whether that is worth it depends on how much
log is stored against one vocabulary. A compressor with a dictionary
trained on the same first part is the fair rival and was not run here. The
packing is this program's own, not KUTE's container.

**The patterns are regularities in order, nothing more.** Each is a
sequence of template ids that follow each other within one `sshd` process.
All of the top 20 occur again in the held-out sessions, but only 8 at half
their learned rate or better, and several nearly vanish: `E33 E36` falls
from 129 per thousand events to 0.9, while `E8 E29` rises from 39 to 132.
This log changes over time, and a pattern learned early is not a promise
about later. Nothing here says what a pattern means or that it matters.

## Limits

- One log from one server, one split, one run.
- Sessions are keyed by process id alone. A process id that is reused
  joins two sessions into one.
- The event ids come from the dataset's own labels. A live system would
  need its own template extraction, which this does not do.
- Timings are a single run on one machine and are not a benchmark.
