use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use car_logger_domain::SignalKind;
use chrono::{DateTime, TimeDelta, Utc};
use duckdb::params;

use crate::DuckdbCanFrameRepository;

/// Defines when raw frames become eligible for loss-aware, one-second compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogRetentionPolicy {
    pub raw_retention: TimeDelta,
    /// Bounds one maintenance pass so startup/idle maintenance remains responsive.
    pub max_frames_per_run: usize,
}

impl Default for LogRetentionPolicy {
    fn default() -> Self {
        Self {
            raw_retention: TimeDelta::days(14),
            max_frames_per_run: 50_000,
        }
    }
}

impl LogRetentionPolicy {
    pub fn cutoff(self, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
        ensure!(
            self.raw_retention > TimeDelta::zero(),
            "rawログ保持期間は正数が必要です"
        );
        ensure!(
            self.max_frames_per_run > 0,
            "1回の回収件数は1以上が必要です"
        );
        now.checked_sub_signed(self.raw_retention)
            .context("rawログ保持期限を計算できませんでした")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogCompactionReport {
    pub raw_frames_removed: u64,
    pub second_buckets_written: u64,
    pub has_more: bool,
}

#[derive(Debug)]
struct RawFrame {
    sequence_id: i64,
    signal_type: String,
    can_id: u32,
    is_extended: bool,
    is_remote: bool,
    data: Vec<u8>,
    received_at: DateTime<Utc>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct BucketKey {
    signal_type: String,
    can_id: u32,
    is_extended: bool,
    is_remote: bool,
    epoch: i64,
}

#[derive(Debug)]
struct SecondBucket {
    first_data: Vec<u8>,
    last_data: Vec<u8>,
    min_data: Vec<u8>,
    max_data: Vec<u8>,
    frame_count: u64,
    change_count: u64,
    first_received_at: DateTime<Utc>,
    last_received_at: DateTime<Utc>,
}

impl SecondBucket {
    fn new(frame: &RawFrame) -> Self {
        Self {
            first_data: frame.data.clone(),
            last_data: frame.data.clone(),
            min_data: frame.data.clone(),
            max_data: frame.data.clone(),
            frame_count: 1,
            change_count: 0,
            first_received_at: frame.received_at,
            last_received_at: frame.received_at,
        }
    }

    fn push(&mut self, frame: &RawFrame) {
        self.change_count += u64::from(self.last_data != frame.data);
        self.last_data.clone_from(&frame.data);
        if frame.data < self.min_data {
            self.min_data.clone_from(&frame.data);
        }
        if frame.data > self.max_data {
            self.max_data.clone_from(&frame.data);
        }
        self.frame_count += 1;
        self.last_received_at = frame.received_at;
    }
}

fn aggregate(frames: &[RawFrame]) -> BTreeMap<BucketKey, SecondBucket> {
    let mut buckets = BTreeMap::new();
    for frame in frames {
        let key = BucketKey {
            signal_type: frame.signal_type.clone(),
            can_id: frame.can_id,
            is_extended: frame.is_extended,
            is_remote: frame.is_remote,
            epoch: frame.received_at.timestamp(),
        };
        buckets
            .entry(key)
            .and_modify(|bucket: &mut SecondBucket| bucket.push(frame))
            .or_insert_with(|| SecondBucket::new(frame));
    }
    buckets
}

impl DuckdbCanFrameRepository {
    /// Compacts one bounded batch. PID frames are removed only after health
    /// backfill has durably marked their sequence range as complete.
    pub fn compact_logs(
        &mut self,
        now: DateTime<Utc>,
        policy: LogRetentionPolicy,
    ) -> Result<LogCompactionReport> {
        let cutoff = policy.cutoff(now)?;
        self.compact_logs_before(cutoff, policy.max_frames_per_run)
    }

    pub fn compact_logs_before(
        &mut self,
        cutoff: DateTime<Utc>,
        max_frames: usize,
    ) -> Result<LogCompactionReport> {
        ensure!(
            !self.read_only,
            "DuckDBログは読み取り専用のため回収できません"
        );
        ensure!(max_frames > 0, "1回の回収件数は1以上が必要です");

        let processed_pid_sequence: i64 = self.connection.query_row(
            "SELECT coalesce(max(last_sequence_id), 0) FROM health_backfill_state WHERE operation='backfill' AND completed=true",
            [],
            |row| row.get(0),
        )?;
        let query_limit = i64::try_from(max_frames)?.saturating_add(1);
        let mut statement = self.connection.prepare(
            r#"
            SELECT sequence_id, signal_type, can_id, is_extended, is_remote, data, epoch_us(received_at)
            FROM can_frames
            WHERE received_at < ?1
              AND (signal_type <> ?2 OR sequence_id <= ?3)
            ORDER BY sequence_id
            LIMIT ?4
            "#,
        )?;
        let rows = statement.query_map(
            params![
                cutoff.to_rfc3339(),
                SignalKind::Pid.as_str(),
                processed_pid_sequence,
                query_limit
            ],
            |row| {
                let received_at_micros: i64 = row.get(6)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    received_at_micros,
                ))
            },
        )?;
        let mut frames = rows
            .collect::<duckdb::Result<Vec<_>>>()?
            .into_iter()
            .map(
                |(
                    sequence_id,
                    signal_type,
                    can_id,
                    is_extended,
                    is_remote,
                    data,
                    received_at_micros,
                )| {
                    Ok(RawFrame {
                        sequence_id,
                        signal_type,
                        can_id,
                        is_extended,
                        is_remote,
                        data,
                        received_at: DateTime::from_timestamp_micros(received_at_micros)
                            .with_context(|| format!("不正なログ日時です: {received_at_micros}"))?,
                    })
                },
            )
            .collect::<Result<Vec<_>>>()?;
        let has_more = frames.len() > max_frames;
        frames.truncate(max_frames);
        if frames.is_empty() {
            return Ok(LogCompactionReport {
                raw_frames_removed: 0,
                second_buckets_written: 0,
                has_more: false,
            });
        }

        let buckets = aggregate(&frames);
        let transaction = self.connection.transaction()?;
        for (key, bucket) in &buckets {
            transaction.execute(
                r#"
                INSERT INTO can_frame_seconds VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                ON CONFLICT (signal_type, can_id, is_extended, is_remote, bucket_epoch) DO UPDATE SET
                    last_data=excluded.last_data,
                    min_data=least(can_frame_seconds.min_data, excluded.min_data),
                    max_data=greatest(can_frame_seconds.max_data, excluded.max_data),
                    frame_count=can_frame_seconds.frame_count + excluded.frame_count,
                    change_count=can_frame_seconds.change_count + excluded.change_count +
                        CASE WHEN can_frame_seconds.last_data <> excluded.first_data THEN 1 ELSE 0 END,
                    last_received_at=excluded.last_received_at
                "#,
                params![key.signal_type, key.can_id, key.is_extended, key.is_remote, key.epoch,
                    bucket.first_data, bucket.last_data, bucket.min_data, bucket.max_data,
                    bucket.frame_count, bucket.change_count, bucket.first_received_at.to_rfc3339(),
                    bucket.last_received_at.to_rfc3339()],
            )?;
        }
        let first_id = frames.first().expect("non-empty checked").sequence_id;
        let last_id = frames.last().expect("non-empty checked").sequence_id;
        let removed = transaction.execute(
            "DELETE FROM can_frames WHERE sequence_id BETWEEN ?1 AND ?2 AND received_at < ?3 AND (signal_type <> ?4 OR sequence_id <= ?5)",
            params![first_id, last_id, cutoff.to_rfc3339(), SignalKind::Pid.as_str(), processed_pid_sequence],
        )?;
        ensure!(
            removed == frames.len(),
            "集約したrawログ件数と削除件数が一致しません"
        );
        transaction
            .commit()
            .context("ログ回収トランザクションを確定できませんでした")?;

        Ok(LogCompactionReport {
            raw_frames_removed: removed as u64,
            second_buckets_written: buckets.len() as u64,
            has_more,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use car_logger_application::{CanFrameRepository, HealthScoreRepository};
    use car_logger_domain::CanFrame;

    fn frame(id: u32, data: &[u8], second: i64, nanos: u32) -> CanFrame {
        CanFrame {
            id,
            is_extended: false,
            is_remote: false,
            data: data.to_vec(),
            received_at: DateTime::from_timestamp(second, nanos).unwrap(),
        }
    }

    #[test]
    fn compacts_old_frames_without_losing_observation_semantics() {
        let mut repository = DuckdbCanFrameRepository::open_in_memory_with_context(1, 1).unwrap();
        repository.save(&frame(0x123, &[20], 100, 100)).unwrap();
        repository.save(&frame(0x123, &[10], 100, 200)).unwrap();
        repository.save(&frame(0x123, &[30], 100, 300)).unwrap();
        repository.save(&frame(0x123, &[30], 101, 0)).unwrap();

        let report = repository
            .compact_logs_before(DateTime::from_timestamp(200, 0).unwrap(), 100)
            .unwrap();

        assert_eq!(report.raw_frames_removed, 4);
        assert_eq!(report.second_buckets_written, 2);
        assert!(!report.has_more);
        assert!(repository.list_recent_frames(10).unwrap().is_empty());
        let observation = repository.list_can_id_observations().unwrap().remove(0);
        assert_eq!(observation.count, 4);
        assert_eq!(observation.raw_payload, vec![30]);

        let bucket: (Vec<u8>, Vec<u8>, u64, u64) = repository
            .connection
            .query_row(
                "SELECT min_data,max_data,frame_count,change_count FROM can_frame_seconds WHERE bucket_epoch=100",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(bucket, (vec![10], vec![30], 3, 2));
    }

    #[test]
    fn bounded_runs_merge_a_split_second_bucket() {
        let mut repository = DuckdbCanFrameRepository::open_in_memory_with_context(1, 1).unwrap();
        for data in [1, 1, 2] {
            repository
                .save(&frame(1, &[data], 100, data as u32))
                .unwrap();
        }

        let cutoff = DateTime::from_timestamp(200, 0).unwrap();
        let first = repository.compact_logs_before(cutoff, 2).unwrap();
        let second = repository.compact_logs_before(cutoff, 2).unwrap();

        assert!(first.has_more);
        assert!(!second.has_more);
        let aggregate: (u64, u64, Vec<u8>, Vec<u8>) = repository
            .connection
            .query_row(
                "SELECT frame_count,change_count,first_data,last_data FROM can_frame_seconds",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(aggregate, (3, 1, vec![1], vec![2]));
    }

    #[test]
    fn pid_frames_wait_for_completed_health_backfill() {
        let mut repository = DuckdbCanFrameRepository::open_in_memory_with_context(1, 1).unwrap();
        repository
            .save_with_kind(SignalKind::Pid, &frame(0x0c, &[0x1a, 0xf8], 100, 0))
            .unwrap();

        let cutoff = DateTime::from_timestamp(200, 0).unwrap();
        assert_eq!(
            repository
                .compact_logs_before(cutoff, 10)
                .unwrap()
                .raw_frames_removed,
            0
        );

        while !repository.backfill(10).unwrap().completed {}
        assert_eq!(
            repository
                .compact_logs_before(cutoff, 10)
                .unwrap()
                .raw_frames_removed,
            1
        );
    }

    #[test]
    fn policy_rejects_invalid_maintenance_bounds() {
        let mut repository = DuckdbCanFrameRepository::open_in_memory_with_context(1, 1).unwrap();
        let policy = LogRetentionPolicy {
            raw_retention: TimeDelta::days(14),
            max_frames_per_run: 0,
        };
        assert!(repository.compact_logs(Utc::now(), policy).is_err());
    }
}
