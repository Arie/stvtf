//! The `stvtf` program as its users run it: arguments, exit status, and what goes to stdout,
//! which carries JSON lines and nothing else.

use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
/// A bound on what should take moments.
const LIMIT: Duration = Duration::from_secs(30);

fn stvtf() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_stvtf"));
    c.env_remove("STVTF_PASSWORD").env_remove("STVTF_RELAY_PASSWORD").env_remove("STVTF_DEBUG");
    c
}

fn run(args: &[&str]) -> Output {
    stvtf().args(args).output().unwrap()
}

fn fixture(name: &str) -> String {
    format!("{FIXTURES}/{name}")
}

fn lines(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8(stdout.to_vec())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

/// A command fails with this on stderr, and stdout stays empty for whatever reads it.
#[track_caller]
fn fails_with(args: &[&str], message: &str) {
    let out = run(args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{args:?}: {stderr}");
    assert!(stderr.contains(message), "{args:?}: {stderr}");
    assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
    assert!(out.stdout.is_empty(), "{args:?}");
}

#[test]
fn no_command_or_help_shows_the_usage() {
    for args in [&[][..], &["--help"], &["demo", "-h"]] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("stvtf connect HOST:PORT"), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn bad_arguments_say_what_is_wrong() {
    fails_with(&["frobnicate", "x"], "unknown command");
    fails_with(&["demo"], "demo needs an argument");
    fails_with(&["demo", "a.dem", "--hz"], "--hz needs a value");
    fails_with(&["demo", "a.dem", "b.dem"], "unexpected argument");
    fails_with(&["demo", "a.dem", "--record", "x"], "demo does not take --record");
    fails_with(&["connect", "127.0.0.1:1", "--hz", "fast"], "--hz fast: not a number");
    fails_with(&["connect", "127.0.0.1:1", "--crc", "xyz"], "--crc xyz: not hex");
    fails_with(&["connect", "127.0.0.1:1", "--seconds", "soon"], "--seconds soon: not a number");
    fails_with(&["connect", "127.0.0.1:1", "--seconds", "-1"], "must be 0 or more");
    let cap = fixture("bots_cp_badlands.cap");
    for slots in ["0", "256"] {
        fails_with(&["relay", &cap, "--relay-password", "x", "--relay-slots", slots], "must be 1 to 255");
    }
    fails_with(&["relay", &cap, "--relay-password", "x", "--relay-slots", "many"], "not a number");
    for speed in ["0", "-1", "inf"] {
        let args = ["relay", &cap, "--relay-password", "x", "--listen", "127.0.0.1:0", "--speed", speed];
        fails_with(&args, "--speed must be over 0");
    }
    fails_with(&["relay", &cap, "--relay-password", "x", "--listen", "not an address"], "binding not an address");
}

#[test]
fn files_that_are_missing_or_of_another_kind_are_errors() {
    fails_with(&["demo", "/nonexistent.dem"], "reading /nonexistent.dem");
    fails_with(&["replay", "/nonexistent.cap"], "reading /nonexistent.cap");
    fails_with(&["replay", &fixture("tc_hydro.dem")], "not an stvtf capture");
    fails_with(&["replay", &fixture("bots_cp_badlands.cap"), "--build", "1"], "no built-in tables");
}

#[test]
fn a_replay_writes_the_recorded_lines_without_times() {
    let out = run(&["replay", &fixture("bots_cp_badlands.cap")]);
    assert!(out.status.success());
    let got = lines(&out.stdout);
    let gz = std::fs::File::open(fixture("bots_cp_badlands.jsonl.gz")).unwrap();
    let mut recorded = Vec::new();
    flate2::read::GzDecoder::new(gz).read_to_end(&mut recorded).unwrap();
    assert_eq!(got, lines(&recorded), "as tests/golden.rs, through stdout");
    assert!(got.iter().all(|l| l.get("time").is_none()), "offline output has no receive time");
}

#[test]
fn a_demo_writes_lines_and_hz_sets_the_snapshots() {
    let demo = fixture("tc_hydro.dem");
    let count = |hz: &str| {
        let out = run(&["demo", &demo, "--hz", hz]);
        assert!(out.status.success());
        lines(&out.stdout).iter().filter(|l| l["type"] == "snapshot").count()
    };
    let (none, some, more) = (count("0"), count("2"), count("10"));
    assert_eq!(none, 0, "0 turns them off");
    assert!(0 < some && some * 2 < more, "{some} at 2 Hz, {more} at 10 Hz");
}

/// As in `stvtf demo x.dem | head`: not an error and no panic, however early the reader leaves.
#[test]
fn a_reader_going_away_ends_the_program_quietly() {
    let mut child =
        stvtf().args(["demo", &fixture("tc_hydro.dem")]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut first = [0u8; 16];
    stdout.read_exact(&mut first).unwrap();
    drop(stdout);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{:?}", out.status);
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// Kills a child when a test ends, passed or not.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Reads a child's lines on a thread, so a test can wait for one with a time limit.
fn line_reader(from: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(from).lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    receive
}

/// The next line that satisfies `want`, which may pick something out of it.
#[track_caller]
fn wait_for<T>(lines: &mpsc::Receiver<String>, what: &str, mut want: impl FnMut(&str) -> Option<T>) -> T {
    loop {
        let line = lines.recv_timeout(LIMIT).unwrap_or_else(|e| panic!("waiting for {what}: {e}"));
        if let Some(found) = want(&line) {
            return found;
        }
    }
}

fn status(line: &str) -> Option<(String, Value)> {
    let v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"));
    (v["type"] == "status").then(|| (v["state"].as_str().unwrap().to_string(), v))
}

#[cfg(unix)]
fn send_signal(to: &Running, signal: &str) {
    let sent = Command::new("kill").args([signal, &to.0.id().to_string()]).status().unwrap();
    assert!(sent.success(), "kill {signal}");
}

/// The whole program: `relay` with a made-up password, `connect --record` to it until SIGTERM,
/// and `replay` of what was recorded.
#[cfg(unix)]
#[test]
fn relay_connect_record_terminate_and_replay() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let cap = dir.join(format!("cli_record_{}.cap", std::process::id()));
    let _ = std::fs::remove_file(&cap);

    let mut relay = Running(
        stvtf()
            .args(["relay", &fixture("bots_cp_badlands.cap"), "--listen", "127.0.0.1:0", "--speed", "8"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let relay_err = line_reader(relay.0.stderr.take().unwrap());
    let relay_out = line_reader(relay.0.stdout.take().unwrap());
    let password =
        wait_for(&relay_err, "the relay's password", |l| l.strip_prefix("relay password: ").map(str::to_string));
    let listen = wait_for(&relay_out, "the relay's address", |l| {
        status(l).filter(|(state, _)| state == "relay").map(|(_, v)| v["listen"].as_str().unwrap().to_string())
    });

    let mut client = Running(
        stvtf()
            .args(["connect", &listen, "--hz", "10", "--record", cap.to_str().unwrap()])
            .env("STVTF_PASSWORD", &password)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let client_out = line_reader(client.0.stdout.take().unwrap());
    let snapshot = wait_for(&client_out, "a snapshot", |l| {
        let v: Value = serde_json::from_str(l).unwrap();
        (v["type"] == "snapshot").then_some(v)
    });
    assert!(snapshot["time"].as_u64().is_some_and(|ms| ms > 1_500_000_000_000), "live lines have a time: {snapshot}");
    assert!(snapshot["players"].as_array().is_some_and(|p| !p.is_empty()));

    // As `docker stop` does: the client says goodbye, frees its slot and exits cleanly.
    send_signal(&client, "-TERM");
    let reason = wait_for(&client_out, "the goodbye", |l| {
        status(l).filter(|(state, _)| state == "disconnected").map(|(_, v)| v["reason"].as_str().unwrap().to_string())
    });
    assert_eq!(reason, "stopped");
    let exit = client.0.wait().unwrap();
    assert!(exit.success(), "{exit:?}");

    // A recording is never overwritten.
    fails_with(&["connect", "127.0.0.1:1", "--record", cap.to_str().unwrap()], "creating capture");

    let replayed = run(&["replay", cap.to_str().unwrap(), "--hz", "10"]);
    assert!(replayed.status.success(), "{}", String::from_utf8_lossy(&replayed.stderr));
    let replayed = lines(&replayed.stdout);
    assert!(replayed.iter().any(|l| l["type"] == "status" && l["state"] == "full"), "the recording signs on");
    assert!(replayed.iter().any(|l| l["type"] == "snapshot"), "and holds entities");
    std::fs::remove_file(&cap).unwrap();
}
