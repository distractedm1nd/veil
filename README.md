# Veil

minimal rpc-free p2p Ironwood wallet using [PIR](https://github.com/distractedm1nd/p2p-spendability-pir), custom sync with [ztreamer](https://github.com/distractedm1nd/ztreamer), and private tx submission with [zonion](https://github.com/distractedm1nd/zonion)

DO NOT USE. INCOMPLETE ALPHA SOFTWARE.

## Run

Copy `examples/mainnet.toml` and supply explicit native peers that provide both
Zakura header sync with commitment auxiliaries and ztreamer compact blocks. The
Zakura patch in `patches/` is also needed on the provider for live-tip auxiliaries.
Paths in the config are relative to the config file. Mainnet parameters are
implemented; the exercised end-to-end network is regtest.

```sh
veild --config examples/mainnet.toml
# In another terminal, after headers have caught up:
veil --config examples/mainnet.toml status
veil --config examples/mainnet.toml create
veil --config examples/mainnet.toml address
veil --config examples/mainnet.toml sync
veil --config examples/mainnet.toml balance
veil --config examples/mainnet.toml send "$RECIPIENT" --amount-zatoshis 100000000
veil --config examples/mainnet.toml transactions
```

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
