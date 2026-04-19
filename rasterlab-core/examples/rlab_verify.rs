//! Verify and optionally repair a `.rlab` project file.
//!
//! Usage:
//!   cargo run --release --example rlab_verify -- <file.rlab>
//!   cargo run --release --example rlab_verify -- --repair <file.rlab>
//!   cargo run --release --example rlab_verify -- --repair-to repaired.rlab <file.rlab>

use std::path::PathBuf;
use std::process;

use rasterlab_core::project::verify_and_repair;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let (repair, repair_to_override, file) = parse_args(&args);

    let repair_to: Option<PathBuf> = if repair {
        Some(repair_to_override.unwrap_or_else(|| file.clone()))
    } else {
        None
    };

    match verify_and_repair(&file, repair_to.as_deref()) {
        Ok(report) => {
            if report.file_hash_ok && report.damaged_chunks.is_empty() {
                println!("OK  {}", file.display());
                println!("    file hash:  ok");
                println!(
                    "    ecc chunk:  {}",
                    if report.recc_present {
                        "present"
                    } else {
                        "absent"
                    }
                );
            } else {
                println!("DAMAGED  {}", file.display());
                println!(
                    "    file hash:  {}",
                    if report.file_hash_ok { "ok" } else { "FAILED" }
                );
                if !report.damaged_chunks.is_empty() {
                    println!("    bad chunks: {}", report.damaged_chunks.join(", "));
                }
                println!(
                    "    ecc chunk:  {}",
                    if report.recc_present {
                        "present"
                    } else {
                        "absent (cannot repair)"
                    }
                );

                if report.repaired {
                    let out = repair_to.as_ref().unwrap();
                    println!("    REPAIRED -> {}", out.display());
                } else if repair {
                    eprintln!("    repair FAILED — too much damage or no RECC chunk");
                    process::exit(2);
                } else {
                    eprintln!("    pass --repair to attempt recovery");
                    process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

fn parse_args(args: &[String]) -> (bool, Option<PathBuf>, PathBuf) {
    let mut repair = false;
    let mut repair_to: Option<PathBuf> = None;
    let mut file: Option<PathBuf> = None;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--repair" => repair = true,
            "--repair-to" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --repair-to requires a path");
                    process::exit(1);
                }
                repair = true;
                repair_to = Some(PathBuf::from(&args[i]));
            }
            arg if arg.starts_with("--") => {
                eprintln!("error: unknown flag {arg}");
                usage();
                process::exit(1);
            }
            path => {
                file = Some(PathBuf::from(path));
            }
        }
        i += 1;
    }

    match file {
        Some(f) => (repair, repair_to, f),
        None => {
            usage();
            process::exit(1);
        }
    }
}

fn usage() {
    eprintln!("Usage: rlab_verify [--repair] [--repair-to <out.rlab>] <file.rlab>");
}
