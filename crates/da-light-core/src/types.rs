use serde::{Deserialize, Serialize};

use crate::error::DaError;

/// Identifier of a data-availability header. Opaque to the core; adapters choose the encoding.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HeaderId(pub String);

impl std::fmt::Display for HeaderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Published data-availability header that sampling is performed against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub id: HeaderId,
    pub height: u64,
    pub commitment: Commitment,
    pub total_shares: u32,
}

/// Commitment to the full set of shares. Bytes are interpreted by the adapter
/// (Merkle root, KZG commitment, or another scheme).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Commitment(pub Vec<u8>);

/// Position of one share in the data square.
///
/// Shares are laid out row-major. The width of the square is [`square_width`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SampleCoordinate {
    pub row: u32,
    pub col: u32,
}

/// Raw bytes of a single share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sample(pub Vec<u8>);

/// Proof that [`Sample`] occupies [`SampleCoordinate`] under a [`Commitment`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SampleProof(pub Vec<u8>);

/// Side length of the square used to map flat share indexes to coordinates.
pub fn square_width(total_shares: u32) -> u32 {
    if total_shares <= 1 {
        return total_shares;
    }
    let mut side = (f64::from(total_shares)).sqrt().ceil() as u32;
    if side == 0 {
        side = 1;
    }
    while u64::from(side) * u64::from(side) < u64::from(total_shares) {
        side = side.saturating_add(1);
    }
    side
}

/// Flat row-major index of `coord` inside a square of `total_shares` shares.
pub fn share_index(coord: SampleCoordinate, total_shares: u32) -> Result<u32, DaError> {
    if total_shares == 0 {
        return Err(DaError::CoordinateOutOfRange);
    }
    let side = square_width(total_shares);
    if coord.row >= side || coord.col >= side {
        return Err(DaError::CoordinateOutOfRange);
    }
    let index = u64::from(coord.row) * u64::from(side) + u64::from(coord.col);
    if index >= u64::from(total_shares) {
        return Err(DaError::CoordinateOutOfRange);
    }
    Ok(u32::try_from(index).unwrap_or(u32::MAX))
}

/// Inverse of [`share_index`] for an index that is known to be in range.
pub fn coordinate_from_index(index: u32, total_shares: u32) -> SampleCoordinate {
    let side = square_width(total_shares).max(1);
    SampleCoordinate {
        row: index / side,
        col: index % side,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_width_covers_the_share_count() {
        let cases = [(0, 0), (1, 1), (2, 2), (3, 2), (4, 2), (5, 3), (256, 16)];
        for (total, expected) in cases {
            assert_eq!(square_width(total), expected, "total {total}");
        }
    }

    #[test]
    fn indexes_round_trip_through_coordinates() {
        for total in [1, 2, 3, 7, 15, 16, 17, 100, 255, 256] {
            for index in 0..total {
                let coord = coordinate_from_index(index, total);
                assert_eq!(share_index(coord, total).unwrap(), index);
            }
        }
    }

    #[test]
    fn rejects_coordinates_past_the_partial_last_row() {
        // 3 shares fit in a 2x2 square; row 1 col 1 would be index 3.
        let coord = SampleCoordinate { row: 1, col: 1 };
        assert_eq!(
            share_index(coord, 3).unwrap_err(),
            DaError::CoordinateOutOfRange
        );
        assert_eq!(
            share_index(SampleCoordinate { row: 0, col: 0 }, 0).unwrap_err(),
            DaError::CoordinateOutOfRange
        );
    }
}
