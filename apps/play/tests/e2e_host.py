#!/usr/bin/env python3
# PS5Upload Play - the app's guest and companion code (play-cli) against the stand-in engine.
# Usage: e2e_host.py PLAY_CLI WORKDIR
# SPDX-License-Identifier: GPL-3.0-or-later
import json
import os
import subprocess
import sys
import time

here = os.path.dirname(os.path.abspath(__file__))
cli, work = sys.argv[1], sys.argv[2]
os.makedirs(work, exist_ok=True)
failures = 0


def check(cond, what):
    global failures
    print(("ok   " if cond else "FAIL ") + what)
    if not cond:
        failures += 1


def engine(name, *extra):
    report = os.path.join(work, name + ".json")
    port_file = os.path.join(work, name + ".port")
    for f in (report, port_file):
        if os.path.exists(f):
            os.remove(f)
    p = subprocess.Popen([sys.executable, "-I", os.path.join(here, "fake_engine.py"), "--report", report,
                          "--port-file", port_file, *extra])
    for _ in range(100):
        if os.path.exists(port_file) and open(port_file).read():
            break
        time.sleep(0.05)
    return p, int(open(port_file).read()), report


def finish(p, report):
    time.sleep(0.5)
    p.terminate()
    p.wait()
    return json.load(open(report))


def run(*args, timeout=30):
    out = subprocess.run([cli, *args], capture_output=True, text=True, timeout=timeout)
    return out.stdout + out.stderr


# 1. A guest joins by code, gets pictures and sound, sends pads with raw bits, leaves.
p, port, rep = engine("guest", "--seconds", "6")
out = run("join", "127.0.0.1", str(port), "abc-def", "--seconds", "3")
r = finish(p, rep)
check("phase joined" in out and "phase ended left" in out, "guest joins and leaves")
h = r["hellos"][0] if r["hellos"] else {}
check(h.get("code") == "abc-def" and h.get("name") == "play-cli", "hello carries the code and name")
caps = h.get("client", {}).get("caps", {})
check(h.get("client", {}).get("kind") == "ps5" and h.get("client", {}).get("app") == "play", "hello says ps5/play")
check(caps == {"hevc": True, "main10": False, "raw_pad": True, "timing": True}, "hello caps")
res = dict(kv.split("=") for kv in out.split("result ", 1)[1].split("\n")[0].split()) if "result " in out else {}
check(int(res.get("pictures", 0)) > 100 and int(res.get("keyframes", 0)) >= 2, "pictures arrive (timed video)")
check(int(res.get("sounds", 0)) > 200, "sound frames arrive")
check(res.get("rumbles") == "1" and res.get("lights") == "1", "rumble and light reach the sinks")
check(r["bad_pads"] == 0 and len(r["pads"]) > 50, "pad frames parse as the engine reads them")
crosses = [x for x in r["pads"] if x["buttons"] == 0x4000]
check(len(crosses) > 0, "raw libScePad bits are sent to a 13.0.0 engine (cross 0x4000)")
check(r["leaves"] == 1, "leave is said")
check(r["pings"] >= 1 and any(s.get("decoder") == "native" for s in r["stats"]), "ping and stats (decoder native)")
check(r["keyframes"] >= 1, "keyframe asked before the first keyframe")
check(int(res.get("delivery_ms", -1)) >= 0, "engine time lines up (delivery_ms)")

# 2. An engine before 13.0.0: the apps' bits (cross = 1).
p, port, rep = engine("old", "--old", "--seconds", "4")
out = run("join", "127.0.0.1", str(port), "--seconds", "2")
r = finish(p, rep)
check(r["hellos"] and "code" not in r["hellos"][0], "a nearby join sends no code")
check(any(x["buttons"] == 1 for x in r["pads"]) and not any(x["buttons"] == 0x4000 for x in r["pads"]),
      "an old engine gets the apps' bits")

# 3. Refused.
p, port, rep = engine("refused", "--refuse", "denied")
out = run("join", "127.0.0.1", str(port), "ABCDEF", "--seconds", "3")
finish(p, rep)
check("phase refused denied" in out, "a refusal reaches the app")

# 4. The socket drops: the app comes back with its token and plays on.
p, port, rep = engine("resume", "--seconds", "8", "--drop-after", "1")
out = run("join", "127.0.0.1", str(port), "ABCDEF", "--seconds", "4")
r = finish(p, rep)
resumes = [x for x in r["hellos"] if x.get("resume") == "tok1"]
check("guest: phase 5" in out and len(resumes) == 1, "resume with the token after a drop")
check(out.count("joined as player 2") == 2, "joined again after the drop")

# 5. The host says bye.
p, port, rep = engine("bye", "--seconds", "1.5", "--bye", "kicked")
out = run("join", "127.0.0.1", str(port), "ABCDEF", "--seconds", "5")
finish(p, rep)
check("phase ended kicked" in out, "bye ends the session")

# 6. Nothing listening.
out = run("join", "127.0.0.1", "9", "ABCDEF", "--seconds", "3")
check("phase failed unreachable" in out, "an engine that is not there")

# 7. Companion: pair, then the status, an invite and an answer to an ask.
p, port, rep = engine("app", "--lifetime", "15")
env_file = os.path.join(work, "token.txt")
os.environ["PLAY_CLI_TOKEN_FILE"] = env_file
out = run("companion", "127.0.0.1", str(port), "--pair", "--seconds", "3", "--invite", "--allow-first")
r = finish(p, rep)
check("phase unpaired" in out and "pair_code PQRSTU" in out and "phase paired" in out, "pairing by code")
check(open(env_file).read() == "apptok", "the token is handed over to be kept")
check("invite=ABC-DEF" in out and "asks=1" in out and "rp=1" in out, "the status document is read")
kinds = [next(k for k in i if k != "req") for i in r["intents"]]
check("app" in kinds and "invite" in kinds and "decide" in kinds, "intents sent: app, invite, decide")
dec = [i for i in r["intents"] if "decide" in i]
check(dec and dec[0]["decide"] == {"ask": 7, "allow": True}, "allow answers the ask by id")
p, port, rep = engine("app2", "--lifetime", "10")
out = run("companion", "127.0.0.1", str(port), "apptok", "--seconds", "1.5")
r = finish(p, rep)
check(r["app_hellos"][0].get("token") == "apptok" and "phase paired" in out, "a kept token goes straight in")
p, port, rep = engine("app3", "--lifetime", "10")
out = run("companion", "127.0.0.1", str(port), "wrong", "--seconds", "1.5")
finish(p, rep)
check("phase unpaired  invalid" in out and "(cleared)" in out, "a token that stopped working is dropped")

# 8. The helper's app API on 127.0.0.1 (stand-in): engines, Remote Play, PIN, Tailscale forward.
p, port, rep = engine("helper", "--lifetime", "10")
out = run("helper", str(port))
r = finish(p, rep)
check("mesh 2" in out and "engine Living room PC 192.168.1.20:19113 tailnet=0" in out
      and "engine Laptop 100.101.102.103:0 tailnet=1" in out, "mesh: engines only, the console left out")
check("rp 1 ready=0 service_on=0 needs_user=1" in out and "enable 1 ready=1" in out, "Remote Play readiness, turned on")
enable = [i["helper"] for i in r["intents"] if "helper" in i and i["helper"][1] == "/v1/rp/enable"]
check(enable and enable[0][2] == {"confirm": True}, "turning Remote Play on says confirm")
check("pin 1 waiting 12345678 300" in out and "pin_get 1 waiting" in out and "pin_cancel 1" in out, "Remote Play PIN")
check("forward 1 9182" in out and "forward_lan 0 not_tailnet_address" in out, "Tailscale forward and its refusal")
check("absent 0" in out, "no helper: calls fail quietly")

# 9. Ways (docs/ROUTES.md). The stand-in's other ways in (--proxy) add a round trip, or stop
# passing anything (a black hole) from some second on.
def ways(name, *extra):
    p, port, rep = engine(name, *extra)
    return p, port, rep, open(os.path.join(work, name + ".port.ways")).read().split()


def route_line(out):
    line = out.split("route way=", 1)[1].split("\n")[0] if "route way=" in out else ""
    return dict(kv.split("=") for kv in ("way=" + line).split()) if line else {}


# 9a. At connect the better tier wins over the shorter kind: the home network at 150 ms is Poor,
# Tailscale at ~0 ms Excellent. The other way stays a backup, checked gently, no bursts in play.
p, port, rep, (slow,) = ways("ways_pick", "--seconds", "12", "--lifetime", "14", "--proxy", "150")
out = run("join", "127.0.0.1", str(port), "ABCDEF", "--seconds", "8", "--way", f"lan:{slow}",
          "--way", f"tailscale:{port}")
r = finish(p, rep)
rl = route_line(out)
check("picked tailscale" in out and rl.get("way") == "tailscale", "the better tier wins at connect")
st = [s for s in r["stats"] if "route" in s]
check(st and st[-1]["route"] == "tailscale" and st[-1]["tier"] == "excellent"
      and st[-1].get("backups") == 1 and st[-1].get("backup_tier") == "poor", "the host hears the way, tier, backup")
check(len(r["probe_checks"]) <= 10 + 8 // 2 + 2, f"backups checked gently ({len(r['probe_checks'])} checks)")
check(all(b < 2 for b in r["bursts"]) and len(r["bursts"]) <= 2, "bursts only at connect")

# 9b. The way in use goes dark mid-game: the guest joins again on the backup with its token
# beside the old socket, the stand-in hands it the seat, and pictures go on.
p, port, rep, (dark,) = ways("ways_lost", "--seconds", "14", "--lifetime", "16", "--proxy", "0:4")
out = run("join", "127.0.0.1", str(port), "ABCDEF", "--seconds", "10", "--way", f"lan:{dark}",
          "--way", f"tailscale:{port}")
r = finish(p, rep)
rl = route_line(out)
check("picked lan" in out, "both Excellent: the home network first")
check(rl.get("way") == "tailscale" and rl.get("switches") == "1" and rl.get("why") == "lost",
      "a dark way is left for the backup")
check(r["replaced"] == 1 and [("resume" in h) for h in r["hellos"]] == [False, True], "resumed beside the old socket")
check(out.count("joined as player 2") == 2 and "phase reconnecting" not in out, "no reconnect screen on a change of way")
res = dict(kv.split("=") for kv in out.split("result ", 1)[1].split("\n")[0].split()) if "result " in out else {}
check(int(res.get("pictures", 0)) > 300, "pictures go on after the change")
check(any(s.get("route") == "tailscale" and s.get("route_switches") == 1 for s in r["stats"]), "the host hears the change")

# 9c. Nothing direct answers: through this home's PS5Upload (the bridge) with the invite's room
# and certificate; a bridge that fails says why.
room, fp = "AAAAAAAAAAAAAAAAAAAAAh", "AB:CD:EF"
p, port, rep, (dark,) = ways("ways_bridge", "--seconds", "6", "--lifetime", "9", "--proxy", "0:0.001")
out = run("join", "127.0.0.1", str(port), "ABCDEF", "--seconds", "3", "--way", f"lan:{dark}",
          "--bridge", f"{port}:{room}:{fp}")
r = finish(p, rep)
check(route_line(out).get("way") == "internet" and "phase joined" in out, "the bridge when nothing direct answers")
check(r["bridges"] == [{"v": 1, "room": room, "fp": fp}], "the bridge is asked for the invite's room and certificate")
p, port, rep, (dark,) = ways("ways_bridge_fail", "--lifetime", "8", "--proxy", "0:0.001", "--bridge-fail", "no_direct")
out = run("join", "127.0.0.1", str(port), "ABCDEF", "--seconds", "3", "--way", f"lan:{dark}",
          "--bridge", f"{port}:{room}:{fp}")
finish(p, rep)
check("failed (no_direct)" in out and "phase failed" in out, "a failed bridge ends the try with its reason")

# 9d. An engine without the checks socket: timed by its answer, joined all the same.
p, port, rep = engine("ways_old", "--seconds", "4", "--no-probe")
out = run("join", "127.0.0.1", str(port), "ABCDEF", "--seconds", "2", "--way", f"lan:{port}")
r = finish(p, rep)
check(route_line(out).get("way") == "lan" and "phase joined" in out and r["probe_sockets"] == 0,
      "an engine without the checks socket still joins")

print("e2e:", "all passed" if not failures else f"{failures} failed")
sys.exit(1 if failures else 0)
