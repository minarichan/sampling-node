//! SQLite copy of sampling progress and the upstream peer score.
//!
//! The node still samples against the in-memory store. This module loads that
//! store at startup and writes it back after each round, so a restarted process
//! skips shares it has already verified.

use std::path::Path;

use da_light_core::{DaError, SampleCoordinate};
use rusqlite::{params, Connection};

use crate::peer_manager::PeerManager;
use crate::state::{MemoryStore, StoredHeader};

pub(crate) struct StateDb {
    conn: std::sync::Mutex<Connection>,
}

impl StateDb {
    pub(crate) fn open(
        path: &Path,
        upstream_id: &str,
        upstream_endpoint: &str,
    ) -> Result<(Self, MemoryStore, PeerManager), DaError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    DaError::Message(format!(
                        "could not create directory for sampling state {}: {err}",
                        parent.display()
                    ))
                })?;
            }
        }
        let conn = Connection::open(path).map_err(|err| {
            DaError::Message(format!(
                "could not open sampling state {}: {err}",
                path.display()
            ))
        })?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS headers (
                id TEXT PRIMARY KEY,
                height INTEGER NOT NULL,
                commitment BLOB NOT NULL,
                total_shares INTEGER NOT NULL,
                failed_samples INTEGER NOT NULL,
                last_newly_sampled INTEGER NOT NULL,
                last_failures TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS successes (
                header_id TEXT NOT NULL,
                row INTEGER NOT NULL,
                col INTEGER NOT NULL,
                PRIMARY KEY (header_id, row, col)
            );
            CREATE TABLE IF NOT EXISTS peers (
                id TEXT PRIMARY KEY,
                score INTEGER NOT NULL
            );
            ",
        )
        .map_err(|err| DaError::Message(format!("could not prepare sampling state: {err}")))?;

        let store = load_headers(&conn)?;
        let score = load_score(&conn, upstream_id)?;
        let peers = PeerManager::with_score(upstream_id, upstream_endpoint, score);
        Ok((
            Self {
                conn: std::sync::Mutex::new(conn),
            },
            store,
            peers,
        ))
    }

    pub(crate) fn save(&self, store: &MemoryStore, peers: &PeerManager) -> Result<(), DaError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| DaError::Message("sampling state lock poisoned".into()))?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|err| DaError::Message(format!("could not write sampling state: {err}")))?;
        tx.execute_batch(
            "
            DELETE FROM successes;
            DELETE FROM headers;
            DELETE FROM peers;
            ",
        )
        .map_err(|err| DaError::Message(format!("could not write sampling state: {err}")))?;

        for row in store.export() {
            let failures = serde_json::to_string(&row.last_failures).map_err(|err| {
                DaError::Message(format!("could not encode sample failures: {err}"))
            })?;
            tx.execute(
                "
                INSERT INTO headers (
                    id, height, commitment, total_shares, failed_samples, last_newly_sampled, last_failures
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ",
                params![
                    row.header.id.0,
                    i64::try_from(row.header.height).unwrap_or(i64::MAX),
                    row.header.commitment.0,
                    i64::from(row.header.total_shares),
                    i64::from(row.failed_samples),
                    i64::from(row.last_newly_sampled),
                    failures,
                ],
            )
            .map_err(|err| DaError::Message(format!("could not write sampling state: {err}")))?;
            for coord in &row.successful {
                tx.execute(
                    "INSERT INTO successes (header_id, row, col) VALUES (?1, ?2, ?3)",
                    params![row.header.id.0, i64::from(coord.row), i64::from(coord.col)],
                )
                .map_err(|err| {
                    DaError::Message(format!("could not write sampling state: {err}"))
                })?;
            }
        }
        for peer in peers.peers() {
            tx.execute(
                "INSERT INTO peers (id, score) VALUES (?1, ?2)",
                params![peer.id, i64::from(peer.score)],
            )
            .map_err(|err| DaError::Message(format!("could not write sampling state: {err}")))?;
        }
        tx.commit()
            .map_err(|err| DaError::Message(format!("could not write sampling state: {err}")))?;
        Ok(())
    }
}

fn load_headers(conn: &Connection) -> Result<MemoryStore, DaError> {
    let mut statement = conn
        .prepare(
            "
            SELECT id, height, commitment, total_shares, failed_samples, last_newly_sampled, last_failures
            FROM headers
            ",
        )
        .map_err(|err| DaError::Message(format!("could not read sampling state: {err}")))?;
    let header_rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|err| DaError::Message(format!("could not read sampling state: {err}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| DaError::Message(format!("could not read sampling state: {err}")))?;
    drop(statement);

    let mut stored = Vec::new();
    for (id, height, commitment, total_shares, failed_samples, last_newly_sampled, failures) in
        header_rows
    {
        let last_failures = serde_json::from_str(&failures)
            .map_err(|err| DaError::Message(format!("could not decode sample failures: {err}")))?;
        let mut successful = std::collections::HashSet::new();
        let mut coords = conn
            .prepare("SELECT row, col FROM successes WHERE header_id = ?1")
            .map_err(|err| DaError::Message(format!("could not read sampling state: {err}")))?;
        let coord_rows = coords
            .query_map(params![id], |coord| {
                Ok((coord.get::<_, i64>(0)?, coord.get::<_, i64>(1)?))
            })
            .map_err(|err| DaError::Message(format!("could not read sampling state: {err}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| DaError::Message(format!("could not read sampling state: {err}")))?;
        for (row, col) in coord_rows {
            successful.insert(SampleCoordinate {
                row: u32::try_from(row).unwrap_or(u32::MAX),
                col: u32::try_from(col).unwrap_or(u32::MAX),
            });
        }
        stored.push(StoredHeader {
            header: da_light_core::Header {
                id: da_light_core::HeaderId(id),
                height: u64::try_from(height).unwrap_or(0),
                commitment: da_light_core::Commitment(commitment),
                total_shares: u32::try_from(total_shares).unwrap_or(0),
            },
            successful,
            failed_samples: u32::try_from(failed_samples).unwrap_or(0),
            last_newly_sampled: u32::try_from(last_newly_sampled).unwrap_or(0),
            last_failures,
        });
    }
    Ok(MemoryStore::import(stored))
}

fn load_score(conn: &Connection, upstream_id: &str) -> Result<i32, DaError> {
    match conn.query_row(
        "SELECT score FROM peers WHERE id = ?1",
        params![upstream_id],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(score) => Ok(i32::try_from(score).unwrap_or(i32::MAX)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(err) => Err(DaError::Message(format!(
            "could not read sampling state: {err}"
        ))),
    }
}
