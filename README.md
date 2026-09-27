# sampling-node

Modular data availability sampling client and light node. A resource-constrained
process verifies that block data was published by sampling random shares and
checking them against a commitment, instead of downloading the block.

Phase 1 is implemented: the `DANetwork` trait, a local adapter with Merkle
proofs, random sampling, a statistical confidence score, a CLI, and an HTTP API.
The Celestia adapter can fetch an extended header from a node. Share sampling
against that header is the next step.

## Run

```bash
cargo run -p da-light-cli -- start
```

That generates a 16×16 mock data square, draws 16 random shares, verifies each
Merkle proof, and serves the API on `http://127.0.0.1:8080`. Sixteen successful
samples at the default reconstruction threshold (25% withheld) is about 99%
confidence.

From another terminal:

```bash
cargo run -p da-light-cli -- status
cargo run -p da-light-cli -- confidence
cargo run -p da-light-cli -- sample
```

Add `--json` on the client commands for the raw response. Useful start flags:

```bash
cargo run -p da-light-cli -- start --samples 16 --shares 256 --withhold 40
cargo run -p da-light-cli -- start --network celestia --celestia-rpc http://127.0.0.1:26658
```

`--network celestia` reads extended headers and samples shares from a
celestia-node JSON-RPC endpoint (`share.GetSamples`). Pass `--celestia-token`
when that node requires a bearer token. Each share is checked with its
namespace Merkle proof against the matching row or column root, and that root
is checked against the header data root. `start` needs a reachable node.

## HTTP API

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/status` | Config, peer score, latest confidence |
| GET | `/confidence` | Latest report, or `?header=<id>` |
| GET | `/headers` | Headers this node has sampled |
| POST | `/sample` | Draw another round on the latest header |

## Confidence

Successful samples are evidence against a block that is missing enough shares
to be unreconstructable. If that missing fraction is `f` (default `0.25`) and
`s` distinct samples all verify:

```text
confidence = 1 - (1 - f) ^ s
```

`coverage` in the report is `s / total_shares`. It stays small on purpose: a
light client does not download the block. Failed samples are counted and shown,
and they do not increase `s`.

This model treats samples as independent and does not yet adjust for peer
correlation. Phase 3 can replace it with a hypergeometric model and adaptive
sampling. The engine lives in `crates/da-light-core/src/sampling/confidence.rs`.

## Layout

```text
crates/da-light-core          trait, planner, workers, Merkle verifier, confidence
crates/da-light-node          state, peer scores, axum API
crates/da-adapter-mock        in-memory DA layer with real proofs
crates/da-adapter-celestia    Celestia headers and namespace-Merkle share proofs
apps/da-light-cli             sampling-node binary
tests/integration             end-to-end sampling tests
```

The mock adapter commits shares with a 32-byte BLAKE3 Merkle root. Leaves are
tagged `0x00` and internal nodes `0x01`. A Celestia header's commitment is the
CometBFT Merkle root of the data availability header's row roots followed by
its column roots, and it must match the block `data_hash`. A sampled share
proves inclusion in one of those roots with a namespace Merkle proof. Shares
sit on a row-major square whose width is `ceil(sqrt(total_shares))`.

## Tests

```bash
cargo test --workspace
```

The Celestia adapter test runs one full sampling round through the node against
a fixture square, so that check does not need a live celestia-node.

On Windows, the GNU toolchain also needs MinGW's `dlltool` and `as` on `PATH`
(a portable copy is at `%USERPROFILE%\.local\w64devkit\bin` on this machine).
Rust should keep its own bundled linker: if both `gcc` and
`x86_64-w64-mingw32-gcc` are on `PATH`, hide the prefixed one so `rustc` does
not pick it up. A new terminal already has `cargo` from rustup.

## Next

Phase 2, still open: retry shares a peer does not return, persist sampling
state, and sample from more than one peer.
Phase 3: row/column-aware sampling for 2D Reed-Solomon layouts, metrics, and a
small dashboard.
