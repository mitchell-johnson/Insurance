//! `ic-simulate`: run the synthetic insurer from the command line.
//!
//! ```text
//! ic-simulate [--seed N] [--policies N] [--members N] [--days N] [--partitions N] [--no-journal]
//! ```
//!
//! Exit status: 0 when every day reconciled, 1 when the run failed, 2 when
//! the arguments were not understood.

use ic_sim::year::{run_year, YearConfig};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = match YearConfig::from_args(&args) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
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
