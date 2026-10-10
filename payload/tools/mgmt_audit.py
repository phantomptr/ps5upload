#!/usr/bin/env python3
"""Static audits of the AVA1 management table (payload/src/mgmt_table.def).

  mgmt_audit.py table    every entry names a real AVA1_METHOD_* constant, a handler defined in
                         runtime.c, a method listed in protocol/ava1/MGMT_METHODS.md, once
  mgmt_audit.py recv     no management handler reads from a socket (the handlers run on AVA1 workers
                         behind the capture sink: there is no connection to read)
  mgmt_audit.py sony     every entry whose handler can reach register/profile/registry/Remote
                         Play/notification code or a Sony API carries MGMT_SONY
  mgmt_audit.py lock     every MGMT_SONY entry of a "P3 Task N" block of mgmt_table.def (and app.launch) reaches a
                         `pthread_mutex_lock(&sony_api_lock)`, and no handler in runtime.c calls a p_sce* Sony
                         function without taking it (the dispatcher adds no lock of its own)
  mgmt_audit.py sonylock every entry whose handler reaches sceUserService / sceRegMgr / sys_registry_* code
                         also reaches a function that takes sony_api_lock (CE-108262-9: those APIs must be
                         called one at a time; AVA1 runs up to 8 management calls at once)
  mgmt_audit.py sonyleaf every function that makes a Sony call (a p_sce*/sceSystemService*/sceNotification*/
                         sceUserService*/sceRegMgr* call, or pop_notification / sys_time_get / sys_time_set /
                         notif_send, whose Sony call hides behind a pointer) takes sony_api_lock itself or is
                         only reached from functions that do (final review: console)
  mgmt_audit.py stack    no stack array of 16 KiB or more is reachable from a table handler
                         (AVA1 workers have 512 KiB, the rule is the SPEC.md section 7.3 one)
  mgmt_audit.py report   every array of 2 KiB or more reachable from each handler in
                         MGMT_METHODS.md (the audit table of the Task 2 report)

  mgmt_audit.py selftest the analysis on synthetic C (struct, 2-D, pointer and function-pointer cases)

Exit status 0 = clean.

THIS IS A TRIPWIRE, NOT A PROOF. It reads C text, it does not compile it:
  * the call graph is by name over every payload/src/*.c function (comments and strings stripped); a
    function whose name appears as an argument counts as called (the `hw_info_get_text` pattern), so it
    over-approximates, but a call through a table, a struct member or a dlsym'd pointer is invisible;
  * array sizes are literals or #define expressions; `sizeof(...)` and unresolvable macros are skipped
    (listed as unknown by `report`), element types are scalars, typedefs of scalars, or structs whose
    members it can sum (a LOWER bound: no padding, a member it cannot size counts 8 bytes), and any other
    element type counts 8 bytes each, so an array it prices at 16 KiB can only be bigger;
  * alloca, VLAs, recursion depth and the stack the Sony libraries use are not seen.
A finding that is not real goes in ALLOW below with the reason; a clean run is evidence, not a guarantee.
"""
import glob
import os
import re
import sys

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
os.chdir(ROOT)

# (handler, function, array) -> reason. Keep empty unless a finding is a false positive.
ALLOW = {}
LIMIT = 16 * 1024
REPORT_MIN = 2048


def strip(b):
    def r(m):
        t = m.group(0)
        if t.startswith("/"):
            return " "
        return '""' if t[0] == '"' else "''"

    return re.sub(r"/\*.*?\*/|//[^\n]*|\"(?:\\.|[^\"\\\n])*\"|'(?:\\.|[^'\\\n])*'", r, b, flags=re.S)


DEFS = {"PATH_MAX": "1024"}
for f in glob.glob("src/*.c") + glob.glob("src/*.inc") + glob.glob("include/*.h") + glob.glob("ava1/*.h"):
    for l in open(f, errors="ignore"):
        m = re.match(r"\s*#\s*define\s+(\w+)\s+(.+)", l)
        if m:
            DEFS.setdefault(m.group(1), strip(m.group(2)).strip())


def ev(e, depth=0):
    if depth > 8:
        return None
    e = re.sub(r"\b(\d+)[uUlL]+\b", r"\1", e)
    e = re.sub(r"\(\s*(size_t|unsigned|int|uint\d+_t)\s*\)", "", e)
    if "sizeof" in e:
        return None

    def rep(m):
        n = m.group(0)
        if n in DEFS:
            v = ev(DEFS[n], depth + 1)
            return "(%d)" % v if v is not None else "None"
        return "None"

    e2 = re.sub(r"[A-Za-z_]\w*", rep, e)
    if "None" in e2 or not re.fullmatch(r"[\d\s+\-*()/]*", e2):
        return None
    try:
        return int(eval(e2))  # digits and + - * / ( ) only, checked above
    except Exception:
        return None


FUNCS = {}
for f in sorted(glob.glob("src/*.c")):
    cur, buf, hdr = None, [], False
    for l in open(f, errors="ignore"):
        if cur is None:
            if l and l[0] not in " \t/*#}\n" and "(" in l and not l.startswith(("typedef", "struct", "enum", "union", "extern")):
                m = re.search(r"(\w+)\s*\(", l)
                if m:
                    cur, buf, hdr = m.group(1), [l], "{" not in l
                    if l.rstrip().endswith(";"):
                        cur = None
                    elif "{" in l and l.rstrip().endswith("}"):
                        # a one-line function (`static inline int f(int x) { return x; }`): without this
                        # it swallowed the next function's header and body (launch_title went missing)
                        FUNCS.setdefault(cur, (l, f))
                        cur = None
        else:
            buf.append(l)
            if hdr:
                if "{" in l:
                    hdr = False
                elif l.rstrip().endswith(";"):
                    cur = None
                    continue
            if l.startswith("}") and not hdr:
                FUNCS.setdefault(cur, ("".join(buf), f))
                cur = None

SCALARS = {"char": 1, "uint8_t": 1, "int8_t": 1, "bool": 1, "_Bool": 1, "short": 2, "uint16_t": 2, "int16_t": 2, "int": 4, "unsigned": 4,
           "uint32_t": 4, "int32_t": 4, "float": 4, "long": 8, "uint64_t": 8, "int64_t": 8, "size_t": 8, "ssize_t": 8,
           "double": 8, "off_t": 8, "time_t": 8, "pid_t": 4, "uintptr_t": 8, "intptr_t": 8, "pthread_mutex_t": 64, "pthread_t": 8}
TYPEDEFS = {}  # name -> bytes (scalar aliases and structs whose members can be summed)
STRUCTS = {}   # tag -> bytes
UNKNOWN_ELEM = 8


def elem_size(tp, depth=0):
    tp = " ".join(tp.split())
    if depth > 6:
        return UNKNOWN_ELEM
    tp = re.sub(r"^(const|volatile|static|unsigned|signed)\s+", lambda m: "" if m.group(1) != "unsigned" else "unsigned ", tp)
    if tp.endswith("*"):
        return 8
    if tp.startswith("struct "):
        return STRUCTS.get(tp[7:], UNKNOWN_ELEM)
    if tp.startswith("unsigned "):
        rest = tp[9:].strip() or "int"
        return SCALARS.get(rest, 4 if rest == "int" else UNKNOWN_ELEM)
    if tp in SCALARS:
        return SCALARS[tp]
    if tp in TYPEDEFS:
        return TYPEDEFS[tp]
    return UNKNOWN_ELEM


MEMBER = re.compile(r"\s*((?:const\s+)?(?:struct\s+\w+|unsigned\s+\w+|\w+)(?:\s*\*)*)\s+(\*?)(\w+)((?:\s*\[[^\]]+\])*)\s*")


def members_size(body):
    total = 0
    for piece in strip(body).split(";"):
        m = MEMBER.fullmatch(piece)
        if not m:
            continue
        tp, star, _, dims = m.groups()
        sz = 8 if star or tp.endswith("*") else elem_size(tp)
        for d in re.findall(r"\[([^\]]+)\]", dims):
            n = ev(d)
            sz *= n if n is not None else 1
        total += sz
    return total


def load_types(sources):
    """Scalar typedefs and struct sizes (lower bounds) from C text."""
    for text in sources:
        t = strip(text)
        for m in re.finditer(r"typedef\s+((?:unsigned\s+)?\w+)\s+(\w+)\s*;", t):
            TYPEDEFS.setdefault(m.group(2), elem_size(m.group(1)))
        for m in re.finditer(r"typedef\s+struct\s*(\w*)\s*\{([^{}]*)\}\s*(\w+)\s*;", t):
            sz = members_size(m.group(2))
            TYPEDEFS.setdefault(m.group(3), sz)
            if m.group(1):
                STRUCTS.setdefault(m.group(1), sz)
        for m in re.finditer(r"(?<!typedef )struct\s+(\w+)\s*\{([^{}]*)\}\s*;", t):
            STRUCTS.setdefault(m.group(1), members_size(m.group(2)))


for _f in glob.glob("include/*.h") + glob.glob("src/*.c") + glob.glob("ava1/*.h"):
    load_types([open(_f, errors="ignore").read()])

# `type [*]name[dim]...` for locals; the type is a scalar, a typedef, `struct tag` or anything else.
ARR = re.compile(r"(?<![\w.>])((?:const\s+)?(?:struct\s+\w+|unsigned\s+(?:char|short|int|long)|\w+))\s*(\*?)\s*(\w+)((?:\s*\[[^\]]+\])+)\s*(?==|;|,|\))")
KEYWORDS = {"return", "else", "goto", "case", "sizeof", "typedef", "if", "while", "for", "switch", "do"}


def arrays(body):
    """(name, dimension text, bytes or None) for each array declared in `body`."""
    out = []
    for m in ARR.finditer(body):
        tp, star, name, dims = m.groups()
        if tp in KEYWORDS:
            continue
        per = 8 if star else elem_size(tp)
        total, text, known = per, [], True
        for d in re.findall(r"\[([^\]]+)\]", dims):
            text.append(d.strip())
            n = ev(d)
            if n is None:
                known = False
            else:
                total *= n  # a 2-D array is every dimension multiplied
        out.append((name, "][".join(text), total if known else None))
    return out


def calls(name):
    """Functions `name` calls, plus any known function whose name appears in its body (a function
    pointer passed as an argument, e.g. handle_hw_text_op(..., hw_info_get_text, ...))."""
    if name not in FUNCS:
        return set()
    body = strip(FUNCS[name][0])
    out = set(re.findall(r"\b(\w+)\s*\(", body))
    out |= {w for w in re.findall(r"\b\w+\b", body) if w in FUNCS}
    out.discard(name)
    return out


def reach(name):
    seen, todo = set(), [name]
    while todo:
        n = todo.pop()
        if n in seen or n not in FUNCS:
            continue
        seen.add(n)
        todo.extend(calls(n))
    return seen


def table():
    out = []
    for l in open("src/mgmt_table.def"):
        m = re.match(r"MGMT_(H[01S]|N)\((.*)\)\s*$", l)
        if m:
            f = [x.strip() for x in m.group(2).split(",")]
            if m.group(1) == "N":  # a native runner: it is the handler, and it lives in its own file
                out.append(dict(kind="N", method=f[0], frame=f[1], ack=f[2], flags=f[3], handler=f[4], runner=f[4]))
            else:
                out.append(dict(kind=m.group(1), method=f[0], frame=f[1], ack=f[2], flags=f[3], handler=f[4], runner=f[5]))
        # job.run operations: MGMT_OP0/OP1/OP1B(op, frame, ack, flags, handler); no runner (the op worker
        # runs the handler, mgmt_rpc.c op_entry_run). They get every audit a method gets.
        m = re.match(r"MGMT_OP(0|1|1B)\((.*)\)\s*$", l)
        if m:
            f = [x.strip() for x in m.group(2).split(",")]
            out.append(dict(kind="OP" + m.group(1), method=f[0], frame=f[1], ack=f[2], flags=f[3], handler=f[4], runner=""))
    return out


def check_table():
    bad, seen = [], set()
    gen = open("ava1/gen/ava1_gen.h").read()
    checklist = open("../protocol/ava1/MGMT_METHODS.md").read()
    for e in table():
        if e["method"] in seen:
            bad.append("duplicate method " + e["method"])
        seen.add(e["method"])
        if "#define " + e["method"] + " " not in gen:
            bad.append("%s is not a generated AVA1_METHOD_* constant" % e["method"])
        if e["kind"].startswith("OP"):
            op = e["method"][len("AVA1_JOB_OP_"):]
            if not e["method"].startswith("AVA1_JOB_OP_") or "job.run (op %s)" % op not in checklist:
                bad.append("%s is not a job.run op row in MGMT_METHODS.md (looked for 'job.run (op %s)')" % (e["method"], op))
        home = "src/mgmt_fs.c" if e["kind"] == "N" else "src/runtime.c"
        if e["handler"] not in FUNCS or FUNCS[e["handler"]][1] != home:
            bad.append("%s: handler %s is not defined in %s" % (e["method"], e["handler"], home))
        if e["kind"].startswith("OP"):
            continue
        dotted = e["method"][len("AVA1_METHOD_"):].lower()
        if "`%s`" % dotted.replace("_", ".", 1) not in checklist and "`%s`" % dotted not in checklist:
            bad.append("%s is not in MGMT_METHODS.md (looked for %s)" % (e["method"], dotted.replace("_", ".", 1)))
    return bad


def check_recv():
    bad = []
    for name, (body, f) in FUNCS.items():
        if f != "src/runtime.c" or not name.startswith("handle_"):
            continue
        # A handler runs on an AVA1 worker with no socket: it must never read one.
        if re.search(r"\brecv(?:_exact|from)?\s*\(", strip(body)):
            bad.append("%s reads from a socket" % name)
    return bad


SONY_RE = re.compile(r"\b(p_?sce(?:Application|LncUtil|SystemService|AppInstUtil|UserService|RegMgr)\w*|sceUserService\w*|sceRegMgr\w*|sceAppInstUtil\w*|sceLncUtil\w*|sceSystemService\w*|sony_api_lock\w*)\b")


def sony_api_names():
    api = set()
    for h in ("register", "profile", "sys_registry", "remoteplay", "notif"):
        for l in open("include/%s.h" % h, errors="ignore"):
            if l and l[0] not in " \t/*#}\n" and "(" in l and not l.startswith(("typedef", "struct", "enum")):
                m = re.search(r"(\w+)\s*\(", l)
                if m:
                    api.add(m.group(1))
    return api


def check_sony():
    api, bad = sony_api_names(), []
    for e in table():
        names = set()
        for fn in reach(e["handler"]):
            names |= calls(fn)
            if SONY_RE.search(strip(FUNCS[fn][0])):
                names.add("sony-call")
        hit = sorted(n for n in names if n in api or n == "sony-call")
        if hit and "MGMT_SONY" not in e["flags"]:
            bad.append("%s (%s) reaches %s but lacks MGMT_SONY" % (e["method"], e["handler"], ", ".join(hit[:4])))
    return bad


# MGMT_SONY entries that take no Sony lock because they call no Sony API (handler -> why).
LOCK_EXEMPT = {
    "handle_app_list_registered": "readdir over /user/app only (flagged MGMT_SONY because its code lives in register.c)",
}
# A direct call of a dlsym'd Sony function (runtime.c keeps them as p_sce* pointers).
SONY_PTR_RE = re.compile(r"\bp_sce(?:Application|LncUtil|SystemService|AppInstUtil|UserService|RegMgr)\w*\s*\(")
LOCK_RE = re.compile(r"pthread_mutex_lock\s*\(\s*&\s*sony_api_lock\s*\)")


def locked_entries():
    """Entries of the `P3 Task N` blocks of mgmt_table.def, plus app.launch (Task 2's), that carry MGMT_SONY."""
    out, in_block = [], False
    for l in open("src/mgmt_table.def"):
        if l.startswith("/* ---- P3 Task"):
            in_block = True
            continue
        m = re.match(r"MGMT_H([01])\((.*)\)\s*$", l)
        if m:
            f = [x.strip() for x in m.group(2).split(",")]
            if "MGMT_SONY" in f[3] and (in_block or f[0] == "AVA1_METHOD_APP_LAUNCH"):
                out.append(dict(method=f[0], handler=f[4]))
    return out


def check_lock():
    bad = []
    for e in locked_entries():
        if e["handler"] in LOCK_EXEMPT:
            continue
        r = reach(e["handler"])
        # MGMT_SONY also marks handlers that only reach Sony-adjacent code (ptrace cheats, the notice
        # ring); the lock is owed by those that call a serialised Sony API (a p_sce* pointer, or
        # sceUserService / sceRegMgr / sys_registry_*).
        calls_api = any(SONY_PTR_RE.search(strip(FUNCS[fn][0])) or REG_RE.search(strip(FUNCS[fn][0])) for fn in r)
        if calls_api and not any(LOCK_RE.search(strip(FUNCS[fn][0])) for fn in r):
            bad.append("%s (%s) is MGMT_SONY but reaches no pthread_mutex_lock(&sony_api_lock)" % (e["method"], e["handler"]))
        for fn in sorted(r):
            body = strip(FUNCS[fn][0])
            if FUNCS[fn][1] == "src/runtime.c" and SONY_PTR_RE.search(body) and not LOCK_RE.search(body):
                bad.append("%s: %s calls a p_sce* Sony function without taking sony_api_lock" % (e["method"], fn))
    return bad


REG_RE = re.compile(r"\b(sceUserService\w*|sceRegMgr\w*|sys_registry_\w+)\s*\(")


def check_sonylock():
    bad = []
    for e in table():
        r = reach(e["handler"])
        touches = sorted(fn for fn in r if REG_RE.search(strip(FUNCS[fn][0])))
        if touches and not any(LOCK_RE.search(strip(FUNCS[fn][0])) for fn in r):
            bad.append("%s (%s) reaches %s but no function on its path takes sony_api_lock" % (e["method"], e["handler"], ", ".join(touches[:3])))
    return bad


# Wrappers whose Sony call is hidden behind a dlsym'd/resolved pointer, so the call text does not show it.
# They are caller-locked: every caller must hold it. pop_notification is checked separately: it takes the lock itself.
SONY_WRAPPERS = {"sys_time_get", "sys_time_set", "notif_send"}
SONYLEAF_RE = re.compile(r"\b(p_sce(?:Application|LncUtil|AppInstUtil)\w*|sceSystemService\w+|sceNotification\w+|sceUserService\w+|sceRegMgr\w+|sceKernelSendNotificationRequest)\s*\(")
# Functions that make a Sony call with no lock and no locking caller, and why that is safe.
SONYLEAF_ALLOW = {}


def sonyleaf_findings(funcs, wrappers=SONY_WRAPPERS, allow=SONYLEAF_ALLOW):
    """funcs: name -> (body text, file). One finding per function that touches a Sony API (directly, or through
    a wrapper) without taking sony_api_lock when some caller does not hold it either."""
    stripped = {n: strip(b) for n, (b, _) in funcs.items()}
    callers = {n: set() for n in funcs}
    for n, body in stripped.items():
        names = set(re.findall(r"\b(\w+)\s*\(", body)) | {w for w in re.findall(r"\b\w+\b", body) if w in funcs}
        for c in names:
            if c in callers and c != n:
                callers[c].add(n)

    def touches(n):
        body = stripped[n]
        if n in wrappers or SONYLEAF_RE.search(body):
            return True
        return any(re.search(r"\b%s\b" % re.escape(w), body) for w in wrappers)

    def ordering(n):
        """A Sony call written before the function's first lock, or after its last unlock, is unlocked."""
        body = stripped[n]
        locks = [m.start() for m in LOCK_RE.finditer(body)]
        if not locks:
            return None
        unlocks = [m.start() for m in re.finditer(r"pthread_mutex_unlock\s*\(\s*&\s*sony_api_lock\s*\)", body)]
        pat = re.compile(SONYLEAF_RE.pattern + "|" + r"\b(?:%s)\s*\(" % "|".join(map(re.escape, sorted(wrappers))))
        for m in pat.finditer(body):
            if m.start() < locks[0]:
                return "%s() before the lock" % m.group(0).rstrip("( ")
            if unlocks and m.start() > unlocks[-1] and m.start() > locks[-1]:
                return "%s() after the unlock" % m.group(0).rstrip("( ")
        return None

    def holds(n):
        return bool(LOCK_RE.search(stripped[n]))

    bad = []
    for n in sorted(funcs):
        o = ordering(n) if n not in allow else None
        if o:
            bad.append("%s (%s): %s" % (n, funcs[n][1], o))
    for n in sorted(funcs):
        if n == "pop_notification" and n not in allow and not holds(n):
            bad.append("pop_notification (%s) toasts without taking sony_api_lock" % funcs[n][1])
            continue
        if not touches(n) or n in allow:
            continue
        if n == "pop_notification" and not holds(n):
            bad.append("pop_notification (%s) toasts without taking sony_api_lock" % funcs[n][1])
            continue
        if holds(n) or n == "pop_notification":
            continue
        # caller-locked: walk up until every path hits a function that takes the lock
        seen, todo, open_ends = set(), [n], []
        while todo:
            f = todo.pop()
            if f in seen:
                continue
            seen.add(f)
            if f != n and holds(f):
                continue
            ups = callers.get(f, set())
            if not ups:
                open_ends.append(f)
            todo.extend(u for u in ups if not holds(u))
        if open_ends:
            bad.append("%s (%s) makes a Sony call without sony_api_lock, and %s reaches it with no lock taken" % (n, funcs[n][1], ", ".join(sorted(open_ends)[:4])))
    return bad


def check_sonyleaf():
    return sonyleaf_findings(FUNCS)


def findings(handler, floor):
    out = []
    for fn in sorted(reach(handler)):
        for (n, expr, sz) in arrays(strip(FUNCS[fn][0])):
            if sz is not None and sz >= floor:
                out.append((fn, FUNCS[fn][1].split("/")[-1], n, expr, sz))
    return out


def check_stack():
    bad = []
    for e in table():
        for (fn, f, n, expr, sz) in findings(e["handler"], LIMIT):
            if (e["handler"], fn, n) not in ALLOW:
                bad.append("%s: %s (%s) has %s[%s] = %d bytes on the stack" % (e["method"], fn, f, n, expr, sz))
    return bad


def selftest():
    bad = []
    load_types(["typedef unsigned char byte_t;\ntypedef struct { char name[300]; uint64_t id; } rec_t;\nstruct big { rec_t r[100]; int n; };"])
    cases = [
        ("struct array", "void f(void) { struct big b[2]; }", 2 * (100 * (300 + 8) + 4)),
        ("typedef struct array", "void f(void) { rec_t r[64]; }", 64 * 308),
        ("typedef scalar array", "void f(void) { byte_t buf[20000]; }", 20000),
        ("2-D array", "void f(void) { char tab[64][512]; }", 64 * 512),
        ("3-D array", "void f(void) { uint32_t t[4][8][16]; }", 4 * 8 * 16 * 4),
        ("pointer array", "void f(void) { char *names[4096]; }", 4096 * 8),
        ("unknown element type is 8 bytes", "void f(void) { mystery_t m[3000]; }", 3000 * 8),
        ("macro dimension", "#define N (4 * 1024)\nvoid f(void) { uint64_t q[N]; }", 4 * 1024 * 8),
    ]
    DEFS["N"] = "(4 * 1024)"
    for label, code, want in cases:
        got = [sz for (_, _, sz) in arrays(strip(code))]
        if want not in got:
            bad.append("%s: expected %d bytes, saw %s" % (label, want, got))
    # function-pointer argument: the callee named only as an argument is followed
    FUNCS["selftest_outer"] = ("int selftest_outer(void) { return selftest_helper(0, selftest_getter); }\n}\n", "src/x.c")
    FUNCS["selftest_helper"] = ("int selftest_helper(int a, int (*g)(void)) { return g(); }\n}\n", "src/x.c")
    FUNCS["selftest_getter"] = ("int selftest_getter(void) { char big[20000]; return big[0]; }\n}\n", "src/x.c")
    hit = [f[0] for f in findings("selftest_outer", LIMIT)]
    if "selftest_getter" not in hit:
        bad.append("a function passed as an argument was not followed")
    # sonyleaf: the lock check on synthetic C (final review: console)
    synth = {
        "unlocked power": ({"handler": ("int handler(void) { sceSystemServiceRequestReboot(); }\n}\n", "src/x.c")}, 1),
        "locked handler": ({"handler": ("int handler(void) { pthread_mutex_lock(&sony_api_lock); sceSystemServiceRequestReboot(); pthread_mutex_unlock(&sony_api_lock); }\n}\n", "src/x.c")}, 0),
        "wrapper unlocked caller": ({"handler": ("int handler(void) { return sys_time_get(&d, &e); }\n}\n", "src/x.c"),
                                      "sys_time_get": ("int sys_time_get(void) { return g_get(); }\n}\n", "src/y.c")}, 2),
        "wrapper locked caller": ({"handler": ("int handler(void) { pthread_mutex_lock(&sony_api_lock); sys_time_get(&d, &e); pthread_mutex_unlock(&sony_api_lock); }\n}\n", "src/x.c"),
                                    "sys_time_get": ("int sys_time_get(void) { return g_get(); }\n}\n", "src/y.c")}, 0),
        "notif via locked chain": ({"top": ("int top(void) { pthread_mutex_lock(&sony_api_lock); mid(); pthread_mutex_unlock(&sony_api_lock); }\n}\n", "src/x.c"),
                                     "mid": ("int mid(void) { notif_send(m, 0); }\n}\n", "src/x.c"),
                                     "notif_send": ("int notif_send(void) { return 0; }\n}\n", "src/y.c")}, 0),
        "notif via unlocked chain": ({"top": ("int top(void) { mid(); }\n}\n", "src/x.c"),
                                       "mid": ("int mid(void) { notif_send(m, 0); }\n}\n", "src/x.c"),
                                       "notif_send": ("int notif_send(void) { return 0; }\n}\n", "src/y.c")}, 2),
        "call before the lock": ({"handler": ("int handler(void) { sceUserServiceInitialize(0); pthread_mutex_lock(&sony_api_lock); a(); pthread_mutex_unlock(&sony_api_lock); }\n}\n", "src/x.c")}, 1),
        "call after the unlock": ({"handler": ("int handler(void) { pthread_mutex_lock(&sony_api_lock); a(); pthread_mutex_unlock(&sony_api_lock); sys_time_get(&d, &e); }\n}\n", "src/x.c"),
                                    "sys_time_get": ("int sys_time_get(void) { return g_get(); }\n}\n", "src/y.c")}, 1),
        "pop_notification must lock": ({"pop_notification": ("void pop_notification(const char *m) { p_send(0, &r); }\n}\n", "src/main.c")}, 1),
        "pop_notification locks": ({"pop_notification": ("void pop_notification(const char *m) { pthread_mutex_lock(&sony_api_lock); p_send(0, &r); pthread_mutex_unlock(&sony_api_lock); }\n}\n", "src/main.c")}, 0),
    }
    for label, (fs, want) in synth.items():
        got = sonyleaf_findings(fs, allow={})
        if len(got) < want or (want == 0 and got):
            bad.append("sonyleaf %s: expected %d finding(s), got %s" % (label, want, got))
    return bad


def report():
    md = open("../protocol/ava1/MGMT_METHODS.md").read()
    skip = {"handle_begin_tx_frame", "handle_query_tx_frame", "handle_commit_tx_frame", "handle_abort_tx_frame", "handle_stream_shard", "handle_status_frame"}
    for h in re.findall(r"`src/runtime.c` `(\w+)`", md):
        if h in skip:
            continue
        for (fn, f, n, expr, sz) in findings(h, REPORT_MIN):
            print("%-34s %-34s %-12s %-10s %-26s %6d%s" % (h, fn, f, n, expr, sz, "  (>=16K)" if sz >= LIMIT else ""))
    return []


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "all"
    checks = dict(table=check_table, recv=check_recv, sony=check_sony, lock=check_lock, sonylock=check_sonylock, sonyleaf=check_sonyleaf, stack=check_stack, report=report, selftest=selftest)
    todo = ["table", "recv", "sony", "lock", "sonylock", "sonyleaf", "stack"] if cmd == "all" else [cmd]
    failed = 0
    for c in todo:
        for line in checks[c]():
            print("%s: %s" % (c, line))
            failed = 1
    sys.exit(failed)
