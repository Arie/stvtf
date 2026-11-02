//! Ignored: needs a directory of demos.
//!
//! ```text
//! STVTF_SMOKE_DIR=~/demos cargo test --release --test smoke -- --ignored --nocapture
//! ```
//!
//! `STVTF_SMOKE_RECORD=FILE` writes a digest of each demo's output, `STVTF_SMOKE_COMPARE=FILE`
//! checks against one. `STVTF_SMOKE_THREADS` caps the threads.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use stvtf::Line;

type Digests = BTreeMap<String, String>;

struct Outcome {
    path: PathBuf,
    bytes: usize,
    digest: String,
    lines: BTreeMap<String, usize>,
    failure: Option<String>,
}

#[test]
#[ignore = "needs a directory of demos in STVTF_SMOKE_DIR"]
fn every_demo_decodes() {
    let dir = PathBuf::from(std::env::var("STVTF_SMOKE_DIR").expect("set STVTF_SMOKE_DIR to a directory of demos"));
    let mut demos = Vec::new();
    find_demos(&dir, &mut demos);
    demos.sort();
    assert!(!demos.is_empty(), "no .dem files under {}", dir.display());
    let threads = std::env::var("STVTF_SMOKE_THREADS")
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from))
        .clamp(1, demos.len());
    eprintln!("{} demos on {threads} threads", demos.len());

    let started = Instant::now();
    let next = AtomicUsize::new(0);
    let outcomes = Mutex::new(Vec::new());
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                while let Some(path) = demos.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let outcome = decode(path);
                    if let Some(why) = &outcome.failure {
                        eprintln!("FAIL  {}  {why}", path.display());
                    }
                    outcomes.lock().unwrap().push(outcome);
                }
            });
        }
    });
    std::panic::set_hook(hook);

    let outcomes = outcomes.into_inner().unwrap();
    let failed: Vec<&Outcome> = outcomes.iter().filter(|o| o.failure.is_some()).collect();
    let mut totals = BTreeMap::new();
    for o in &outcomes {
        for (kind, n) in &o.lines {
            *totals.entry(kind.as_str()).or_insert(0) += n;
        }
    }
    let mib = outcomes.iter().map(|o| o.bytes).sum::<usize>() as f64 / f64::from(1 << 20);
    let secs = started.elapsed().as_secs_f64();
    eprintln!("\n{} demos, {mib:.0} MiB in {secs:.1} s ({:.0} MiB/s)", outcomes.len(), mib / secs);
    eprintln!("lines: {totals:?}");
    let mut list: Vec<String> =
        failed.iter().map(|o| format!("{}: {}", o.path.display(), o.failure.as_ref().unwrap())).collect();

    let digests: Digests = outcomes
        .iter()
        .filter(|o| o.failure.is_none())
        .map(|o| (o.path.strip_prefix(&dir).unwrap().display().to_string(), o.digest.clone()))
        .collect();
    if let Ok(file) = std::env::var("STVTF_SMOKE_RECORD") {
        let text = digests.iter().fold(String::new(), |mut text, (path, digest)| {
            writeln!(text, "{digest}  {path}").unwrap();
            text
        });
        std::fs::write(&file, text).unwrap_or_else(|e| panic!("{file}: {e}"));
        eprintln!("recorded {} digests in {file}", digests.len());
    }
    if let Ok(file) = std::env::var("STVTF_SMOKE_COMPARE") {
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file}: {e}"));
        let before: Digests = text
            .lines()
            .filter_map(|l| l.split_once("  "))
            .map(|(digest, path)| (path.to_owned(), digest.to_owned()))
            .collect();
        let mut same = 0;
        for (path, digest) in &digests {
            match before.get(path) {
                Some(old) if old == digest => same += 1,
                Some(_) => list.push(format!("{path}: output differs from {file}")),
                None => list.push(format!("{path}: not in {file}")),
            }
        }
        for path in before.keys().filter(|p| !digests.contains_key(*p)) {
            list.push(format!("{path}: in {file}, but did not decode now"));
        }
        eprintln!("{same} of {} demos give the same output as in {file}", before.len());
    }
    assert!(list.is_empty(), "{} problems in {} demos:\n{}", list.len(), outcomes.len(), list.join("\n"));
}

fn find_demos(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            find_demos(&path, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dem")) {
            out.push(path);
        }
    }
}

fn decode(path: &Path) -> Outcome {
    let mut outcome =
        Outcome { path: path.to_path_buf(), bytes: 0, digest: String::new(), lines: BTreeMap::new(), failure: None };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            outcome.failure = Some(format!("reading: {e}"));
            return outcome;
        }
    };
    outcome.bytes = bytes.len();
    let mut md5 = md5::Context::new();
    let mut emit = |line: Line| {
        let text = serde_json::to_string(&line).unwrap();
        md5.consume(text.as_bytes());
        md5.consume(b"\n");
        let kind = text.strip_prefix(r#"{"type":""#).and_then(|t| t.split_once('"')).map_or("?", |(k, _)| k);
        *outcome.lines.entry(kind.to_owned()).or_insert(0) += 1;
    };
    let result =
        catch_unwind(AssertUnwindSafe(|| stvtf::decode::demo_file(&bytes, stvtf::DEFAULT_SNAPSHOT_HZ, &mut emit)));
    outcome.digest = format!("{:x}", md5.compute());
    outcome.failure = match result {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("{e:#}")),
        Err(panic) => Some(format!(
            "panicked: {}",
            panic.downcast_ref::<String>().map(String::as_str).or(panic.downcast_ref::<&str>().copied()).unwrap_or("?")
        )),
    };
    outcome
}
