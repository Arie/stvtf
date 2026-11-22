//! No source file grows past [`LIMIT`] lines of code (blank and comment lines aside). Those
//! already past it are in [`PAST`] with the most they may have: they may shrink, not grow.

use std::path::Path;

const LIMIT: usize = 500;
const PAST: &[(&str, usize)] = &[
    ("src/decode/decoder.rs", 628),
    ("src/decode/detail.rs", 598),
    ("src/decode/extract/entities.rs", 561),
    ("src/decode/extract/tests.rs", 667),
    ("src/net/handshake.rs", 636),
    ("src/output/events.rs", 557),
    ("src/relay/hub.rs", 510),
    ("src/relay/play/player.rs", 562),
    ("src/relay/tests.rs", 644),
    ("src/session.rs", 668),
];

fn sources(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn no_source_file_grows_past_the_limit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    let mut grown = Vec::new();
    for file in files {
        let name = file.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(&file).unwrap();
        let code = text.lines().filter(|l| !l.trim().is_empty() && !l.trim().starts_with("//")).count();
        let most = PAST.iter().find(|(path, _)| *path == name).map_or(LIMIT, |(_, most)| *most);
        if code > most {
            grown.push(format!("{name}: {code} lines of code, at most {most}"));
        }
    }
    assert!(grown.is_empty(), "split these:\n{}", grown.join("\n"));
}
