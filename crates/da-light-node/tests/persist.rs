use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use da_adapter_mock::{MockConfig, MockNetwork};
use da_light_core::DANetwork;
use da_light_node::{Node, NodeConfig};

fn db_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("sampling-node-{}-{nanos}.db", std::process::id()))
}

fn config(path: PathBuf) -> NodeConfig {
    NodeConfig {
        samples_per_header: 2,
        data_path: Some(path),
        ..NodeConfig::default()
    }
}

#[tokio::test]
async fn a_restart_keeps_verified_shares_and_the_peer_score() {
    let path = db_path();
    let network = MockNetwork::generate(MockConfig {
        share_count: 4,
        header_count: 1,
        withheld_per_header: 0,
        share_size: 32,
    })
    .unwrap();

    let first = Node::new(
        Arc::new(network.clone()) as Arc<dyn DANetwork>,
        config(path.clone()),
    )
    .unwrap();
    let report = first.sample_latest().await.unwrap();
    assert_eq!(report.successful_samples, 2);
    assert_eq!(report.newly_sampled, 2);
    assert_eq!(first.status().await.peers[0].score, 2);
    drop(first);

    let second = Node::new(
        Arc::new(network) as Arc<dyn DANetwork>,
        config(path.clone()),
    )
    .unwrap();
    let restored = second.status().await;
    assert_eq!(restored.headers_tracked, 1);
    assert_eq!(restored.latest.unwrap().successful_samples, 2);
    assert_eq!(restored.peers[0].score, 2);

    let again = second.sample_latest().await.unwrap();
    assert_eq!(again.successful_samples, 4);
    assert_eq!(again.newly_sampled, 2);
    assert_eq!(again.failed_samples, 0);
    assert_eq!(second.status().await.peers[0].score, 4);

    let _ = std::fs::remove_file(&path);
}
