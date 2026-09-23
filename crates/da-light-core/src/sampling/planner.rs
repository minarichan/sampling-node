use std::collections::HashSet;

use rand::seq::SliceRandom;
use rand::thread_rng;

use crate::types::{coordinate_from_index, Header, SampleCoordinate};

/// Chooses which shares to request for a header.
///
/// The MVP strategy is uniform random sampling without replacement. Already
/// verified coordinates can be excluded so later rounds collect new evidence.
pub struct SamplePlanner {
    /// How many new samples to draw on each round.
    pub samples_per_header: u32,
}

impl SamplePlanner {
    pub fn new(samples_per_header: u32) -> Self {
        Self { samples_per_header }
    }

    /// Draw up to `samples_per_header` unique coordinates.
    pub fn plan_samples(&self, header: &Header) -> Vec<SampleCoordinate> {
        self.plan_samples_excluding(header, &HashSet::new())
    }

    /// Draw unique coordinates, skipping any that are already in `exclude`.
    pub fn plan_samples_excluding(
        &self,
        header: &Header,
        exclude: &HashSet<SampleCoordinate>,
    ) -> Vec<SampleCoordinate> {
        let total = header.total_shares;
        if total == 0 || self.samples_per_header == 0 {
            return Vec::new();
        }

        let mut indices: Vec<u32> = (0..total)
            .filter(|index| !exclude.contains(&coordinate_from_index(*index, total)))
            .collect();
        let count = usize::try_from(self.samples_per_header)
            .unwrap_or(usize::MAX)
            .min(indices.len());

        indices.shuffle(&mut thread_rng());
        indices.truncate(count);
        indices
            .into_iter()
            .map(|index| coordinate_from_index(index, total))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{share_index, Commitment, HeaderId};

    fn header(total_shares: u32) -> Header {
        Header {
            id: HeaderId("test".into()),
            height: 1,
            commitment: Commitment(vec![0; 32]),
            total_shares,
        }
    }

    #[test]
    fn plans_unique_coordinates_inside_the_square() {
        let planned = SamplePlanner::new(50).plan_samples(&header(256));
        assert_eq!(planned.len(), 50);
        let unique: HashSet<_> = planned.iter().copied().collect();
        assert_eq!(unique.len(), 50);
        for coord in planned {
            assert!(share_index(coord, 256).is_ok());
        }
    }

    #[test]
    fn never_requests_more_shares_than_exist() {
        let planned = SamplePlanner::new(100).plan_samples(&header(10));
        assert_eq!(planned.len(), 10);
    }

    #[test]
    fn skips_coordinates_that_were_already_sampled() {
        let header = header(10);
        let exclude: HashSet<_> = (0..8)
            .map(|index| coordinate_from_index(index, 10))
            .collect();
        let planned = SamplePlanner::new(5).plan_samples_excluding(&header, &exclude);
        assert_eq!(planned.len(), 2);
        assert!(planned.iter().all(|coord| !exclude.contains(coord)));
    }
}
