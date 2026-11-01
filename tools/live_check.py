#!/usr/bin/env python3
"""stvtf against a real TF2 server with bots, for what recordings cannot show. Not part of `cargo test`.

Starts a throwaway server in Docker on its private bridge network, runs every check against it and
removes it. Build first (`cargo build --release`), then from the repository's root:

    tools/live_check.py                    # the image's TF2 build
    tools/live_check.py --update           # the current build, through steamcmd (some minutes)

The image needs TF2 in /home/tf2/hlserver/tf2, a map `ctf_turbine` and bash; its own entrypoint is
not run, and SourceMod is moved aside. Exits non-zero if a check fails.
"""
import argparse
import collections
import json
import os
import re
import secrets
import select
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time

NAME = "stvtf-live-check"
GAME_PORT, TV_PORT = 27915, 27920
RELAY_PORT, LOSSY_PORT = 27930, 27931

results = []


def check(what, ok, detail=""):
    results.append(bool(ok))
    print(f"{'PASS' if ok else 'FAIL'}  {what}{'  ' + str(detail) if detail else ''}", flush=True)


class Server:
    """The container, and rcon into it."""

    def __init__(self, image, update):
        self.password = secrets.token_hex(12)
        subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)
        flags = (
            f"-game tf -ip 0.0.0.0 -port {GAME_PORT} +tv_port {TV_PORT} +clientport 27905 +map ctf_turbine "
            f"+maxplayers 24 +sv_lan 1 -nomaster +rcon_password {self.password} +tv_enable 1 +tv_delay 0 "
            "+tv_maxrate 0 +tv_maxclients 8 +tf_bot_quota 8 +tf_bot_quota_mode normal "
            "+mp_autoteambalance 0 +sv_pure 0 +mp_waitingforplayers_cancel 1"
        )
        steps = ["cd /home/tf2/hlserver/tf2"]
        if update:
            # It often needs another go; without the update there is nothing to check.
            install = "/usr/games/steamcmd +force_install_dir /home/tf2/hlserver/tf2 +login anonymous +app_update 232250 +quit"
            steps.append(
                f"for i in 1 2 3 4; do {install} > /tmp/steamcmd.log 2>&1; grep -q 'Success! App' /tmp/steamcmd.log && break; "
                "[ $i = 4 ] && { tail -5 /tmp/steamcmd.log; exit 1; }; done"
            )
        steps += ["mv tf/addons tf/addons.off", "rm -f tf/cfg/server.cfg", f"while true; do ./srcds_run {flags}; sleep 1; done"]
        subprocess.run(
            ["docker", "run", "-d", "--rm", "--name", NAME, "--entrypoint", "bash", image, "-c", "; ".join(steps)],
            check=True,
            capture_output=True,
        )
        self.ip = subprocess.run(
            ["docker", "inspect", NAME, "--format", "{{.NetworkSettings.IPAddress}}"], check=True, capture_output=True, text=True
        ).stdout.strip()
        self.tv = f"{self.ip}:{TV_PORT}"

    def remove(self):
        subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)

    def a2s(self):
        """The build SourceTV says it is, or None."""
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.settimeout(1)
        query = b"\xff\xff\xff\xffTSource Engine Query\0"
        try:
            s.sendto(query, (self.ip, TV_PORT))
            d = s.recv(2000)
            if d[4:5] == b"A":
                s.sendto(query + d[5:9], (self.ip, TV_PORT))
                d = s.recv(2000)
        except OSError:
            return None
        # Name, map, folder, game, then 9 bytes: app id, players, slots, bots, type, OS, password, VAC.
        strings = d[6:].split(b"\0", 4)
        return strings[4][9:].split(b"\0")[0].decode() if len(strings) == 5 else None

    def running(self):
        ps = subprocess.run(["docker", "ps", "-q", "--filter", f"name=^{NAME}$"], capture_output=True, text=True)
        return bool(ps.stdout.strip())

    def wait_ready(self, timeout):
        end = time.time() + timeout
        while time.time() < end and self.running():
            build = self.a2s()
            if build and "players : " in self.rcon("status"):
                return build
            time.sleep(2)
        sys.exit("the server never came up:\n" + subprocess.run(["docker", "logs", "--tail", "20", NAME], capture_output=True, text=True).stdout)

    def rcon(self, *commands):
        out = ""
        try:
            with socket.create_connection((self.ip, GAME_PORT), timeout=5) as s:

                def send(i, kind, body):
                    d = struct.pack("<ii", i, kind) + body.encode() + b"\0\0"
                    s.sendall(struct.pack("<i", len(d)) + d)

                def read(n):
                    d = b""
                    while len(d) < n:
                        c = s.recv(n - len(d))
                        if not c:
                            raise OSError("closed")
                        d += c
                    return d

                def packet():
                    d = read(struct.unpack("<i", read(4))[0])
                    return *struct.unpack("<ii", d[:8]), d[8:-2].decode(errors="replace")

                send(1, 3, self.password)
                while packet()[1] != 2:
                    pass
                s.settimeout(1)
                for command in commands:
                    send(2, 2, command)
                    try:
                        while True:
                            out += packet()[2]
                    except socket.timeout:
                        pass
        except OSError:
            pass
        return out


class Lossy(threading.Thread):
    """UDP between a client and a port here; `lose` drops that many packets on their way to the client."""

    def __init__(self, to, port):
        super().__init__(daemon=True)
        self.front = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.front.bind(("127.0.0.1", port))
        self.back = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.back.connect(("127.0.0.1", to))
        self.client, self.lose, self.dropped = None, 0, 0
        self.start()

    def run(self):
        while True:
            for s in select.select([self.front, self.back], [], [], 0.5)[0]:
                if s is self.front:
                    d, self.client = s.recvfrom(65535)
                    self.back.send(d)
                else:
                    d = s.recv(65535)
                    if self.lose > 0:
                        self.lose -= 1
                        self.dropped += 1
                    elif self.client:
                        self.front.sendto(d, self.client)


class Proc:
    """A running stvtf, whose lines are kept with the time each came."""

    def __init__(self, *args, env=None):
        self.lines, self.err = [], []
        self.p = subprocess.Popen([BIN, *args], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env={**os.environ, **(env or {})})
        threading.Thread(target=lambda: [self.lines.append((time.time(), json.loads(l))) for l in self.p.stdout], daemon=True).start()
        threading.Thread(target=lambda: [self.err.append((time.time(), l.rstrip())) for l in self.p.stderr], daemon=True).start()

    def wait_line(self, want, timeout=45, since=0):
        end = time.time() + timeout
        while time.time() < end:
            for t, v in list(self.lines):
                if t >= since and want(v):
                    return t, v
            time.sleep(0.02)
        return None

    def stop(self):
        self.p.terminate()
        try:
            self.p.wait(10)
        except subprocess.TimeoutExpired:
            self.p.kill()
        time.sleep(0.1)

    def goodbyes(self):
        return [v for _, v in self.lines if v.get("state") == "disconnected"]

    def said(self, pattern, since=0):
        return [(round(t - since, 2), l) for t, l in self.err if t >= since and re.search(pattern, l)]


def run(*args, timeout=120):
    p = subprocess.run([BIN, *args], capture_output=True, text=True, timeout=timeout)
    return p.returncode, [json.loads(l) for l in p.stdout.splitlines()], p.stderr.strip()


def states(lines):
    return [l["state"] for l in lines if l["type"] == "status"]


def has_players(v):
    return v["type"] == "snapshot" and v["players"]


def longest_gap(c, since):
    ts = [t for t, v in c.lines if v["type"] == "snapshot" and t >= since]
    return round(max((b - a for a, b in zip(ts, ts[1:])), default=99), 2)


def direct(server):
    code, lines, err = run("connect", server.tv, "--hz", "5", "--seconds", "12")
    connecting = next((l for l in lines if l.get("state") == "connecting"), {})
    check("offers the build the server reports", connecting.get("build") == server.build, connecting.get("build"))
    bye = [l for l in lines if l.get("state") == "disconnected"]
    players = [l for l in lines if has_players(l)]
    check("a watch gets entities", players, f"{len(players)} snapshots, {len(players[-1]['players']) if players else 0} players")
    check("  with nothing undecoded", bye and bye[-1]["decode_errors"] == 0, bye[-1:])


def through_a_relay(server):
    relay = Proc("relay", server.tv, "--listen", f"127.0.0.1:{RELAY_PORT}", "--relay-password", "pw", env={"STVTF_DEBUG": "relay"})
    clients = []

    def client(port):
        c = Proc("connect", f"127.0.0.1:{port}", "--hz", "10", "--password", "pw")
        clients.append(c)
        return c

    try:
        if not relay.wait_line(lambda v: v.get("state") == "full"):
            check("the relay gets in", False, relay.err[-3:])
            return
        a = client(RELAY_PORT)
        check("a client of a relay gets entities", a.wait_line(has_players))
        connecting = next((v for _, v in a.lines if v.get("state") == "connecting"), {})
        check("  and is offered the server's build by the relay", connecting.get("build") == server.build, connecting.get("build"))
        time.sleep(3)

        t0 = time.time()
        b = client(RELAY_PORT)
        got = b.wait_line(has_players, since=t0)
        check("a late joiner gets entities", got, f"after {round(got[0] - t0, 2)} s" if got else "")
        time.sleep(1)
        check("  and the first client kept going", longest_gap(a, t0) < 2, f"longest gap {longest_gap(a, t0)} s")

        proxy = Lossy(RELAY_PORT, LOSSY_PORT)
        c = client(LOSSY_PORT)
        check("a client through a lossy link gets in", c.wait_line(has_players))
        time.sleep(3)
        t0 = time.time()
        before = [v["tick"] for _, v in c.lines if v["type"] == "snapshot"][-1]
        proxy.lose = 40
        back, end = None, time.time() + 20
        while time.time() < end and not back:
            full = relay.said("gets a full update", t0)
            if full:
                back = c.wait_line(has_players, 5, t0 + full[0][0])
            time.sleep(0.05)
        check("  it recovers through the server's full update", back, f"{round(back[0] - t0, 2)} s after the loss began, {proxy.dropped} packets dropped" if back else relay.said("waits for|gets", t0)[:3])
        check("  with ticks moving on", back and back[1]["tick"] > before, f"{before} -> {back[1]['tick']}" if back else "")
        check("  and the other clients do not notice", longest_gap(a, t0) < 2, f"longest gap {longest_gap(a, t0)} s")

        t0 = time.time()
        server.rcon("changelevel ctf_turbine")
        for name, who in (("the relay", relay), ("a client", a), ("a late joiner", b), ("a client that lost frames", c)):
            changed = who.wait_line(lambda v: v.get("state") == "map_change", 60, t0)
            full = changed and who.wait_line(lambda v: v.get("state") == "full", 60, changed[0])
            on = full and (who is relay or who.wait_line(has_players, 60, full[0]))
            check(f"a map change is followed by {name}", full and on, f"full again after {round(full[0] - t0, 1)} s" if full else "")
    finally:
        for c in clients:
            c.stop()
        relay.stop()
    # A client that truly lost packets can meet an entity whose creation it missed: that is how it
    # finds out, so only the others must have decoded everything.
    undecoded = [g["decode_errors"] for c in clients[:2] for g in c.goodbyes()]
    check("no decode errors in clients that lost nothing", len(undecoded) == 2 and not any(undecoded), undecoded)
    check("every client says it stopped", all(c.goodbyes() and c.goodbyes()[-1]["reason"] == "stopped" for c in clients))


def rejections(server):
    server.rcon("tv_password sesame")
    time.sleep(0.5)
    started = time.time()
    code, lines, err = run("connect", server.tv, "--password", "wrong", "--seconds", "30")
    check("a wrong password stops at once with an error", code != 0 and time.time() - started < 10, f"{round(time.time() - started, 1)} s: {err[:60]!r}")
    code, lines, err = run("connect", server.tv, "--password", "sesame", "--seconds", "5", "--hz", "2")
    check("the right one gets in", "full" in states(lines), states(lines)[-2:])
    server.rcon('tv_password ""')

    # SourceTV reads its slot count at a map load.
    server.rcon("tv_maxclients 1", "changelevel ctf_turbine")
    time.sleep(3)
    server.wait_ready(90)
    time.sleep(8)
    first = Proc("connect", server.tv, "--seconds", "12", "--hz", "0")
    first.wait_line(lambda v: v.get("state") == "full", 10)
    code, lines, err = run("connect", server.tv, "--seconds", "4", "--hz", "0")
    reasons = [l.get("reason", "") for l in lines if l.get("state") == "connect_failed"]
    check("a full SourceTV is reported and retried", code == 0 and any("Full" in r for r in reasons), reasons[:1])
    first.stop()
    server.rcon("tv_maxclients 8", "changelevel ctf_turbine")
    time.sleep(3)
    server.wait_ready(90)
    time.sleep(8)


def recording(server):
    with tempfile.TemporaryDirectory() as d:
        cap = os.path.join(d, "live.cap")
        code, live, err = run("connect", server.tv, "--seconds", "20", "--hz", "5", "--record", cap)
        code, replayed, err = run("replay", cap, "--hz", "5")
    strip = lambda ls: [{k: v for k, v in l.items() if k != "time"} for l in ls if l["type"] != "status"]
    a, b = strip(live), strip(replayed)
    check("a replay of a recording gives the same lines as the live run", a == b and len(a) > 20, f"{len(a)} lines live, {len(b)} replayed")


def soak(server, seconds):
    code, lines, err = run("connect", server.tv, "--seconds", str(seconds), "--hz", "20", timeout=seconds + 60)
    bye = [l for l in lines if l.get("state") == "disconnected"]
    check(f"{seconds} s of real play decodes cleanly", bye and bye[-1]["packets"] > seconds * 5 and not bye[-1]["decode_errors"] and not bye[-1]["lost"], {k: bye[-1][k] for k in ("packets", "lost", "decode_errors")} if bye else "")
    print("      line kinds:", dict(collections.Counter(l["type"] for l in lines).most_common(16)), flush=True)


def restart(server):
    watcher = Proc("connect", server.tv, "--hz", "2", "--seconds", "90")
    watcher.wait_line(has_players)
    t0 = time.time()
    server.rcon("_restart")
    full = watcher.wait_line(lambda v: v.get("state") == "full", 80, t0)
    check("a watch comes back after a server restart", full, f"full again after {round(full[0] - t0, 1)} s" if full else "")
    check("  with entities", full and watcher.wait_line(has_players, 30, full[0]))
    watcher.stop()


parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--image", default="serveme/tf2-cloud-server:latest")
parser.add_argument("--update", action="store_true", help="update TF2 first, through steamcmd")
parser.add_argument("--bin", default="target/release/stvtf")
parser.add_argument("--soak", type=int, default=60, help="seconds of play to decode")
options = parser.parse_args()
BIN = os.path.abspath(options.bin)

# So that being killed still removes the server.
signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
server = Server(options.image, options.update)
try:
    server.build = server.wait_ready(1800 if options.update else 300)
    print(f"server of build {server.build} at {server.tv}", flush=True)
    server.rcon("mp_waitingforplayers_cancel 1")
    time.sleep(5)
    for step in (direct, through_a_relay, rejections, recording, lambda s: soak(s, options.soak), restart):
        step(server)
finally:
    server.remove()
print(f"{sum(results)} of {len(results)} checks passed")
sys.exit(0 if all(results) else 1)
