#!/usr/bin/env python3
"""Collect the fakelib library corpus from every game on a console.

SUPERSEDED. ps5upload now does this itself — Backport > Scan this console, or
Settings > Backport libraries — into an app-managed corpus at
~/.ps5upload/fakelibs. Prefer that: it is the corpus the app reads.

This script writes an OLDER manifest schema into ./fakelibs/, which the app
will not load. It is kept only as a headless/offline way to inspect a corpus.
Its SELF param-locating and code_id logic now live, tested, in
engine/crates/ps5upload-core/src/fakelibs.rs.

Backporting a game needs replacement system libraries in a `fakelib/` folder.
They come from games that already ship them.

The corpus is small and heavily SHARED: across ~34 titles there are only 13
library names and ~52 distinct builds, and the commonest build of libSceAgc
ships in 13 of those titles. Storing one directory per source game therefore
duplicated 15 MiB of real content into 58 MiB on disk (3.8x) and buried the
thing that actually varies — which BUILD of a library you have.

So the unit stored is the build, addressed by content:

    fakelibs/
      manifest.json                 <- what ps5upload reads
      builds/<library>/<sha8>.sprx  <- every distinct build, stored once

`manifest.json` carries two things:

  libraries[]      every library name, and each distinct build of it, with the
                   titles shipping that build. `shipped_by` is the evidence for
                   picking one: a build 13 games use is better travelled than a
                   singleton.

  observed_sets[]  which build each real game ships, as references into the
                   build store. A set is a combination known to work somewhere,
                   which is worth recording — but it is metadata, not a
                   directory, so recording one duplicates nothing.

Sources are the console's OWN games: folder-backed titles and image-backed
(ShadowMount+) titles alike, since an image's `fakelib/` is readable straight
through its mount.

Output is gitignored. These are Sony binaries: never commit or redistribute.

Usage:
    python3 scripts/gather-fakelibs.py [--engine URL] [--addr IP] [--keep-cache]
"""
import argparse, ftplib, hashlib, io, json, os, shutil, struct, sys, time
import urllib.request
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "fakelibs")
# Outside OUT: the output directory is replaced wholesale at the end of a
# run, which would take a cache living inside it with it.
CACHE = OUT + ".cache"

PROC, MOD = 0x61000001, 0x61000002
MAGICS = {PROC: 0x4942524F, MOD: 0x3C13F4BF}
SELF_MAGICS = (0x1D3D154F, 0xEEF51454)


def param_site(data):
    """Byte offset of the module's param segment, or None.

    Inside a SELF the ELF keeps the offsets of the original unwrapped file
    while the entry table relocates the bytes, so a segment is reached through
    its entry (matched on id AND size) or the PT_LOAD containing it."""
    try:
        d = data
        base, ents = 0, []
        if struct.unpack_from("<I", d, 0)[0] in SELF_MAGICS:
            n = struct.unpack_from("<H", d, 0x18)[0]
            base = 0x20 + n * 0x20
            for i in range(n):
                fl, off, fsz, _ = struct.unpack_from("<QQQQ", d, 0x20 + i * 0x20)
                ents.append(((fl >> 20) & 0xFFF, off, fsz))
        e = d[base:]
        if e[:4] != b"\x7fELF":
            return None
        phoff = struct.unpack_from("<Q", e, 0x20)[0]
        pes = struct.unpack_from("<H", e, 0x36)[0]
        phn = struct.unpack_from("<H", e, 0x38)[0]
        ph = [(struct.unpack_from("<I", e, phoff + i * pes)[0],
               struct.unpack_from("<Q", e, phoff + i * pes + 8)[0],
               struct.unpack_from("<Q", e, phoff + i * pes + 0x20)[0]) for i in range(phn)]
        for i, (t, off, fsz) in enumerate(ph):
            if t not in (PROC, MOD):
                continue
            fo = None
            for sid, soff, sfsz in ents:
                if sid == i and sfsz == fsz:
                    fo = soff
                    break
            if fo is None and base == 0:
                fo = off
            if fo is None:
                for j, (t2, off2, fsz2) in enumerate(ph):
                    if t2 == 1 and off2 <= off and off + 0x18 <= off2 + fsz2:
                        for sid, soff, sfsz in ents:
                            if sid == j and sfsz == fsz2:
                                fo = soff + (off - off2)
                                break
                        if fo is not None:
                            break
            if fo is None or fo + 0x18 > len(d):
                continue
            if struct.unpack_from("<I", d, fo + 8)[0] != MAGICS[t]:
                continue
            return fo
    except Exception:
        pass
    return scan_param_site(data)


def scan_param_site(data):
    """Locate the param segment by its magic instead of by walking the ELF.

    The walk above fails on some real libraries (libSceAmpr, libScePlayGo and
    friends all report no SDK pair), which is why those showed "sdk unknown"
    across the whole corpus. The param magic occurs exactly once in each of
    those files, and the 4-byte size field in front of it confirms the hit, so
    scanning is unambiguous where parsing gives up."""
    for magic in MAGICS.values():
        pat = struct.pack("<I", magic)
        first = data.find(pat)
        if first < 8 or data.find(pat, first + 1) != -1:
            continue  # absent, or ambiguous -- do not guess
        site = first - 8
        if struct.unpack_from("<I", data, site)[0] != 0x20:
            continue  # param structures are 0x20 bytes; this is a false hit
        if site + 0x18 <= len(data):
            return site
    return None


# The 32-byte per-file digest a fake-signer rewrites whenever the file changes.
DIGEST_SITE = (0x510, 32)


def code_id(data):
    """Identity of the CODE in a library, ignoring how it was stamped.

    Rippers ship the same library patched to different SDK pairs, which changes
    the 8-byte pair in the param segment and the 32-byte digest that covers it
    — and nothing else. Measured: two libSceAmpr builds with different sha256
    became byte-identical once those two regions were masked. Treating them as
    different libraries inflates the corpus and poses a choice that does not
    exist, since our own SDK patcher can move either one to any pair."""
    b = bytearray(data)
    off, ln = DIGEST_SITE
    b[off:off + ln] = b"\0" * ln
    site = param_site(bytes(b))
    if site is not None:
        b[site + 0x10:site + 0x18] = b"\0" * 8
    return hashlib.sha256(bytes(b)).hexdigest()


def sdk_pair(data):
    """(ps4, ps5) SDK words from a module's param segment, or None.

    Shares param_site's locator, including its magic-scan fallback: reading the
    pair only when the ELF walk succeeded left 27 of 52 corpus builds recorded
    as "sdk unknown", which made the SDK look far less informative than it is.
    """
    site = param_site(data)
    if site is None:
        return None
    try:
        return (struct.unpack_from("<I", data, site + 0x10)[0],
                struct.unpack_from("<I", data, site + 0x14)[0])
    except Exception:
        return None


def is_library(name):
    """Dot-prefixed names are metadata, never libraries. Copying a game folder
    from a Mac leaves a `._<name>.sprx` AppleDouble sidecar beside every file;
    an earlier build offered eight of them as installable libraries."""
    return not name.startswith(".") and name.lower().endswith((".sprx", ".prx"))


def collect_local(root):
    """Rebuild the corpus from an existing fakelibs/ directory.

    Lets the on-disk layout be regenerated without a console — the libraries
    are already here, and re-scanning 34 games over FTP to change how they are
    filed is absurd. Reads either layout: the current content-addressed one via
    manifest.json, or the older one-directory-per-game form."""
    builds, seen, by_title = {}, defaultdict(set), defaultdict(dict)
    meta = {}
    manifest_path = os.path.join(root, "manifest.json")
    old = {}
    if os.path.isfile(manifest_path):
        with open(manifest_path) as f:
            old = json.load(f)
    for entry in old.get("profiles", []) + old.get("observed_sets", []):
        meta[entry["title_id"]] = {
            "title_name": entry.get("title_name", ""),
            "image_backed": entry.get("image_backed", False),
            "sdk_version": entry.get("sdk_version", ""),
        }

    def absorb(tid, name, path):
        data = open(path, "rb").read()
        sha = hashlib.sha256(data).hexdigest()
        b = builds.get(sha)
        if b is None:
            b = builds[sha] = {"sha256": sha, "name": name, "size": len(data),
                               "sdk": sdk_pair(data), "code_id": code_id(data),
                               "titles": [], "path": path}
        if tid not in b["titles"]:
            b["titles"].append(tid)
        seen[name].add(sha)
        by_title[tid][name] = sha

    prof_root = os.path.join(root, "profiles")
    if os.path.isdir(prof_root):
        for tid in sorted(os.listdir(prof_root)):
            d = os.path.join(prof_root, tid)
            if not os.path.isdir(d):
                continue
            for n in sorted(os.listdir(d)):
                if is_library(n):
                    absorb(tid, n, os.path.join(d, n))
    else:
        # Already content-addressed: recover sets from the manifest.
        by_sha = {b["sha256"]: (lib["name"], os.path.join(root, b["path"]))
                  for lib in old.get("libraries", []) for b in lib["builds"]}
        for st in old.get("observed_sets", []):
            for name, sha in st.get("libraries", {}).items():
                if sha in by_sha:
                    absorb(st["title_id"], name, by_sha[sha][1])
    return builds, seen, by_title, meta


def installed_titles(engine, addr):
    url = "%s/api/ps5/apps/installed?addr=%s:9114" % (engine, addr)
    with urllib.request.urlopen(url, timeout=180) as r:
        titles = json.loads(r.read()).get("titles", [])
    return [t for t in titles if t.get("source") and not t.get("system")]


def title_sdk(engine, addr, source):
    """(sdkVersion, requiredSystemSoftwareVersion) from the title's param.json.

    sdkVersion is the one that matters: it is what the game was BUILT against,
    and therefore which firmware's libraries it needs. requiredSystemSoftware-
    Version disagrees with it routinely and predicts nothing — SILENT HILL 2
    claims 10.20, was built with 9.00, and runs on a 9.60 console with no
    fakelib at all. Every title measured with sdkVersion > console firmware
    ships a fakelib; every title without one was built at or below it."""
    body = json.dumps({"addr": "%s:9114" % addr,
                       "path": source + "/sce_sys/param.json",
                       "max_bytes": 262144}).encode()
    req = urllib.request.Request(engine + "/api/ps5/fs/read-preview", data=body,
                                 headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            import base64
            j = json.loads(base64.b64decode(json.loads(r.read())["base64"]))
        return j.get("sdkVersion") or "", j.get("requiredSystemSoftwareVersion") or ""
    except Exception:
        return "", ""


def collect(addr, titles):
    """Download every fakelib file from every title.

    Returns (builds, seen, by_title):
      builds   {sha256: {name,size,sdk,titles[],path}}
      seen     {library name: {sha256, ...}}
      by_title {title_id: {library name: sha256}}  -- one profile per title
    """
    builds, seen, by_title = {}, defaultdict(set), defaultdict(dict)
    ftp = ftplib.FTP()
    ftp.connect(addr, 2121, 30)
    ftp.login()
    os.makedirs(CACHE, exist_ok=True)
    for t in titles:
        tid, src = t["title_id"], t["source"]
        try:
            names = [n.rsplit("/", 1)[-1] for n in ftp.nlst(src + "/fakelib")]
        except Exception:
            continue
        names = [n for n in names if is_library(n)]
        if not names:
            continue
        print("  %-10s %-34s %2d file(s)" % (tid, (t.get("title_name") or "")[:34], len(names)))
        for n in sorted(names):
            buf = io.BytesIO()
            try:
                ftp.retrbinary("RETR %s/fakelib/%s" % (src, n), buf.write)
            except Exception as e:
                print("      ! %s: %s" % (n, e))
                continue
            data = buf.getvalue()
            sha = hashlib.sha256(data).hexdigest()
            b = builds.get(sha)
            if b is None:
                p = os.path.join(CACHE, "%s-%s" % (sha[:12], n))
                with open(p, "wb") as fh:
                    fh.write(data)
                b = builds[sha] = {"sha256": sha, "name": n, "size": len(data),
                                   "sdk": sdk_pair(data), "code_id": code_id(data),
                                   "titles": [], "path": p}
            if tid not in b["titles"]:
                b["titles"].append(tid)
            seen[n].add(sha)
            by_title[tid][n] = sha
    ftp.quit()
    return builds, seen, by_title


# NOTE: there is deliberately no "pick the best build per name" function here.
# That was the previous design and it produced a set that broke a working game
# (see the module docstring). Selection happens per TARGET at backport time, by
# choosing a whole profile, never by mixing builds across profiles.


def emit(builds, seen, by_title, meta, titles, folder, image, a):
    # Build the new layout BESIDE the old one and swap at the end. Writing in
    # place destroyed the corpus once: --from-existing reads its sources from
    # inside fakelibs/, and wiping the directory first deleted the very files
    # about to be copied. Never remove the old tree until the new one is whole.
    staging = OUT + ".new"
    shutil.rmtree(staging, ignore_errors=True)
    os.makedirs(staging, exist_ok=True)

    # Every distinct build, stored once, addressed by content hash.
    libraries, stored = [], 0
    for name in sorted(seen):
        stem = name.rsplit(".", 1)[0]
        ext = name[len(stem):]
        entries = []
        for sha in sorted(seen[name], key=lambda x: -len(builds[x]["titles"])):
            b = builds[sha]
            rel = "builds/%s/%s%s" % (stem, sha[:8], ext)
            dst = os.path.join(staging, rel)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(b["path"], dst)
            stored += b["size"]
            entries.append({"sha256": sha, "size": b["size"],
                            "sdk": list(b["sdk"]) if b["sdk"] else None,
                            "code_id": b["code_id"],
                            "shipped_by": sorted(b["titles"]), "path": rel})
        libraries.append({"name": name, "builds": entries})
        spread = " ".join("%dx" % len(e["shipped_by"]) for e in entries)
        print("  %-38s %2d build(s)  [%s]" % (name, len(entries), spread))

    # What each real game ships, as references. No bytes are duplicated to
    # express a set, so recording all of them is free.
    observed = []
    for tid in sorted(by_title):
        observed.append({
            "title_id": tid,
            "title_name": meta.get(tid, {}).get("title_name", ""),
            "image_backed": bool(meta.get(tid, {}).get("image_backed")),
            "sdk_version": meta.get(tid, {}).get("sdk_version", ""),
            "libraries": {n: sha for n, sha in sorted(by_title[tid].items())},
        })

    # Builds that are shipped by exactly the same set of titles came out of the
    # same rip kit — one harvest of libraries lifted from one firmware. Measured:
    # the 13 titles carrying libSceAgc e1c8f6dc are exactly the 13 carrying
    # libSceAgcDriver d83edb3a and libScePsml 6632020e. Choosing a build in
    # isolation is therefore the wrong question; you choose a harvest.
    harvest_of = defaultdict(list)
    for sha, b in builds.items():
        harvest_of[frozenset(b["titles"])].append(sha)
    harvests = []
    for n, (titles_key, shas) in enumerate(
            sorted(harvest_of.items(), key=lambda kv: (-len(kv[0]), -len(kv[1]))), 1):
        if len(shas) < 2:
            continue  # a lone build is not evidence of a harvest
        harvests.append({
            "id": "harvest-%d" % n,
            "titles": sorted(titles_key),
            "builds": [{"name": builds[s]["name"], "sha256": s} for s in
                       sorted(shas, key=lambda x: builds[x]["name"])],
        })

    manifest = {
        "schema": 3,
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "source": {"titles_scanned": len(titles),
                   "folder_titles": len(folder),
                   "image_titles": len(image)},
        "note": ("Sony system libraries. Never commit or redistribute. "
                 "Paths in libraries[].builds[].path are relative to this file."),
        "libraries": libraries,
        "harvests": harvests,
        "observed_sets": observed,
    }
    with open(os.path.join(staging, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=2)

    # The new tree is complete: only now is it safe to drop the old one.
    if os.path.isdir(OUT):
        shutil.rmtree(OUT)
    os.rename(staging, OUT)

    dup = sum(builds[sha]["size"] for t in by_title for sha in by_title[t].values())
    distinct_code = len({(builds[s]["name"], builds[s]["code_id"]) for s in builds})
    print("\n%d library name(s), %d stored build(s) -> %d distinct librar(y/ies) once"
          " the SDK stamp and digest are masked" % (len(libraries), len(builds), distinct_code))
    print("%d harvest(s) (builds shipped by exactly the same titles), %d observed set(s)"
          % (len(harvests), len(observed)))
    print("%.1f MiB stored  (%.1f MiB if one directory per game — %.1fx saved)"
          % (stored / 1048576.0, dup / 1048576.0, dup / max(stored, 1)))
    print("-> %s (gitignored)" % OUT)
    if not a.keep_cache:
        shutil.rmtree(CACHE, ignore_errors=True)
    return 0


def main():
    print("NOTE: superseded by ps5upload's own scan (Settings > Backport "
          "libraries).\n      This writes an older schema the app will not "
          "read. See the module docstring.\n", file=sys.stderr)
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine", default="http://127.0.0.1:19113")
    ap.add_argument("--addr", default="192.168.86.100")
    ap.add_argument("--keep-cache", action="store_true")
    ap.add_argument("--from-existing", action="store_true",
                    help="re-lay-out the corpus already in fakelibs/, no console needed")
    a = ap.parse_args()

    if a.from_existing:
        builds, seen, by_title, meta = collect_local(OUT)
        if not builds:
            print("no existing corpus in %s" % OUT)
            return 1
        titles, folder, image = [], [], [t for t in meta if meta[t].get("image_backed")]
        print("re-laying out %d title(s) already in %s\n" % (len(by_title), OUT))
        return emit(builds, seen, by_title, meta, titles, folder, image, a)

    titles = installed_titles(a.engine, a.addr)
    folder = [t for t in titles if not t.get("image_backed")]
    image = [t for t in titles if t.get("image_backed")]
    print("scanning %d titles (%d folder, %d image-backed) on %s\n"
          % (len(titles), len(folder), len(image), a.addr))

    builds, seen, by_title = collect(a.addr, titles)
    meta = {}
    for t in titles:
        sdk, _req = title_sdk(a.engine, a.addr, t["source"])
        meta[t["title_id"]] = {"title_name": t.get("title_name") or "",
                               "image_backed": bool(t.get("image_backed")),
                               "sdk_version": sdk}
    if not builds:
        print("no fakelib folders found")
        return 1

    return emit(builds, seen, by_title, meta, titles, folder, image, a)


    if not a.keep_cache:
        shutil.rmtree(CACHE, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
