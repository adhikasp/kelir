"""Brief check on a real session: compare the brief's first request with the parent's previous one
(logging proxy bodies) and print the cached_tokens each reported."""
import json, os, sys
state, proxy = sys.argv[1], sys.argv[2]
reqs = [json.loads(l) for l in open(proxy, encoding="utf-8") if l.strip()]
sd = os.path.join(state, "sessions")
logs = {sid: [json.loads(l) for l in open(os.path.join(sd, sid, "events.jsonl"), encoding="utf-8") if l.strip()] for sid in os.listdir(sd)}
root = [s for s, e in logs.items() if not e[0].get("parent")][0]
brief = [s for s, e in logs.items() if e[0].get("parent") == root]
print("root", root[-8:], "briefs", [b[-8:] for b in brief])
q = "which keys cap the number of children"
rr = [r for r in reqs if r["body"].get("session_id") == root]
print("requests with the root's session_id:", len(rr))
idx = [i for i, r in enumerate(rr) if q in json.dumps(r["body"]["messages"][-1])]
if not idx:
    print("NO brief request found"); sys.exit(1)
b = rr[idx[0]]
prev = [r for r in rr[:idx[0]] if json.dumps(b["body"]["messages"][:len(r["body"]["messages"])]) == json.dumps(r["body"]["messages"])]
p = prev[-1] if prev else None
print("brief first request: %d messages; parent's previous request: %s" % (len(b["body"]["messages"]), len(p["body"]["messages"]) if p else None))
if p:
    same = json.dumps(b["body"]["messages"][:len(p["body"]["messages"])]) == json.dumps(p["body"]["messages"])
    print("messages prefix identical:", same, "| tools identical:", json.dumps(b["body"].get("tools")) == json.dumps(p["body"].get("tools")),
          "| model identical:", b["body"]["model"] == p["body"]["model"], "| extra messages:", len(b["body"]["messages"]) - len(p["body"]["messages"]))
def usage(sid):
    return [e for e in logs[sid] if e["type"] == "usage"]
ru = usage(root)
print("parent usage (prompt, cached) per step:", [(u["prompt_tokens"], u["cached_tokens"]) for u in ru])
for bs in brief:
    bu = usage(bs)
    own = [u for u in bu]
    print("brief usage (prompt, cached) per step [copied parent usage events included]:", [(u["prompt_tokens"], u["cached_tokens"]) for u in own])
