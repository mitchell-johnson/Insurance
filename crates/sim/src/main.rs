//! `ic-simulate`: run the synthetic insurer from the command line.
//!
//! ```text
//! ic-simulate [--seed N] [--policies N] [--members N] [--days N] [--partitions N] [--no-journal]
//! ```

use ic_sim::year::{run_year, YearConfig};

fn main() {
    let mut cfg = YearConfig::small(20271115);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).and_then(|v| v.parse::<u64>().ok());
        match (args[i].as_str(), value) {
            ("--seed", Some(v)) => cfg.seed = v,
            ("--policies", Some(v)) => cfg.retail_policies = v,
            ("--members", Some(v)) => cfg.scheme_members = v as u32,
            ("--days", Some(v)) => cfg.days = v as u32,
            ("--partitions", Some(v)) => cfg.partitions = v as u16,
            ("--no-journal", _) => {
                cfg.verify_journal = false;
                i += 1;
                continue;
            }
            (other, _) => {
                eprintln!("unknown or incomplete argument: {other}");
                std::process::exit(2);
            }
        }
        i += 2;
    }
    let started = std::time::Instant::now();
    match run_year(&cfg) {
        Ok((_, report)) => {
            println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
            eprintln!(
                "clean: {} days, {} entries, {:.1}s",
                report.days,
                report.journal_entries,
                started.elapsed().as_secs_f64()
            );
        }
        Err(e) => {
            eprintln!("FAILED: {e}");
            std::process::exit(1);
        }
    }
}
