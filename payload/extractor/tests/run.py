#!/usr/bin/env python3
"""Host tests for ps5upload-extractor (make host-test).

Each case builds its archives in a fresh temporary folder, runs the host build of the extractor on a
request the way the helper writes one, and checks the status it reports and what is on disk: names
that escape the destination, links, multi-volume sets, passwords, truncated and damaged archives,
running out of space, conflicts and cancel.

7z cases need 7-Zip's command line (7zz or 7z); without it they are skipped, said so in the output.
The RAR fixtures are libarchive's own test archives (tests/fixtures/README.md).
"""
import bz2
import gzip
import io
import hashlib
import json
import lzma
import os
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")
BIN = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.join(HERE, "..", "build", "host", "ps5upload-extractor-host")
SEVENZIP = shutil.which("7zz") or shutil.which("7z")
ZSTD = shutil.which("zstd")

failures = []
skipped = []
passed = 0


def run(root, archive, dest, password=None, merge=False, replace=False, env=None, cancel=False, wait=True, list_only=False, top=None,
        free=False):
    req = {"id": "t1", "archive": archive, "dest": dest, "merge": merge, "replace": replace}
    if free:
        req["free"] = True
    if password is not None:
        req["password"] = password
    if list_only:
        req["list"] = True
    if top is not None:
        req["top"] = top
    rq, st, cn = (os.path.join(root, n) for n in ("request.json", "status.json", "cancel"))
    with open(rq, "w") as f:
        json.dump(req, f)
    if os.path.exists(st):
        os.unlink(st)
    if cancel:
        open(cn, "w").close()
    elif os.path.exists(cn):
        os.unlink(cn)
    e = dict(os.environ)
    e.update(env or {})
    p = subprocess.Popen([BIN, rq, st, cn], env=e, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if not wait:
        return p, st, cn
    p.wait(timeout=120)
    with open(st) as f:
        s = json.load(f)
    assert not os.path.exists(rq), "the request (it can hold a password) must be deleted"
    return s


def check(name, cond, detail=""):
    if not cond:
        failures.append(f"{name}: {detail}")
    return cond


def leftovers(top):
    out = []
    for dp, dn, fn in os.walk(top):
        out += [os.path.join(dp, n) for n in fn + dn if n.startswith(".ps5u-x-")]
    return out


def tree(top):
    out = {}
    for dp, dn, fn in os.walk(top):
        for n in fn:
            p = os.path.join(dp, n)
            rel = os.path.relpath(p, top)
            out[rel] = None if os.path.islink(p) else open(p, "rb").read()
        for n in dn:
            p = os.path.join(dp, n)
            if os.path.islink(p):
                out[os.path.relpath(p, top)] = "link"
    return out


def make_src(root):
    src = os.path.join(root, "src")
    os.makedirs(os.path.join(src, "sub", "deep"))
    with open(os.path.join(src, "a.txt"), "w") as f:
        f.write("hello\n")
    with open(os.path.join(src, "sub", "b.bin"), "wb") as f:
        f.write(os.urandom(300000))
    with open(os.path.join(src, "sub", "deep", "c.txt"), "w") as f:
        f.write("deep " * 1000)
    return src


def sevenzip(*args, cwd):
    subprocess.run([SEVENZIP, *args], cwd=cwd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def case(fn):
    global passed
    name = fn.__name__
    root = tempfile.mkdtemp(prefix="x-" + name + "-")
    try:
        before = len(failures)
        r = fn(root)
        if r == "skip":
            skipped.append(name)
            print(f"SKIP {name}")
            return
        bad = leftovers(root)
        check(name, not bad, f"temporary files left: {bad}")
        if len(failures) == before:
            passed += 1
            print(f"ok   {name}")
        else:
            print(f"FAIL {name}")
    except Exception as e:  # noqa: BLE001 - a test harness reports everything
        failures.append(f"{name}: {type(e).__name__}: {e}")
        print(f"FAIL {name}: {e}")
    finally:
        shutil.rmtree(root, ignore_errors=True)


# ---- formats ----


def test_zip_tree(root):
    src = make_src(root)
    arc = os.path.join(root, "t.zip")
    with zipfile.ZipFile(arc, "w", zipfile.ZIP_DEFLATED) as z:
        for dp, dn, fn in os.walk(src):
            for n in fn:
                p = os.path.join(dp, n)
                z.write(p, os.path.relpath(p, src))
    dest = os.path.join(root, "out")
    s = run(root, arc, dest)
    check("zip", s["state"] == "done" and s["message"] == "", s)
    check("zip", tree(dest) == tree(src), "tree differs")
    check("zip", s["files"] == 3 and s["created_dest"] is True, s)


def test_tar_compounds(root):
    src = make_src(root)
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w") as t:
        t.add(src, arcname=".")
    data = raw.getvalue()
    variants = {"t.tar": data, "t.tar.gz": None, "t.tgz": None, "t.tar.bz2": bz2.compress(data), "t.tar.xz": lzma.compress(data)}
    import gzip

    variants["t.tar.gz"] = gzip.compress(data)
    variants["t.tgz"] = gzip.compress(data)
    if ZSTD:
        p = subprocess.run([ZSTD, "-q", "-c"], input=data, stdout=subprocess.PIPE, check=True)
        variants["t.tar.zst"] = p.stdout
    for name, blob in variants.items():
        arc = os.path.join(root, name)
        with open(arc, "wb") as f:
            f.write(blob)
        dest = os.path.join(root, "out-" + name)
        s = run(root, arc, dest)
        check(name, s["state"] == "done", s)
        check(name, tree(dest) == tree(src), "tree differs")
        check(name, s["unsafe"] == 0, s)


def test_single_gz_and_xz(root):
    import gzip

    blob = os.urandom(70000)
    for name, packed in (("data.bin.gz", gzip.compress(blob)), ("data.bin.xz", lzma.compress(blob)), ("data.bin.bz2", bz2.compress(blob))):
        arc = os.path.join(root, name)
        with open(arc, "wb") as f:
            f.write(packed)
        dest = os.path.join(root, "out-" + name)
        s = run(root, arc, dest)
        check(name, s["state"] == "done", s)
        got = tree(dest)
        check(name, list(got.values()) == [blob], f"files: {list(got)}")


def test_7z_and_split(root):
    if not SEVENZIP:
        return "skip"
    src = make_src(root)
    sevenzip("a", "-mx=5", os.path.join(root, "t.7z"), ".", cwd=src)
    dest = os.path.join(root, "out")
    s = run(root, os.path.join(root, "t.7z"), dest)
    check("7z", s["state"] == "done" and tree(dest) == tree(src), s)
    check("7z", s["bytes_total"] == s["bytes"] and s["files_total"] == 3, s)
    # A numbered split set, opened from its first volume.
    os.makedirs(os.path.join(root, "vol"))
    sevenzip("a", "-v100k", "-mx=0", os.path.join(root, "vol", "s.7z"), ".", cwd=src)
    vols = sorted(os.listdir(os.path.join(root, "vol")))
    check("split", len(vols) >= 3, vols)
    dest = os.path.join(root, "out-split")
    s = run(root, os.path.join(root, "vol", "s.7z.001"), dest)
    check("split", s["state"] == "done" and tree(dest) == tree(src), s)
    # One volume gone.
    os.unlink(os.path.join(root, "vol", "s.7z.002"))
    dest = os.path.join(root, "out-missing")
    s = run(root, os.path.join(root, "vol", "s.7z.001"), dest)
    check("split missing", s["state"] == "failed", s)
    check("split missing", s["message"].split(":")[0] in ("fs_extract_missing_volume", "fs_extract_truncated", "fs_extract_corrupt"), s)


def test_passwords(root):
    if not SEVENZIP:
        return "skip"
    src = make_src(root)
    cases = {
        "aes.7z": ["a", "-pSecret", "-mhe=on"],
        "aes-data.7z": ["a", "-pSecret"],
        "aes.zip": ["a", "-tzip", "-pSecret", "-mem=AES256"],
        "crypto.zip": ["a", "-tzip", "-pSecret", "-mem=ZipCrypto"],
    }
    for name, args in cases.items():
        arc = os.path.join(root, name)
        sevenzip(*args, arc, ".", cwd=src)
        dest = os.path.join(root, "out-" + name)
        s = run(root, arc, dest)
        check(name + " none", s["message"] == "fs_extract_password_required", s)
        check(name + " none", not os.path.exists(dest), "a folder was made before the password was known")
        s = run(root, arc, dest, password="wrong")
        check(name + " wrong", s["message"] == "fs_extract_wrong_password", s)
        s = run(root, arc, dest + "-ok", password="Secret")
        check(name + " right", s["state"] == "done" and tree(dest + "-ok") == tree(src), s)


def test_unicode_password(root):
    if not SEVENZIP:
        return "skip"
    src = make_src(root)
    arc = os.path.join(root, "u.7z")
    sevenzip("a", "-pпароль密码", "-mhe=on", arc, ".", cwd=src)
    s = run(root, arc, os.path.join(root, "out"), password="пароль密码")
    check("unicode password", s["state"] == "done", s)


# ---- safety ----


def test_traversal(root):
    arc = os.path.join(root, "evil.zip")
    with zipfile.ZipFile(arc, "w") as z:
        for n in ("../evil.txt", "/abs.txt", "a/../../evil2.txt", "C:/drive.txt", "..\\win.txt", "ok/./fine.txt", "ok.txt"):
            z.writestr(zipfile.ZipInfo(n), b"x")
    dest = os.path.join(root, "d", "out")
    os.makedirs(os.path.dirname(dest))
    s = run(root, arc, dest)
    check("traversal", s["state"] == "done", s)
    check("traversal", s["unsafe"] == 5 and s["files"] == 2, s)
    check("traversal", sorted(tree(dest)) == ["ok.txt", os.path.join("ok", "fine.txt")], sorted(tree(dest)))
    outside = [p for p in os.listdir(root) if p not in ("d", "evil.zip", "status.json", "status.json.tmp")]
    check("traversal", not outside and os.listdir(os.path.join(root, "d")) == ["out"], outside)


def test_tar_traversal_and_links(root):
    arc = os.path.join(root, "links.tar")
    outside = os.path.join(root, "outside")
    os.makedirs(outside)
    with tarfile.open(arc, "w") as t:
        def add(name, data=b"x", kind=tarfile.REGTYPE, link=""):
            ti = tarfile.TarInfo(name)
            ti.type = kind
            ti.linkname = link
            ti.size = len(data) if kind == tarfile.REGTYPE else 0
            t.addfile(ti, io.BytesIO(data) if kind == tarfile.REGTYPE else None)
        add("esc", kind=tarfile.SYMTYPE, link=outside)
        add("esc/pwned.txt")  # through the link, if it had been made
        add("hard", kind=tarfile.LNKTYPE, link="/etc/passwd")
        add("fifo", kind=tarfile.FIFOTYPE)
        add("../up.txt")
        add("good.txt", b"good")
    dest = os.path.join(root, "out")
    s = run(root, arc, dest)
    check("tar links", s["state"] == "done", s)
    check("tar links", s["links"] == 3 and s["unsafe"] == 1, s)
    check("tar links", not os.listdir(outside), "wrote through a link")
    check("tar links", not any(os.path.islink(os.path.join(dp, n)) for dp, dn, fn in os.walk(dest) for n in dn + fn), "made a link")
    # The file named under the link's name lands in a real folder of that name, inside.
    check("tar links", tree(dest).get("good.txt") == b"good" and tree(dest).get(os.path.join("esc", "pwned.txt")) == b"x", tree(dest))


def test_merge_never_follows_existing_link(root):
    outside = os.path.join(root, "outside")
    dest = os.path.join(root, "out")
    os.makedirs(outside)
    os.makedirs(dest)
    os.symlink(outside, os.path.join(dest, "evil"))
    os.symlink(os.path.join(outside, "f.txt"), os.path.join(dest, "f.txt"))
    arc = os.path.join(root, "m.zip")
    with zipfile.ZipFile(arc, "w") as z:
        z.writestr("evil/x.txt", b"x")
        z.writestr("f.txt", b"new")
    s = run(root, arc, dest, merge=True, replace=True)
    check("merge link", s["state"] == "done" and s["unsafe"] == 1, s)
    check("merge link", not os.listdir(outside), "wrote through an existing link")
    check("merge link", not os.path.islink(os.path.join(dest, "f.txt")) and open(os.path.join(dest, "f.txt"), "rb").read() == b"new",
          "replace must swap the link itself")


def test_zip_symlink_entry(root):
    arc = os.path.join(root, "sl.zip")
    with zipfile.ZipFile(arc, "w") as z:
        zi = zipfile.ZipInfo("link")
        zi.create_system = 3
        zi.external_attr = (stat.S_IFLNK | 0o777) << 16
        z.writestr(zi, "/etc/passwd")
        z.writestr("plain.txt", b"p")
    dest = os.path.join(root, "out")
    s = run(root, arc, dest)
    check("zip symlink", s["state"] == "done" and s["links"] == 1 and sorted(tree(dest)) == ["plain.txt"], s)


# ---- RAR ----


def copy_fixtures(root, names):
    for n in names:
        shutil.copy(os.path.join(FIXTURES, n), os.path.join(root, n))


def test_rar(root):
    copy_fixtures(root, ["rar4.rar", "rar5.rar", "rar5-symlink.rar"])
    s = run(root, os.path.join(root, "rar4.rar"), os.path.join(root, "o4"))
    check("rar4", s["state"] == "done" and s["links"] == 1, s)
    t = tree(os.path.join(root, "o4"))
    check("rar4", t.get("test.txt") == t.get(os.path.join("testdir", "test.txt")) and t.get("test.txt"), t)
    check("rar4", os.path.isdir(os.path.join(root, "o4", "testemptydir")), "empty folder missing")
    s = run(root, os.path.join(root, "rar5.rar"), os.path.join(root, "o5"))
    check("rar5", s["state"] == "done" and s["files"] == 1 and len(tree(os.path.join(root, "o5"))["test.bin"]) == 1200, s)
    s = run(root, os.path.join(root, "rar5-symlink.rar"), os.path.join(root, "osl"))
    check("rar5 symlink", s["state"] == "done" and s["links"] == 2, s)
    # #801: a RAR5 dictionary over libarchive's 64 MiB cap (made with RAR 7.23 -md128m: 87.5 MiB
    # of text, so the declared window is 128 MiB), plain and solid.
    copy_fixtures(root, ["rar5-dict128m.rar", "rar5-dict128m-solid.rar"])
    for name in ("rar5-dict128m.rar", "rar5-dict128m-solid.rar"):
        out = os.path.join(root, "od-" + name)
        s = run(root, os.path.join(root, name), out)
        body = tree(out).get("big-dict.bin", b"") if s["state"] == "done" else b""
        check(f"{name} (dictionary over 64 MiB)",
              s["state"] == "done" and len(body) == 91750400
              and hashlib.sha256(body).hexdigest().startswith("3beb294098a675c2"), s)


def test_rar_multivolume(root):
    vols = [f"rar5-solid.part0{i}.rar" for i in range(1, 5)]
    copy_fixtures(root, vols + [f"rar4-multi.part{i}.rar" for i in range(1, 7)])
    s = run(root, os.path.join(root, vols[0]), os.path.join(root, "o"))
    check("rar5 multi", s["state"] == "done" and s["files"] >= 2, s)
    s = run(root, os.path.join(root, "rar4-multi.part1.rar"), os.path.join(root, "o4"))
    check("rar4 multi", s["state"] == "done" and s["files"] >= 1, s)
    os.unlink(os.path.join(root, vols[2]))
    s = run(root, os.path.join(root, vols[0]), os.path.join(root, "o2"))
    check("rar missing volume", s["state"] == "failed" and s["message"].startswith(("fs_extract_missing_volume", "fs_extract_truncated")), s)


def test_rar_mixed_passwords(root):
    # libarchive's own fixtures: a.txt and c.txt have no password, b.txt has "password", d.txt
    # another one. The files the password opens are written; d.txt is a wrong password.
    copy_fixtures(root, ["rar4-encrypted.rar", "rar5-encrypted.rar"])
    for n in ("rar4-encrypted.rar", "rar5-encrypted.rar"):
        out = os.path.join(root, "o-" + n)
        s = run(root, os.path.join(root, n), out, password="password")
        check(n, s["state"] == "failed" and s["message"] == "fs_extract_wrong_password", s)
        check(n, "d.txt" not in tree(out), "a file was kept under a wrong password")
        s = run(root, os.path.join(root, n), os.path.join(root, "none-" + n))
        check(n + " none", s["message"] == "fs_extract_password_required", s)


# #958: password-protected RAR, made by tests/make_encrypted_rar.py (RAR 7 and RAR 6).
sys.path.insert(0, HERE)
import make_encrypted_rar as pwrar  # noqa: E402

PW_RAR = {
    "pw-rar5-data.rar": 1, "pw-rar5-headers.rar": 1, "pw-rar5-stored.rar": 1, "pw-rar5-blake2.rar": 1,
    "pw-rar5-multi.part1.rar": 3, "pw-rar5-solid-multi.part1.rar": 3, "pw-rar5-stored-multi.part1.rar": 4,
    "pw-rar4-data.rar": 1, "pw-rar4-headers.rar": 1, "pw-rar4-stored.rar": 1, "pw-rar4-multi.part1.rar": 3,
}


def pw_rar_copy(root, first):
    n = PW_RAR[first]
    names = [first] if n == 1 else [first.replace(".part1.", f".part{i}.") for i in range(1, n + 1)]
    copy_fixtures(root, names)
    return os.path.join(root, first), names


def pw_of(name):
    return pwrar.PW5 if "rar5" in name else pwrar.PW4


def files_in(top):
    return {k.replace(os.sep, "/"): v for k, v in tree(top).items()} if os.path.exists(top) else {}


def test_rar_passwords(root):
    want = pwrar.encrypted_rar_files()
    for first in PW_RAR:
        arc, _ = pw_rar_copy(root, first)
        out = os.path.join(root, "ok-" + first)
        s = run(root, arc, out, password=pw_of(first))
        got = files_in(out)
        check(first, s["state"] == "done" and got == want, (s, sorted(got)))
        check(first, s["files"] == len(want), s)
        # No password: refused before the folder is made.
        out = os.path.join(root, "none-" + first)
        s = run(root, arc, out)
        check(first + " none", s["message"] == "fs_extract_password_required", s)
        check(first + " none", not os.path.exists(out), "a folder was made without the password")
        # A wrong one: refused, nothing written. RAR 3/4 with only the data encrypted has no
        # password check, so its first file is decoded (and dropped) first; the helper then removes
        # the folder this run made.
        out = os.path.join(root, "bad-" + first)
        s = run(root, arc, out, password="not the password")
        check(first + " wrong", s["state"] == "failed" and s["message"] == "fs_extract_wrong_password", s)
        check(first + " wrong", not files_in(out), files_in(out))
        if first not in ("pw-rar4-data.rar", "pw-rar4-stored.rar"):
            check(first + " wrong", not os.path.exists(out), "a folder was made under a wrong password")


def test_rar_password_list_and_volumes(root):
    # Listing: the names are readable without the password unless the headers are encrypted.
    arc, _ = pw_rar_copy(root, "pw-rar5-data.rar")
    s = run(root, arc, os.path.join(root, "l1"), list_only=True)
    check("list data-only", s["state"] == "done" and sorted(s["top"].split("/")) == ["a.txt", "sub"], s)
    arc, _ = pw_rar_copy(root, "pw-rar4-headers.rar")
    s = run(root, arc, os.path.join(root, "l2"), list_only=True)
    check("list headers none", s["message"] == "fs_extract_password_required", s)
    s = run(root, arc, os.path.join(root, "l3"), list_only=True, password=pwrar.PW4)
    check("list headers", s["state"] == "done" and sorted(s["top"].split("/")) == ["a.txt", "sub"], s)
    # A volume missing in the middle of an encrypted file.
    arc, names = pw_rar_copy(root, "pw-rar5-multi.part1.rar")
    os.unlink(os.path.join(root, names[1]))
    s = run(root, arc, os.path.join(root, "m1"), password=pwrar.PW5)
    check("missing volume", s["state"] == "failed"
          and s["message"].startswith(("fs_extract_missing_volume", "fs_extract_truncated")), s)
    # Not enough room: known before anything is made (the sizes come from the decrypted headers).
    arc, _ = pw_rar_copy(root, "pw-rar4-headers.rar")
    s = run(root, arc, os.path.join(root, "sp"), password=pwrar.PW4, env={"XTEST_FREE": str(1 << 20)})
    check("space", s["message"].startswith("fs_extract_no_space: needs"), s)
    check("space", not os.path.exists(os.path.join(root, "sp")), "made the folder anyway")


def test_rar_password_damaged(root):
    # Damaged encrypted data under the right password (RAR5 checks the password itself): damage,
    # not a wrong password, and the damaged file is not kept.
    arc, _ = pw_rar_copy(root, "pw-rar5-data.rar")
    data = bytearray(open(arc, "rb").read())
    at = data.find(b"b.bin")
    for i in range(at + 2000, at + 2016):
        data[i] ^= 0x5A
    with open(arc, "wb") as f:
        f.write(data)
    out = os.path.join(root, "o")
    s = run(root, arc, out, password=pwrar.PW5)
    check("damaged", s["state"] == "failed" and s["message"].startswith("fs_extract_corrupt"), s)
    check("damaged", "sub/b.bin" not in files_in(out), "the damaged file was kept")


def test_rar_hostile_header_sizes(root):
    # Crafted RAR5 archives (fixtures/README.md), password "pw": a header whose data size runs past
    # the volume and wraps the scan back onto itself after an encrypted file split mid-block, and a
    # second encryption header inside the encrypted headers. Both looped forever (one with memory
    # growing), deaf to Stop; each is a cut archive or damage now, at once.
    copy_fixtures(root, ["crafted-rar5-size-wrap.rar", "crafted-rar5-enc-header-loop.rar"])
    for name in ("crafted-rar5-size-wrap.rar", "crafted-rar5-enc-header-loop.rar"):
        out = os.path.join(root, "o-" + name)
        p, st, _ = run(root, os.path.join(root, name), out, password="pw", wait=False)
        try:
            p.wait(timeout=20)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()
            check(name, False, "still running after 20 s")
            continue
        with open(st) as f:
            s = json.load(f)
        check(name, s["state"] == "failed" and s["message"].startswith(("fs_extract_truncated", "fs_extract_corrupt")), s)


def test_rar4_key_derivation_stops(root):
    # A RAR 4 archive whose service blocks each need a fresh key derivation (fixtures/README.md):
    # libarchive reads them all inside one call, so only the derivation and the header scanner can
    # see Stop. It used to run on for over a minute after Stop and end "done".
    copy_fixtures(root, ["crafted-rar4-many-salts.rar"])
    p, st, cn = run(root, os.path.join(root, "crafted-rar4-many-salts.rar"), os.path.join(root, "o"),
                    password="x" * 127, wait=False)
    time.sleep(1.0)
    open(cn, "w").close()
    t0 = time.time()
    try:
        p.wait(timeout=15)
    except subprocess.TimeoutExpired:
        p.kill()
        p.wait()
        check("kdf stop", False, "still running 15 s after Stop")
        return
    took = time.time() - t0
    with open(st) as f:
        s = json.load(f)
    check("kdf stop", s["message"] == "fs_extract_cancelled", s)
    check("kdf stop", took < 5, f"took {took:.1f} s after Stop")


# ---- damage ----


def test_truncated_and_corrupt(root):
    src = make_src(root)
    arc = os.path.join(root, "t.zip")
    with zipfile.ZipFile(arc, "w", zipfile.ZIP_DEFLATED) as z:
        z.write(os.path.join(src, "sub", "b.bin"), "b.bin")
    data = open(arc, "rb").read()
    with open(os.path.join(root, "cut.zip"), "wb") as f:
        f.write(data[: len(data) // 2])
    s = run(root, os.path.join(root, "cut.zip"), os.path.join(root, "o1"))
    check("zip cut", s["state"] == "failed", s)
    bad = bytearray(data)
    for i in range(200, 400):
        bad[i] ^= 0x5A
    with open(os.path.join(root, "bad.zip"), "wb") as f:
        f.write(bad)
    s = run(root, os.path.join(root, "bad.zip"), os.path.join(root, "o2"))
    check("zip damaged", s["state"] == "failed" and s["message"].startswith("fs_extract_corrupt"), s)
    check("zip damaged", not os.path.exists(os.path.join(root, "o2", "b.bin")), "a damaged file was kept")
    import gzip

    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w") as t:
        t.add(src, arcname=".")
    gz = gzip.compress(raw.getvalue())
    with open(os.path.join(root, "cut.tar.gz"), "wb") as f:
        f.write(gz[: len(gz) * 2 // 3])
    s = run(root, os.path.join(root, "cut.tar.gz"), os.path.join(root, "o3"))
    check("tgz cut", s["state"] == "failed" and s["message"].startswith(("fs_extract_truncated", "fs_extract_corrupt")), s)
    if SEVENZIP:
        sevenzip("a", os.path.join(root, "t.7z"), ".", cwd=src)
        d = open(os.path.join(root, "t.7z"), "rb").read()
        with open(os.path.join(root, "cut.7z"), "wb") as f:
            f.write(d[: len(d) // 2])
        s = run(root, os.path.join(root, "cut.7z"), os.path.join(root, "o4"))
        check("7z cut", s["state"] == "failed", s)
    with open(os.path.join(root, "noise.zip"), "wb") as f:
        f.write(os.urandom(5000))
    s = run(root, os.path.join(root, "noise.zip"), os.path.join(root, "o5"))
    check("not an archive", s["message"] == "fs_extract_not_archive" and not os.path.exists(os.path.join(root, "o5")), s)
    copy_fixtures(root, ["rar5.rar"])
    d = open(os.path.join(root, "rar5.rar"), "rb").read()
    with open(os.path.join(root, "cut.rar"), "wb") as f:
        f.write(d[: len(d) - 120])
    s = run(root, os.path.join(root, "cut.rar"), os.path.join(root, "o6"))
    check("rar cut", s["state"] == "failed", s)


# ---- space, conflicts, cancel ----


def test_out_of_space(root):
    src = make_src(root)
    arc = os.path.join(root, "t.zip")
    with zipfile.ZipFile(arc, "w", zipfile.ZIP_STORED) as z:
        for dp, dn, fn in os.walk(src):
            for n in fn:
                p = os.path.join(dp, n)
                z.write(p, os.path.relpath(p, src))
    # Known total, short drive: refused before anything is made.
    s = run(root, arc, os.path.join(root, "o1"), env={"XTEST_FREE": str(1 << 20)})
    check("space precheck", s["message"].startswith("fs_extract_no_space: needs"), s)
    check("space precheck", not os.path.exists(os.path.join(root, "o1")), "made the folder anyway")
    # The same when the folder is to be made several levels down (the room is asked of the
    # nearest folder that exists).
    s = run(root, arc, os.path.join(root, "na", "nb", "o1"), env={"XTEST_FREE": str(1 << 20)})
    check("space precheck deep", s["message"].startswith("fs_extract_no_space: needs"), s)
    check("space precheck deep", not os.path.exists(os.path.join(root, "na")), "made the folder anyway")
    # A merge is not held to the whole archive: it skips what is already there.
    s = run(root, arc, os.path.join(root, "m1"))
    check("space merge", s["state"] == "done", s)
    one = max(len(v) for v in tree(os.path.join(root, "m1")).values() if isinstance(v, bytes))
    s = run(root, arc, os.path.join(root, "m1"), env={"XTEST_FREE": str((64 << 20) + one)}, merge=True)
    check("space merge", s["state"] == "done", s)
    # The drive fills up while writing.
    s = run(root, arc, os.path.join(root, "o2"), env={"XTEST_WRITE_LIMIT": "100000"})
    check("space mid-write", s["message"].startswith("fs_extract_no_space"), s)
    check("space mid-write", "b.bin" not in str(tree(os.path.join(root, "o2"))), "a partial file was kept")


def test_out_of_space_without_a_size(root):
    # A .gz says nothing of its size, so nothing is checked when the file begins: the room is
    # looked at again while writing, and the extraction ends when none is left (it used to write on).
    big = os.path.join(root, "big.bin")
    with open(big, "wb") as f:
        f.write(b"\0" * (12 << 20))
    with open(big, "rb") as src, gzip.open(os.path.join(root, "big.bin.gz"), "wb") as dst:
        shutil.copyfileobj(src, dst)
    os.unlink(big)
    s = run(root, os.path.join(root, "big.bin.gz"), os.path.join(root, "o1"), env={"XTEST_FREE": str(2 << 20)})
    check("space no size", s["state"] == "failed" and s["message"].startswith("fs_extract_no_space"), s)
    check("space no size", "big.bin" not in str(tree(os.path.join(root, "o1"))), "a partial file was kept")


def test_rar_total_precheck(root):
    # #869: a RAR that is not solid is refused before anything is made when all of it cannot fit.
    # rar4-multi holds several files across six volumes; each file alone would fit.
    vols = [f"rar4-multi.part{i}.rar" for i in range(1, 7)]
    copy_fixtures(root, vols + ["rar5.rar", "rar5-dict128m.rar"])
    first = os.path.join(root, vols[0])
    s = run(root, first, os.path.join(root, "full"))
    check("rar total", s["state"] == "done", s)
    sizes = sorted(len(v) for v in tree(os.path.join(root, "full")).values() if isinstance(v, bytes))
    total, margin = sum(sizes), 64 << 20
    check("rar total", len(sizes) >= 2 and sizes[-1] < total, sizes)
    # Room for the largest file, not for all of them.
    free = margin + total - 1
    s = run(root, first, os.path.join(root, "o1"), env={"XTEST_FREE": str(free)})
    check("rar total short", s["message"] == f"fs_extract_no_space: needs {total + margin} bytes, {free} free", s)
    check("rar total short", not os.path.exists(os.path.join(root, "o1")), "made the folder anyway")
    s = run(root, first, os.path.join(root, "o2"), env={"XTEST_FREE": str(margin + total)})
    check("rar total fits", s["state"] == "done", s)
    for n in ("rar5.rar", "rar5-dict128m.rar"):
        s = run(root, os.path.join(root, n), os.path.join(root, "o-" + n), env={"XTEST_FREE": str(1 << 20)})
        check("rar5 total " + n, s["message"].startswith("fs_extract_no_space: needs"), s)
        check("rar5 total " + n, not os.path.exists(os.path.join(root, "o-" + n)), "made the folder anyway")
    # A merge keeps the per-file check (it may skip what is already there).
    s = run(root, first, os.path.join(root, "o2"), env={"XTEST_FREE": str(free)}, merge=True)
    check("rar total merge", s["state"] == "done", s)


def test_conflicts(root):
    arc = os.path.join(root, "c.zip")
    with zipfile.ZipFile(arc, "w") as z:
        z.writestr("keep.txt", b"from archive")
        z.writestr("new.txt", b"new")
        z.writestr("dir", b"a file where a folder is")
    dest = os.path.join(root, "out")
    os.makedirs(os.path.join(dest, "dir"))
    with open(os.path.join(dest, "keep.txt"), "wb") as f:
        f.write(b"mine")
    s = run(root, arc, dest)
    check("exists", s["message"] == "fs_extract_destination_exists", s)
    s = run(root, arc, dest, merge=True)
    check("merge skip", s["state"] == "done" and s["skipped"] == 2 and s["files"] == 1, s)
    check("merge skip", open(os.path.join(dest, "keep.txt"), "rb").read() == b"mine", "replaced without asking")
    s = run(root, arc, dest, merge=True, replace=True)
    check("merge replace", s["state"] == "done" and s["skipped"] == 1, s)
    check("merge replace", open(os.path.join(dest, "keep.txt"), "rb").read() == b"from archive", "not replaced")
    check("merge replace", os.path.isdir(os.path.join(dest, "dir")), "a folder was replaced by a file")


def test_list_top_level(root):
    """Extract here (#891): the archive's top-level names, read without writing anything."""
    arc = os.path.join(root, "l.zip")
    with zipfile.ZipFile(arc, "w") as z:
        z.writestr("Game/sce_sys/param.json", b"{}")
        z.writestr("Game/eboot.bin", b"x")
        z.writestr("readme.txt", b"hi")
        z.writestr("./notes/a.txt", b"a")
        z.writestr("../evil.txt", b"no")
    dest = os.path.join(root, "here")
    os.makedirs(dest)
    s = run(root, arc, dest, list_only=True)
    check("list zip", s["state"] == "done" and s["message"] == "", s)
    check("list zip", sorted(s["top"].split("/")) == ["Game", "notes", "readme.txt"], s)
    check("list zip", sorted(s["top_dirs"].split("/")) == ["Game", "notes"], s)
    check("list zip", s["top_more"] is False and s["files"] == 0 and s["created_dest"] is False, s)
    check("list zip", os.listdir(dest) == [], "a listing wrote something")
    # a listing never makes the folder, nor needs room
    s = run(root, arc, os.path.join(root, "nowhere"), list_only=True, env={"XTEST_FREE": "1"})
    check("list no dest", s["state"] == "done" and not os.path.exists(os.path.join(root, "nowhere")), s)
    # a compressed tar is read through
    src = make_src(root)
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w") as t:
        t.add(src, arcname="pack")
    tgz = os.path.join(root, "l.tar.gz")
    with open(tgz, "wb") as f:
        f.write(gzip.compress(raw.getvalue()))
    s = run(root, tgz, dest, list_only=True)
    check("list tgz", s["state"] == "done" and s["top"] == "pack" and s["top_dirs"] == "pack", s)
    check("list tgz", os.listdir(dest) == [], "a listing wrote something")
    # a lone .gz is one file named after the archive
    one = os.path.join(root, "data.bin.gz")
    with open(one, "wb") as f:
        f.write(gzip.compress(b"z" * 1000))
    s = run(root, one, dest, list_only=True)
    check("list gz", s["state"] == "done" and s["top"] == "data.bin" and s["top_dirs"] == "", s)
    if SEVENZIP:
        sevenzip("a", "-mx=5", os.path.join(root, "l.7z"), "sub", "a.txt", cwd=src)
        s = run(root, os.path.join(root, "l.7z"), dest, list_only=True)
        check("list 7z", s["state"] == "done" and sorted(s["top"].split("/")) == ["a.txt", "sub"], s)
    copy_fixtures(root, ["rar4.rar"])
    s = run(root, os.path.join(root, "rar4.rar"), dest, list_only=True)
    check("list rar", s["state"] == "done" and s["top"] != "", s)
    check("list rar", os.listdir(dest) == [], "a listing wrote something")


def test_top_level_renames(root):
    """Extract here (#891): Keep both renames the incoming entry, Skip leaves it out."""
    arc = os.path.join(root, "r.zip")
    with zipfile.ZipFile(arc, "w") as z:
        z.writestr("Game/eboot.bin", b"new eboot")
        z.writestr("Game/extra.txt", b"extra")
        z.writestr("readme.txt", b"new readme")
        z.writestr("fresh.txt", b"fresh")
    dest = os.path.join(root, "here")
    os.makedirs(os.path.join(dest, "Game"))
    with open(os.path.join(dest, "Game", "eboot.bin"), "wb") as f:
        f.write(b"mine")
    with open(os.path.join(dest, "readme.txt"), "wb") as f:
        f.write(b"my readme")
    # Keep both: the incoming Game goes to "Game (2)", readme.txt to "readme (2).txt"
    s = run(root, arc, dest, merge=True, top="Game/Game (2)/readme.txt/readme (2).txt")
    check("keep both", s["state"] == "done" and s["skipped"] == 0 and s["files"] == 4, s)
    check("keep both", open(os.path.join(dest, "Game", "eboot.bin"), "rb").read() == b"mine", "the old one was touched")
    check("keep both", not os.path.exists(os.path.join(dest, "Game", "extra.txt")), "merged into the old folder")
    check("keep both", open(os.path.join(dest, "Game (2)", "eboot.bin"), "rb").read() == b"new eboot", "not renamed")
    check("keep both", open(os.path.join(dest, "readme (2).txt"), "rb").read() == b"new readme", "not renamed")
    check("keep both", open(os.path.join(dest, "readme.txt"), "rb").read() == b"my readme", "the old one was touched")
    # Skip existing: what is there stays, nothing is added into it, the rest comes out
    dest2 = os.path.join(root, "here2")
    os.makedirs(os.path.join(dest2, "Game"))
    with open(os.path.join(dest2, "readme.txt"), "wb") as f:
        f.write(b"my readme")
    s = run(root, arc, dest2, merge=True, top="Game//readme.txt/")
    check("skip", s["state"] == "done" and s["skipped"] == 3 and s["files"] == 1, s)
    check("skip", os.listdir(os.path.join(dest2, "Game")) == [], "wrote into a folder left out")
    check("skip", open(os.path.join(dest2, "readme.txt"), "rb").read() == b"my readme", "replaced a file left out")
    check("skip", open(os.path.join(dest2, "fresh.txt"), "rb").read() == b"fresh", "the rest did not come out")
    # a "to" that is not a name stays inside the folder
    dest3 = os.path.join(root, "here3")
    os.makedirs(dest3)
    s = run(root, arc, dest3, merge=True, top="readme.txt/..")
    check("bad rename", s["state"] == "done" and not os.path.exists(os.path.join(root, "readme.txt")), s)


def test_cancel(root):
    big = os.path.join(root, "big.bin")
    with open(big, "wb") as f:
        for _ in range(64):
            f.write(os.urandom(1 << 20))
    arc = os.path.join(root, "big.zip")
    with zipfile.ZipFile(arc, "w", zipfile.ZIP_DEFLATED) as z:
        z.write(big, "big.bin")
        z.write(big, "big2.bin")
    os.unlink(big)
    # Asked before it starts.
    s = run(root, arc, os.path.join(root, "o1"), cancel=True)
    check("cancel early", s["message"] == "fs_extract_cancelled", s)
    # Asked while it runs: ends within a buffer, keeps no partial file.
    p, st, cn = run(root, arc, os.path.join(root, "o2"), wait=False, env={"XTEST_WRITE_DELAY_US": "2000"})
    deadline = time.time() + 20
    while time.time() < deadline:
        try:
            with open(st) as f:
                cur = json.load(f)
            if cur.get("bytes_done", 0) > 0:
                break
        except (OSError, ValueError):
            pass
        time.sleep(0.02)
    open(cn, "w").close()
    t0 = time.time()
    p.wait(timeout=30)
    with open(st) as f:
        s = json.load(f)
    check("cancel", s["message"] == "fs_extract_cancelled", s)
    check("cancel", time.time() - t0 < 5, "slow to stop")
    check("cancel", "big2.bin" not in tree(os.path.join(root, "o2")), tree(os.path.join(root, "o2")).keys())


def test_bad_request(root):
    with open(os.path.join(root, "request.json"), "w") as f:
        f.write('{"id":"x","archive":"relative.zip","dest":"/tmp/x"}')
    p = subprocess.run([BIN, os.path.join(root, "request.json"), os.path.join(root, "status.json"), os.path.join(root, "c")],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    s = json.load(open(os.path.join(root, "status.json")))
    check("bad request", p.returncode != 0 and s["message"] == "fs_extract_bad_request", s)


def test_no_status_no_extraction(root):
    # The status cannot be written (its folder is gone, its drive is full): the helper could never
    # follow or stop this run, so nothing is extracted.
    src = make_src(root)
    arc = os.path.join(root, "t.zip")
    with zipfile.ZipFile(arc, "w", zipfile.ZIP_STORED) as z:
        for dp, dn, fn in os.walk(src):
            for n in fn:
                p = os.path.join(dp, n)
                z.write(p, os.path.relpath(p, src))
    dest = os.path.join(root, "o1")
    with open(os.path.join(root, "request.json"), "w") as f:
        json.dump({"id": "t1", "archive": arc, "dest": dest, "merge": False, "replace": False}, f)
    p = subprocess.run([BIN, os.path.join(root, "request.json"), os.path.join(root, "no-such-folder", "status.json"),
                        os.path.join(root, "c")], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60)
    check("no status", p.returncode != 0, p.returncode)
    check("no status", not os.path.exists(dest), "extracted without a status")
    check("no status", not os.path.exists(os.path.join(root, "request.json")), "the request was left behind")


# ---- freeing the archive as it is unpacked ("free": true, src/xfree.h) ----


def free_src(root, n=8, size=150000):
    """Files of random bytes (so stored ones can be found in the archive by their content)."""
    src = tempfile.mkdtemp(prefix="fsrc-", dir=root)
    os.makedirs(os.path.join(src, "sub"))
    files = {}
    for i in range(n):
        rel = f"f{i}.bin" if i % 2 else os.path.join("sub", f"f{i}.bin")
        data = os.urandom(size)
        with open(os.path.join(src, rel), "wb") as f:
            f.write(data)
        files[rel] = data
    return src, files


def set_parts(folder, prefix):
    return [os.path.join(folder, n) for n in sorted(os.listdir(folder)) if n.startswith(prefix)]


def read_trace(path):
    if not os.path.exists(path):
        return []
    return [tuple(line.split(" ", 1)) for line in open(path).read().splitlines()]


def check_freed_when_safe(name, parts, files, trace, groups=None):
    """Every part was deleted only once every file with a byte in it, or in a part before it, was out
    (and, for a solid block, every file of each such block); and at least one before the last file
    came out (the space really came back on the way)."""
    sizes = [os.path.getsize(p) for p in parts]
    blob = b"".join(open(p, "rb").read() for p in parts)
    ends, at = {}, 0
    for p, n in zip(parts, sizes):
        at += n
        ends[os.path.basename(p)] = at
    # Where each file starts: its first bytes (a solid LZMA2 block stores random data in chunks of its
    # own, so only a file's start is sure to be in one piece).
    off = {rel: blob.find(data[:200]) for rel, data in files.items()}
    if not check(name, all(o >= 0 for o in off.values()), "a file's bytes are not stored as they are"):
        return
    groups = {rel: g for rel, g in (groups or {rel: rel for rel in files}).items() if rel in files}
    done = set()
    freed_before_last = False
    for what, arg in trace:
        if what == "file":
            done.add(arg)
            continue
        end = ends[arg]
        early = [g for rel, g in groups.items() if off[rel] < end]
        needed = {rel for rel, g in groups.items() if g in early}
        missing = sorted(needed - done)
        check(name, not missing, f"{arg} deleted before {missing} were out")
        if len(done) < len(files):
            freed_before_last = True
    check(name, freed_before_last, f"no part was deleted before the end: {trace}")


def free_case(root, name, archive_args, prefix, groups_fn=None):
    src, files = free_src(root)
    vol = os.path.join(root, "vol-" + name)
    os.makedirs(vol)
    sevenzip("a", *archive_args, os.path.join(vol, prefix), ".", cwd=src)
    parts = set_parts(vol, prefix + ".")
    check(name, len(parts) >= 4, parts)
    keep = {os.path.basename(p): open(p, "rb").read() for p in parts}
    groups = groups_fn(parts[0]) if groups_fn else None
    tr = os.path.join(root, "trace-" + name)
    dest = os.path.join(root, "out-" + name)
    s = run(root, parts[0], dest, free=True, env={"XTEST_FREE_TRACE": tr})
    check(name, s["state"] == "done" and tree(dest) == tree(src), s)
    check(name, s["free_mode"] == "parts" and s["parts"] == len(parts) and s["parts_freed"] == len(parts), s)
    check(name, s["deleting"] and not s["checking"], s)
    check(name, s["bytes_freed"] == sum(len(b) for b in keep.values()), s)
    check(name, not any(os.path.exists(p) for p in parts) and not os.listdir(vol), os.listdir(vol))
    check(name, s["plan_known"] and s["same_drive"] and s["free_peak"] < s["unpacked"], s)
    # The trace holds the archive's own names; put the parts back to read where the files were.
    for n, b in keep.items():
        with open(os.path.join(vol, n), "wb") as f:
            f.write(b)
    rel_trace = [(w, a if w == "freed" else os.path.normpath(a)) for w, a in read_trace(tr)]
    check_freed_when_safe(name, parts, files, rel_trace, groups)
    return parts, files


def test_free_7z_split_each_file_a_block(root):
    if not SEVENZIP:
        return "skip"
    free_case(root, "7z non-solid", ["-mx=0", "-ms=off", "-v200k"], "s.7z")


def seven_blocks(first):
    out = subprocess.run([SEVENZIP, "l", "-slt", first], check=True, capture_output=True, text=True).stdout
    groups, path = {}, None
    for line in out.splitlines():
        if line.startswith("Path = "):
            path = line[7:]
        elif line.startswith("Block = ") and path:
            groups[os.path.normpath(path)] = line[8:]
    return groups


def test_free_7z_split_solid_blocks(root):
    if not SEVENZIP:
        return "skip"
    # Three files to a solid block: a part goes only when every block with a byte in it is out.
    free_case(root, "7z solid blocks", ["-m0=lzma2", "-mx=1", "-mmt=1", "-ms=3f", "-v200k"], "b.7z", seven_blocks)


def test_free_zip_and_tar_split(root):
    if not SEVENZIP:
        return "skip"
    free_case(root, "zip split", ["-tzip", "-mx=0", "-v200k"], "z.zip")
    free_case(root, "tar split", ["-ttar", "-v200k"], "t.tar")


def test_free_one_solid_block_goes_at_the_end(root):
    if not SEVENZIP:
        return "skip"
    src, files = free_src(root)
    vol = os.path.join(root, "vol")
    os.makedirs(vol)
    sevenzip("a", "-m0=lzma2", "-mx=1", "-mmt=1", "-ms=on", "-v200k", os.path.join(vol, "one.7z"), ".", cwd=src)
    parts = set_parts(vol, "one.7z.")
    tr = os.path.join(root, "trace")
    s = run(root, parts[0], os.path.join(root, "out"), free=True, env={"XTEST_FREE_TRACE": tr})
    check("one block", s["state"] == "done" and tree(os.path.join(root, "out")) == tree(src), s)
    t = read_trace(tr)
    first_freed = next(i for i, (w, _) in enumerate(t) if w == "freed")
    check("one block", all(w == "freed" for w, _ in t[first_freed:]) and sum(w == "file" for w, _ in t) == len(files), t)
    check("one block", not os.listdir(vol), os.listdir(vol))


def test_free_rar_sets(root):
    # Not solid: freed part by part, the result the same as a plain extraction.
    multi = [f"rar4-multi.part{i}.rar" for i in range(1, 7)]
    solid = [f"rar5-solid.part0{i}.rar" for i in range(1, 5)]
    copy_fixtures(root, multi + solid)
    plain = os.path.join(root, "plain4")
    s = run(root, os.path.join(root, multi[0]), plain)
    check("rar4 multi plain", s["state"] == "done", s)
    tr = os.path.join(root, "trace4")
    s = run(root, os.path.join(root, multi[0]), os.path.join(root, "free4"), free=True, env={"XTEST_FREE_TRACE": tr})
    check("rar4 multi free", s["state"] == "done" and tree(os.path.join(root, "free4")) == tree(plain), s)
    check("rar4 multi free", s["free_mode"] == "parts" and s["parts_freed"] == 6, s)
    check("rar4 multi free", not any(os.path.exists(os.path.join(root, n)) for n in multi), "parts left")
    t = read_trace(tr)
    nfiles = sum(w == "file" for w, _ in t)
    seen = 0
    early = 0
    for w, a in t:
        seen += w == "file"
        early += w == "freed" and seen < nfiles
    check("rar4 multi free", early >= 1, f"no part freed before the end: {t}")
    # Solid: one stream from the first file to the last, so every part stays until all are out.
    plain = os.path.join(root, "plain5")
    s = run(root, os.path.join(root, solid[0]), plain)
    tr = os.path.join(root, "trace5")
    s = run(root, os.path.join(root, solid[0]), os.path.join(root, "free5"), free=True, env={"XTEST_FREE_TRACE": tr})
    check("rar5 solid free", s["state"] == "done" and tree(os.path.join(root, "free5")) == tree(plain), s)
    check("rar5 solid free", s["free_mode"] == "end" and s["parts_freed"] == 4, s)
    t = read_trace(tr)
    first_freed = next((i for i, (w, _) in enumerate(t) if w == "freed"), len(t))
    check("rar5 solid free", all(w == "freed" for w, _ in t[first_freed:]) and first_freed > 0, t)


def test_free_single_archive_at_the_end(root):
    src = make_src(root)
    arc = os.path.join(root, "t.zip")
    with zipfile.ZipFile(arc, "w", zipfile.ZIP_DEFLATED) as z:
        for dp, dn, fn in os.walk(src):
            for n in fn:
                p = os.path.join(dp, n)
                z.write(p, os.path.relpath(p, src))
    keep = open(arc, "rb").read()
    tr = os.path.join(root, "trace")
    s = run(root, arc, os.path.join(root, "o"), free=True, env={"XTEST_FREE_TRACE": tr})
    check("zip end", s["state"] == "done" and tree(os.path.join(root, "o")) == tree(src), s)
    check("zip end", s["free_mode"] == "end" and s["parts_freed"] == 1 and not os.path.exists(arc), s)
    check("zip end", read_trace(tr)[-1] == ("freed", "t.zip") and len(read_trace(tr)) == 4, read_trace(tr))
    # Damaged: it fails, and the archive stays as it was.
    bad = bytearray(keep)
    at = bad.find(b"deep deep")
    if at < 0:
        at = len(bad) // 2
    bad[at + 40] ^= 0xFF
    with open(arc, "wb") as f:
        f.write(bad)
    s = run(root, arc, os.path.join(root, "o2"), free=True)
    check("zip end damaged", s["state"] == "failed" and s["parts_freed"] == 0 and os.path.exists(arc), s)


def make_set(root, name="s.7z", n=8, size=150000, vol="200k"):
    src, files = free_src(root, n, size)
    folder = os.path.join(root, "vol")
    os.makedirs(folder, exist_ok=True)
    sevenzip("a", "-mx=0", "-ms=off", "-v" + vol, os.path.join(folder, name), ".", cwd=src)
    return src, files, set_parts(folder, name + ".")


def test_free_damage_found_before_anything_goes(root):
    if not SEVENZIP:
        return "skip"
    src, files, parts = make_set(root)
    last = parts[-2]
    good = open(last, "rb").read()
    bad = bytearray(good)
    bad[len(bad) // 2] ^= 0xFF
    with open(last, "wb") as f:
        f.write(bad)
    s = run(root, parts[0], os.path.join(root, "o"), free=True)
    check("damage", s["state"] == "failed" and s["parts_freed"] == 0 and not s["deleting"], s)
    check("damage", all(os.path.exists(p) for p in parts) and not os.path.exists(os.path.join(root, "o")), "touched")
    check("damage", not any(n.endswith(".partial-deleted") for n in os.listdir(os.path.dirname(parts[0]))), "renamed")
    # Whole again (the damaged part fetched again): the same job simply runs.
    with open(last, "wb") as f:
        f.write(good)
    s = run(root, parts[0], os.path.join(root, "o"), free=True)
    check("damage then whole", s["state"] == "done" and tree(os.path.join(root, "o")) == tree(src), s)


def stop_midway(root, env, cancel):
    """A set freed as it goes, stopped (a Stop, or a write that fails) once a part is gone."""
    src, files, parts = make_set(root, n=8, size=300000, vol="300k")
    dest = os.path.join(root, "o")
    p, st, cn = run(root, parts[0], dest, wait=False, free=True, env=env)
    deadline = time.time() + 60
    cur = {}
    while cancel and time.time() < deadline:
        try:
            with open(st) as f:
                cur = json.load(f)
            if cur.get("parts_freed", 0) >= 1:
                break
        except (OSError, ValueError):
            pass
        time.sleep(0.01)
    if cancel:
        open(cn, "w").close()
    p.wait(timeout=60)
    with open(st) as f:
        s = json.load(f)
    return s, src, files, parts, dest


def check_stopped(name, s, parts, dest, files):
    folder = os.path.dirname(parts[0])
    left = sorted(os.listdir(folder))
    check(name, s["state"] == "failed" and s["parts_freed"] >= 1, s)
    # Nothing more went after it stopped: what was reported gone is gone, the rest renamed, kept.
    check(name, s["parts_freed"] + s["parts_left"] == len(parts), s)
    check(name, len(left) == s["parts_left"] and all(n.endswith(".partial-deleted") for n in left), left)
    check(name, s["left_first"] == left[0], (s["left_first"], left))
    # What came out before it stopped is kept, whole.
    out = tree(dest)
    check(name, out and all(out[rel] == files[os.path.normpath(rel)] for rel in out), sorted(out))


def test_free_cancel_midway(root):
    if not SEVENZIP:
        return "skip"
    s, src, files, parts, dest = stop_midway(root, {"XTEST_WRITE_DELAY_US": "100000"}, cancel=True)
    check("free cancel", s["message"] == "fs_extract_cancelled", s)
    check_stopped("free cancel", s, parts, dest, files)
    # It can't go on from here (its first part is gone), and trying again changes nothing.
    left = sorted(os.listdir(os.path.dirname(parts[0])))
    s = run(root, parts[0], os.path.join(root, "again"), free=True)
    check("free cancel, again", s["state"] == "failed" and sorted(os.listdir(os.path.dirname(parts[0]))) == left, s)


def test_free_failure_midway(root):
    if not SEVENZIP:
        return "skip"
    # The drive fails a write half way through.
    s, src, files, parts, dest = stop_midway(root, {"XTEST_WRITE_LIMIT": str(8 * 300000 // 2)}, cancel=False)
    check("free failure", s["message"].startswith("fs_extract_no_space"), s)
    check_stopped("free failure", s, parts, dest, files)


def test_free_room_counts_the_parts(root):
    if not SEVENZIP:
        return "skip"
    src, files, parts = make_set(root)
    total = sum(len(b) for b in files.values())
    margin = 64 << 20
    # The plan a listing shows, from the archive's listing alone.
    s = run(root, parts[0], os.path.join(root, "x"), list_only=True, free=True)
    check("free plan", s["state"] == "done" and s["free_mode"] == "parts" and s["parts"] == len(parts), s)
    check("free plan", s["unpacked"] == total and s["plan_known"] and s["free_peak"] < total, s)
    plan_peak = s["free_peak"]
    # A drive with room for the unpack only with the parts given back on the way.
    room = margin + (plan_peak + total) // 2
    s = run(root, parts[0], os.path.join(root, "o1"), env={"XTEST_FREE": str(room)})
    check("room, kept", s["state"] == "failed" and s["message"].startswith("fs_extract_no_space"), s)
    s = run(root, parts[0], os.path.join(root, "o2"), free=True, env={"XTEST_FREE": str(room)})
    check("room, freed", s["state"] == "done" and tree(os.path.join(root, "o2")) == tree(src), s)
    check("room, freed", s["free_peak"] <= plan_peak + 512 * len(files), (s["free_peak"], plan_peak))
    # Too little even so: refused before anything is deleted.
    src2, files2, parts2 = make_set(root, name="t.7z")
    s = run(root, parts2[0], os.path.join(root, "o3"), free=True, env={"XTEST_FREE": str(margin + plan_peak // 4)})
    check("room, too little", s["state"] == "failed" and s["message"].startswith("fs_extract_no_space"), s)
    check("room, too little", s["parts_freed"] == 0 and all(os.path.exists(p) for p in parts2), s)


def test_free_plan_for_what_goes_at_the_end(root):
    copy_fixtures(root, [f"rar5-solid.part0{i}.rar" for i in range(1, 5)] + [f"rar4-multi.part{i}.rar" for i in range(1, 7)])
    s = run(root, os.path.join(root, "rar5-solid.part01.rar"), os.path.join(root, "x"), list_only=True, free=True)
    check("plan solid rar", s["state"] == "done" and s["free_mode"] == "end" and s["parts"] == 4, s)
    s = run(root, os.path.join(root, "rar4-multi.part1.rar"), os.path.join(root, "x"), list_only=True, free=True)
    check("plan rar", s["state"] == "done" and s["free_mode"] == "parts" and s["parts"] == 6 and s["plan_known"], s)
    check("plan rar", s["free_peak"] < s["unpacked"] + s["archive_bytes"], s)
    arc = os.path.join(root, "one.zip")
    with zipfile.ZipFile(arc, "w") as z:
        z.writestr("a.txt", "x" * 1000)
    s = run(root, arc, os.path.join(root, "x"), list_only=True, free=True)
    check("plan zip", s["free_mode"] == "end" and s["free_peak"] == 1000 and s["plan_known"], s)
    # A second name (a hard link) would be damaged with it: refused, and said why in the plan.
    os.link(arc, os.path.join(root, "other-name.zip"))
    s = run(root, arc, os.path.join(root, "x"), list_only=True, free=True)
    check("plan hard link", s["state"] == "done" and s["free_why"].startswith("fs_extract_free_refused"), s)
    s = run(root, arc, os.path.join(root, "o"), free=True)
    check("hard link", s["state"] == "failed" and s["message"].startswith("fs_extract_free_refused"), s)
    check("hard link", os.path.exists(arc) and not os.path.exists(os.path.join(root, "o")), s)

def test_free_never_with_items_left_out(root):
    # "Choose items" leaves some out: freeing would delete them with the archive, unseen.
    arc = os.path.join(root, "r.zip")
    with zipfile.ZipFile(arc, "w") as z:
        z.writestr("Game/eboot.bin", b"eboot")
        z.writestr("extra.txt", b"extra")
    s = run(root, arc, os.path.join(root, "o"), free=True, top="extra.txt//")
    check("free, left out", s["state"] == "failed" and s["message"].startswith("fs_extract_free_refused"), s)
    check("free, left out", os.path.exists(arc) and not os.path.exists(os.path.join(root, "o")), s)
    # Renamed is fine: everything still comes out.
    s = run(root, arc, os.path.join(root, "o2"), free=True, top="extra.txt/extra (2).txt/")
    check("free, renamed", s["state"] == "done" and not os.path.exists(arc), s)
    check("free, renamed", tree(os.path.join(root, "o2")).get("extra (2).txt") == b"extra", s)


def main():
    if not os.path.exists(BIN):
        print(f"missing {BIN}: run make host")
        return 2
    for name, fn in list(globals().items()):
        if name.startswith("test_") and callable(fn):
            case(fn)
    print(f"\n{passed} passed, {len(failures)} failed, {len(skipped)} skipped" + (f" ({', '.join(skipped)}: no 7zz/7z)" if skipped else ""))
    for f in failures:
        print("  " + f)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
