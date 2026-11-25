"""`kute shadow` on the frozen and evolved vocabularies of `evolution_drift.py`.

    python benchmarks/shadow_drift.py KUTE_BINARY DRIFT_WORK_DIR > report.json

DRIFT_WORK_DIR is the work directory of `evolution_drift.py`: one
directory per run, named `drifting-N` or `stationary-N`, each holding
`frozen.kvfb`, `evolved.kvfb` and the held-out `eval.jsonl`. For every
run the evolved vocabulary is put to `kute shadow` as a candidate
against the frozen one, at several horizons. The data is synthetic.
"""
import json
import pathlib
import subprocess
import sys

HORIZONS = ["1", "10", "100", "1000"]


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    kute = str(pathlib.Path(sys.argv[1]).resolve())
    report = {"horizons": HORIZONS, "min_improvement": 0.01, "distributions": 1, "kinds": {}}
    for kind in ("drifting", "stationary"):
        runs = sorted(pathlib.Path(sys.argv[2]).glob(f"{kind}-*"), key=lambda p: int(p.name.split("-")[1]))
        rows = []
        for run in runs:
            row = {"run": run.name, "decisions": {}}
            for horizon in HORIZONS:
                out = subprocess.run(
                    [kute, "shadow", "--parent", str(run / "frozen.kvfb"), "--candidate", str(run / "evolved.kvfb"), "--input", str(run / "eval.jsonl"), "--horizon", horizon],
                    check=True, capture_output=True,
                ).stdout
                result = json.loads(out)
                comparison = result["comparison"]
                row["decisions"][horizon] = {"decision": result["decision"], "net_improvement": round(comparison["net_improvement"], 5), "reasons": result["reasons"]}
                row.update(
                    container_ratio=round(comparison["container_ratio"], 5),
                    token_ratio=round(comparison["token_ratio"], 5),
                    candidate_file_bytes=result["candidate"]["file_bytes"],
                    parent_container_bytes=result["parent"]["measured"]["container_bytes"],
                    break_even_horizon=comparison["break_even_horizon"],
                    lineage=result["candidate"]["lineage"],
                )
            rows.append(row)
        report["kinds"][kind] = {
            "runs": len(rows),
            "approved_at_horizon": {h: sum(r["decisions"][h]["decision"] == "approve" for r in rows) for h in HORIZONS},
            "container_ratio_least_and_greatest": [min(r["container_ratio"] for r in rows), max(r["container_ratio"] for r in rows)],
            "token_ratio_least_and_greatest": [min(r["token_ratio"] for r in rows), max(r["token_ratio"] for r in rows)],
            "rows": rows,
        }
    print(json.dumps(report, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
