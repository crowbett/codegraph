//! codegraph benchmarks: accuracy against SCIP ground truth (and, later, scale).

mod accuracy;
mod corpus;
mod scipdata;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "codegraph-bench", about = "codegraph benchmarks")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Clone the pinned corpus and build SCIP ground-truth indexes.
    Prepare {
        /// Only these repos.
        #[arg(long = "repo")]
        repos: Vec<String>,
        /// Rebuild SCIP indexes even when up to date.
        #[arg(long)]
        force: bool,
    },
    /// Index the corpus with codegraph and score it against SCIP.
    Accuracy {
        /// Only these repos.
        #[arg(long = "repo")]
        repos: Vec<String>,
        /// Write the JSON results here.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Compare against a previous results JSON and print the delta.
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Rewrite the results table in docs/ACCURACY.md.
        #[arg(long)]
        write_docs: bool,
        /// Dump misses and false edges per repo to bench/.cache/report/.
        #[arg(long)]
        report: bool,
    },
    /// Print occurrences of a SCIP index on one file/line (debugging).
    Dump {
        repo: String,
        path: String,
        #[arg(long)]
        line: Option<u32>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let dirs = corpus::Dirs::new();
    match cli.cmd {
        Cmd::Prepare { repos, force } => corpus::prepare(&dirs, &repos, force),
        Cmd::Accuracy {
            repos,
            out,
            baseline,
            write_docs,
            report,
        } => accuracy::run(&dirs, &repos, out, baseline, write_docs, report),
        Cmd::Dump { repo, path, line } => {
            let s = scipdata::Scip::load(&dirs.scip(&repo))?;
            if path == "-" {
                for (a, set) in &s.implements {
                    for b in set {
                        println!("IMPL {a} -> {b}");
                    }
                }
                return Ok(());
            }
            for o in s.occs.get(&path).into_iter().flatten() {
                if line.is_some_and(|l| l != o.line + 1) {
                    continue;
                }
                println!(
                    "{}:{}-{} {} {}",
                    o.line + 1,
                    o.start,
                    o.end,
                    if o.is_def { "DEF" } else { "ref" },
                    o.symbol
                );
            }
            Ok(())
        }
    }
}
