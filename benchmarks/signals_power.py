"""`kute signals` on a real meter log, against the obvious ways to store it.

    python benchmarks/signals_power.py KUTE_BINARY household_power_consumption.txt WORK_DIR > report.json

The data is "Individual household electric power consumption" (UCI
Machine Learning Repository, id 235; CC BY 4.0): 2,075,259 readings a
minute apart with seven measured columns, `?` (or, in the last column,
nothing) where a reading is missing. It is real data, from one house.

For each column: the values packed exactly, and packed within an error
bound of five units of the column's last digit in use (that is, one
digit dropped; for a column of whole numbers, readings to the nearest ten). Each is read back and checked here, independently of
the check `kute` makes. Rivals are given the same values:

  text_xz         the column as the file prints it, one per line, xz -9
  binary_xz       eight bytes a value (IEEE double), xz -9
  scaled_delta_xz whole numbers of the last printed digit, differences
                  taken once, four bytes each, xz -9: the same idea as
                  the container, done by hand with a general compressor
  rounded_text_xz for the bounded case: the values rounded to the same
                  step and printed, xz -9
"""
import hashlib
import json
import lzma
import math
import pathlib
import struct
import subprocess
import sys
import time


# How the file marks a reading that was not taken: `?`, or nothing at all in the last column.
MISSING = ("?", "")


def xz(data: bytes) -> int:
    return len(lzma.compress(data, preset=9))


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    kute = str(pathlib.Path(sys.argv[1]).resolve())
    source = pathlib.Path(sys.argv[2])
    work = pathlib.Path(sys.argv[3])
    work.mkdir(parents=True, exist_ok=True)
    raw = source.read_bytes()
    lines = raw.decode("ascii").splitlines()
    names = lines[0].split(";")
    rows = [line.split(";") for line in lines[1:]]

    def run(*args):
        started = time.perf_counter()
        done = subprocess.run([kute, *map(str, args)], capture_output=True)
        if done.returncode != 0:
            raise SystemExit(f"kute {' '.join(map(str, args))} failed: {done.stderr.decode(errors='replace')[:500]}")
        return done.stdout, time.perf_counter() - started

    def read_back(path):
        out, seconds = run("signals", "unpack", path)
        return [float(x) for x in out.decode().split()], seconds

    report = {
        "data": {"name": "UCI Individual household electric power consumption (id 235)", "license": "CC BY 4.0", "file": source.name, "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest(), "readings": len(rows)},
        "kute_version": run("--version")[0].decode().strip(),
        "columns": [],
    }
    totals = {}
    for index in range(2, len(names)):
        strings = [row[index] for row in rows]
        column = work / f"{names[index]}.txt"
        column.write_text("\n".join(strings) + "\n", encoding="ascii")
        values = [math.nan if s in MISSING else float(s) for s in strings]
        present = [v for v in values if not math.isnan(v)]
        # The file prints three decimals in every column; what counts is how many are ever non-zero.
        decimals = max(len(s.split(".")[1].rstrip("0")) if "." in s else 0 for s in strings if s not in MISSING)
        scale = 10 ** decimals

        out, pack_seconds = run("signals", "pack", column, "--output", work / "exact.kts")
        exact = json.loads(out)
        back, unpack_seconds = read_back(work / "exact.kts")
        same = len(back) == len(values) and all((math.isnan(a) and math.isnan(b)) or a == b for a, b in zip(values, back))

        whole = [0 if math.isnan(v) else round(v * scale) for v in values]
        deltas = [b - a for a, b in zip([0] + whole[:-1], whole)]
        row = {
            "column": names[index],
            "missing": len(values) - len(present),
            "decimals_used": decimals,
            "distinct_values": len(set(present)),
            "exact": {
                "container_bytes": exact["report"]["container_bytes"],
                "bits_per_value": round(exact["report"]["bits_per_value"], 3),
                "scheme": exact["report"]["header"]["scheme"],
                "decimals_found": exact["report"]["header"].get("decimals"),
                "delta_order": exact["report"]["header"]["delta_order"],
                "kept_exactly": exact["report"]["header"]["kept_exactly"],
                "every_value_read_back_equal": same,
                "pack_seconds": round(pack_seconds, 2),
                "unpack_seconds": round(unpack_seconds, 2),
            },
            "rivals_exact": {
                "text_xz": xz(column.read_bytes()),
                "binary_xz": xz(struct.pack(f"<{len(values)}d", *values)),
                # Missing values are zero here and would need a list of their own: this rival is flattered.
                "scaled_delta_xz": xz(struct.pack(f"<{len(deltas)}i", *deltas)),
            },
        }
        # One printed digit dropped: steps of ten units of the last digit.
        max_error = 5 / scale
        out, _ = run("signals", "pack", column, "--output", work / "bounded.kts", "--max-error", max_error)
        bounded = json.loads(out)
        back, _ = read_back(work / "bounded.kts")
        worst = max(abs(a - b) for a, b in zip(values, back) if not math.isnan(a))
        gaps_kept = all(math.isnan(b) for a, b in zip(values, back) if math.isnan(a))
        step = 2 * max_error
        rounded = "\n".join("?" if math.isnan(v) else f"{round(v / step) * step:.{max(decimals - 1, 0)}f}" for v in values) + "\n"
        row["bounded"] = {
            "max_error": max_error,
            "container_bytes": bounded["report"]["container_bytes"],
            "bits_per_value": round(bounded["report"]["bits_per_value"], 3),
            "max_error_stated_by_header": bounded["report"]["header"]["max_error_observed"],
            "max_error_measured_here": worst,
            "within_bound": worst <= max_error and gaps_kept,
            "scheme": bounded["report"]["header"]["scheme"],
            "kept_exactly": bounded["report"]["header"]["kept_exactly"],
        }
        row["rivals_bounded"] = {"rounded_text_xz": xz(rounded.encode())}
        report["columns"].append(row)
        for key, value in (("exact", row["exact"]["container_bytes"]), ("bounded", row["bounded"]["container_bytes"]), *row["rivals_exact"].items(), *row["rivals_bounded"].items()):
            totals[key] = totals.get(key, 0) + value
    report["totals_bytes"] = totals
    report["all_exact_columns_equal"] = all(c["exact"]["every_value_read_back_equal"] for c in report["columns"])
    report["all_bounded_columns_within_bound"] = all(c["bounded"]["within_bound"] for c in report["columns"])
    print(json.dumps(report, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
