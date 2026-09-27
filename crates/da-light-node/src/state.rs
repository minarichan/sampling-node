use std::collections::{HashMap, HashSet};

use da_light_core::{
    ConfidenceEngine, ConfidenceReport, Header, HeaderId, SampleCoordinate, SampleFailure,
    SampleOutcome, SampleResult,
};

#[derive(Debug)]
struct HeaderState {
    header: Header,
    successful: HashSet<SampleCoordinate>,
    failed_samples: u32,
    last_newly_sampled: u32,
    last_failures: Vec<SampleFailure>,
}

#[derive(Debug, Default)]
pub struct MemoryStore {
    by_id: HashMap<HeaderId, HeaderState>,
}

impl MemoryStore {
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn successful_coords(&self, id: &HeaderId) -> HashSet<SampleCoordinate> {
        self.by_id
            .get(id)
            .map(|state| state.successful.clone())
            .unwrap_or_default()
    }

    pub fn headers(&self) -> Vec<Header> {
        let mut headers: Vec<_> = self
            .by_id
            .values()
            .map(|state| state.header.clone())
            .collect();
        headers.sort_by_key(|header| header.height);
        headers
    }

    pub fn record(
        &mut self,
        header: &Header,
        results: &[SampleResult],
        engine: &ConfidenceEngine,
    ) -> ConfidenceReport {
        let state = self
            .by_id
            .entry(header.id.clone())
            .or_insert_with(|| HeaderState {
                header: header.clone(),
                successful: HashSet::new(),
                failed_samples: 0,
                last_newly_sampled: 0,
                last_failures: Vec::new(),
            });
        state.header = header.clone();

        if results.is_empty() {
            state.last_newly_sampled = 0;
        } else {
            let mut newly = 0u32;
            let mut failures = Vec::new();
            for result in results {
                if result.outcome == SampleOutcome::Verified {
                    if state.successful.insert(result.coordinate) {
                        newly = newly.saturating_add(1);
                    }
                } else {
                    state.failed_samples = state.failed_samples.saturating_add(1);
                    failures.push(SampleFailure {
                        row: result.coordinate.row,
                        col: result.coordinate.col,
                        reason: result
                            .detail
                            .clone()
                            .unwrap_or_else(|| result.outcome.label().to_string()),
                    });
                }
            }
            state.last_newly_sampled = newly;
            state.last_failures = failures;
        }

        render(state, engine)
    }

    pub fn report(&self, id: &HeaderId, engine: &ConfidenceEngine) -> Option<ConfidenceReport> {
        self.by_id.get(id).map(|state| render(state, engine))
    }

    pub fn latest_report(&self, engine: &ConfidenceEngine) -> Option<ConfidenceReport> {
        self.by_id
            .values()
            .max_by_key(|state| state.header.height)
            .map(|state| render(state, engine))
    }
}

pub(crate) struct StoredHeader {
    pub header: Header,
    pub successful: HashSet<SampleCoordinate>,
    pub failed_samples: u32,
    pub last_newly_sampled: u32,
    pub last_failures: Vec<SampleFailure>,
}

impl MemoryStore {
    pub(crate) fn export(&self) -> Vec<StoredHeader> {
        self.by_id
            .values()
            .map(|state| StoredHeader {
                header: state.header.clone(),
                successful: state.successful.clone(),
                failed_samples: state.failed_samples,
                last_newly_sampled: state.last_newly_sampled,
                last_failures: state.last_failures.clone(),
            })
            .collect()
    }

    pub(crate) fn import(rows: Vec<StoredHeader>) -> Self {
        let by_id = rows
            .into_iter()
            .map(|row| {
                (
                    row.header.id.clone(),
                    HeaderState {
                        header: row.header,
                        successful: row.successful,
                        failed_samples: row.failed_samples,
                        last_newly_sampled: row.last_newly_sampled,
                        last_failures: row.last_failures,
                    },
                )
            })
            .collect();
        Self { by_id }
    }
}

fn render(state: &HeaderState, engine: &ConfidenceEngine) -> ConfidenceReport {
    engine.report(
        &state.header,
        u32::try_from(state.successful.len()).unwrap_or(u32::MAX),
        state.failed_samples,
        state.last_newly_sampled,
        &state.last_failures,
    )
}
