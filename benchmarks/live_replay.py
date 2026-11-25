"""Replay a day of real events through `kute live` and measure it.

    python benchmarks/live_replay.py KUTE_BINARY CLOCK_EVENTS.jsonl WORK_DIR > report.json

CLOCK_EVENTS.jsonl is the output of `hdfs_clock.py`. The first 24 hours
of it, in the order the log was written, are replayed as fast as they
can be read (this is a replay, not a wait of 24 hours). Four things are
measured:

1. A run with small configured bounds: throughput, peak memory of the
   process, the state it reports against the bound it computes, what it
   dropped and what it approximated.
2. The same run killed partway and resumed from its checkpoint: are the
   windows the same as those of the run that was not interrupted.
3. Accuracy of the bounded run: each hourly window against a run with
   bounds large enough that every window is exact.
4. The exact stream against offline discovery: the whole day as one
   window against `kute discover` on the same file.
5. The same events delivered out of order, within the lateness allowed.

One run of each. Times are wall-clock.
"""
import ctypes
import json
import pathlib
import subprocess
import sys
import time

FIELDS = ["--symbol-field", "event", "--time-field", "t", "--group-field", "block"]
HOUR = ["--pane", "3600", "--min-support", "5", "--max-patterns", "200"]
SMALL = ["--max-candidates", "2000", "--max-entities", "20000", "--max-buffered", "20000"]
LARGE = ["--max-candidates", "5000000", "--max-entities", "2000000", "--max-buffered", "2000000"]


def peak_memory(process):
    """Peak working set of a finished child, in bytes (Windows), or None."""
    if sys.platform != "win32":
        return None

    class Counters(ctypes.Structure):
        _fields_ = [("cb", ctypes.c_ulong), ("PageFaultCount", ctypes.c_ulong)] + [(n, ctypes.c_size_t) for n in ("PeakWorkingSetSize", "WorkingSetSize", "a", "b", "c", "d", "PagefileUsage", "PeakPagefileUsage")]

    counters = Counters()
    counters.cb = ctypes.sizeof(counters)
    get = ctypes.windll.psapi.GetProcessMemoryInfo
    get.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_ulong]
    ok = get(int(process._handle), ctypes.byref(counters), counters.cb)
    return counters.PeakWorkingSetSize if ok else None


def live(kute, source, extra, stop_after=None):
    """Run `kute live`; returns windows, summary, seconds, peak memory.
    With stop_after, the process is killed once that many windows are out."""
    started = time.perf_counter()
    process = subprocess.Popen([kute, "live", str(source), *FIELDS, *extra], stdout=subprocess.PIPE)
    windows, summary = [], None
    for line in process.stdout:
        value = json.loads(line)
        if "window" in value:
            windows.append(value["window"])
            if stop_after and len(windows) == stop_after:
                process.kill()
                break
        else:
            summary = value["summary"]
    process.stdout.close()
    process.wait()
    seconds = time.perf_counter() - started
    return windows, summary, round(seconds, 2), peak_memory(process)


def strip(window):
    """A window without the figure that depends on the moment it was taken."""
    return {k: v for k, v in window.items() if k != "state_bytes"}


def accuracy(bounded, exact):
    """Each window of a bounded run against the same window of the exact run."""
    rows = []
    for small, large in zip(bounded, exact):
        assert small["start"] == large["start"]
        truth = {tuple(p["symbols"]): p["support"] for p in large["patterns"]}
        found = {tuple(p["symbols"]): p for p in small["patterns"]}
        both = [k for k in truth if k in found]
        top = [tuple(p["symbols"]) for p in large["patterns"][:20]]
        errors = [(truth[k] - found[k]["support"]) / truth[k] for k in both]
        rows.append({
            "start": small["start"],
            "events": small["events"],
            "exact_patterns": len(truth),
            "found_of_them": len(both),
            "found_of_the_top_20": sum(k in found for k in top),
            "reported_but_not_in_exact_top": len(found) - len(both),
            "largest_relative_undercount": round(max(errors), 4) if errors else None,
            "any_support_above_the_truth": any(found[k]["support"] > truth[k] for k in both),
        })
    return {
        "found_of_exact_patterns": round(sum(r["found_of_them"] for r in rows) / sum(r["exact_patterns"] for r in rows), 4),
        "found_of_top_20": round(sum(r["found_of_the_top_20"] for r in rows) / (20 * len(rows)), 4),
        "largest_relative_undercount": max(r["largest_relative_undercount"] or 0 for r in rows),
        "any_support_above_the_truth": any(r["any_support_above_the_truth"] for r in rows),
        "windows": rows,
    }


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    kute = str(pathlib.Path(sys.argv[1]).resolve())
    work = pathlib.Path(sys.argv[3])
    work.mkdir(parents=True, exist_ok=True)
    day = work / "day.jsonl"
    events = 0
    back = 0
    latest = 0
    blocks = set()
    with open(sys.argv[2], "rb") as source, open(day, "wb") as out:
        for line in source:
            record = json.loads(line)
            if record["t"] >= 86400:
                continue
            back = max(back, latest - record["t"])
            latest = max(latest, record["t"])
            blocks.add(record["block"])
            out.write(line)
            events += 1
    lateness = ["--lateness", str(back + 1)]
    report = {
        "workload": {"events": events, "blocks": len(blocks), "span_seconds": latest + 1, "bytes": day.stat().st_size, "largest_step_back_seconds": back, "lateness_used": back + 1},
    }

    # 1. Small bounds, with a checkpoint at every window.
    checkpoint = work / "state.json"
    windows, summary, seconds, peak = live(kute, day, HOUR + SMALL + lateness + ["--checkpoint", str(checkpoint)])
    report["bounded"] = {
        "options": SMALL,
        "seconds": seconds,
        "events_per_second": round(events / seconds),
        "peak_process_memory_bytes": peak,
        "state_bytes_bound": summary["state_bytes_bound"],
        "largest_state_bytes_reported": max(w["state_bytes"] for w in windows),
        "windows": len(windows),
        "totals": summary["totals"],
        "checkpoints_written": summary["checkpoints_written"],
        "checkpoint_bytes": checkpoint.stat().st_size,
        "windows_exact": sum(w["exact"] for w in windows),
        "events_folded": sum(w["events_folded"] for w in windows),
        "entities_evicted": sum(w["entities_evicted"] for w in windows),
        "largest_undercount_bound": max(max(w["max_undercount"]) for w in windows),
    }

    # 2. Killed after eight windows, resumed from the checkpoint.
    restart = work / "restart.json"
    before, _, _, _ = live(kute, day, HOUR + SMALL + lateness + ["--checkpoint", str(restart)], stop_after=8)
    saved = json.loads(restart.read_bytes())
    after, resumed, _, _ = live(kute, day, HOUR + SMALL + lateness + ["--checkpoint", str(restart), "--resume"])
    # The kill can land after a window is written and before its
    # checkpoint is, so the resumed run may repeat windows already seen.
    seen = {w["start"] for w in after}
    joined = [w for w in before if w["start"] not in seen] + after
    report["restart"] = {
        "killed_after_windows": len(before),
        "checkpoint_was_at_line": saved["lines"],
        "windows_repeated_after_resume": len(before) + len(after) - len(joined),
        "windows_identical_to_uninterrupted_run": [strip(w) for w in joined] == [strip(w) for w in windows],
        "totals_identical": resumed["totals"] == summary["totals"],
    }

    # 3. Against a run in which nothing is approximated: the small bounds
    # above, and the bounds the command has when none are given.
    exact, exact_summary, exact_seconds, exact_peak = live(kute, day, HOUR + LARGE + lateness)
    defaults, default_summary, default_seconds, default_peak = live(kute, day, HOUR + lateness)
    report["default_bounds"] = {
        "seconds": default_seconds,
        "events_per_second": round(events / default_seconds),
        "peak_process_memory_bytes": default_peak,
        "state_bytes_bound": default_summary["state_bytes_bound"],
        "largest_state_bytes_reported": max(w["state_bytes"] for w in defaults),
        "windows_exact": sum(w["exact"] for w in defaults),
        "entities_evicted": sum(w["entities_evicted"] for w in defaults),
        "largest_undercount_bound": max(max(w["max_undercount"]) for w in defaults),
    }
    report["accuracy_against_exact_run"] = {
        "exact_run": {"options": LARGE, "seconds": exact_seconds, "peak_process_memory_bytes": exact_peak, "windows_exact": sum(w["exact"] for w in exact), "windows": len(exact), "largest_state_bytes_reported": max(w["state_bytes"] for w in exact)},
        "patterns_compared_per_window": 200,
        "small_bounds": accuracy(windows, exact),
        "default_bounds": accuracy(defaults, exact),
    }
    # 4. The whole day as one window, against offline discovery.
    one, _, _, _ = live(kute, day, ["--pane", "86400", "--min-support", "5", "--max-patterns", "200", "--max-length", "6"] + LARGE + lateness)
    assert len(one) == 1 and one[0]["exact"]
    started = time.perf_counter()
    offline = json.loads(subprocess.run([kute, "discover", "--input", str(day), *FIELDS, "--min-support", "5", "--max-length", "6", "--window", "0", "--top", "200"], check=True, capture_output=True).stdout)
    offline_seconds = round(time.perf_counter() - started, 2)
    listed = offline["discovery"]["patterns"] if "discovery" in offline else offline["patterns"]
    theirs = [(p["symbols"], p["support"]) for p in listed if p.get("kind", "contiguous") == "contiguous"]
    ours = [(p["symbols"], p["support"]) for p in one[0]["patterns"]]
    report["against_offline_discovery"] = {
        "patterns_compared": len(theirs),
        "same_patterns_same_support_same_order": ours == theirs,
        "same_patterns_and_support_in_any_order": sorted(ours) == sorted(theirs),
        "offline_seconds": offline_seconds,
    }

    # 5. The log is in time order as written. Delivered out of order
    # instead, each event up to 30 seconds late, with that much lateness
    # allowed: the windows should be those of the run in order.
    import random

    rng = random.Random(20261125)
    with open(day, "rb") as source:
        lines = source.readlines()
    keyed = sorted(range(len(lines)), key=lambda i: (json.loads(lines[i])["t"] + rng.randint(0, 30), i))
    moved = sum(1 for position, i in enumerate(keyed) if position != i)
    shuffled = work / "day-shuffled.jsonl"
    with open(shuffled, "wb") as out:
        out.writelines(lines[i] for i in keyed)
    # What "in order" can mean for this delivery: by time, and events of
    # the same second in the order they were delivered. The log's own
    # order within a second cannot be recovered from times in whole
    # seconds, so the original log is a second, different reference.
    resorted = work / "day-shuffled-sorted.jsonl"
    with open(resorted, "wb") as out:
        out.writelines(lines[i] for i in sorted(keyed, key=lambda i: json.loads(lines[i])["t"]))
    del lines
    late, late_summary, _, _ = live(kute, shuffled, HOUR + LARGE + ["--lateness", "31"])
    sorted_first, _, _, _ = live(kute, resorted, HOUR + LARGE + ["--lateness", "1"])
    same_as_log = sum(strip(a) == strip(b) for a, b in zip(late, exact))
    report["delivered_out_of_order"] = {
        "events_not_in_their_place": moved,
        "lateness_allowed": 31,
        "late_dropped": late_summary["totals"]["late_dropped"],
        "forced_releases": late_summary["totals"]["forced_releases"],
        "windows_identical_to_the_same_delivery_sorted_by_time_first": [strip(w) for w in late] == [strip(w) for w in sorted_first],
        "windows_identical_to_the_log_in_its_own_order": same_as_log,
        "of_windows": len(exact),
    }
    print(json.dumps(report, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
