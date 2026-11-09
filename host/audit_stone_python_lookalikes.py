"""Python-lookalike audit (docs/STONE_PYTHON_LOOKALIKE_AUDIT.md): run each snippet in Stone and in Python 3 and compare.

Each case is (id, area, operation, source[, python_source]). The source must
end by calling emit(value). Output: results.json and a printed table.
"""
import json
import subprocess
import sys
from pathlib import Path

import tempfile
REPO = Path(__file__).resolve().parents[1]
HERE = Path(tempfile.mkdtemp(prefix="stone-lookalikes-"))
WAYMARK = str(REPO / "target" / "debug" / "waymark")
PRELUDE = (
    "import json\n"
    "def _norm(o):\n"
    "    if isinstance(o, (set, frozenset)): return {'<set>': sorted(o, key=repr)}\n"
    "    if isinstance(o, BaseException): return {'<exc>': type(o).__name__, 'msg': str(o)}\n"
    "    return repr(o)\n"
    "def emit(v): print('EMIT ' + json.dumps(v, default=_norm))\n"
)

C = []


def case(cid, area, op, src, py=None):
    C.append({"id": cid, "area": area, "op": op, "src": src, "py": py if py is not None else src})


# ---------------- tuples (represented as lists) ----------------
case("T1", "tuple", "item assignment t[0] = 5", "t = (1, 2)\nt[0] = 5\nemit(t)")
case("T2", "tuple", "t.append(3)", "t = (1, 2)\nt.append(3)\nemit(t)")
case("T3", "tuple", "del t[0]", "t = (1, 2)\ndel t[0]\nemit(t)")
case("T4", "tuple", "t.sort()", "t = (2, 1)\nt.sort()\nemit(t)")
case("T5", "tuple", "tuple as dict key d[(1,2)] = 3", "d = {}\nd[(1, 2)] = 3\nemit(d[(1, 2)])")
case("T6", "tuple", "tuple key in dict literal", "d = {(1, 2): 'a'}\nemit(d[(1, 2)])")
case("T7", "tuple", "(1, 2) == [1, 2]", "emit((1, 2) == [1, 2])")
case("T8", "tuple", "isinstance((1,2), list)", "emit(isinstance((1, 2), list))")
case("T9", "tuple", "isinstance((1,2), tuple)", "emit(isinstance((1, 2), tuple))")
case("T10", "tuple", "str((1, 2))", "emit(str((1, 2)))")
case("T11", "tuple", "f-string of tuple", "t = (1, 'a')\nemit(f'{t}')")
case("T12", "tuple", "print((1, 2))", "print((1, 2))\nemit(0)")
case("T13", "tuple", "aliasing: u = t; t += (3,)", "t = (1, 2)\nu = t\nt += (3,)\nemit(u)")
case("T14", "tuple", "unpacking a, b = (1, 2)", "a, b = (1, 2)\nemit([a, b])")
case("T15", "tuple", "tuple inside set membership", "s = set([(1, 2)])\nemit((1, 2) in s)")
case("T16", "tuple", "single-element (1,)", "emit(len((1,)))")
case("T17", "tuple", "tuple + list", "emit((1,) + [2])")
case("T18", "tuple", "tuple(x) is a new copy", "a = [1]\nt = tuple(a)\na.append(2)\nemit(t)")
case("T19", "tuple", "repr((1,))", "emit(repr((1,)))")
case("T20", "tuple", "hash((1,2))", "emit(hash((1, 2)) == hash((1, 2)))")

# ---------------- set() (unique list) ----------------
case("S1", "set", "set literal {1, 2}", "s = {1, 2}\nemit(len(s))")
case("S2", "set", "set comprehension", "s = {x % 2 for x in [1, 2, 3]}\nemit(len(s))")
case("S3", "set", "iteration order of set([3, 1, 2])", "emit(list(set([3, 1, 2])))")
case("S4", "set", "equality set([1,2]) == set([2,1])", "emit(set([1, 2]) == set([2, 1]))")
case("S5", "set", "union s | t", "emit(sorted(set([1]) | set([2])))")
case("S6", "set", "intersection s & t", "emit(sorted(set([1, 2]) & set([2])))")
case("S7", "set", "difference s - t", "emit(sorted(set([1, 2]) - set([2])))")
case("S8", "set", "symmetric difference s ^ t", "emit(sorted(set([1, 2]) ^ set([2, 3])))")
case("S9", "set", "s.union(t)", "emit(sorted(set([1]).union([2])))")
case("S10", "set", "s.intersection(t)", "emit(sorted(set([1, 2]).intersection([2])))")
case("S11", "set", "s.difference(t)", "emit(sorted(set([1, 2]).difference([2])))")
case("S12", "set", "s.update(t)", "s = set([1])\ns.update([2])\nemit(sorted(s))")
case("S13", "set", "s.remove(x)", "s = set([1, 2])\ns.remove(1)\nemit(sorted(s))")
case("S14", "set", "s.discard(missing)", "s = set([1])\ns.discard(5)\nemit(sorted(s))")
case("S15", "set", "s.issubset(t)", "emit(set([1]).issubset([1, 2]))")
case("S16", "set", "s.add(x) twice", "s = set()\ns.add(1)\ns.add(1)\nemit(len(s))")
case("S17", "set", "indexing s[0]", "s = set([5])\nemit(s[0])")
case("S18", "set", "s.append(1)", "s = set([1])\ns.append(1)\nemit(len(s))")
case("S19", "set", "s + [1]", "emit(len(set([1]) + [1]))")
case("S20", "set", "isinstance(s, set)", "emit(isinstance(set(), set))")
case("S21", "set", "isinstance(s, list)", "emit(isinstance(set(), list))")
case("S22", "set", "set of dicts", "emit(len(set([{'a': 1}, {'a': 1}])))")
case("S23", "set", "set of lists", "emit(len(set([[1], [1]])))")
case("S24", "set", "str(set([1]))", "emit(str(set([1])))")
case("S25", "set", "1 and 1.0 in one set", "emit(len(set([1, 1.0])))")
case("S26", "set", "s.pop()", "s = set([1])\nemit(s.pop())")
case("S27", "set", "frozenset", "emit(len(frozenset([1, 1])))")
case("S28", "set", "s <= t (subset operator)", "emit(set([1]) <= set([1, 2]))")
case("S29", "set", "sorted(s)", "emit(sorted(set([3, 1])))")
case("S30", "set", "s.sort()", "s = set([3, 1])\ns.sort()\nemit(s)")
case("S31", "set", "aliasing after s.add", "s = set()\nt = s\nt.add(1)\nemit(len(s))")

# ---------------- truthiness and equality ----------------
case("B1", "truth", "bool of falsey values", "emit([bool(0), bool(0.0), bool(''), bool([]), bool({}), bool(None)])")
case("B2", "truth", "if on empty record", "x = {}\nemit('t' if x else 'f')")
case("B3", "truth", "True == 1", "emit(True == 1)")
case("B4", "truth", "True + 1", "emit(True + 1)")
case("B5", "truth", "sum([True, True])", "emit(sum([True, True]))")
case("B6", "truth", "True in [1]", "emit(True in [1])")
case("B7", "truth", "1 in [True]", "emit(1 in [True])")
case("B8", "truth", "0 == False", "emit(0 == False)")
case("B9", "truth", "d[True] vs d[1]", "d = {1: 'a'}\nemit(d[True])")
case("B10", "truth", "[0, 1].count(False)", "emit([0, 1].count(False))")
case("B11", "truth", "max(True, 2)", "emit(max(True, 2))")
case("B12", "truth", "a or b returns operand", "emit([] or 'x')")
case("B13", "truth", "not on string", "emit(not 'a')")
case("B14", "truth", "truthiness of run() failure", "r = run(['false'])\nemit('truthy' if r else 'falsey')",
     py="import subprocess\nr = subprocess.run(['false'])\nemit('truthy' if r else 'falsey')")
case("B15", "truth", "x is None", "x = None\nemit(x is None)")
case("B16", "truth", "[] is []", "emit([] is [])")
case("B17", "truth", "a is b for equal lists", "a = [1]\nb = [1]\nemit(a is b)")
case("B18", "truth", "1 is 1.0", "emit(1 is 1.0)")
case("B19", "truth", "set([True, 1])", "emit(len(set([True, 1])))")
case("B20", "truth", "sorted([True, 0])", "emit(sorted([True, 0]))")
case("B21", "truth", "list.index(True) on [1]", "emit([1].index(True))")
case("B22", "truth", "int(True)", "emit(int(True))")

# ---------------- with ----------------
case("W1", "with", "body names visible after block", "with open('w.txt', 'w') as f:\n    f.write('hi')\n    n = 1\nemit(n)")
case("W2", "with", "read after close", "with open('w.txt', 'w') as f:\n    f.write('hi')\nemit(f.read())")
case("W3", "with", "file flushed at exit", "with open('w.txt', 'w') as f:\n    f.write('abc')\nemit(open('w.txt').read())")
case("W4", "with", "two items in one with", "with open('w.txt', 'w') as f, open('w2.txt', 'w') as g:\n    f.write('a')\n    g.write('b')\nemit(open('w2.txt').read())")
case("W5", "with", "with on a non-context value", "with 5 as x:\n    pass\nemit(x)")
case("W6", "with", "error inside with propagates", "try:\n    with open('w.txt', 'w') as f:\n        raise ValueError('boom')\nexcept ValueError as e:\n    emit('caught')")
case("W7", "with", "with without as", "with open('w.txt', 'w'):\n    pass\nemit(1)")
case("W8", "with", "write after close", "with open('w.txt', 'w') as f:\n    pass\nf.write('x')\nemit(1)")
case("W9", "with", "f.closed after block", "with open('w.txt', 'w') as f:\n    pass\nemit(f.closed)")
case("W10", "with", "iterate file lines", "open('w.txt', 'w').write('a\\nb\\n')\nwith open('w.txt') as f:\n    emit([l for l in f])")

# ---------------- except / try ----------------
case("E1", "except", "bare except:", "try:\n    x = 1 / 0\nexcept:\n    x = 0\nemit(x)")
case("E2", "except", "except BaseException", "try:\n    x = 1 / 0\nexcept BaseException as e:\n    x = 0\nemit(x)")
case("E3", "except", "except Exception: pass", "try:\n    x = 1 / 0\nexcept Exception:\n    pass\nemit(1)")
case("E4", "except", "except error: (lowercase)", "try:\n    x = 1 / 0\nexcept error:\n    x = 0\nemit(x)")
case("E5", "except", "try/finally", "out = []\ntry:\n    out.append(1)\nfinally:\n    out.append(2)\nemit(out)")
case("E6", "except", "try/except/else", "try:\n    x = 1\nexcept ValueError as e:\n    x = 2\nelse:\n    x = 3\nemit(x)")
case("E7", "except", "str(e) of caught error", "try:\n    int('x')\nexcept ValueError as e:\n    emit(str(e))")
case("E8", "except", "e.args[0]", "try:\n    raise ValueError('bad')\nexcept ValueError as e:\n    emit(e.args[0])")
case("E9", "except", "raise ... from e", "try:\n    try:\n        int('x')\n    except ValueError as e:\n        raise RuntimeError('wrap') from e\nexcept RuntimeError as e:\n    emit('ok')")
case("E10", "except", "except KeyboardInterrupt", "try:\n    x = 1\nexcept KeyboardInterrupt:\n    x = 0\nemit(x)")
case("E11", "except", "except json.JSONDecodeError", "import json\ntry:\n    json.loads('{')\nexcept json.JSONDecodeError as e:\n    emit('caught')")
case("E12", "except", "f-string of caught error", "try:\n    raise ValueError('bad')\nexcept ValueError as e:\n    emit(f'{e}')")
case("E13", "except", "isinstance(e, Exception)", "try:\n    raise ValueError('bad')\nexcept Exception as e:\n    emit(isinstance(e, ValueError))")
case("E14", "except", "assert False, msg", "try:\n    assert False, 'no'\nexcept AssertionError as e:\n    emit('caught')")
case("E15", "except", "ValueError == 'ValueError'", "emit(ValueError == 'ValueError')")
case("E16", "except", "type(e).__name__", "try:\n    raise ValueError('bad')\nexcept ValueError as e:\n    emit(type(e).__name__)")
case("E17", "except", "except Exception catches hook denial",
     "try:\n    r = run(['printf', 'x'], hooks={'pre': lambda step: {'allow': False, 'reason': 'denied'}})\n    emit({'ok': r.ok})\nexcept Exception as e:\n    emit({'caught': e.type})", py="emit('n/a')")
case("E18", "except", "except Exception around write outside roots",
     "try:\n    write_text('/proc/denied_probe', 'x')\n    emit('wrote')\nexcept Exception as e:\n    emit({'caught': e.type})", py="emit('n/a')")
case("E19", "except", "raise in except re-raises", "try:\n    try:\n        int('x')\n    except ValueError as e:\n        raise\nexcept ValueError as e:\n    emit('outer')")
case("E20", "except", "repr(e)", "try:\n    raise ValueError('bad')\nexcept ValueError as e:\n    emit(repr(e))")

# ---------------- other lookalikes found along the way ----------------
case("X1", "other", "list aliasing b = a; b.append", "a = [1]\nb = a\nb.append(2)\nemit(a)")
case("X2", "other", "dict aliasing", "a = {}\nb = a\nb['k'] = 1\nemit(a)")
case("X3", "other", "function mutates argument", "def f(x):\n    x.append(1)\na = []\nf(a)\nemit(a)")
case("X4", "other", "list += mutates alias", "a = [1]\nb = a\na += [2]\nemit(b)")
case("X5", "other", "int dict key", "d = {1: 'a'}\nemit(d[1])")
case("X6", "other", "int and str keys collide?", "d = {}\nd[1] = 'int'\nd['1'] = 'str'\nemit(len(d))")
case("X7", "other", "print of list", "print([1, 'x'])\nemit(0)")
case("X8", "other", "nonlocal rebinding", "def outer():\n    n = 0\n    def inc():\n        nonlocal n\n        n += 1\n    inc()\n    return n\nemit(outer())")
case("X9", "other", "closure reads rebound name", "def outer():\n    n = 0\n    def get():\n        return n\n    n = 5\n    return get()\nemit(outer())")
case("X10", "other", "star args f(*xs)", "def f(a, b):\n    return a + b\nxs = [1, 2]\nemit(f(*xs))")
case("X11", "other", "keyword call to user function", "def f(a, b=0):\n    return a - b\nemit(f(b=1, a=5))")
case("X12", "other", "**kwargs", "def f(**kw):\n    return len(kw)\nemit(f(a=1))")
case("X13", "other", "default argument shared", "def f(x=[]):\n    x.append(1)\n    return len(x)\nf()\nemit(f())")
case("X14", "other", "string item assignment", "s = 'ab'\ns[0] = 'x'\nemit(s)")
case("X15", "other", "big int", "emit(2 ** 70)")
case("X16", "other", "d.keys() == list", "emit({'a': 1}.keys() == ['a'])")
case("X17", "other", "import os", "import os\nemit(os.getcwd() != '')")
case("X18", "other", "global statement", "n = 0\ndef inc():\n    global n\n    n += 1\ninc()\nemit(n)")
case("X19", "other", "for ... else", "for i in []:\n    pass\nelse:\n    i = 9\nemit(i)")
case("X20", "other", "walrus", "if (n := 3) > 2:\n    emit(n)")
case("X21", "other", "list(generator)", "emit(list(x * 2 for x in [1, 2]))")
case("X22", "other", "% formatting", "emit('%s-%d' % ('a', 3))")
case("X23", "other", "s.removeprefix", "emit('abc'.removeprefix('a'))")
case("X24", "other", "class with method", "class P:\n    def __init__(self, x):\n        self.x = x\nemit(P(1).x)")
case("X25", "other", "slice with step", "emit([1, 2, 3, 4][::2])")
case("X26", "other", "chained comparison", "x = 2\nemit(1 < x < 3)")
case("X27", "other", "copy of dict via dict(d)", "a = {'k': [1]}\nb = dict(a)\nb['k'].append(2)\nemit(a)")
case("X28", "other", "sort stability and key", "emit(sorted(['bb', 'a', 'cc'], key=len))")
case("X29", "other", "float repr 0.1+0.2", "emit(str(0.1 + 0.2))")
case("X30", "other", "dict.get default", "emit({}.get('x', 5))")
case("X31", "other", "int key in dict literal", "d = {1: 'a'}\nemit(list(d.keys()))")
case("X32", "other", "bool key in dict literal", "d = {True: 'a'}\nemit(list(d.keys()))")
case("X33", "other", "power operator", "emit(2 ** 3)")


def run_stone(src):
    p = HERE / "case.stone"
    p.write_text(src + "\n")
    out = subprocess.run([WAYMARK, "eval", str(p)], cwd=HERE, capture_output=True, text=True, timeout=60)
    try:
        text = (out.stdout.strip() or out.stderr.strip())
        r = json.loads(text.splitlines()[-1]) if text.startswith("{") and "\n{" not in text else json.loads(text)
    except Exception:
        return {"status": "crash", "detail": (out.stdout + out.stderr)[-500:]}
    warn = (r.get("diagnostics") or {}).get("warnings")
    if r.get("ok"):
        return {"status": "ok", "value": r.get("value"), "stdout": r["output"]["stdout"], "warnings": warn}
    e = r.get("error", {})
    return {"status": "error", "type": e.get("type"), "code": e.get("code"),
            "message": e.get("message"), "detail": e.get("detail"), "help": e.get("help"),
            "phase": "admission" if "admission" in json.dumps(e).lower() or e.get("code", "").startswith("stone_parse") else "runtime"}


def run_python(src):
    out = subprocess.run([sys.executable, "-c", PRELUDE + src], cwd=HERE, capture_output=True, text=True, timeout=60)
    emits = [l[5:] for l in out.stdout.splitlines() if l.startswith("EMIT ")]
    stdout = "\n".join(l for l in out.stdout.splitlines() if not l.startswith("EMIT "))
    if out.returncode == 0 and emits:
        return {"status": "ok", "value": json.loads(emits[-1]), "stdout": stdout}
    last = (out.stderr.strip().splitlines() or ["?"])[-1]
    return {"status": "error", "type": last.split(":")[0], "message": last}


def main():
    results = []
    for c in C:
        s, p = run_stone(c["src"]), run_python(c["py"])
        results.append({**c, "stone": s, "python": p})
        sv = s.get("value") if s["status"] == "ok" else f"{s.get('type')}: {s.get('detail') or s.get('message')}"
        pv = p.get("value") if p["status"] == "ok" else p.get("message")
        print(f"{c['id']:4} {c['op'][:38]:38} | stone {s['status']:5} {json.dumps(sv)[:110]}\n{'':44}| py    {p['status']:5} {json.dumps(pv)[:110]}")
        if s.get("stdout") or p.get("stdout"):
            print(f"{'':44}| stdout stone={s.get('stdout')!r} py={p.get('stdout')!r}")
    print("results:", HERE / "results.json"); (HERE / "results.json").write_text(json.dumps(results, indent=1))


main()
