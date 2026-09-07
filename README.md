# Veil

A minimal Ironwood wallet: one Rust package, `veild` as the background process,
and `veil` as a one-shot JSON-RPC client. It supports create, restore, address,
sync, balance, send, and transaction history. Signing keys persist for unattended
use. Submission is currently a local mock; PIR and zonion are deferred.

## Build

Requires Rust 1.97.1, a C/C++ toolchain, CMake, pkg-config, libclang, and OpenSSL
development headers. The pinned development dependencies live beside this repo.
The setup script checks existing revisions and never resets an existing checkout.

```sh
./scripts/setup-deps.sh
cargo build --locked --bins
./scripts/check.sh
```

`cargo fmt --package veil` formats this package. The check script and CI fail on
`cargo fmt --package veil -- --check`. Avoid `cargo fmt --all` here: Cargo would
also format the sibling path dependencies.

## Run

Copy `examples/mainnet.toml` and supply explicit native peers that provide both
Zakura header sync with commitment auxiliaries and ztreamer compact blocks. The
Zakura patch in `patches/` is also needed on the provider for live-tip auxiliaries.
Paths in the config are relative to the config file. Mainnet parameters are
implemented; the exercised end-to-end network is regtest.

```sh
target/debug/veild --config examples/mainnet.toml
# In another terminal, after headers have caught up:
target/debug/veil --config examples/mainnet.toml status
target/debug/veil --config examples/mainnet.toml create
target/debug/veil --config examples/mainnet.toml address
target/debug/veil --config examples/mainnet.toml sync
target/debug/veil --config examples/mainnet.toml balance
target/debug/veil --config examples/mainnet.toml send "$RECIPIENT" --amount-zatoshis 100000000
target/debug/veil --config examples/mainnet.toml transactions
```

`create` returns the recovery phrase, address, and birthday as JSON. Save the
phrase and birthday privately. To restore, use a fresh data directory and
`veil restore --mnemonic-file /path/to/phrase --birthday HEIGHT`. Use `-` for
redirected stdin. The earliest supported birthday is Ironwood activation.

`send` builds and proves a real Ironwood transaction, stores it in the wallet,
and writes raw bytes to `data_dir/mock-submissions/<txid>.tx`. Its response says
`"submission": "mocked"`; Veil does not broadcast it. Pending sends reserve their
inputs through the wallet library. Fees and confirmation policy come from that
library. Only Ironwood funds can be spent; receivers omit transparent and Sapling.

The RPC binds only loopback and requires the bearer token in `rpc.cookie`; the
CLI reads it automatically. The data directory is private and protected against
concurrent daemon opens. The recovery phrase is age-encrypted in SQLite, with a
separate mode-0600 `encryption-identity.txt` for noninteractive signing. This
protects a database-only copy, not a copy of the entire directory. Back up the
phrase and birthday, or the entire directory while the daemon is stopped.

## Data flow

```text
veil → cookie-authenticated jsonrpsee → veild
                                      ├─ serialized wallet access → wallet-libraries SQLite
                                      ├─ embedded Zakura → validated, selected headers
                                      └─ native ztreamer service → compact blocks / tree states
                                             ↓
                               ZIP-221 root authentication → scan → balance
```

`network/node.rs` embeds Zakura in its existing HeadersOnly engine mode. It
commits the embedded genesis body once, gates body services, and retains finalized
headers and auxiliary data for restart and historical restore. It does not run
ongoing block-body sync. `network/compact.rs` provides bounded native P2P requests.

`network/verify.rs` reuses Zakura's history tree and commitment verifier. A
successor header authenticates the preceding block's roots; supplied tree-state
frontiers must match all three authenticated roots. Compact commitments are
appended and checked against those roots before scanning. A matching block hash
alone is insufficient. Scanning remains one header behind the selected tip and
fails closed when required auxiliaries are unavailable. Initial authentication
replays auxiliary history from the relevant network-upgrade boundary.

`sync.rs` coordinates bounded scans, transaction enhancement and library-supported
reorg truncation. A reorg beyond retained wallet checkpoints requires restore.
`wallet.rs`, `storage.rs`, and `keys.rs` own accounts, persistence, and key access.
`send.rs` uses the library's proposal and proof flow. `daemon.rs` serializes sync,
account creation, restore, and sends; blocking database/proof work runs off the
async executor. There is one account and no general plugin or transport framework.

## Tests

```sh
CARGO_PROFILE_TEST_OPT_LEVEL=0 cargo test --locked --lib --test wallet_storage
cargo build --locked --bins --example regtest_fund
(cd ../ztreamer-veil && CARGO_TARGET_DIR=../veil/target CARGO_PROFILE_DEV_DEBUG=0 cargo build --locked -p ztreamerd)
python3 tests/regtest.py
```

The integration test requires Docker. It uses a digest-pinned Ironwood-capable
zcashd fixture, a real ztreamer full node, and Veil over native P2P. The harness
funds Ironwood, checks receipt, verifies that mock submission did not broadcast,
submits the captured signed bytes to zcashd, mines and checks change, advances
past header finality, restarts, verifies continued header progress, and restores
the original phrase and birthday into a fresh header node.
It also checks RPC authentication. Containers and processes are cleaned up;
failed runs retain private fixture data and logs for diagnosis.

The unit checks also compare wallet and node activation heights on both networks,
reject a mismatched Ironwood frontier despite a matching block hash, and cover
encrypted storage, exclusive opens, network mismatch, and seed recovery.

The small `regtest_fund` example is a fixture helper: it shields a mature
transparent coinbase using the real transaction builder, since the pinned
zcashd wallet does not construct Ironwood sends itself.

The checked-in patches capture the isolated `zakura-veil` and `ztreamer-veil`
development checkouts. They include native service gating, header-only startup
and persistence, exact-hash auxiliary serving, matching crypto dependency pins,
and a regtest-only empty funding-stream allocation fix.
