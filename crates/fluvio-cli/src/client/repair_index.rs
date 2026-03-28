use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;

use fluvio_storage::config::ReplicaConfig;
use fluvio_storage::repair::{self, RepairError};
use fluvio_storage::{LogIndex, OffsetPosition};

/// Offline index repair tool for SPU data directories.
///
/// Walks the data directory for partition subdirectories, enumerates .log files,
/// validates matching .index files against the log contents, and optionally
/// rebuilds corrupt indexes.
#[derive(Debug, Parser)]
pub struct RepairIndexOpt {
    /// Path to SPU data directory
    #[arg(value_name = "path")]
    data_dir: PathBuf,

    /// Only report corruption, don't fix
    #[arg(long)]
    dry_run: bool,
}

impl RepairIndexOpt {
    pub async fn process(self) -> Result<()> {
        let data_dir = &self.data_dir;
        if !data_dir.is_dir() {
            anyhow::bail!("Data directory does not exist: {}", data_dir.display());
        }

        let mut total_segments = 0u32;
        let mut total_errors = 0u32;
        let mut total_repaired = 0u32;

        // Walk data_dir for partition subdirectories.
        // Fluvio data layout: data_dir/<topic>-<partition>/
        // Each partition dir contains 00000000000000000000.log, .index pairs.
        let mut partition_dirs = Vec::new();
        collect_partition_dirs(data_dir, &mut partition_dirs)?;

        if partition_dirs.is_empty() {
            println!("No partition directories found under {}", data_dir.display());
            return Ok(());
        }

        for partition_dir in &partition_dirs {
            println!(
                "\nPartition: {}",
                partition_dir
                    .strip_prefix(data_dir)
                    .unwrap_or(partition_dir)
                    .display()
            );

            // Enumerate .log files in this partition directory
            let mut log_files: Vec<PathBuf> = Vec::new();
            let entries = std::fs::read_dir(partition_dir)?;
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("log") && path.is_file() {
                    log_files.push(path);
                }
            }
            log_files.sort();

            if log_files.is_empty() {
                println!("  No .log files found");
                continue;
            }

            for log_path in &log_files {
                total_segments += 1;

                // Derive base offset from filename (00000000000000000000.log -> offset 0)
                let stem = log_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                let base_offset: i64 = match stem.parse() {
                    Ok(v) => v,
                    Err(_) => {
                        println!("  Skipping non-standard log file: {}", log_path.display());
                        continue;
                    }
                };

                let index_path = log_path.with_extension("index");

                // Scan the log for batch positions
                let log_batches = match repair::scan_log_batches(log_path).await {
                    Ok(b) => b,
                    Err(e) => {
                        println!(
                            "  Segment {}: ERROR scanning log: {}",
                            base_offset, e
                        );
                        total_errors += 1;
                        continue;
                    }
                };

                let log_size = std::fs::metadata(log_path)?.len();

                // Read and validate the index
                let (index_entry_count, errors) = if index_path.exists() {
                    match LogIndex::open_from_path(&index_path).await {
                        Ok(index) => {
                            // LogIndex derefs to &[Entry] — entries are in big-endian on disk.
                            // validate_index expects entries with .offset()/.position() accessible,
                            // which works on the raw (big-endian stored) Entry tuples via the
                            // OffsetPosition trait. The entries from Deref are raw memory-mapped
                            // values. We need to convert them to native order for validate_index.
                            let native_entries: Vec<(u32, u32)> = index
                                .iter()
                                .map(|e| e.to_be())
                                .collect();
                            let entry_count = native_entries
                                .iter()
                                .take_while(|e| e.offset() != 0 || e.position() != 0)
                                .count() as u32;
                            let errors =
                                repair::validate_index(&native_entries, &log_batches, log_size);
                            (entry_count, errors)
                        }
                        Err(e) => {
                            println!(
                                "  Segment {}: ERROR opening index: {}",
                                base_offset, e
                            );
                            // Treat unreadable index as needing rebuild
                            (0u32, vec![RepairError::PositionPastEof {
                                slot: 0,
                                position: 0,
                                log_size,
                            }])
                        }
                    }
                } else {
                    // No index file at all — needs rebuild
                    (0u32, vec![RepairError::PositionMismatch {
                        offset: 0,
                        index_pos: 0,
                        actual_pos: 0,
                    }])
                };

                if errors.is_empty() {
                    println!(
                        "  Segment {}: OK ({} batches, {} index entries)",
                        base_offset,
                        log_batches.len(),
                        index_entry_count
                    );
                } else {
                    total_errors += 1;
                    println!(
                        "  Segment {}: CORRUPT ({} errors, {} batches, {} index entries)",
                        base_offset,
                        errors.len(),
                        log_batches.len(),
                        index_entry_count
                    );
                    for err in &errors {
                        match err {
                            RepairError::PositionPastEof {
                                slot,
                                position,
                                log_size,
                            } => {
                                println!(
                                    "    - Slot {}: position {} past EOF (log size {})",
                                    slot, position, log_size
                                );
                            }
                            RepairError::NonMonotonicOffset {
                                slot,
                                offset,
                                prev_offset,
                            } => {
                                println!(
                                    "    - Slot {}: offset {} < previous offset {}",
                                    slot, offset, prev_offset
                                );
                            }
                            RepairError::PositionMismatch {
                                offset,
                                index_pos,
                                actual_pos,
                            } => {
                                println!(
                                    "    - Offset {}: index position {} != actual position {}",
                                    offset, index_pos, actual_pos
                                );
                            }
                        }
                    }

                    if self.dry_run {
                        println!("    (dry-run: skipping rebuild)");
                    } else {
                        // Rebuild the index
                        let config = ReplicaConfig {
                            base_dir: partition_dir.clone(),
                            ..Default::default()
                        };
                        let shared_config = Arc::new(config.into());

                        match repair::rebuild_index(base_offset, &log_batches, shared_config).await
                        {
                            Ok(entries_written) => {
                                total_repaired += 1;
                                println!(
                                    "    Rebuilt: {} index entries written",
                                    entries_written
                                );
                            }
                            Err(e) => {
                                println!("    ERROR rebuilding index: {}", e);
                            }
                        }
                    }
                }
            }
        }

        println!("\n--- Summary ---");
        println!("Segments scanned: {}", total_segments);
        println!("Corrupt segments: {}", total_errors);
        if self.dry_run {
            println!("Mode: dry-run (no changes made)");
        } else {
            println!("Segments repaired: {}", total_repaired);
        }

        Ok(())
    }
}

/// Recursively collect directories that contain .log files (partition directories).
fn collect_partition_dirs(dir: &PathBuf, result: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir)?;
    let mut has_log_files = false;

    let mut subdirs = Vec::new();

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("log") {
            has_log_files = true;
        } else if path.is_dir() {
            subdirs.push(path);
        }
    }

    if has_log_files {
        result.push(dir.clone());
    }

    for subdir in subdirs {
        collect_partition_dirs(&subdir, result)?;
    }

    Ok(())
}
