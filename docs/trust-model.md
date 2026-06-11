# Minerva Mint — Trust Model

This document describes how Minerva Mint proves solvency and limits operator
rug-pull capability. It is the public specification for auditors, wallet
integrators, and users evaluating `https://minervamnt.xyz`.

## Threat model

| Attack | Mitigation |
|--------|------------|
| Mint inflates ecash supply (sign without payment) | Remote signatory + `SignatoryPolicy` gate; PoL liability tree |
| Mint signs wrong amount vs quote | Policy checks quote amount == output sum; NUT-20 (planned) |
| Mint issues ecash without Ark backing | VTXO allocation required before sign; PoR reconciliation |
| Mint accepts fake/backdoored VTXOs | V-PACK verification at `insert_vtxo` (production mode) |
| Silent keyset rotation | Published `GET /v1/keysets`; wallets reject unknown keysets |
| Operator hides liability snapshots | PoL epoch chaining + OpenTimestamps anchoring |
| ASP steals exit path | User-held V-PACK bundles (roadmap); unilateral exit API |

**Honest limit:** Cashu is custodial ecash. Users trust the mint to redeem.
Minerva reduces—but does not eliminate—that trust by making inflation and
backing failures **detectable** rather than invisible.

## Dual attestation architecture

```text
                         ┌─────────────────────────────────────┐
                         │           Minerva Mint              │
                         │                                     │
  Wallets ──HTTP──▶      │  API (NUT + /transparency/*)        │
                         │       │                             │
                         │  SignatoryPolicy ──▶ Signatory      │
                         │  (no sign without VTXO + paid quote)  │
                         │       │                             │
                         │  ┌────┴────┐    ┌──────────────┐    │
                         │  │ PoL     │    │ VTXO verify  │    │
                         │  │ ledger  │    │ (vpack)      │    │
                         │  └────┬────┘    └──────┬───────┘    │
                         │       │                │            │
                         │  VtxoInventory ◀── ArkClient        │
                         └───────┬────────────────┬──────────┘
                                 │                │
                    PoL epoch hash (OTS)     ASP VTXOs on-chain
```

### Layer 1 — Proof of Liabilities (PoL)

**Question:** Did the mint issue more ecash than it admits?

PoL follows the [Cashu PoL specification](https://gist.github.com/victorandre957/4f497d385e1fd9a47898480903f56b3e)
(Calle's original [2023 proposal](https://gist.github.com/callebtc/ed5228d1d8cbaade0104db5d1cf63939)).

- Every **mint** (blind signature issued) is logged with keyset id and amount.
- Every **burn** (secret spent in melt/swap) is logged.
- Daily **epochs** close at midnight UTC; each epoch gets a Merkle root.
- Epochs chain via `previous_epoch_hash` (tamper-evident history).
- Closed epochs are anchored with **OpenTimestamps** on Bitcoin L1 via public
  calendar servers (`[trust.ots]` in config).

Wallets verify locally:

```text
outstanding_ecash = Σ mint_events − Σ burn_events   (per keyset)
```

Endpoints (scaffold + target):

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/v1/pol/status` | Current epoch, last closed root |
| GET | `/v1/pol/roots/{keyset_id}` | Historical Merkle roots |
| GET | `/v1/pol/ots/{epoch_day}` | OTS proof hex for a closed epoch |

### Layer 2 — Proof of Reserves (PoR)

**Question:** Is outstanding ecash backed by Ark VTXOs?

PoR is Minerva-specific (not in base Cashu):

```text
allocated_vtxo_msat = Σ token_vtxo_map.amount_msat  (active VTXOs)
active_vtxo_msat    = Σ vtxo_inventory.amount_msat    (status = active)
```

**Solvency invariant:**

```text
outstanding_ecash_msat ≤ allocated_vtxo_msat ≤ active_vtxo_msat
```

Public endpoint: `GET /transparency/summary` exposes these figures without
secrets.

Third parties run a reconciliation script against:

- PoL outstanding balance
- `/transparency/summary` VTXO totals
- ASP/on-chain data (future: libvpack + bitcoind)

### Layer 3 — VTXO verification (V-PACK)

Before any VTXO enters inventory, the mint verifies structural validity:

- **Scaffold mode:** structural checks only (development / mock ASP).
- **Vpack mode:** [libvpack-rs](https://github.com/jgmcalpine/libvpack-rs)
  `verify()` against the VTXO id and optional `vpack_hex` payload.

This prevents boarding malformed or backdoored VTXO trees from a malicious
or buggy ASP integration.

Config: `[trust] vtxo_verify_mode = "scaffold" | "vpack"`.

### Layer 4 — Signatory policy

The mint process **never holds signing keys** in production. A separate
**signatory** service (CDK `cdk-signatory` gRPC) performs BDHKE blind
signatures.

Before signing, the mint calls `SignatoryPolicy::can_sign_mint()`:

1. Quote exists and state is `PAID` (not `ISSUED`).
2. Output amounts sum equals quote amount.
3. VTXO mapping exists for this quote id (`token_vtxo_map`).
4. Amount ≤ configured `max_mint` (when set).

Swaps use a lighter policy (valid inputs, balanced outputs, no VTXO change).

A compromised API server cannot mint ecash without passing policy checks **and**
compromising the signatory.

## Transparency summary

`GET /transparency/summary` returns:

- Outstanding ecash (from PoL / inventory)
- Active and allocated VTXO msat
- Free reserve, refresh queue depth
- Active keyset ids
- Last PoL epoch id and root hash
- Build version and git commit (when set at compile time)
- VTXO verification mode

Publish weekly **signed attestations** (separate audit key) linking PoL root +
VTXO inventory hash — manual process until automated.

## Keyset policy

- One **active** keyset at launch; rotation only with public announcement.
- Wallets **must** monitor `GET /v1/keysets` for new ids.
- Inactive keysets: inputs accepted, no new outputs (NUT-02).
- PoL reports are per-keyset; tokens from inactive keysets may be unverifiable.

## Deployment trust checklist

Production go-live requires:

- [ ] `cdk-signatory` on separate host (Tailscale only)
- [ ] `mint_management_rpc.enabled = false`
- [ ] `[trust] vtxo_verify_mode = "vpack"`
- [ ] PoL epoch worker running; OTS anchoring configured
- [ ] `/transparency/summary` publicly reachable
- [ ] Open-source release tag + SHA256SUMS published
- [ ] Third-party audit of sign + board path

## Roadmap

| Item | Status |
|------|--------|
| PoL event logging + epoch stub | Scaffold |
| `/transparency/summary` | Implemented |
| SignatoryPolicy trait | Implemented |
| V-PACK verify at insert | Implemented (scaffold + vpack modes) |
| Remote CDK signatory | Planned |
| OTS epoch anchoring | Implemented (calendar POST + backfill worker) |
| User-delivered V-PACK at mint | Planned |
| NUT-20 signed quotes | Planned |
| STARK-verified mint ops | Research |

## References

- [Cashu NUTs](https://github.com/cashubtc/nuts)
- [CDK signatory](https://docs.rs/cdk-signatory/latest/cdk_signatory/)
- [PoL specification](https://gist.github.com/victorandre957/4f497d385e1fd9a47898480903f56b3e)
- [libvpack-rs](https://github.com/jgmcalpine/libvpack-rs)
- [Ark protocol](https://ark-protocol.org/)
