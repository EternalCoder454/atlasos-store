//! Timing tool, not a test: parses a gzipped AppStream catalog, builds a
//! `Library` and times search queries on it (median and max of many runs).
//!   cargo run --release -p atlas-store-core --example search -- <appstream.xml.gz>

use std::process::ExitCode;
use std::time::{Duration, Instant};

use atlas_store_core::appstream::{ParseOptions, parse_gz_file};
use atlas_store_core::catalog::{CatalogSource, Category, Filter, Library, Sort};
use atlas_store_core::flatpak::Scope;

const RUNS: usize = 200;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: search <appstream.xml.gz>");
        return ExitCode::from(2);
    };
    let opts = ParseOptions {
        origin: "flathub".into(),
        ..ParseOptions::default()
    };
    let cat = match parse_gz_file(path.as_ref(), &opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("parse failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let total = cat.components.len();
    let source = CatalogSource {
        scope: Scope::User,
        remote: "flathub".into(),
        title: "Flathub".into(),
        priority: 0,
        dir: None,
        commit: None,
        updated: None,
    };
    let t = Instant::now();
    let lib = Library::new(vec![(source, cat)]);
    println!(
        "Library::new: {:?} ({} apps of {total} components)",
        t.elapsed(),
        lib.len()
    );

    let filters = [
        ("none", Filter::default()),
        (
            "verified+free",
            Filter {
                verified_only: true,
                free_only: true,
            },
        ),
    ];
    let queries = [
        "a",
        "e",
        "gam",
        "browser",
        "text editor",
        "video player free",
        "org.gnome",
        "xyzzyq",
        "café",
    ];
    for (fname, filter) in filters {
        for q in queries {
            let mut times: Vec<Duration> = Vec::with_capacity(RUNS);
            let mut found = 0;
            for _ in 0..RUNS {
                let t = Instant::now();
                let r = lib.search(std::hint::black_box(q), filter, 50);
                times.push(t.elapsed());
                found = r.len();
            }
            times.sort();
            println!(
                "search {q:<18} filter {fname:<13} median {:>9?} max {:>9?} ({found} shown)",
                times[RUNS / 2],
                times[RUNS - 1]
            );
        }
    }
    let t = Instant::now();
    let n = lib
        .browse(Some(Category::Games), Filter::default(), Sort::Name)
        .len();
    println!("browse Games by name: {:?} ({n} apps)", t.elapsed());
    let t = Instant::now();
    let c = lib.category_counts(Filter::default());
    println!(
        "category_counts: {:?} ({} categories)",
        t.elapsed(),
        c.len()
    );
    ExitCode::SUCCESS
}
