"""Independent check: recompute totals from events.jsonl and compare with a `status --json` file, and the
provider cost with the price-table estimate. usage: steer8_check.py events.jsonl [status.json]"""
import json, sys
P = (0.30, 0.006, 1.20)  # peak row of the built-in table, $/M
ev = [json.loads(l) for l in open(sys.argv[1], encoding="utf-8")]
u = [e for e in ev if e["type"] == "usage"]
tot = dict(prompt=0, cached=0, completion=0, cost=0.0, est=0.0)
for e in u:
    est = ((e["prompt_tokens"] - e["cached_tokens"]) * P[0] + e["cached_tokens"] * P[1] + e["completion_tokens"] * P[2]) / 1e6
    print(f'step {e["step"]}: prompt {e["prompt_tokens"]} cached {e["cached_tokens"]} out {e["completion_tokens"]} provider {e["cost"]} ({e.get("cost_source")}) table-est {est:.9f} ratio {est/e["cost"] if e["cost"] else float("nan"):.2f}')
    tot["prompt"] += e["prompt_tokens"]; tot["cached"] += e["cached_tokens"]; tot["completion"] += e["completion_tokens"]
    tot["cost"] += e["cost"]; tot["est"] += est
print(tot)
if len(sys.argv) > 2:
    s = json.load(open(sys.argv[2]))["state"]
    ok = (s["tokens"]["prompt"], s["tokens"]["cached"], s["tokens"]["completion"]) == (tot["prompt"], tot["cached"], tot["completion"]) and abs(s["cost"]["usd"] - tot["cost"]) < 1e-9
    print("status matches log:", ok, s["tokens"], s["cost"]["usd"], "session hit", s["cache"]["session_hit_ratio"], round(tot["cached"]/tot["prompt"],3))
