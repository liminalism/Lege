#!/usr/bin/env python3
"""Summarize completed quality-worker rows, retaining metric scope distinctions.

No missing/failed worker becomes a passed row. Raw logs and input identities
remain the evidence; this is a derived view of completed results.csv entries.
"""
import argparse
import csv
import json
import math
from pathlib import Path
import statistics


def percentile(values, fraction):
    values = sorted(values)
    if not values:
        return None
    return values[max(0, math.ceil(fraction * len(values)) - 1)]


def optional_integer(value):
    return int(value[5:-1]) if value.startswith("Some(") else None


def fields(parts):
    return dict(part.split("=",1) for part in parts if "=" in part)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory",type=Path)
    args = parser.parse_args()
    summary = {"scope": "completed CSV rows only; worker RSS includes loading/verification; work and bitmap storage are explicit proxies, not device limits",
               "modes": []}
    for row in csv.DictReader((args.directory/"results.csv").open()):
        pages, resources, matching, calibration = [], [], [], []
        for line in (args.directory/(row["mode"]+".log")).read_text().splitlines():
            parts = line.split("\t")
            if parts[0] == "QUALITY_PAGE":
                pages.append({"index":int(parts[1]),"cold":float(parts[4]),"warm":float(parts[5])})
            elif parts[0] == "QUALITY_PAGE_RESOURCES":
                resources.append({key:int(value) for key,value in fields(parts[2:]).items()})
            elif parts[0] == "QUALITY_MATCHING":
                matching.append(fields(parts[1:]))
            elif parts[0] == "QUALITY_TRIAL":
                estimated, measured = optional_integer(parts[4]),optional_integer(parts[5])
                if estimated is not None and measured is not None:
                    raw = optional_integer(parts[7].removeprefix("raw=")) if len(parts) > 7 else estimated
                    calibration.append({"representation":parts[2],"estimated_gain":estimated,"measured_gain":measured,"raw_gain":raw})
        if len(pages) != int(row["pages"]) or len({p["index"] for p in pages}) != len(pages):
            raise ValueError(f"missing/repeated page measurements: {row['mode']}")
        mode = {"csv":row,"page_latency_seconds":{
            kind:{"median":statistics.median(p[kind] for p in pages),
                  "p95":percentile([p[kind] for p in pages],.95),
                  "max":max(p[kind] for p in pages)} for kind in ["cold","warm"]}}
        if resources:
            if len(resources) != len(pages):
                raise ValueError("incomplete page resource metrics")
            mode["selected_resources"] = {key:sum(p.get(key,0) for p in resources)
                for key in ["exact","refined","substituted","generic_instances","generic_region_pixels","generic_ink","page_pixels"]}
            mode["selected_resources"]["max_local_bitmap_bytes"] = max(p["local_bitmap"] for p in resources)
            mode["selected_resources"]["max_cold_work_pixels"] = max(p["cold_work"] for p in resources)
        if matching:
            components = sum(int(m["components"]) for m in matching)
            mode["comparisons"] = {"input_components":components,
                "ingest_per_component":sum(int(m["ingest_comparisons"]) for m in matching)/max(components,1),
                "measured_planner_per_component":sum(int(m["planner_comparisons"]) for m in matching)/max(components,1),
                "legacy_post_ingest": "unreported; do not interpret ingest counts as total comparisons"}
        if calibration:
            positive = [trial for trial in calibration if trial["estimated_gain"] > 0]
            mode["shortlist_estimate_calibration"] = {
                "measured_trials":len(calibration),
                "mean_absolute_gain_error_bytes":statistics.mean(abs(t["estimated_gain"]-t["measured_gain"]) for t in calibration),
                "raw_mean_absolute_gain_error_bytes":statistics.mean(abs(t["raw_gain"]-t["measured_gain"]) for t in calibration),
                "online_corrected_trials":sum(t["raw_gain"] != t["estimated_gain"] for t in calibration),
                "raw_predicted_positive":sum(t["raw_gain"] > 0 for t in calibration),
                "predicted_positive":len(positive),
                "actual_positive_among_predicted_positive":sum(t["measured_gain"] > 0 for t in positive),
                "scope":"gain against the incumbent at trial time; contexts change, so this is diagnostic calibration, not an additive cost model"}
        summary["modes"].append(mode)
    (args.directory/"summary.json").write_text(json.dumps(summary,indent=2)+"\n")
    print(args.directory/"summary.json")


if __name__ == "__main__":
    main()
