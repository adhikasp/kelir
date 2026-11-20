#!/usr/bin/env python3
"""Independent recompute of the STEER-12 BEFORE numbers (reviewer script; does not use scripts/metrics)."""
import json, sys, re, os, datetime, glob

def ts(s):
    s = s.rstrip('Z')
    if '.' in s:
        a, b = s.split('.')
        s = a + '.' + b[:6]
    return datetime.datetime.fromisoformat(s).replace(tzinfo=datetime.timezone.utc).timestamp()

READ = {'sed','grep','egrep','fgrep','rg','cat','ls','dir','head','tail','wc','find','awk','less','more','file','stat','tree','type','pwd','which','diff','sort','uniq','cut','tr','nl','od','xxd','basename','dirname','realpath','readlink','test','[','date','env','printenv','du','df','get-content','get-childitem','select-string','get-item','test-path','gc','gci','sls','cmp','jq','column','fold','rev','tac'}
NEUTRAL = {'cd','echo','printf','true',':','set','export','pushd','popd','write-output','write-host','exit'}
GIT_READ = {'grep','log','show','diff','status','ls-files','rev-parse','blame','branch','describe','cat-file','ls-tree','shortlog','remote','config','tag','rev-list','show-ref','merge-base','diff-tree','for-each-ref'}

def split_cmds(s):
    out = []; cur = []; q = None; i = 0; n = len(s)
    while i < n:
        c = s[i]
        if q:
            cur.append(c)
            if c == q:
                q = None
            elif c == '\\' and q == '"' and i + 1 < n:
                cur.append(s[i+1]); i += 1
        elif c in '\'"':
            q = c; cur.append(c)
        elif c == '\\' and i + 1 < n:
            cur.append(c); cur.append(s[i+1]); i += 1
        elif c in ';\n' or c == '|' or (c == '&' and i + 1 < n and s[i+1] == '&'):
            if c == '|' and i + 1 < n and s[i+1] == '|':
                i += 1
            if c == '&':
                i += 1
            out.append(''.join(cur)); cur = []
        else:
            cur.append(c)
        i += 1
    out.append(''.join(cur))
    return [x.strip() for x in out if x.strip()]

def words(cmd):
    ws = []; cur = ''; q = None; has = False
    for c in cmd:
        if q:
            if c == q:
                q = None
            else:
                cur += c
        elif c in '\'"':
            q = c; has = True
        elif c.isspace():
            if cur or has:
                ws.append(cur); cur = ''; has = False
        else:
            cur += c
    if cur or has:
        ws.append(cur)
    return ws

def is_read_cmd(cmd):
    if re.search(r'<<-?\s*[\'"]?\w+', cmd):
        return False
    stripped = re.sub(r'"(?:\\.|[^"\\])*"|' + "'[^']*'", '""', cmd)
    stripped = re.sub(r'[0-9]*>&[0-9]', '', stripped)
    for m in re.finditer(r'[0-9]?>>?\s*([^\s;|&]*)', stripped):
        if m.group(1) not in ('/dev/null', 'nul', 'NUL', '$null'):
            return False
    ws = words(cmd)
    while ws and re.match(r'^[A-Za-z_]\w*=', ws[0]):
        ws = ws[1:]
    if not ws:
        return None
    w = ws[0].lower().replace('\\', '/').split('/')[-1]
    if w.endswith('.exe'):
        w = w[:-4]
    if w in NEUTRAL:
        return None
    if w in ('bash', 'sh', 'pwsh', 'powershell'):
        for k, x in enumerate(ws[1:], 1):
            if x.lower() in ('-c', '-command', '-lc') and k + 1 < len(ws):
                return is_read_call(ws[k+1])
        return False
    if w == 'git':
        j = 1; v = None
        while j < len(ws):
            if ws[j] == '-C':
                j += 2; continue
            if ws[j].startswith('-'):
                j += 1; continue
            v = ws[j]; break
        if v in ('branch', 'tag', 'remote') and len(ws) > j + 1 and not ws[j+1].startswith('-'):
            return False
        if v == 'config' and len(ws) > j + 2 and not ws[j+1].startswith('-'):
            return False
        return v in GIT_READ
    if w == 'sed' and any(x.startswith('-i') or x == '--in-place' for x in ws[1:]):
        return False
    if w == 'find' and any(x in ('-delete', '-exec') for x in ws):
        return False
    if w == 'sort' and '-o' in ws:
        return False
    return w in READ

def is_read_call(command):
    seen = False
    for c in split_cmds(command):
        r = is_read_cmd(c)
        if r is False:
            return False
        if r is True:
            seen = True
    return seen

def analyse(path, big=2000):
    ev = [json.loads(l) for l in open(path, encoding='utf-8') if l.strip()]
    steps = {}; calls = {}
    for e in ev:
        t = e['type']
        if t == 'step_started':
            steps[e['step']] = {'start': ts(e['ts']), 'calls': [], 'ct': 0, 'pt': 0}
        elif t == 'assistant' and e['step'] in steps:
            s = steps[e['step']]; s['end'] = ts(e['ts'])
            s['calls'] = [c['args'].get('command', '') if c['name'] == 'bash' else '\0' + c['name'] for c in e.get('tool_calls', [])]
        elif t == 'usage' and e.get('step') in steps and 'kind' not in e:
            steps[e['step']]['ct'] += e.get('completion_tokens', 0); steps[e['step']]['pt'] += e.get('prompt_tokens', 0)
        elif t == 'tool_started':
            calls[e['call_id']] = [ts(e['ts']), None]
        elif t == 'tool_settled' and e['call_id'] in calls:
            calls[e['call_id']][1] = ts(e['ts'])
    iv = sorted((a, b) for a, b in calls.values() if b)
    tool = 0; cs = ce = None
    for a, b in iv:
        if ce is None or a > ce:
            if ce is not None:
                tool += ce - cs
            cs, ce = a, b
        else:
            ce = max(ce, b)
    if ce is not None:
        tool += ce - cs
    ncalls = sum(len(s['calls']) for s in steps.values())
    reads = 0; bc = 0; bcT = 0; bigs = 0; model = 0; pt = 0; multi = 0
    for s in steps.values():
        pt += s['pt']
        if 'end' in s:
            model += s['end'] - s['start']
        rc = [(is_read_call(c) if not c.startswith('\0') else False) for c in s['calls']]
        reads += sum(rc)
        if len(s['calls']) >= 2:
            multi += 1
        if s['ct'] >= big:
            bigs += 1
            if s['calls'] and all(rc):
                bc += 1; bcT += s.get('end', s['start']) - s['start']
    first = ts(ev[0]['ts']); last = ts(ev[-1]['ts'])
    return dict(steps=len(steps), calls=ncalls, reads=reads, model=model/60, tool=tool/60, big=bigs, bc=bc, bcT=bcT/60,
                multi=multi, pt=pt, wall=(last-first)/3600, first=first, last=last,
                noassist=sum(1 for s in steps.values() if 'end' not in s), cwd=ev[0].get('cwd'))

if __name__ == '__main__':
    paths = []
    for a in sys.argv[1:]:
        paths += glob.glob(a)
    tot = {}
    for p in paths:
        r = analyse(p)
        print(os.path.basename(os.path.dirname(p))[-8:], {k: (round(v, 2) if isinstance(v, float) else v) for k, v in r.items() if k not in ('first', 'last')})
        for k, v in r.items():
            if isinstance(v, (int, float)) and k not in ('first', 'last'):
                tot[k] = tot.get(k, 0) + v
    print('TOTAL', {k: round(v, 2) for k, v in tot.items()})
    t = tot
    print(f"calls/step {t['calls']/t['steps']:.3f} read share {t['reads']/t['calls']:.3f} model/(model+tool) {t['model']/(t['model']+t['tool']):.3f} "
          f"bigcheap/big {t['bc']/t['big']:.3f} bigcheap model min {t['bcT']:.1f} share of model {t['bcT']/t['model']:.3f} pt/step {t['pt']/t['steps']:.0f} multi-call steps {t['multi']}")
