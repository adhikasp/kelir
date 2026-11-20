import sys, glob, json, os, re, statistics
sys.path.insert(0, 'C:/Users/the maintainer/kelir-wt/steer12/scripts/live')
import steer12_recompute as R

root = sys.argv[1] if len(sys.argv) > 1 else 'C:/Users/the maintainer/kelir-scratch/steer12-rev/ab'
rows = []
for d in sorted(glob.glob(root + '/*-t*-r*')):
    name = os.path.basename(d)
    arm, t, r = name.split('-')
    ev = glob.glob(d + '/state/sessions/*/events.jsonl')
    if not ev:
        print(name, 'NO LOG'); continue
    a = R.analyse(ev[0])
    spilled = 0; pt_total = 0; ct_total = 0; maxcalls = 0; semis = 0; notes = 0; cached = 0
    for l in open(ev[0], encoding='utf-8'):
        e = json.loads(l)
        if e['type'] == 'tool_settled' and e.get('spilled'):
            spilled += 1
        if e['type'] == 'usage' and 'kind' not in e:
            pt_total += e.get('prompt_tokens', 0); ct_total += e.get('completion_tokens', 0); cached += e.get('cached_tokens', 0)
        if e['type'] == 'assistant':
            cs = e.get('tool_calls', [])
            maxcalls = max(maxcalls, len(cs))
            for c in cs:
                cmd = c.get('args', {}).get('command', '') if isinstance(c.get('args'), dict) else ''
                if c['name'] == 'bash' and cmd.count(';') + cmd.count('&&') >= 2 and R.is_read_call(cmd):
                    semis += 1
                if c['name'] == 'edit':
                    notes += 1
    final = ''
    try:
        final = open(d + '/stdout.txt', encoding='utf-8', errors='replace').read()
    except Exception:
        pass
    rows.append(dict(arm=arm, t=t, run=r, steps=a['steps'], calls=a['calls'], reads=a['reads'], multi=a['multi'], maxcalls=maxcalls,
                     batched=semis, spilled=spilled, pt=pt_total, ct=ct_total, model=round(a['model'], 1), tool=round(a['tool'], 2),
                     big=a['big'], bc=a['bc'], edits=notes, finlen=len(final)))
for r in rows:
    print(r)
print()
for t in ('t1', 't2'):
    for arm in ('old', 'new'):
        g = [r for r in rows if r['t'] == t and r['arm'] == arm]
        if not g:
            continue
        def m(k):
            return round(statistics.mean(x[k] for x in g), 1)
        print(t, arm, 'n=%d' % len(g), 'steps', [x['steps'] for x in g], 'mean', m('steps'), 'calls/step', round(sum(x['calls'] for x in g) / sum(x['steps'] for x in g), 2),
              'reads', [x['reads'] for x in g], 'batched-reads', [x['batched'] for x in g], 'spilled', [x['spilled'] for x in g], 'prompt_tok_total', [x['pt'] for x in g], 'compl', [x['ct'] for x in g], 'model_min', [x['model'] for x in g], 'edits', [x['edits'] for x in g])
