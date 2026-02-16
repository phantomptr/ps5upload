#!/usr/bin/env python3

import argparse
import ftplib
import os
import socket
import sys
import time
import shutil

DEFAULT_PORTS = [2121, 1337]
REMOTE_CANDIDATES = [
    "/data/ps5upload/debug",
    "/ps5upload/debug",
]


def check_port(ip, port, timeout=1.5):
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.settimeout(timeout)
    try:
        sock.connect((ip, port))
        return True
    except OSError:
        return False
    finally:
        sock.close()


def pick_port(ip):
    for port in DEFAULT_PORTS:
        if check_port(ip, port):
            return port
    return None


def ftp_connect(ip, port, user, password):
    ftp = ftplib.FTP()
    ftp.connect(ip, port, timeout=5)
    ftp.login(user, password)
    return ftp


def find_remote_root(ftp):
    original_pwd = "/"
    try:
        original_pwd = ftp.pwd()
    except ftplib.all_errors:
        pass

    for candidate in REMOTE_CANDIDATES:
        try:
            ftp.cwd(candidate)
            return candidate
        except ftplib.all_errors:
            continue

    # Auto-discovery (non-recursive): walk a small set of base locations.
    search_bases = ["/", "/data", "/mnt", "/user", "/system_data"]
    rel_candidates = [
        "ps5upload/debug",
        "data/ps5upload/debug",
        "debug",
    ]

    visited = set()
    for base in search_bases:
        if base in visited:
            continue
        visited.add(base)
        try:
            ftp.cwd(base)
        except ftplib.all_errors:
            continue
        for rel in rel_candidates:
            try:
                ftp.cwd(rel)
                return ftp.pwd()
            except ftplib.all_errors:
                try:
                    ftp.cwd(base)
                except ftplib.all_errors:
                    break

    # Last-resort probing from current root listing.
    try:
        ftp.cwd("/")
    except ftplib.all_errors:
        pass

    entries = list_dir(ftp)
    names = [name for name, _ in entries]
    for top in ("data", "ps5upload"):
        if top not in names:
            continue
        try:
            ftp.cwd(f"/{top}")
            child_names = [n for n, _ in list_dir(ftp)]
            if "ps5upload" in child_names:
                ftp.cwd("ps5upload")
                child_names = [n for n, _ in list_dir(ftp)]
            if "debug" in child_names:
                ftp.cwd("debug")
                return ftp.pwd()
        except ftplib.all_errors:
            continue
        finally:
            try:
                ftp.cwd("/")
            except ftplib.all_errors:
                pass

    try:
        ftp.cwd(original_pwd)
    except ftplib.all_errors:
        pass

    return None


def ensure_dir(path):
    os.makedirs(path, exist_ok=True)


def list_dir(ftp):
    try:
        return list(ftp.mlsd())
    except ftplib.all_errors:
        pass
    try:
        names = ftp.nlst()
        return [(name, {}) for name in names]
    except ftplib.all_errors:
        pass
    lines = []
    try:
        ftp.retrlines("LIST", lines.append)
    except ftplib.all_errors:
        return []
    entries = []
    for line in lines:
        parts = line.split(maxsplit=8)
        if len(parts) < 9:
            continue
        name = parts[8]
        entry_type = "dir" if parts[0].startswith("d") else "file"
        entries.append((name, {"type": entry_type}))
    return entries


def is_dir(ftp, name):
    cur = ftp.pwd()
    try:
        ftp.cwd(name)
        ftp.cwd(cur)
        return True
    except ftplib.error_perm:
        return False


def download_file(ftp, remote_name, local_path, verbose=False):
    if verbose:
        print(f"RETR {remote_name} -> {local_path}")
    with open(local_path, "wb") as fp:
        ftp.retrbinary(f"RETR {remote_name}", fp.write)


def download_dir(ftp, remote_dir, local_dir, verbose=False):
    ensure_dir(local_dir)
    parent_pwd = ftp.pwd()
    ftp.cwd(remote_dir)
    current_pwd = ftp.pwd()
    if verbose:
        print(f"CWD {remote_dir} (pwd={current_pwd})")
    entries = list_dir(ftp)
    for name, facts in entries:
        if name in (".", ".."):
            continue
        entry_type = facts.get("type") if facts else None
        if entry_type == "dir":
            download_dir(ftp, name, os.path.join(local_dir, name), verbose=verbose)
            ftp.cwd(current_pwd)
            continue
        if entry_type is None:
            if is_dir(ftp, name):
                download_dir(ftp, name, os.path.join(local_dir, name), verbose=verbose)
                ftp.cwd(current_pwd)
                continue
        try:
            download_file(ftp, name, os.path.join(local_dir, name), verbose=verbose)
        except ftplib.error_perm:
            # Some FTP servers return 550 for files mis-typed as dirs in LIST/MLSD.
            if is_dir(ftp, name):
                download_dir(ftp, name, os.path.join(local_dir, name), verbose=verbose)
                ftp.cwd(current_pwd)
                continue
            raise
    ftp.cwd(parent_pwd)


def main():
    parser = argparse.ArgumentParser(description="Pull PS5 Upload debug bundle via FTP")
    parser.add_argument("ip", help="PS5 IP address")
    parser.add_argument("--port", type=int, help="FTP port (overrides auto-detect)")
    parser.add_argument("--user", default="anonymous", help="FTP username (default: anonymous)")
    parser.add_argument("--password", default="", help="FTP password")
    parser.add_argument("--dest", default="debug", help="Local destination folder")
    parser.add_argument("--remote", help="Remote debug directory (overrides auto-discovery)")
    parser.add_argument("--list", action="store_true", help="List remote debug directory and exit")
    parser.add_argument("--verbose", action="store_true", help="Verbose FTP logging")
    parser.add_argument("--clean-local", action="store_true", help="Delete local dest folder before download")
    args = parser.parse_args()

    port = args.port or pick_port(args.ip)
    if port is None:
        print("No FTP port open on 2121 or 1337.")
        return 1

    try:
        ftp = ftp_connect(args.ip, port, args.user, args.password)
    except ftplib.all_errors as exc:
        print(f"FTP connect failed: {exc}")
        return 1

    if args.verbose:
        print(f"Connected to {args.ip}:{port}, pwd={ftp.pwd()}")

    remote_root = args.remote or find_remote_root(ftp)
    if remote_root is None:
        print("Could not find /data/ps5upload/debug on FTP server.")
        try:
            print("Root listing:", ftp.nlst())
        except ftplib.all_errors:
            pass
        ftp.quit()
        return 1
    if args.verbose:
        print(f"Remote root: {remote_root}")
    if args.list:
        try:
            ftp.cwd(remote_root)
            print("Remote listing:", [name for name, _ in list_dir(ftp)])
        except ftplib.all_errors as exc:
            print(f"List failed: {exc}")
            ftp.quit()
            return 1
        ftp.quit()
        return 0

    timestamp = time.strftime("%Y%m%d_%H%M%S")
    local_root = os.path.join(args.dest, f"{args.ip}_{timestamp}")
    if args.clean_local and os.path.exists(args.dest):
        shutil.rmtree(args.dest, ignore_errors=True)
    ensure_dir(local_root)

    try:
        download_dir(ftp, remote_root, local_root, verbose=args.verbose)
    except ftplib.all_errors as exc:
        print(f"Download failed: {exc}")
        ftp.quit()
        return 1

    ftp.quit()
    print(f"Debug bundle saved to {local_root}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
