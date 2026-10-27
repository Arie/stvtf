use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use stvtf::capture;
use stvtf::decode::{self, Tables};
use stvtf::output::Stamped;
use stvtf::relay::{Relay, RelayConfig};
use stvtf::watch::{self, Watch};
use stvtf::{Line, Options};

const USAGE: &str = "\
stvtf: headless TF2 SourceTV spectator emitting top-down overview data as JSON lines

  stvtf connect HOST:PORT [--password tv] [--hz 20] [--seconds N] [--record FILE.cap]
                  [--name stv.tf] [--tables SAME_BUILD.dem] [--crc HEX] [--build N]
                  [--relay HOST:PORT [--relay-password PW] [--relay-slots 10]]
  stvtf replay  FILE.cap [--hz 20] [--tables SAME_BUILD.dem] [--build N]
  stvtf demo    FILE.dem [--hz 20]
  stvtf relay   HOST:PORT|FILE.cap [--listen 0.0.0.0:27020] [--relay-password PW] [--relay-slots 10]
                  [--password tv] [--name stv.tf] [--tables SAME_BUILD.dem] [--crc HEX] [--build N]
                  [--speed 1]

HOST:PORT is the SourceTV port (tv_port); given the game port, it tries game port + 5.
The password can also come from the STVTF_PASSWORD environment variable, which
other local users cannot read the way they can read command-line arguments.
--relay passes the server on to TF2 clients that connect there, as tv_relay does, at no cost of
a slot on the server. Its password comes from --relay-password or STVTF_RELAY_PASSWORD, or is
made up and printed to stderr. `relay` does only that, printing only status lines; given a
capture, it replays it once the first client connects.
Tables and CRC of the build reported by the server are built in for known builds;
pass --tables (any demo recorded on that build) and --crc for others.";

/// Set by Ctrl-C, or when nobody reads the output any more.
static STOP: AtomicBool = AtomicBool::new(false);

struct Args {
    command: String,
    /// The one positional argument after the command (address or file).
    target: String,
    flags: HashMap<String, String>,
}

/// The flags each command accepts; anything else is an error, so a typo is not ignored.
fn allowed_flags(command: &str) -> Option<&'static [&'static str]> {
    match command {
        "connect" => Some(&[
            "password",
            "hz",
            "seconds",
            "record",
            "name",
            "tables",
            "crc",
            "build",
            "relay",
            "relay-password",
            "relay-slots",
        ]),
        "replay" => Some(&["hz", "tables", "build"]),
        "demo" => Some(&["hz"]),
        "relay" => {
            Some(&["listen", "relay-password", "relay-slots", "password", "name", "tables", "crc", "build", "speed"])
        }
        _ => None,
    }
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<Args>> {
    let mut positional = Vec::new();
    let mut flags = HashMap::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "-h" || a == "--help" {
            return Ok(None);
        }
        if let Some(flag) = a.strip_prefix("--") {
            let (name, value) = match flag.split_once('=') {
                Some((n, v)) => (n.to_string(), v.to_string()),
                None => (flag.to_string(), it.next().ok_or_else(|| anyhow!("--{flag} needs a value"))?),
            };
            if flags.insert(name.clone(), value).is_some() {
                bail!("--{name} given twice");
            }
        } else {
            positional.push(a);
        }
    }
    let mut positional = positional.into_iter();
    let Some(command) = positional.next() else { return Ok(None) };
    let allowed = allowed_flags(&command).ok_or_else(|| anyhow!("unknown command {command:?}\n\n{USAGE}"))?;
    let target = positional.next().ok_or_else(|| anyhow!("{command} needs an argument\n\n{USAGE}"))?;
    if let Some(extra) = positional.next() {
        bail!("unexpected argument {extra:?}");
    }
    if let Some(bad) = flags.keys().find(|k| !allowed.contains(&k.as_str())) {
        bail!("{command} does not take --{bad}\n\n{USAGE}");
    }
    Ok(Some(Args { command, target, flags }))
}

impl Args {
    fn get(&self, k: &str) -> Option<&str> {
        self.flags.get(k).map(String::as_str)
    }

    fn hz(&self) -> Result<f32> {
        self.get("hz").map_or(Ok(stvtf::DEFAULT_SNAPSHOT_HZ), stvtf::parse_snapshot_hz)
    }

    fn tables(&self, build: &str) -> Result<Tables> {
        watch::tables(build, self.get("tables").map(PathBuf::from).as_deref())
    }
}

fn main() -> Result<()> {
    let Some(args) = parse_args(std::env::args().skip(1))? else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let live = matches!(args.command.as_str(), "connect" | "relay");
    let mut emit = |line: Line| {
        if STOP.load(Ordering::Relaxed) && !live {
            return;
        }
        // Wall-clock receive time (ms since the Unix epoch), for live streams only.
        let time = live.then(|| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
        });
        // Live lines go out at once; offline output is flushed when the buffer fills.
        let written = serde_json::to_writer(&mut out, &Stamped { line, time })
            .map_err(std::io::Error::from)
            .and_then(|()| writeln!(out))
            .and_then(|()| if live { out.flush() } else { Ok(()) });
        if written.is_err() {
            // The reader went away (e.g. `| head`): stop, which frees the SourceTV slot.
            STOP.store(true, Ordering::Relaxed);
        }
    };
    match args.command.as_str() {
        "demo" => {
            let bytes = std::fs::read(&args.target).with_context(|| format!("reading {}", args.target))?;
            decode::demo_file(&bytes, args.hz()?, &mut emit)?;
        }
        "replay" => {
            let bytes = std::fs::read(&args.target).with_context(|| format!("reading {}", args.target))?;
            let records = capture::read(&bytes)?;
            let opt = Options { snapshot_hz: args.hz()?, ..Options::default() };
            let build = args.get("build").unwrap_or(stvtf::CURRENT_BUILD);
            capture::replay(&records, &args.tables(build)?, &opt, &mut emit)?;
        }
        "relay" => relay(&args, &mut |line| {
            if matches!(line, Line::Status(_)) {
                emit(line);
            }
        })?,
        _ => connect(&args, &mut emit)?,
    }
    // Ignore a final flush error: it is the same closed pipe already handled above.
    let _ = out.flush();
    Ok(())
}

fn relay(args: &Args, emit: &mut dyn FnMut(Line)) -> Result<()> {
    let relay = Relay::bind(&relay_config(args, args.get("listen").unwrap_or("0.0.0.0:27020"))?)?;
    watch::stop_on_ctrl_c(&STOP)?;
    if std::path::Path::new(&args.target).is_file() {
        let bytes = std::fs::read(&args.target).with_context(|| format!("reading {}", args.target))?;
        let records = capture::read(&bytes)?;
        let speed: f64 = args.get("speed").unwrap_or("1").parse().context("--speed: not a number")?;
        if !speed.is_finite() || speed <= 0.0 {
            bail!("--speed must be over 0");
        }
        let build = args.get("build").unwrap_or(stvtf::CURRENT_BUILD);
        relay.replay(&records, &args.tables(build)?, speed, &STOP, emit)?;
    } else {
        relay.watch(&watch_args(args)?, &STOP, emit)?;
    }
    Ok(())
}

fn connect(args: &Args, emit: &mut dyn FnMut(Line)) -> Result<()> {
    let w = watch_args(args)?;
    watch::stop_on_ctrl_c(&STOP)?;
    watch::watch(&w, &STOP, emit)?;
    Ok(())
}

/// What to watch, from the flags `connect` and `relay` share.
fn watch_args(args: &Args) -> Result<Watch> {
    let password = match args.get("password") {
        Some(p) => p.to_string(),
        None => std::env::var("STVTF_PASSWORD").unwrap_or_default(),
    };
    let mut w = Watch::new(&args.target, &password);
    if let Some(n) = args.get("name") {
        w.name = n.to_string();
    }
    w.snapshot_hz = if args.command == "relay" { 0.0 } else { args.hz()? };
    w.tables = args.get("tables").map(PathBuf::from);
    w.crc = args
        .get("crc")
        .map(|h| u32::from_str_radix(h.trim_start_matches("0x"), 16).with_context(|| format!("--crc {h}: not hex")))
        .transpose()?;
    w.build = args.get("build").map(str::to_string);
    w.duration = args
        .get("seconds")
        .map(|s| -> Result<Duration> {
            let secs: f64 = s.parse().with_context(|| format!("--seconds {s}: not a number"))?;
            Duration::try_from_secs_f64(secs).with_context(|| format!("--seconds {s}: must be 0 or more"))
        })
        .transpose()?;
    w.record = args.get("record").map(PathBuf::from);
    if args.command == "connect" {
        w.relay = args.get("relay").map(|listen| relay_config(args, listen)).transpose()?;
    }
    Ok(w)
}

/// The relay's password and slots from the flags; a password not given is made up and shown.
fn relay_config(args: &Args, listen: &str) -> Result<RelayConfig> {
    let mut config = RelayConfig::new(listen);
    match args.get("relay-password").map(str::to_string).or_else(|| std::env::var("STVTF_RELAY_PASSWORD").ok()) {
        Some(p) => config.password = p,
        None => eprintln!("relay password: {}", config.password),
    }
    if let Some(n) = args.get("relay-slots") {
        config.slots = n.parse().with_context(|| format!("--relay-slots {n}: not a number"))?;
        if !(1..=255).contains(&config.slots) {
            bail!("--relay-slots must be 1 to 255");
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Args>> {
        parse_args(args.iter().map(ToString::to_string))
    }

    #[test]
    fn parses_flags_both_ways() {
        let a = parse(&["connect", "1.2.3.4:27020", "--password", "tv", "--hz=10"]).unwrap().unwrap();
        assert_eq!(a.command, "connect");
        assert_eq!(a.target, "1.2.3.4:27020");
        assert_eq!(a.get("password"), Some("tv"));
        assert_eq!(a.hz().unwrap(), 10.0);
    }

    #[test]
    fn rejects_typos_and_misplaced_flags() {
        assert!(parse(&["connect", "h:1", "--pasword", "tv"]).is_err());
        assert!(parse(&["replay", "x.cap", "--record", "y.cap"]).is_err());
        assert!(parse(&["demo", "a.dem", "b.dem"]).is_err());
        assert!(parse(&["demo", "a.dem", "--hz", "1", "--hz", "2"]).is_err());
        assert!(parse(&["frobnicate", "x"]).is_err());
    }

    #[test]
    fn help_and_no_args_show_usage() {
        assert!(parse(&["--help"]).unwrap().is_none());
        assert!(parse(&["connect", "-h"]).unwrap().is_none());
        assert!(parse(&[]).unwrap().is_none());
    }

    #[test]
    fn validates_numbers() {
        assert!(parse(&["demo", "a.dem", "--hz", "-1"]).unwrap().unwrap().hz().is_err());
        assert!(parse(&["demo", "a.dem", "--hz", "NaN"]).unwrap().unwrap().hz().is_err());
    }
}
