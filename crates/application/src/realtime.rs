use car_logger_domain::{CanFrame, CanId, DecodedSignalValue};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

/// CAN IDごとの最新表示状態。ペイロードは `latest_frame.data` を唯一の所有元とする。
#[derive(Debug, Clone, PartialEq)]
pub struct RealtimeSignalState {
    pub latest_frame: CanFrame,
    pub last_seen: DateTime<Utc>,
    pub count: u64,
    pub decoded_values: Vec<DecodedSignalValue>,
    pub is_known: bool,
}

impl RealtimeSignalState {
    fn new(frame: CanFrame, decoded_values: Vec<DecodedSignalValue>, is_known: bool) -> Self {
        Self {
            last_seen: frame.received_at,
            count: 1,
            latest_frame: frame,
            decoded_values,
            is_known,
        }
    }

    fn update(&mut self, frame: CanFrame, decoded_values: Vec<DecodedSignalValue>, is_known: bool) {
        self.last_seen = frame.received_at;
        self.latest_frame = frame;
        self.decoded_values = decoded_values;
        self.is_known = is_known;
        self.count = self.count.saturating_add(1);
    }
}

/// 最新状態表示専用の並行状態ストア。
///
/// 全履歴は保持せず、CAN IDごとに最後に見えたフレームとdecode済み値だけを差し替える。
#[derive(Debug, Default)]
pub struct RealtimeState {
    signals: DashMap<CanId, RealtimeSignalState>,
}

impl RealtimeState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert_unknown(&self, frame: CanFrame) {
        self.upsert(frame, Vec::new(), false);
    }

    pub fn upsert_known(&self, frame: CanFrame, decoded_values: Vec<DecodedSignalValue>) {
        self.upsert(frame, decoded_values, true);
    }

    fn upsert(&self, frame: CanFrame, decoded_values: Vec<DecodedSignalValue>, is_known: bool) {
        match self.signals.entry(frame.id) {
            Entry::Occupied(mut entry) => entry.get_mut().update(frame, decoded_values, is_known),
            Entry::Vacant(entry) => {
                entry.insert(RealtimeSignalState::new(frame, decoded_values, is_known));
            }
        }
    }

    pub fn snapshot(&self) -> Vec<(CanId, RealtimeSignalState)> {
        let mut rows = Vec::with_capacity(self.signals.len());
        rows.extend(
            self.signals
                .iter()
                .map(|entry| (*entry.key(), entry.value().clone())),
        );
        rows.sort_unstable_by_key(|(id, _)| *id);
        rows
    }

    pub fn clear(&self) {
        self.signals.clear();
    }

    pub fn len(&self) -> usize {
        self.signals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.signals.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;

    fn frame(id: CanId, byte: u8) -> CanFrame {
        CanFrame::new(id, false, false, vec![byte])
    }

    #[test]
    fn update_reuses_the_frame_payload_as_the_single_source_of_truth() {
        let state = RealtimeState::new();
        state.upsert_unknown(frame(0x100, 1));
        state.upsert_known(frame(0x100, 2), Vec::new());

        let snapshot = state.snapshot();
        assert_eq!(snapshot[0].1.latest_frame.data, vec![2]);
        assert_eq!(snapshot[0].1.count, 2);
        assert!(snapshot[0].1.is_known);
    }

    #[test]
    fn concurrent_updates_do_not_lose_counts() {
        const THREADS: usize = 4;
        const UPDATES: usize = 250;
        let state = Arc::new(RealtimeState::new());
        let handles = (0..THREADS)
            .map(|_| {
                let state = Arc::clone(&state);
                thread::spawn(move || {
                    for value in 0..UPDATES {
                        state.upsert_unknown(frame(0x123, value as u8));
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(state.snapshot()[0].1.count, (THREADS * UPDATES) as u64);
    }

    #[test]
    fn snapshot_is_sorted_and_clear_resets_the_store() {
        let state = RealtimeState::new();
        state.upsert_unknown(frame(2, 0));
        state.upsert_unknown(frame(1, 0));
        assert_eq!(
            state.snapshot().iter().map(|row| row.0).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(state.len(), 2);

        state.clear();
        assert!(state.is_empty());
    }
}
