# Example vocabularies

Four KVF 0.1 files trained on the seeded data from `kute sample-data`. The
data is **synthetic**, built from small templates. These files are for trying
the tools and for checking that another KVF reader agrees with this one (each
carries its own test vectors). They are not useful vocabularies for real data.

| file | id | algorithm | adapter | tokens |
|------|----|-----------|---------|-------:|
| `words.kvf` | `example:words` | bpe | text | 2,000 |
| `frames.kvf` | `example:frames` | bpe | binary | 600 |
| `agent-messages.kvf` | `example:agent:messages` | bpe | jsonl | 1,200 |
| `industrial-events.kvf` | `kute:industrial:events` | sequence-bpe | jsonl-events | 64 |

Training is deterministic, so the commands in `docs/use-cases.md` (with the
`--description` shown in each file's manifest) reproduce these files and their
digests.

```
kute validate examples/vocabularies/words.kvf
kute inspect examples/vocabularies/industrial-events.kvf --longest 5
```
