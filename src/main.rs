use anyhow::{Result, anyhow};
use serde_json::Value;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use stvmap::capture;
use stvmap::decode::{self, Tables};
use stvmap::session::Options;
use stvmap::watch::{self, Watch};

const USAGE: &str = "\
stvmap: headless TF2 SourceTV spectator emitting top-down overview data as JSON lines

  stvmap connect HOST:PORT [--password tv] [--hz 20] [--seconds N] [--record FILE.cap]
                   [--name stvmap] [--tables SAME_BUILD.dem] [--crc HEX] [--build N]
  stvmap replay  FILE.cap [--hz 20] [--tables SAME_BUILD.dem] [--build N]
  stvmap demo    FILE.dem [--hz 20]

HOST:PORT is the SourceTV port (tv_port), not the game port.
Tables and CRC of the build reported by the server are built in for known builds;
pass --tables (any demo recorded on that build) and --crc for others.";

struct Args {
    positional: Vec<String>,
    flags: std::collections::HashMap<String, String>,
}

fn parse_args() -> Result<Args> {
    let mut positional = Vec::new();
    let mut flags = std::collections::HashMap::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if let Some(name) = a.strip_prefix("--") {
            let v = it.next().ok_or_else(|| anyhow!("--{name} needs a value"))?;
            flags.insert(name.to_string(), v);
        } else {
            positional.push(a);
        }
    }
    Ok(Args { positional, flags })
}

impl Args {
    fn get(&self, k: &str) -> Option<&str> {
        self.flags.get(k).map(String::as_str)
    }
    fn hz(&self) -> Result<f32> {
        Ok(self.get("hz").map(str::parse).transpose()?.unwrap_or(20.0))
    }
    fn tables(&self, build: &str) -> Result<Tables> {
        watch::tables(build, self.get("tables").map(PathBuf::from).as_deref())
    }
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let live = args.positional.first().map(String::as_str) == Some("connect");
    let mut emit = |mut v: Value| {
        // Wall-clock receive time (ms since the Unix epoch), for live streams only.
        if live && let Some(o) = v.as_object_mut() {
            let ms =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
            o.insert("time".into(), ms.into());
        }
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    };
    match args.positional.first().map(String::as_str) {
        Some("demo") => {
            let path = args.positional.get(1).ok_or_else(|| anyhow!(USAGE))?;
            let bytes = std::fs::read(path)?;
            decode::demo_file(&bytes, args.hz()?, &mut emit)
        }
        Some("replay") => {
            let path = args.positional.get(1).ok_or_else(|| anyhow!(USAGE))?;
            let bytes = std::fs::read(path)?;
            let records = capture::read(&bytes)?;
            let opt = Options { snapshot_hz: args.hz()?, ..Options::default() };
            let build = args.get("build").unwrap_or(stvmap::CURRENT_BUILD);
            capture::replay(&records, &args.tables(build)?, &opt, &mut emit)
        }
        Some("connect") => connect(&args, &mut emit),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

fn connect(args: &Args, emit: &mut dyn FnMut(Value)) -> Result<()> {
    let target = args.positional.get(1).ok_or_else(|| anyhow!(USAGE))?;
    let mut w = Watch::new(target, args.get("password").unwrap_or(""));
    if let Some(n) = args.get("name") {
        w.name = n.to_string();
    }
    w.snapshot_hz = args.hz()?;
    w.tables = args.get("tables").map(PathBuf::from);
    w.crc = args.get("crc").map(|h| u32::from_str_radix(h.trim_start_matches("0x"), 16)).transpose()?;
    w.build = args.get("build").map(str::to_string);
    w.duration =
        args.get("seconds").map(|s| Ok::<_, anyhow::Error>(Duration::try_from_secs_f64(s.parse()?)?)).transpose()?;
    w.record = args.get("record").map(PathBuf::from);
    static STOP: AtomicBool = AtomicBool::new(false);
    watch::stop_on_ctrl_c(&STOP)?;
    watch::watch(&w, &STOP, emit)?;
    Ok(())
}
