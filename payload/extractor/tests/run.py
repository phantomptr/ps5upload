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


def run(root, archive, dest, password=None, merge=False, replace=False, env=None, cancel=False, wait=True):
    req = {"id": "t1", "archive": archive, "dest": dest, "merge": merge, "replace": replace}
    if password is not None:
        req["password"] = password
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


def test_rar_encrypted(root):
    copy_fixtures(root, ["rar4-encrypted.rar", "rar5-encrypted.rar"])
    for n in ("rar4-encrypted.rar", "rar5-encrypted.rar"):
        s = run(root, os.path.join(root, n), os.path.join(root, "o-" + n), password="password")
        check(n, s["message"] == "fs_extract_rar_encrypted", s)


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
    # The drive fills up while writing.
    s = run(root, arc, os.path.join(root, "o2"), env={"XTEST_WRITE_LIMIT": "100000"})
    check("space mid-write", s["message"].startswith("fs_extract_no_space"), s)
    check("space mid-write", "b.bin" not in str(tree(os.path.join(root, "o2"))), "a partial file was kept")


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
