use serde::{Deserialize, Serialize};

use crate::types::Header;

/// One share the node asked for and could not verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleFailure {
    pub row: u32,
    pub col: u32,
    pub reason: String,
}

/// Availability report for a single header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceReport {
    pub header_id: String,
    pub height: u64,
    pub total_shares: u32,
    pub successful_samples: u32,
    pub failed_samples: u32,
    pub newly_sampled: u32,
    pub coverage: f64,
    pub confidence: f64,
    pub level: String,
    pub unavailable_fraction: f64,
    pub last_failures: Vec<SampleFailure>,
}

/// Estimates how confident we are that a block is reconstructable.
///
/// If a block is unavailable, at least `unavailable_fraction` of its shares are
/// missing (the reconstruction threshold of the erasure code). Each successful
/// uniform sample then had at most a `1 - unavailable_fraction` chance of
/// landing on data the producer was willing to serve. After `s` distinct
/// successes the probability of missing a block that is actually unavailable is
/// `(1 - unavailable_fraction) ^ s`, and confidence is one minus that quantity.
///
/// The default fraction `0.25` matches a rate-1/2 2D Reed-Solomon square, where
/// hiding a quarter of the extended shares can prevent reconstruction. Sixteen
/// successful samples at that fraction are about 99% confidence. Raw coverage
/// (`successful / total`) is reported separately because a light client never
/// downloads most of the block.
pub struct ConfidenceEngine {
    pub unavailable_fraction: f64,
}

impl ConfidenceEngine {
    pub fn new(unavailable_fraction: f64) -> Self {
        let unavailable_fraction = if unavailable_fraction.is_finite() {
            unavailable_fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self {
            unavailable_fraction,
        }
    }

    /// Probability in `0.0..=1.0` that the sampled block is available,
    /// under the reconstruction-threshold assumption above.
    pub fn calculate(&self, successful_samples: u32) -> f64 {
        if successful_samples == 0 || self.unavailable_fraction <= 0.0 {
            return 0.0;
        }
        let hidden = (1.0 - self.unavailable_fraction).powf(f64::from(successful_samples));
        (1.0 - hidden).clamp(0.0, 1.0)
    }

    pub fn level(confidence: f64) -> &'static str {
        if confidence >= 0.95 {
            "Very High"
        } else if confidence >= 0.80 {
            "High"
        } else if confidence >= 0.50 {
            "Medium"
        } else if confidence >= 0.25 {
            "Low"
        } else {
            "Very Low"
        }
    }

    pub fn report(
        &self,
        header: &Header,
        successful_samples: u32,
        failed_samples: u32,
        newly_sampled: u32,
        failures: &[SampleFailure],
    ) -> ConfidenceReport {
        let coverage = if header.total_shares == 0 {
            0.0
        } else {
            (f64::from(successful_samples) / f64::from(header.total_shares)).min(1.0)
        };
        let confidence = self.calculate(successful_samples);
        ConfidenceReport {
            header_id: header.id.0.clone(),
            height: header.height,
            total_shares: header.total_shares,
            successful_samples,
            failed_samples,
            newly_sampled,
            coverage,
            confidence,
            level: Self::level(confidence).to_string(),
            unavailable_fraction: self.unavailable_fraction,
            last_failures: failures.to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Commitment, HeaderId};

    fn header(total_shares: u32) -> Header {
        Header {
            id: HeaderId("hdr".into()),
            height: 7,
            commitment: Commitment(vec![1; 32]),
            total_shares,
        }
    }

    #[test]
    fn sixteen_samples_at_quarter_withheld_are_very_high() {
        let engine = ConfidenceEngine::new(0.25);
        let confidence = engine.calculate(16);
        let expected = 1.0 - 0.75_f64.powf(16.0);
        assert!((confidence - expected).abs() < 1e-12);
        assert_eq!(ConfidenceEngine::level(confidence), "Very High");
    }

    #[test]
    fn no_samples_have_no_confidence() {
        let engine = ConfidenceEngine::new(0.25);
        assert_eq!(engine.calculate(0), 0.0);
        assert_eq!(ConfidenceEngine::level(0.0), "Very Low");
    }

    #[test]
    fn confidence_is_monotonic_in_successful_samples() {
        let engine = ConfidenceEngine::new(0.25);
        let mut previous = 0.0;
        for samples in 0..40 {
            let confidence = engine.calculate(samples);
            assert!(confidence + 1e-12 >= previous);
            previous = confidence;
        }
    }

    #[test]
    fn level_thresholds() {
        assert_eq!(ConfidenceEngine::level(0.95), "Very High");
        assert_eq!(ConfidenceEngine::level(0.949), "High");
        assert_eq!(ConfidenceEngine::level(0.80), "High");
        assert_eq!(ConfidenceEngine::level(0.50), "Medium");
        assert_eq!(ConfidenceEngine::level(0.25), "Low");
        assert_eq!(ConfidenceEngine::level(0.249), "Very Low");
    }

    #[test]
    fn report_keeps_coverage_separate_from_confidence() {
        let engine = ConfidenceEngine::new(0.25);
        let report = engine.report(&header(256), 16, 0, 16, &[]);
        assert!((report.coverage - 16.0 / 256.0).abs() < 1e-12);
        assert!(report.confidence > 0.98);
        assert_eq!(report.level, "Very High");
        assert_eq!(report.height, 7);
    }
}
