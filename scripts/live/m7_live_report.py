"""Summarise an M7 live drive from a state dir: the tree, the registry events, the wake messages,
what each session's model was and what the bash calls returned. Never prints a secret (the logs hold none)."""
import json, os, sys
state = sys.argv[1]
sd = os.path.join(state, "sessions")
rows = []
for sid in sorted(os.listdir(sd)):
    ev = [json.loads(l) for l in open(os.path.join(sd, sid, "events.jsonl"), encoding="utf-8") if l.strip()]
    rows.append((sid, ev))
for sid, ev in rows:
    s0 = ev[0]
    us = [e for e in ev if e["type"] == "usage"]
    pt = sum(u["prompt_tokens"] for u in us); ct = sum(u.get("cached_tokens", 0) for u in us)
    cost = sum(u.get("cost") or 0 for u in us)
    print(f"== {sid[-8:]} depth={s0.get('depth')} parent={(s0.get('parent') or '-')[-8:]} model={s0.get('model')} steps={len(us)} prompt={pt} cached={ct} cost=${cost:.5f}")
    for e in ev:
        t = e["type"]
        if t in ("child_started", "child_settled"):
            print(f"   {t} {e['session'][-8:]} depth={e['depth']} " + (f"kind={e.get('kind')} bg={e.get('background')}" if t == "child_started" else f"outcome={e['outcome']}"))
        elif t == "user" and e.get("from") in ("child", "parent", "brief"):
            print(f"   user(from {e['from']}): " + json.dumps(e["text"])[:160])
        elif t == "tool_started" and e["name"] == "bash":
            print("   bash: " + json.dumps(e["input"].get("command"))[:150])
        elif t == "tool_settled":
            print(f"   -> {e['outcome']} rc={e.get('exit_code')}: " + json.dumps(e.get("result", ""))[:160])
        elif t == "turn_ended":
            print("   FINAL: " + json.dumps(e.get("final_text"))[:200])
