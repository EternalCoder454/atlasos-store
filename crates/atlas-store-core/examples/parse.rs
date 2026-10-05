//! Timing tool, not a test: parses a gzipped AppStream catalog, writes and
//! reads the index in a scratch directory and prints the numbers.
//!   cargo run --release -p atlas-store-core --example parse -- <appstream.xml.gz> [lang]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use atlas_store_core::appstream::{
    Kind, ParseOptions,
    index::{self, FORMAT, IndexKey},
    parse_gz_file,
};

fn peak_rss_kib() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmHWM:").map(|v| v.trim().to_string()))
        })
        .unwrap_or_else(|| "unknown".into())
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: parse <appstream.xml.gz> [lang]");
        return ExitCode::from(2);
    };
    let langs: Vec<String> = args.next().into_iter().collect();
    let opts = ParseOptions {
        origin: "flathub".into(),
        langs,
        ..ParseOptions::default()
    };

    let t = Instant::now();
    let cat = match parse_gz_file(path.as_ref(), &opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("parse failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let parse_t = t.elapsed();
    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &cat.components {
        let k = match c.kind {
            Kind::DesktopApp => "desktop-app",
            Kind::ConsoleApp => "console-app",
            Kind::Addon => "addon",
            Kind::Runtime => "runtime",
            Kind::Other => "other",
        };
        *by_kind.entry(k).or_default() += 1;
    }
    println!(
        "components: {} (skipped {})",
        cat.components.len(),
        cat.skipped
    );
    for (k, n) in &by_kind {
        println!("  {k}: {n}");
    }
    println!("parse: {:.1} ms", parse_t.as_secs_f64() * 1000.0);

    let dir = std::env::var_os("ATLAS_EXAMPLE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("atlas-parse-{}", std::process::id()))
        });
    let key = IndexKey {
        origin: "flathub".into(),
        commit: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
        langs: opts.langs.clone(),
        format: FORMAT,
    };
    let t = Instant::now();
    let file = match index::write(&dir, &key, &cat) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("index write failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("index write: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    println!("index size: {:.2} MB", size as f64 / 1e6);
    let mut best = f64::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        match index::read(&file, &key) {
            Ok(c) if c == cat => {}
            Ok(_) => {
                eprintln!("index read back differs");
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("index read failed: {e}");
                return ExitCode::FAILURE;
            }
        }
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!("index read (best of 5, includes comparison): {best:.1} ms");
    let t = Instant::now();
    let _ = index::read(&file, &key);
    println!(
        "index read (one more): {:.1} ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
    println!("peak RSS (VmHWM): {}", peak_rss_kib());
    let _ = std::fs::remove_dir_all(&dir);
    ExitCode::SUCCESS
}
