"""Read a NATS JetStream stream from its start, read-only, into a local JSON Lines file.

    python bus_dump.py SERVER STREAM OUT.jsonl [MAX]

SERVER is the NATS address, for example nats://127.0.0.1:4222.

One line per message: {"seq", "ns", "subject", "data"} with data as text.
Uses an ordered ephemeral consumer, which the server forgets when this ends.
Credentials, if the server wants them, come from NATS_USER and
NATS_PASSWORD in the environment. Needs the `nats-py` package. It only
reads: nothing is published and no durable consumer is left behind.
"""
import asyncio
import json
import os
import sys

import nats
from nats.js.api import ConsumerConfig, DeliverPolicy, AckPolicy


async def main(server, stream, out, most):
    nc = await nats.connect(server, user=os.environ.get("NATS_USER"), password=os.environ.get("NATS_PASSWORD"), name="kute-read-only-dump")
    js = nc.jetstream()
    info = await js.stream_info(stream)
    total = info.state.messages
    # ">" within the stream: every subject it keeps, however many it was
    # made with.
    sub = await js.subscribe(">", stream=stream, ordered_consumer=True, config=ConsumerConfig(deliver_policy=DeliverPolicy.ALL, ack_policy=AckPolicy.NONE))
    n = 0
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        while n < min(total, most):
            try:
                m = await sub.next_msg(timeout=20)
            except Exception as e:  # noqa: BLE001
                print("stopped:", type(e).__name__, file=sys.stderr)
                break
            md = m.metadata
            f.write(json.dumps({"seq": md.sequence.stream, "ns": int(md.timestamp.timestamp() * 1e9), "subject": m.subject, "data": m.data.decode("utf-8", "replace")}, ensure_ascii=False) + "\n")
            n += 1
            if n % 20000 == 0:
                print(n, "of", total, file=sys.stderr)
    await sub.unsubscribe()
    await nc.close()
    print(json.dumps({"stream": stream, "messages_in_stream": total, "written": n, "first_seq": info.state.first_seq, "last_seq": info.state.last_seq}))


asyncio.run(main(sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]) if len(sys.argv) > 4 else 10**9))
