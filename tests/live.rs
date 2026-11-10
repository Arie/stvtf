//! stvtf against a real TF2 server with bots, for what recordings cannot show: a relay of a live
//! server, a late joiner, recovery through a real full update, map changes, rejections, a restart.
//!
//! Ignored, as it needs Docker and an image with TF2, and takes minutes. It starts a throwaway
//! server on Docker's private bridge network and removes it after:
//!
//! ```text
//! cargo test --test live -- --ignored --nocapture
//! STVTF_LIVE_UPDATE=1 cargo test --test live -- --ignored --nocapture   # the current TF2 build
//! ```
//!
//! `STVTF_LIVE_IMAGE` picks the image (by default `serveme/tf2-cloud-server:latest`): it needs TF2 in
//! /home/tf2/hlserver/tf2, a map `ctf_turbine` and bash. Its own entrypoint is not run, and `SourceMod`
//! is moved aside. `STVTF_LIVE_SOAK` sets the seconds of play to decode (60).

mod common;

use common::{A2S_INFO, has_players, offered_build, stvtf};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use stvtf::Line;
use stvtf::output::{Stamped, Status};

const NAME: &str = "stvtf-live-check";
const GAME_PORT: u16 = 27915;
const TV_PORT: u16 = 27920;
const RELAY_PORT: u16 = 27930;
const LOSSY_PORT: u16 = 27931;
/// How long a step waits for what should come.
const WAIT: Duration = Duration::from_secs(45);

/// Every check, so that one failure does not hide the rest.
#[derive(Default)]
struct Checks {
    passed: usize,
    failed: Vec<String>,
}

impl Checks {
    fn check(&mut self, what: &str, ok: bool, detail: impl Display) {
        let detail = detail.to_string();
        let detail = if detail.is_empty() { String::new() } else { format!("  {detail}") };
        eprintln!("{}  {what}{detail}", if ok { "PASS" } else { "FAIL" });
        if ok {
            self.passed += 1;
        } else {
            self.failed.push(format!("{what}{detail}"));
        }
    }
}

/// The container, and rcon into it. Dropping it removes the container.
struct Server {
    ip: String,
    tv: String,
    password: String,
    build: String,
}

fn docker(args: &[&str]) -> Output {
    Command::new("docker").args(args).output().expect("docker")
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom").unwrap().read_exact(&mut buf).unwrap();
    buf.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

impl Server {
    fn start(image: &str, update: bool) -> Self {
        let password = random_hex(12);
        docker(&["rm", "-f", NAME]);
        let flags = format!(
            "-game tf -ip 0.0.0.0 -port {GAME_PORT} +tv_port {TV_PORT} +clientport 27905 +map ctf_turbine \
             +maxplayers 24 +sv_lan 1 -nomaster +rcon_password {password} +tv_enable 1 +tv_delay 0 \
             +tv_maxrate 0 +tv_maxclients 8 +tf_bot_quota 8 +tf_bot_quota_mode normal \
             +mp_autoteambalance 0 +sv_pure 0 +mp_waitingforplayers_cancel 1"
        );
        let mut steps = vec!["cd /home/tf2/hlserver/tf2".to_string()];
        if update {
            // It often needs another go; without the update there is nothing to check.
            let install = "/usr/games/steamcmd +force_install_dir /home/tf2/hlserver/tf2 +login anonymous \
                           +app_update 232250 +quit";
            steps.push(format!(
                "for i in 1 2 3 4; do {install} > /tmp/steamcmd.log 2>&1; grep -q 'Success! App' /tmp/steamcmd.log \
                 && break; [ $i = 4 ] && {{ tail -5 /tmp/steamcmd.log; exit 1; }}; done"
            ));
        }
        steps.push("mv tf/addons tf/addons.off".into());
        steps.push("rm -f tf/cfg/server.cfg".into());
        // A restart comes back, as on a real host.
        steps.push(format!("while true; do ./srcds_run {flags}; sleep 1; done"));
        let started =
            docker(&["run", "-d", "--rm", "--name", NAME, "--entrypoint", "bash", image, "-c", &steps.join("; ")]);
        assert!(started.status.success(), "docker run: {}", String::from_utf8_lossy(&started.stderr));
        let inspect = docker(&["inspect", NAME, "--format", "{{.NetworkSettings.IPAddress}}"]);
        let ip = String::from_utf8(inspect.stdout).unwrap().trim().to_string();
        let tv = format!("{ip}:{TV_PORT}");
        Server { ip, tv, password, build: String::new() }
    }

    fn running() -> bool {
        !docker(&["ps", "-q", "--filter", &format!("name=^{NAME}$")]).stdout.is_empty()
    }

    /// The build `SourceTV` says it is.
    fn a2s(&self) -> Option<String> {
        let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
        sock.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
        let mut buf = [0u8; 2048];
        sock.send_to(A2S_INFO, (self.ip.as_str(), TV_PORT)).ok()?;
        let mut n = sock.recv(&mut buf).ok()?;
        if buf.get(4) == Some(&b'A') {
            sock.send_to(&[A2S_INFO, &buf[5..9]].concat(), (self.ip.as_str(), TV_PORT)).ok()?;
            n = sock.recv(&mut buf).ok()?;
        }
        // Name, map, folder, game, then 9 bytes: app id, players, slots, bots, type, OS, password, VAC.
        let mut strings = buf.get(6..n)?.splitn(5, |&b| b == 0);
        let rest = strings.nth(4)?;
        let build = rest.get(9..)?.split(|&b| b == 0).next()?;
        Some(String::from_utf8_lossy(build).into_owned())
    }

    /// Waits until both `SourceTV` and rcon answer, and gives the server's build.
    fn wait_ready(&self, timeout: Duration) -> String {
        let end = Instant::now() + timeout;
        while Instant::now() < end && Self::running() {
            if let Some(build) = self.a2s()
                && self.rcon(&["status"]).contains("players : ")
            {
                return build;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        let logs = docker(&["logs", "--tail", "20", NAME]);
        panic!("the server never came up:\n{}", String::from_utf8_lossy(&logs.stdout));
    }

    /// What the server said to `commands`; nothing if it could not be reached.
    fn rcon(&self, commands: &[&str]) -> String {
        self.try_rcon(commands).unwrap_or_default()
    }

    fn try_rcon(&self, commands: &[&str]) -> std::io::Result<String> {
        let addr: SocketAddr = format!("{}:{GAME_PORT}", self.ip).parse().unwrap();
        let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        let send = |s: &mut TcpStream, id: i32, kind: i32, body: &str| {
            let mut d = id.to_le_bytes().to_vec();
            d.extend_from_slice(&kind.to_le_bytes());
            d.extend_from_slice(body.as_bytes());
            d.extend_from_slice(&[0, 0]);
            s.write_all(&[(d.len() as i32).to_le_bytes().as_slice(), &d].concat())
        };
        let packet = |s: &mut TcpStream| -> std::io::Result<(i32, String)> {
            let mut len = [0u8; 4];
            s.read_exact(&mut len)?;
            let mut d = vec![0u8; usize::try_from(i32::from_le_bytes(len)).unwrap_or(0)];
            s.read_exact(&mut d)?;
            let kind = i32::from_le_bytes(d[4..8].try_into().unwrap());
            let body = String::from_utf8_lossy(&d[8..d.len().saturating_sub(2)]).into_owned();
            Ok((kind, body))
        };
        send(&mut s, 1, 3, &self.password)?;
        while packet(&mut s)?.0 != 2 {}
        s.set_read_timeout(Some(Duration::from_secs(1)))?;
        let mut out = String::new();
        for command in commands {
            send(&mut s, 2, 2, command)?;
            while let Ok((_, body)) = packet(&mut s) {
                out += &body;
            }
        }
        Ok(out)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        docker(&["rm", "-f", NAME]);
    }
}

/// UDP between a client and a port here, which can lose what goes to the client.
struct Lossy {
    lose: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

impl Lossy {
    fn start(to: u16, port: u16) -> Self {
        let front = Arc::new(UdpSocket::bind(("127.0.0.1", port)).unwrap());
        let back = UdpSocket::bind("127.0.0.1:0").unwrap();
        back.connect(("127.0.0.1", to)).unwrap();
        let back = Arc::new(back);
        let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
        let (lose, dropped) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        {
            let (front, back, client) = (front.clone(), back.clone(), client.clone());
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 65536];
                while let Ok((n, from)) = front.recv_from(&mut buf) {
                    *client.lock().unwrap() = Some(from);
                    let _ = back.send(&buf[..n]);
                }
            });
        }
        {
            let (lose, dropped) = (lose.clone(), dropped.clone());
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 65536];
                while let Ok(n) = back.recv(&mut buf) {
                    if lose.load(Ordering::Relaxed) > 0 {
                        lose.fetch_sub(1, Ordering::Relaxed);
                        dropped.fetch_add(1, Ordering::Relaxed);
                    } else if let Some(to) = *client.lock().unwrap() {
                        let _ = front.send_to(&buf[..n], to);
                    }
                }
            });
        }
        Lossy { lose, dropped }
    }
}

/// A running stvtf, whose lines are kept with the time each came. Dropping it stops it.
struct Proc {
    child: Child,
    lines: Arc<Mutex<Vec<(Instant, Line)>>>,
    err: Arc<Mutex<Vec<(Instant, String)>>>,
}

fn line(text: &str) -> Line {
    serde_json::from_str::<Stamped>(text).unwrap_or_else(|e| panic!("{e}: {text}")).line
}

impl Proc {
    fn start(args: &[&str], env: &[(&str, &str)]) -> Self {
        let mut command = stvtf();
        command.args(args).envs(env.iter().copied()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let (lines, err): (Arc<Mutex<Vec<_>>>, Arc<Mutex<Vec<_>>>) = Default::default();
        let (stdout, stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let keep = lines.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(stdout).lines().map_while(Result::ok) {
                keep.lock().unwrap().push((Instant::now(), line(&l)));
            }
        });
        let keep = err.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(stderr).lines().map_while(Result::ok) {
                keep.lock().unwrap().push((Instant::now(), l));
            }
        });
        Proc { child, lines, err }
    }

    /// The first line since `since` that `want` takes, within `timeout`.
    fn wait_line(&self, since: Instant, timeout: Duration, want: impl Fn(&Line) -> bool) -> Option<(Instant, Line)> {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if let Some(found) = self.lines.lock().unwrap().iter().find(|(t, l)| *t >= since && want(l)) {
                return Some(found.clone());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    fn lines(&self) -> Vec<(Instant, Line)> {
        self.lines.lock().unwrap().clone()
    }

    /// When stderr since `since` first had `needle`.
    fn said(&self, since: Instant, needle: &str) -> Option<Instant> {
        self.err.lock().unwrap().iter().find(|(t, l)| *t >= since && l.contains(needle)).map(|(t, _)| *t)
    }

    /// As `docker stop` does: SIGTERM, then a kill if it lingers.
    fn stop(&mut self) {
        let _ = Command::new("kill").args(["-TERM", &self.child.id().to_string()]).status();
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if let Ok(Some(_)) = self.child.try_wait() {
                std::thread::sleep(Duration::from_millis(100));
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
    }

    fn goodbyes(&self) -> Vec<Goodbye> {
        self.lines().into_iter().filter_map(|(_, l)| goodbye(&l)).collect()
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Ran {
    code: Option<i32>,
    lines: Vec<Line>,
    /// The lines as written, for comparing exactly.
    text: Vec<String>,
    stderr: String,
}

/// Runs stvtf to its end, or kills it after `timeout`. Its output is read as it comes: a full
/// pipe would stop it.
fn run(args: &[&str], timeout: Duration) -> Ran {
    let mut child = stvtf().args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let read = |mut from: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut all = Vec::new();
            from.read_to_end(&mut all).unwrap();
            String::from_utf8_lossy(&all).into_owned()
        })
    };
    let stdout = read(Box::new(child.stdout.take().unwrap()));
    let stderr = read(Box::new(child.stderr.take().unwrap()));
    let end = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= end {
            let _ = child.kill();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let text: Vec<String> = stdout.join().unwrap().lines().map(str::to_string).collect();
    Ran {
        code: status.code(),
        lines: text.iter().map(|t| line(t)).collect(),
        text,
        stderr: stderr.join().unwrap().trim().to_string(),
    }
}

struct Goodbye {
    reason: String,
    decode_errors: u64,
    packets: u64,
    lost: u64,
}

fn goodbye(l: &Line) -> Option<Goodbye> {
    match l {
        Line::Status(Status::Disconnected { reason, decode_errors, packets, lost }) => {
            Some(Goodbye { reason: reason.clone(), decode_errors: *decode_errors, packets: *packets, lost: *lost })
        }
        _ => None,
    }
}

/// A status line's `state`, as it is written.
fn state(l: &Line) -> Option<String> {
    match l {
        Line::Status(_) => serde_json::to_value(l).ok()?["state"].as_str().map(str::to_string),
        _ => None,
    }
}

fn is_state(name: &str) -> impl Fn(&Line) -> bool + '_ {
    move |l| state(l).as_deref() == Some(name)
}

fn snapshot_tick(l: &Line) -> Option<u32> {
    match l {
        Line::Snapshot(s) => Some(s.tick),
        _ => None,
    }
}

/// The longest time between a client's snapshots since `since`, in seconds.
fn longest_gap(c: &Proc, since: Instant) -> f64 {
    let times: Vec<Instant> =
        c.lines().into_iter().filter(|(t, l)| *t >= since && matches!(l, Line::Snapshot(_))).map(|(t, _)| t).collect();
    times.windows(2).map(|w| (w[1] - w[0]).as_secs_f64()).fold(f64::NAN, f64::max).max(0.0)
}

fn secs(d: Duration) -> String {
    format!("{:.1} s", d.as_secs_f64())
}

fn direct(server: &Server, checks: &mut Checks) {
    let ran = run(&["connect", &server.tv, "--hz", "5", "--seconds", "12"], WAIT);
    let build = offered_build(&ran.lines);
    checks.check("offers the build the server reports", build == Some(&server.build), format!("{build:?}"));
    let players: Vec<&Line> = ran.lines.iter().filter(|l| has_players(l)).collect();
    checks.check("a watch gets entities", !players.is_empty(), format!("{} snapshots", players.len()));
    let undecoded = ran.lines.iter().rev().find_map(goodbye).map(|g| g.decode_errors);
    checks.check("  with nothing undecoded", undecoded == Some(0), format!("{undecoded:?} decode errors"));
}

/// A client of the relay, which says why it could not decode something.
fn client(port: u16) -> Proc {
    Proc::start(
        &["connect", &format!("127.0.0.1:{port}"), "--hz", "10", "--password", "pw"],
        &[("STVTF_DEBUG", "errors")],
    )
}

/// What a client said about what it could not decode, or else about a panic.
fn why_undecoded(c: &Proc) -> String {
    let err = c.err.lock().unwrap();
    let said: Vec<&str> = err
        .iter()
        .map(|(_, l)| l.as_str())
        .filter(|l| l.contains("decode error") || l.contains("panicked"))
        .take(3)
        .collect();
    said.join(" | ")
}

/// A late joiner and a client that loses frames, on a relay with a first client.
fn relay_clients(server: &Server, relay: &Proc, checks: &mut Checks) -> Vec<Proc> {
    let a = client(RELAY_PORT);
    checks.check("a client of a relay gets entities", a.wait_line(Instant::now(), WAIT, has_players).is_some(), "");
    let lines: Vec<Line> = a.lines().into_iter().map(|(_, l)| l).collect();
    let build = offered_build(&lines);
    let ok = build == Some(&server.build);
    checks.check("  and is offered the server's build by the relay", ok, format!("{build:?}"));
    std::thread::sleep(Duration::from_secs(3));

    let t0 = Instant::now();
    let b = client(RELAY_PORT);
    let got = b.wait_line(t0, WAIT, has_players);
    checks.check("a late joiner gets entities", got.is_some(), got.map_or(String::new(), |(t, _)| secs(t - t0)));
    std::thread::sleep(Duration::from_secs(1));
    let gap = longest_gap(&a, t0);
    checks.check("  and the first client kept going", gap < 2.0, format!("longest gap {gap:.2} s"));

    let lossy = Lossy::start(RELAY_PORT, LOSSY_PORT);
    let c = client(LOSSY_PORT);
    checks.check("a client through a lossy link gets in", c.wait_line(Instant::now(), WAIT, has_players).is_some(), "");
    std::thread::sleep(Duration::from_secs(3));
    let t0 = Instant::now();
    let before = c.lines().iter().rev().find_map(|(_, l)| snapshot_tick(l)).unwrap_or(0);
    lossy.lose.store(40, Ordering::Relaxed);
    let mut back = None;
    let end = t0 + Duration::from_secs(20);
    while Instant::now() < end && back.is_none() {
        if let Some(full) = relay.said(t0, "gets a full update") {
            back = c.wait_line(full, Duration::from_secs(5), has_players);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let dropped = lossy.dropped.load(Ordering::Relaxed);
    let when = back.as_ref().map_or(String::new(), |(t, _)| secs(*t - t0));
    checks.check(
        "  it recovers through the server's full update",
        back.is_some(),
        format!("{when} after the loss began, {dropped} packets dropped"),
    );
    let after = back.as_ref().and_then(|(_, l)| snapshot_tick(l)).unwrap_or(0);
    checks.check("  with ticks moving on", after > before, format!("{before} -> {after}"));
    let gap = longest_gap(&a, t0);
    checks.check("  and the other clients do not notice", gap < 2.0, format!("longest gap {gap:.2} s"));
    vec![a, b, c]
}

fn map_change(server: &Server, relay: &Proc, clients: &[Proc], checks: &mut Checks) {
    let t0 = Instant::now();
    server.rcon(&["changelevel ctf_turbine"]);
    let names = ["a client", "a late joiner", "a client that lost frames"];
    let everyone = std::iter::once(("the relay", relay)).chain(names.into_iter().zip(clients));
    for (name, who) in everyone {
        let changed = who.wait_line(t0, Duration::from_secs(60), is_state("map_change"));
        let full = changed.and_then(|(t, _)| who.wait_line(t, Duration::from_secs(60), is_state("full")));
        let on = full.as_ref().is_some_and(|(t, _)| {
            std::ptr::eq(who, relay) || who.wait_line(*t, Duration::from_secs(60), has_players).is_some()
        });
        let detail = full.as_ref().map_or(String::new(), |(t, _)| format!("full again after {}", secs(*t - t0)));
        checks.check(&format!("a map change is followed by {name}"), on, detail);
    }
}

fn through_a_relay(server: &Server, checks: &mut Checks) {
    let listen = format!("127.0.0.1:{RELAY_PORT}");
    let args = ["relay", &server.tv, "--listen", &listen, "--relay-password", "pw"];
    let mut relay = Proc::start(&args, &[("STVTF_DEBUG", "relay")]);
    if relay.wait_line(Instant::now(), WAIT, is_state("full")).is_none() {
        checks.check("the relay gets in", false, "");
        return;
    }
    let mut clients = relay_clients(server, &relay, checks);
    map_change(server, &relay, &clients, checks);
    for c in &mut clients {
        c.stop();
    }
    relay.stop();
    // A client that truly lost packets can meet an entity whose creation it missed: that is how it
    // finds out, so only the others must have decoded everything.
    let undecoded: Vec<u64> = clients[..2].iter().flat_map(Proc::goodbyes).map(|g| g.decode_errors).collect();
    let ok = undecoded.len() == 2 && undecoded.iter().all(|&e| e == 0);
    let why = if ok { String::new() } else { clients[..2].iter().map(why_undecoded).collect::<Vec<_>>().join(" || ") };
    checks.check("no decode errors in clients that lost nothing", ok, format!("{undecoded:?} {why}"));
    let stopped = clients.iter().all(|c| c.goodbyes().last().is_some_and(|g| g.reason == "stopped"));
    checks.check("every client says it stopped", stopped, "");
}

/// A map change, after which the server is ready again.
fn reload(server: &Server, commands: &[&str]) {
    server.rcon(commands);
    std::thread::sleep(Duration::from_secs(3));
    server.wait_ready(Duration::from_secs(90));
    std::thread::sleep(Duration::from_secs(8));
}

fn rejections(server: &Server, checks: &mut Checks) {
    server.rcon(&["tv_password sesame"]);
    std::thread::sleep(Duration::from_millis(500));
    let started = Instant::now();
    let ran = run(&["connect", &server.tv, "--password", "wrong", "--seconds", "30"], WAIT);
    let took = started.elapsed();
    let ok = ran.code != Some(0) && took < Duration::from_secs(10);
    let first_line: String = ran.stderr.lines().next().unwrap_or("").chars().take(70).collect();
    checks.check("a wrong password stops at once with an error", ok, format!("{}: {first_line}", secs(took)));
    let ran = run(&["connect", &server.tv, "--password", "sesame", "--seconds", "5", "--hz", "2"], WAIT);
    checks.check("the right one gets in", ran.lines.iter().any(is_state("full")), "");
    server.rcon(&["tv_password \"\""]);

    // SourceTV reads its slot count at a map load.
    reload(server, &["tv_maxclients 1", "changelevel ctf_turbine"]);
    let mut first = Proc::start(&["connect", &server.tv, "--seconds", "12", "--hz", "0"], &[]);
    first.wait_line(Instant::now(), Duration::from_secs(10), is_state("full"));
    let ran = run(&["connect", &server.tv, "--seconds", "4", "--hz", "0"], WAIT);
    let reasons: Vec<&String> = ran
        .lines
        .iter()
        .filter_map(|l| match l {
            Line::Status(Status::ConnectFailed { reason }) => Some(reason),
            _ => None,
        })
        .collect();
    let ok = ran.code == Some(0) && reasons.iter().any(|r| r.contains("Full"));
    checks.check("a full SourceTV is reported and retried", ok, format!("{:?}", reasons.first()));
    first.stop();
    reload(server, &["tv_maxclients 8", "changelevel ctf_turbine"]);
}

/// The lines but statuses, without the receive time live lines have.
fn without_times(text: &[String]) -> Vec<Value> {
    text.iter()
        .map(|t| serde_json::from_str::<Value>(t).unwrap())
        .filter(|v| v["type"] != "status")
        .map(|mut v| {
            v.as_object_mut().unwrap().remove("time");
            v
        })
        .collect()
}

fn recording(server: &Server, checks: &mut Checks) {
    let cap = format!("{}/live_{}.cap", env!("CARGO_TARGET_TMPDIR"), std::process::id());
    let _ = std::fs::remove_file(&cap);
    let live = run(&["connect", &server.tv, "--seconds", "20", "--hz", "5", "--record", &cap], WAIT);
    let replayed = run(&["replay", &cap, "--hz", "5"], WAIT);
    let _ = std::fs::remove_file(&cap);
    let (a, b) = (without_times(&live.text), without_times(&replayed.text));
    checks.check(
        "a replay of a recording gives the same lines as the live run",
        a == b && a.len() > 20,
        format!("{} lines live, {} replayed", a.len(), b.len()),
    );
}

fn soak(server: &Server, seconds: u64, checks: &mut Checks) {
    let ran = run(
        &["connect", &server.tv, "--seconds", &seconds.to_string(), "--hz", "20"],
        Duration::from_secs(seconds + 60),
    );
    let bye = ran.lines.iter().rev().find_map(goodbye);
    let ok = bye.as_ref().is_some_and(|g| g.packets > seconds * 5 && g.decode_errors == 0 && g.lost == 0);
    let detail =
        bye.map_or(String::new(), |g| format!("{} packets, {} lost, {} undecoded", g.packets, g.lost, g.decode_errors));
    checks.check(&format!("{seconds} s of real play decodes cleanly"), ok, detail);
    let mut kinds = BTreeMap::<String, usize>::new();
    for v in without_times(&ran.text) {
        *kinds.entry(v["type"].as_str().unwrap_or("?").to_string()).or_default() += 1;
    }
    eprintln!("      line kinds: {kinds:?}");
}

fn restart(server: &Server, checks: &mut Checks) {
    let mut watcher = Proc::start(&["connect", &server.tv, "--hz", "2", "--seconds", "90"], &[]);
    watcher.wait_line(Instant::now(), WAIT, has_players);
    let t0 = Instant::now();
    server.rcon(&["_restart"]);
    let full = watcher.wait_line(t0, Duration::from_secs(80), is_state("full"));
    let detail = full.as_ref().map_or(String::new(), |(t, _)| format!("full again after {}", secs(*t - t0)));
    checks.check("a watch comes back after a server restart", full.is_some(), detail);
    let on = full.is_some_and(|(t, _)| watcher.wait_line(t, Duration::from_secs(30), has_players).is_some());
    checks.check("  with entities", on, "");
    watcher.stop();
}

#[test]
#[ignore = "needs Docker and a TF2 server image, and takes minutes"]
fn a_real_server() {
    let image = std::env::var("STVTF_LIVE_IMAGE").unwrap_or_else(|_| "serveme/tf2-cloud-server:latest".into());
    let update = std::env::var("STVTF_LIVE_UPDATE").is_ok_and(|v| v == "1");
    let soak_seconds = std::env::var("STVTF_LIVE_SOAK").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
    let mut server = Server::start(&image, update);
    server.build = server.wait_ready(Duration::from_secs(if update { 1800 } else { 300 }));
    eprintln!("server of build {} at {}", server.build, server.tv);
    server.rcon(&["mp_waitingforplayers_cancel 1"]);
    std::thread::sleep(Duration::from_secs(5));

    let mut checks = Checks::default();
    direct(&server, &mut checks);
    through_a_relay(&server, &mut checks);
    rejections(&server, &mut checks);
    recording(&server, &mut checks);
    soak(&server, soak_seconds, &mut checks);
    restart(&server, &mut checks);
    eprintln!("{} of {} checks passed", checks.passed, checks.passed + checks.failed.len());
    assert!(checks.failed.is_empty(), "failed:\n{}", checks.failed.join("\n"));
}
