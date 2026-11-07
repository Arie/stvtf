//! Compiles in the data of a TF2 server build, from any demo recorded on it.
//!
//! ```text
//! cargo run --example add_build -- DEMO BUILD
//! ```
//!
//! DEMO is a .dem file, a demos.tf link (`https://demos.tf/1234567`) or a URL of a .dem.
//! BUILD is the server's build: the `build` of stvtf's `connecting` status, or `version` in
//! `A2S_INFO`, as `status` shows it on the server.
//!
//! The demo's data tables give the build's `SendTable` CRC. When a compiled-in build has the
//! same tables, as after most updates, the new build shares them. Otherwise the demo is cut
//! after its data tables into `assets/tables_BUILD.dem`. Either way the build joins `BUILDS`
//! in `src/lib.rs`; the newest one there is `CURRENT_BUILD`.

#[path = "common/demo.rs"]
mod demo;

use anyhow::{Context, Result, bail, ensure};
use demo::{DATA_TABLES, DURATION, FRAMES, HEADER, TICKS, packets};
use std::path::{Path, PathBuf};
use std::process::Command;
use stvtf::decode::Tables;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const LIB: &str = "src/lib.rs";
const BUILDS_MARK: &str = "    // add_build: builds\n";
const TABLES_MARK: &str = "// add_build: tables\n";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [source, build] = args.as_slice() else {
        bail!("usage: cargo run --example add_build -- DEMO.dem|DEMOS.TF-LINK|URL BUILD");
    };
    let number: u64 = build.parse().with_context(|| format!("BUILD {build}: not a number"))?;
    let data = load(source)?;
    let crc = Tables::from_demo_bytes(&data).context("reading the demo's data tables")?.sendtable_crc();
    eprintln!("build {build}: SendTable CRC {crc:08X}");

    let lib_path = Path::new(ROOT).join(LIB);
    let lib = std::fs::read_to_string(&lib_path)?;
    let known = known_tables(&lib)?;
    ensure!(!lib.contains(&format!("(\"{build}\", ")), "build {build} is already in {LIB}");

    let (name, lib) = if let Some((name, _)) = known.iter().find(|(_, c)| *c == crc) {
        eprintln!("same tables as {name}: nothing to add to assets/");
        (name.clone(), lib)
    } else {
        let file = format!("tables_{build}.dem");
        let cut = cut_after_data_tables(&data)?;
        let again = Tables::from_demo_bytes(&cut)?.sendtable_crc();
        ensure!(again == crc, "the cut demo gives CRC {again:08X}, not {crc:08X}");
        let path = Path::new(ROOT).join("assets").join(&file);
        std::fs::File::create_new(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, &cut))
            .with_context(|| format!("writing {}", path.display()))?;
        eprintln!("new tables: assets/{file}, {} KiB", cut.len() >> 10);
        let name = format!("TABLES_{build}");
        let line = format!("const {name}: &[u8] = include_bytes!(\"../assets/{file}\");\n");
        (name, insert_after(&lib, TABLES_MARK, &line)?)
    };
    let lib = add_build(&lib, number, &name)?;
    std::fs::write(&lib_path, &lib)?;
    eprintln!("added to {LIB}; CURRENT_BUILD is {}. Next: cargo test", lib_current(&lib)?);
    Ok(())
}

/// The demo's bytes, from a file or the web.
fn load(source: &str) -> Result<Vec<u8>> {
    let path = PathBuf::from(source);
    if path.is_file() {
        return std::fs::read(&path).with_context(|| format!("reading {source}"));
    }
    let url = match demos_tf_id(source) {
        Some(id) => {
            let info = fetch(&format!("https://api.demos.tf/demos/{id}"))?;
            let info: serde_json::Value = serde_json::from_slice(&info).context("demos.tf: not JSON")?;
            info["url"].as_str().context("demos.tf: no url for that demo")?.to_string()
        }
        None if source.starts_with("https://") || source.starts_with("http://") => source.to_string(),
        None => bail!("{source}: no such file, and not a demos.tf link or URL"),
    };
    eprintln!("downloading {url}");
    fetch(&url)
}

/// The id of a demos.tf link: `https://demos.tf/1234567`, `demos.tf/1234567`.
fn demos_tf_id(source: &str) -> Option<u64> {
    let rest = source.trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.");
    rest.strip_prefix("demos.tf/")?.trim_end_matches('/').parse().ok()
}

/// A URL's body, with curl.
fn fetch(url: &str) -> Result<Vec<u8>> {
    let out = Command::new("curl")
        .args(["--fail", "--location", "--silent", "--show-error", "--user-agent", "stvtf-add-build", url])
        .output()
        .context("running curl")?;
    ensure!(out.status.success(), "fetching {url}: {}", String::from_utf8_lossy(&out.stderr).trim());
    Ok(out.stdout)
}

/// The demo up to and with its data tables, then a stop: all `Tables` reads.
fn cut_after_data_tables(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = data[..HEADER].to_vec();
    let mut tick = None;
    for p in packets(data)? {
        out.extend_from_slice(&data[p.start..p.end]);
        if p.cmd == DATA_TABLES {
            tick = Some(p.tick);
            break;
        }
    }
    let tick = tick.context("no data tables in the demo")?;
    demo::stop(&mut out, tick);
    // No game packets are left.
    for field in [DURATION, TICKS, FRAMES] {
        out[field..field + 4].fill(0);
    }
    Ok(out)
}

/// The compiled-in tables (their const's name) and their CRCs.
fn known_tables(lib: &str) -> Result<Vec<(String, u32)>> {
    let mut known = Vec::new();
    for line in lib.lines().filter(|l| l.starts_with("const TABLES_")) {
        let name = line["const ".len()..].split(':').next().unwrap_or_default().to_string();
        let file = line.split('"').nth(1).with_context(|| format!("no path in {line:?}"))?;
        let path = Path::new(ROOT).join("src").join(file);
        let crc = Tables::from_demo(&path)?.sendtable_crc();
        known.push((name, crc));
    }
    ensure!(!known.is_empty(), "no `const TABLES_` lines in {LIB}");
    Ok(known)
}

fn insert_after(text: &str, mark: &str, line: &str) -> Result<String> {
    let at = text.find(mark).with_context(|| format!("no {:?} in {LIB}", mark.trim()))? + mark.len();
    Ok(format!("{}{line}{}", &text[..at], &text[at..]))
}

/// Puts `("BUILD", TABLES),` into `BUILDS`, keeping it newest first.
fn add_build(lib: &str, build: u64, tables: &str) -> Result<String> {
    let start = lib.find(BUILDS_MARK).with_context(|| format!("no builds mark in {LIB}"))? + BUILDS_MARK.len();
    let mut at = start;
    for line in lib[start..].split_inclusive('\n') {
        let Some(existing) = line.trim().strip_prefix("(\"").and_then(|l| l.split('"').next()) else { break };
        if existing.parse::<u64>().with_context(|| format!("build {existing:?} in {LIB}"))? < build {
            break;
        }
        at += line.len();
    }
    Ok(format!("{}    (\"{build}\", {tables}),\n{}", &lib[..at], &lib[at..]))
}

/// The first build in `BUILDS`.
fn lib_current(lib: &str) -> Result<&str> {
    let start = lib.find(BUILDS_MARK).context("no builds mark")? + BUILDS_MARK.len();
    lib[start..].split('"').nth(1).context("no builds")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB_TEXT: &str = "\
const BUILDS: &[(&str, &[u8])] = &[
    // add_build: builds
    (\"300\", TABLES_200),
    (\"100\", TABLES_100),
];
";

    #[test]
    fn builds_stay_newest_first() {
        let newest = add_build(LIB_TEXT, 400, "TABLES_400").unwrap();
        assert_eq!(lib_current(&newest).unwrap(), "400");
        let between = add_build(LIB_TEXT, 200, "TABLES_200").unwrap();
        assert!(between.contains("(\"300\", TABLES_200),\n    (\"200\", TABLES_200),\n    (\"100\""), "{between}");
        let oldest = add_build(LIB_TEXT, 50, "TABLES_100").unwrap();
        assert!(oldest.contains("(\"100\", TABLES_100),\n    (\"50\", TABLES_100),\n];"), "{oldest}");
    }

    #[test]
    fn demos_tf_links() {
        assert_eq!(demos_tf_id("https://demos.tf/1234567"), Some(1_234_567));
        assert_eq!(demos_tf_id("demos.tf/1234567/"), Some(1_234_567));
        assert_eq!(demos_tf_id("https://static.demos.tf/x.dem"), None);
    }

    #[test]
    fn the_cut_keeps_the_tables() {
        let data = std::fs::read(Path::new(ROOT).join("tests/fixtures/pl_upward.dem")).unwrap();
        let cut = cut_after_data_tables(&data).unwrap();
        assert!(cut.len() < data.len() / 4);
        let (full, cut) = (Tables::from_demo_bytes(&data).unwrap(), Tables::from_demo_bytes(&cut).unwrap());
        assert_eq!(full.sendtable_crc(), cut.sendtable_crc());
        assert_eq!(full.class_names().count(), cut.class_names().count());
    }

    #[test]
    fn the_lib_lists_its_tables() {
        let lib = std::fs::read_to_string(Path::new(ROOT).join(LIB)).unwrap();
        let current = stvtf::builtin(stvtf::CURRENT_BUILD).unwrap().tables;
        let crc = Tables::from_demo_bytes(current).unwrap().sendtable_crc();
        assert!(known_tables(&lib).unwrap().iter().any(|t| t.1 == crc));
        assert_eq!(lib_current(&lib).unwrap(), stvtf::CURRENT_BUILD);
    }
}
