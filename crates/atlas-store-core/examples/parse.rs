//! Timing tool, not a test: parses a gzipped AppStream catalog, writes and
//! reads the index in a scratch directory and prints the numbers. The scratch
//! directory is made here and removed at the end; `ATLAS_EXAMPLE_DIR` names
//! one to use instead, which is never removed (only the index written in it).
//!   cargo run --release -p atlas-store-core --example parse -- <appstream.xml.gz> [lang]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
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
        "components: {} (invalid {}, duplicate IDs {})",
        cat.components.len(),
        cat.skipped,
        cat.duplicates
    );
    for (k, n) in &by_kind {
        println!("  {k}: {n}");
    }
    println!("parse: {:.1} ms", parse_t.as_secs_f64() * 1000.0);

    // A directory is only removed at the end when this made it: `create`
    // fails on one that exists, so a named directory is never wiped.
    let dir = std::env::var_os("ATLAS_EXAMPLE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("atlas-parse-{}", std::process::id()))
        });
    let made = fs::DirBuilder::new().mode(0o700).create(&dir).is_ok();
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
            cleanup(&dir, made, None);
            return ExitCode::FAILURE;
        }
    };
    println!("index write: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    let size = fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    println!("index size: {:.2} MB", size as f64 / 1e6);
    // Only the read is timed; the comparison is outside the timer.
    let mut times = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let r = index::read(&file, &key);
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        match r {
            Ok(c) if c == cat => {}
            Ok(_) => {
                eprintln!("index read back differs");
                cleanup(&dir, made, Some(&file));
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("index read failed: {e}");
                cleanup(&dir, made, Some(&file));
                return ExitCode::FAILURE;
            }
        }
    }
    times.sort_by(f64::total_cmp);
    println!(
        "index read (5 runs, read only): best {:.1} ms, median {:.1} ms",
        times[0],
        times[times.len() / 2]
    );
    println!(
        "peak RSS of the whole run so far, parse included (VmHWM): {}",
        peak_rss_kib()
    );
    cleanup(&dir, made, Some(&file));
    ExitCode::SUCCESS
}

/// Removes what this run made: the whole directory when it created it, else
/// only the index file it wrote.
fn cleanup(dir: &std::path::Path, made: bool, file: Option<&std::path::Path>) {
    let r = if made {
        fs::remove_dir_all(dir)
    } else if let Some(f) = file {
        fs::remove_file(f)
    } else {
        Ok(())
    };
    if let Err(e) = r {
        eprintln!("can't clean up {}: {e}", dir.display());
    }
}
