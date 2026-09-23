use std::sync::Arc;

use da_adapter_mock::{MockConfig, MockNetwork};
use da_light_core::{coordinate_from_index, DANetwork};
use da_light_node::{Node, NodeConfig};

#[tokio::test]
async fn full_sample_of_an_available_square_reaches_the_statistical_confidence() {
    let network = MockNetwork::generate(MockConfig {
        share_count: 16,
        header_count: 1,
        withheld_per_header: 0,
        share_size: 32,
    })
    .unwrap();
    let node = Arc::new(
        Node::new(
            Arc::new(network) as Arc<dyn DANetwork>,
            NodeConfig {
                samples_per_header: 16,
                ..NodeConfig::default()
            },
        )
        .unwrap(),
    );

    let report = node.sample_latest().await.unwrap();
    let expected = 1.0 - 0.75_f64.powf(16.0);
    assert_eq!(report.successful_samples, 16);
    assert_eq!(report.failed_samples, 0);
    assert!((report.confidence - expected).abs() < 1e-12);
    assert_eq!(report.level, "Very High");

    let status = node.status().await;
    assert_eq!(status.peers[0].id, "mock-local");
    assert_eq!(status.peers[0].score, 16);
}

#[tokio::test]
async fn withheld_shares_lower_the_success_count_and_the_peer_score() {
    let network = MockNetwork::generate(MockConfig {
        share_count: 16,
        header_count: 1,
        withheld_per_header: 4,
        share_size: 32,
    })
    .unwrap();
    let node = Arc::new(
        Node::new(
            Arc::new(network) as Arc<dyn DANetwork>,
            NodeConfig {
                samples_per_header: 16,
                ..NodeConfig::default()
            },
        )
        .unwrap(),
    );

    let report = node.sample_latest().await.unwrap();
    assert_eq!(report.successful_samples, 12);
    assert_eq!(report.failed_samples, 4);
    assert!(report.confidence < 1.0 - 0.75_f64.powf(16.0));

    let mut failed: Vec<_> = report
        .last_failures
        .iter()
        .map(|failure| (failure.row, failure.col))
        .collect();
    failed.sort();
    let expected: Vec<_> = (12..16)
        .map(|index| {
            let coord = coordinate_from_index(index, 16);
            (coord.row, coord.col)
        })
        .collect();
    assert_eq!(failed, expected);

    let status = node.status().await;
    assert_eq!(status.peers[0].score, 12 - 4);
}
