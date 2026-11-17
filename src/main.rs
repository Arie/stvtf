use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use stvtf::decode::{self, Tables};
use stvtf::output::Stamped;
use stvtf::relay::{DEFAULT_SLOTS, MAX_SLOTS, Playback, Relay, RelayConfig, Want};
use stvtf::watch::{self, Watch};
use stvtf::{Line, Options, capture};

/// Headless TF2 `SourceTV` spectator emitting top-down overview data as JSON lines.
#[derive(Parser)]
#[command(
    name = "stvtf",
    version,
    arg_required_else_help = true,
    after_help = "\
HOST:PORT is the SourceTV port (tv_port); given the game port, it tries game port + 5.
The password can also come from STVTF_PASSWORD, which other local users cannot read the way they
can read command-line arguments. Tables of the build reported by the server are built in for known
builds; pass --tables (any demo recorded on that build) for others."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Watches a `SourceTV` server, writing its lines.
    Connect {
        /// The `SourceTV` address.
        #[arg(value_name = "HOST:PORT")]
        target: String,
        #[command(flatten)]
        server: Server,
        #[command(flatten)]
        hz: Hz,
        /// Stops after this many seconds.
        #[arg(long, value_name = "N", value_parser = seconds, allow_negative_numbers = true)]
        seconds: Option<Duration>,
        /// Records the traffic to a capture, for `replay`.
        #[arg(long, value_name = "FILE.cap")]
        record: Option<PathBuf>,
        /// Also passes the server on to TF2 clients that connect here, as `tv_relay` does, at no
        /// cost of a slot on the server.
        #[arg(long, value_name = "HOST:PORT")]
        relay: Option<String>,
        #[command(flatten)]
        relay_to: RelayTo,
    },
    /// Writes the lines of a capture.
    Replay {
        #[arg(value_name = "FILE.cap")]
        file: PathBuf,
        #[command(flatten)]
        hz: Hz,
        #[command(flatten)]
        build: Build,
    },
    /// Writes the lines of a demo.
    Demo {
        #[arg(value_name = "FILE.dem")]
        file: PathBuf,
        #[command(flatten)]
        hz: Hz,
    },
    /// Only relays, printing only status lines: a server; a capture, replayed once the first client
    /// connects; or a demo, played from the start to clients that can join any time.
    Relay {
        #[arg(value_name = "HOST:PORT|FILE.cap|FILE.dem")]
        target: String,
        /// Where TF2 clients connect.
        #[arg(long, default_value = "0.0.0.0:27020")]
        listen: String,
        #[command(flatten)]
        relay_to: RelayTo,
        #[command(flatten)]
        server: Server,
        /// How much faster than the game a capture or demo plays.
        #[arg(long, default_value_t = 1.0, value_parser = speed, allow_negative_numbers = true)]
        speed: f64,
    },
}

/// The server watched.
#[derive(Args)]
struct Server {
    /// Its `tv_password`.
    #[arg(long, env = "STVTF_PASSWORD", hide_env_values = true, default_value = "")]
    password: String,
    /// Our name on the server.
    #[arg(long)]
    name: Option<String>,
    #[command(flatten)]
    build: Build,
    /// Overrides the `SendTable` CRC the tables give.
    #[arg(long, value_name = "HEX", value_parser = hex)]
    crc: Option<u32>,
}

/// The build whose tables decode.
#[derive(Args)]
struct Build {
    /// A demo recorded on the server's build, for a build not built in.
    #[arg(long, value_name = "SAME_BUILD.dem")]
    tables: Option<PathBuf>,
    /// The build offered (`A2S_INFO` `version`), else the server's.
    #[arg(long, value_name = "N")]
    build: Option<String>,
}

#[derive(Args)]
struct Hz {
    /// Snapshots a second (0: none).
    #[arg(long, default_value_t = stvtf::DEFAULT_SNAPSHOT_HZ, value_parser = stvtf::parse_snapshot_hz, allow_negative_numbers = true)]
    hz: f32,
}

/// The relay's clients.
#[derive(Args)]
struct RelayTo {
    /// Their password; made up and printed to stderr if not given.
    #[arg(long, value_name = "PW", env = "STVTF_RELAY_PASSWORD", hide_env_values = true)]
    relay_password: Option<String>,
    /// How many of them at once.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_SLOTS, value_parser = slots)]
    relay_slots: usize,
}

/// Set by Ctrl-C, or when nobody reads the output any more.
static STOP: AtomicBool = AtomicBool::new(false);

fn seconds(s: &str) -> Result<Duration> {
    let secs: f64 = s.parse().context("cannot read a number")?;
    Duration::try_from_secs_f64(secs).context("cannot be below 0")
}

fn speed(s: &str) -> Result<f64> {
    let speed: f64 = s.parse().context("cannot read a number")?;
    anyhow::ensure!(speed.is_finite() && speed > 0.0, "cannot be 0, below or infinite");
    Ok(speed)
}

fn hex(s: &str) -> Result<u32> {
    u32::from_str_radix(s.trim_start_matches("0x"), 16).context("cannot read hex")
}

fn slots(s: &str) -> Result<usize> {
    let n: usize = s.parse().context("cannot read a number")?;
    anyhow::ensure!((1..=MAX_SLOTS).contains(&n), "cannot be outside 1 to {MAX_SLOTS}");
    Ok(n)
}

impl Build {
    /// The tables of `--build`, or of the current build.
    fn tables(&self) -> Result<Tables> {
        let build = self.build.as_deref().unwrap_or(stvtf::builds::CURRENT);
        match stvtf::builds::tables(build, self.tables.as_deref()) {
            Err(e @ stvtf::Error::UnknownBuild { .. }) => Err(anyhow::anyhow!("{e}: pass --tables SAME_BUILD.dem")),
            tables => Ok(tables?),
        }
    }
}

fn read(file: &Path) -> Result<Vec<u8>> {
    std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))
}

fn main() -> Result<()> {
    // The library's log on stderr; `RUST_LOG` sets what (default info).
    let level = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    tracing_subscriber::fmt()
        .with_env_filter(level)
        .with_target(false)
        .with_ansi(tty)
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let live = matches!(cli.command, Command::Connect { .. } | Command::Relay { .. });
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
    match cli.command {
        Command::Demo { file, hz } => decode::demo_file(&read(&file)?, hz.hz, &mut emit)?,
        Command::Replay { file, hz, build } => {
            let bytes = read(&file)?;
            let opt = Options { snapshot_hz: hz.hz, ..Options::default() };
            capture::replay(&capture::read(&bytes)?, &build.tables()?, &opt, &mut emit)?;
        }
        Command::Relay { target, listen, relay_to, server, speed } => {
            relay(&target, &relay_config(&relay_to, &listen), &server, speed, &mut |line| {
                if matches!(line, Line::Status(_)) {
                    emit(line);
                }
            })?;
        }
        Command::Connect { target, server, hz, seconds, record, relay, relay_to } => {
            let mut w = watch_of(&target, &server);
            w.snapshot_hz = hz.hz;
            w.duration = seconds;
            w.record = record;
            w.relay = relay.map(|listen| relay_config(&relay_to, &listen));
            watch::stop_on_ctrl_c(&STOP)?;
            watch::watch(&w, &STOP, &mut emit)?;
        }
    }
    // Ignore a final flush error: it is the same closed pipe already handled above.
    let _ = out.flush();
    Ok(())
}

fn relay(target: &str, config: &RelayConfig, server: &Server, speed: f64, emit: &mut dyn FnMut(Line)) -> Result<()> {
    let relay = Relay::bind(config)?;
    watch::stop_on_ctrl_c(&STOP)?;
    let file = Path::new(target);
    if file.is_file() && file.extension().is_some_and(|e| e.eq_ignore_ascii_case("dem")) {
        let at = std::time::Instant::now();
        let want = Want { demo: file.to_owned(), tick: 0, at, paused: false, speed, from_start: true, camera: None };
        relay.play(&Playback::new(want), &STOP, emit)?;
    } else if file.is_file() {
        let bytes = read(file)?;
        relay.replay(&capture::read(&bytes)?, &server.build.tables()?, speed, &STOP, emit)?;
    } else {
        let mut w = watch_of(target, server);
        w.snapshot_hz = 0.0;
        relay.watch(&w, &STOP, emit)?;
    }
    Ok(())
}

/// What to watch, from the flags `connect` and `relay` share.
fn watch_of(target: &str, server: &Server) -> Watch {
    let mut w = Watch::new(target, &server.password);
    if let Some(n) = &server.name {
        w.name.clone_from(n);
    }
    w.tables.clone_from(&server.build.tables);
    w.crc = server.crc;
    w.build.clone_from(&server.build.build);
    w
}

/// The relay's password and slots; a password not given is made up and shown.
fn relay_config(to: &RelayTo, listen: &str) -> RelayConfig {
    let mut config = RelayConfig::new(listen);
    match &to.relay_password {
        Some(p) => config.password.clone_from(p),
        None => eprintln!("relay password: {}", config.password),
    }
    config.slots = to.relay_slots;
    config
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn flags_go_either_way_and_numbers_are_checked() {
        let Cli { command: Command::Connect { target, server, hz, .. } } =
            Cli::try_parse_from(["stvtf", "connect", "1.2.3.4:27020", "--password", "tv", "--hz=10"]).unwrap()
        else {
            panic!("not connect")
        };
        assert_eq!((target.as_str(), server.password.as_str(), hz.hz), ("1.2.3.4:27020", "tv", 10.0));
        for bad in [
            &["demo", "a.dem", "--hz", "-1"][..],
            &["demo", "a.dem", "--hz", "NaN"],
            &["connect", "h:1", "--pasword", "tv"],
        ] {
            assert!(Cli::try_parse_from(std::iter::once("stvtf").chain(bad.iter().copied())).is_err(), "{bad:?}");
        }
    }
}
