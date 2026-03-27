use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use fluvio_protocol::record::Offset;
use tracing::{info, warn};

use crate::batch_header::BatchHeaderStream;
use crate::config::SharedReplicaConfig;
use crate::index::{Index, OffsetPosition, INDEX_ENTRY_SIZE};
use crate::mut_index::MutLogIndex;

/// Report of a repair operation.
#[derive(Debug)]
pub struct RepairReport {
    pub base_offset: Offset,
    pub batches_found: u32,
    pub index_entries_before: u32,
    pub index_entries_after: u32,
    pub errors: Vec<RepairError>,
}

/// Types of index corruption detected during validation.
#[derive(Debug)]
pub enum RepairError {
    PositionPastEof {
        slot: u32,
        position: u32,
        log_size: u64,
    },
    NonMonotonicOffset {
        slot: u32,
        offset: u32,
        prev_offset: u32,
    },
    PositionMismatch {
        offset: u32,
        index_pos: u32,
        actual_pos: u32,
    },
}

/// Scan a .log file and return all valid (base_offset, file_position) pairs.
pub async fn scan_log_batches(log_path: &Path) -> Result<Vec<(Offset, u32)>> {
    let mut entries = Vec::new();
    let mut stream = BatchHeaderStream::open(log_path).await?;
    while let Some(batch_pos) = stream.try_next().await? {
        let pos = batch_pos.get_pos();
        let batch = batch_pos.inner();
        entries.push((batch.get_base_offset(), pos));
    }
    Ok(entries)
}

/// Validate index entries against actual log batch positions.
///
/// Returns a list of errors found. An empty vec means the index is consistent.
/// The `index_entries` slice should contain entries already converted to native
/// byte order (via `.to_be()` on each entry from the memory-mapped index).
pub fn validate_index(
    index_entries: &[(u32, u32)],
    log_batches: &[(Offset, u32)],
    log_size: u64,
) -> Vec<RepairError> {
    let mut errors = Vec::new();
    let mut prev_offset = 0u32;

    for (slot, entry) in index_entries.iter().enumerate() {
        let (offset, position) = (entry.offset(), entry.position());

        // Empty slot signals end of valid entries
        if offset == 0 && position == 0 {
            break;
        }

        if position as u64 > log_size {
            errors.push(RepairError::PositionPastEof {
                slot: slot as u32,
                position,
                log_size,
            });
        }

        if slot > 0 && offset < prev_offset {
            errors.push(RepairError::NonMonotonicOffset {
                slot: slot as u32,
                offset,
                prev_offset,
            });
        }

        // Check if a batch actually exists at this position
        if !log_batches.iter().any(|(_, pos)| *pos == position) {
            let actual_pos = log_batches
                .iter()
                .find(|(o, _)| *o as u32 == offset)
                .map(|(_, p)| *p)
                .unwrap_or(0);
            errors.push(RepairError::PositionMismatch {
                offset,
                index_pos: position,
                actual_pos,
            });
        }

        prev_offset = offset;
    }

    errors
}

/// Rebuild an index file from log scan results.
///
/// Creates a new `MutLogIndex`, writes entries based on the scanned batches
/// while respecting the `max_index_interval` configuration, then shrinks the
/// file to its actual size.
///
/// Returns the number of entries written.
pub async fn rebuild_index(
    base_offset: Offset,
    log_batches: &[(Offset, u32)],
    config: Arc<SharedReplicaConfig>,
) -> Result<u32> {
    let mut new_index = MutLogIndex::create(base_offset, config.clone()).await?;

    let mut prev_pos: u32 = 0;
    for (batch_offset, batch_pos) in log_batches {
        let relative_offset = (*batch_offset - base_offset) as u32;
        // Use the distance between batch positions as a proxy for batch size,
        // which is what the original write_index logic uses to decide when
        // to actually commit an index entry based on max_index_interval.
        let batch_size = if *batch_pos > prev_pos {
            *batch_pos - prev_pos
        } else {
            // First batch or non-standard ordering: use a large value to
            // force the first entry to be written.
            config.index_max_interval_bytes.get() + 1
        };
        new_index
            .write_index(relative_offset, *batch_pos, batch_size)
            .await?;
        prev_pos = *batch_pos;
    }

    // len() returns first_empty_slot * INDEX_ENTRY_SIZE
    let entries_written = (new_index.len() / INDEX_ENTRY_SIZE) as u32;
    new_index.shrink().await?;
    Ok(entries_written)
}

/// Perform index repair for a segment: scan the log and rebuild the index.
///
/// Called from `validate_and_repair()` when an index error is detected.
pub(crate) async fn repair_index(
    log_path: &Path,
    base_offset: Offset,
    config: Arc<SharedReplicaConfig>,
) -> Result<RepairReport> {
    warn!(
        base_offset,
        log_path = %log_path.display(),
        "Index corruption detected, rebuilding from log"
    );

    let log_batches = scan_log_batches(log_path).await?;
    let batches_found = log_batches.len() as u32;

    let entries_after = rebuild_index(base_offset, &log_batches, config).await?;

    info!(
        base_offset,
        batches_found,
        entries_after,
        "Index rebuilt from log scan"
    );

    Ok(RepairReport {
        base_offset,
        batches_found,
        index_entries_before: 0, // caller can fill this in if needed
        index_entries_after: entries_after,
        errors: Vec::new(),
    })
}

#[cfg(test)]
#[cfg(feature = "fixture")]
mod tests {
    use std::env::temp_dir;

    use fluvio_protocol::record::Offset;
    use flv_util::fixture::ensure_new_dir;

    use crate::config::ReplicaConfig;
    use crate::fixture::BatchProducer;
    use crate::mut_records::MutFileRecords;
    use crate::records::FileRecords;

    use super::*;

    #[fluvio_future::test]
    async fn test_scan_log_batches() {
        const BASE_OFFSET: Offset = 500;

        let test_dir = temp_dir().join("repair_scan_batches");
        ensure_new_dir(&test_dir).expect("new");

        let options = ReplicaConfig {
            base_dir: test_dir,
            segment_max_bytes: 1000,
            ..Default::default()
        }
        .shared();

        let mut msg_sink = MutFileRecords::create(BASE_OFFSET, options)
            .await
            .expect("create");

        let mut builder = BatchProducer::builder()
            .base_offset(BASE_OFFSET)
            .build()
            .expect("build");

        // Write 3 batches of 2 records each
        msg_sink.write_batch(&builder.batch()).await.expect("write");
        msg_sink.write_batch(&builder.batch()).await.expect("write");
        msg_sink.write_batch(&builder.batch()).await.expect("write");

        let log_path = msg_sink.get_path().to_owned();
        drop(msg_sink);

        let batches = scan_log_batches(&log_path).await.expect("scan");
        assert_eq!(batches.len(), 3);

        // First batch at position 0
        assert_eq!(batches[0].0, BASE_OFFSET);
        assert_eq!(batches[0].1, 0);

        // Second batch at position > 0
        assert_eq!(batches[1].0, BASE_OFFSET + 2);
        assert!(batches[1].1 > 0);

        // Third batch
        assert_eq!(batches[2].0, BASE_OFFSET + 4);
        assert!(batches[2].1 > batches[1].1);

        // Positions should be monotonically increasing
        assert!(batches[0].1 < batches[1].1);
        assert!(batches[1].1 < batches[2].1);
    }

    #[test]
    fn test_validate_index_clean() {
        // Simulate a correct index: entries match log positions
        // Note: index entries use relative offsets, not absolute
        let log_batches: Vec<(Offset, u32)> = vec![(100, 0), (102, 79), (104, 158)];

        // Index stores (relative_offset, file_position).
        let index_entries: Vec<(u32, u32)> = vec![
            (2, 79),    // relative offset 2, position 79
            (4, 158),   // relative offset 4, position 158
            (0, 0),     // empty slot
        ];

        let errors = validate_index(&index_entries, &log_batches, 237);
        assert!(errors.is_empty(), "Expected no errors for clean index");
    }

    #[test]
    fn test_validate_index_position_past_eof() {
        let log_batches: Vec<(Offset, u32)> = vec![(100, 0), (102, 79)];

        let index_entries: Vec<(u32, u32)> = vec![
            (2, 79),
            (4, 500), // position 500 > log_size 158
            (0, 0),
        ];

        let errors = validate_index(&index_entries, &log_batches, 158);
        assert!(!errors.is_empty());
        assert!(matches!(errors[0], RepairError::PositionPastEof { slot: 1, position: 500, log_size: 158 }));
    }

    #[test]
    fn test_validate_index_non_monotonic() {
        let log_batches: Vec<(Offset, u32)> = vec![(100, 0), (102, 79), (104, 158)];

        let index_entries: Vec<(u32, u32)> = vec![
            (4, 158), // offset jumps to 4
            (2, 79),  // then back to 2 - non-monotonic
            (0, 0),
        ];

        let errors = validate_index(&index_entries, &log_batches, 237);
        let has_non_monotonic = errors.iter().any(|e| matches!(e, RepairError::NonMonotonicOffset { .. }));
        assert!(has_non_monotonic, "Expected non-monotonic offset error");
    }

    #[test]
    fn test_validate_index_position_mismatch() {
        let log_batches: Vec<(Offset, u32)> = vec![(100, 0), (102, 79), (104, 158)];

        // Entry at offset 2 claims position 100, but actual is 79
        let index_entries: Vec<(u32, u32)> = vec![
            (2, 100), // wrong position - no batch at position 100
            (0, 0),
        ];

        let errors = validate_index(&index_entries, &log_batches, 237);
        let has_mismatch = errors.iter().any(|e| matches!(e, RepairError::PositionMismatch { offset: 2, index_pos: 100, .. }));
        assert!(has_mismatch, "Expected position mismatch error");
    }

    #[fluvio_future::test]
    async fn test_rebuild_index() {
        const BASE_OFFSET: Offset = 700;

        let test_dir = temp_dir().join("repair_rebuild_index");
        ensure_new_dir(&test_dir).expect("new");

        let options = ReplicaConfig {
            base_dir: test_dir,
            segment_max_bytes: 1000,
            index_max_bytes: 1000,
            index_max_interval_bytes: 0, // write every batch
            ..Default::default()
        }
        .shared();

        let mut msg_sink = MutFileRecords::create(BASE_OFFSET, options.clone())
            .await
            .expect("create");

        let mut builder = BatchProducer::builder()
            .base_offset(BASE_OFFSET)
            .build()
            .expect("build");

        msg_sink.write_batch(&builder.batch()).await.expect("write");
        msg_sink.write_batch(&builder.batch()).await.expect("write");

        let log_path = msg_sink.get_path().to_owned();
        drop(msg_sink);

        let log_batches = scan_log_batches(&log_path).await.expect("scan");
        assert_eq!(log_batches.len(), 2);

        let entries_written = rebuild_index(BASE_OFFSET, &log_batches, options)
            .await
            .expect("rebuild");
        // With interval 0 every batch triggers an index write
        assert!(entries_written > 0);
    }
}
