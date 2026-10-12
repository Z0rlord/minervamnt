# Design note: reserve, settle, release, and signed receipts

Status: proposal. Design only, no code change. The mint service stays offline
and nothing here changes the deployed behavior.

## Origin and attribution

The shape of this note is adapted from the payment-control flow in
[jooray/routstr-confidential](https://github.com/jooray/routstr-confidential)
(MIT, see `docs/APPROACH.md`, "Negotiation and billing") and its settlement
counterpart [Routstr/routstr-core](https://github.com/Routstr/routstr-core)
(GPL-3.0). Both are Python/TypeScript payment gateways, not mints. No code was
copied. This is an independent description of the pattern, written against
Minerva's own types. Minerva is AGPL-3.0-or-later, which is compatible with the
GPL-3.0 source if code is ever ported; any such port must carry its notice.

## What the pattern is

A gateway that accepts Cashu payment for metered work does five things:

1. Reserve a worst-case amount before any work starts.
2. Freeze the rates in a signed offer so the price cannot move mid-session.
3. Settle on measured use, never above the reservation.
4. Release the whole reservation if the work never started or failed on the
   provider side. Charge in full only on defined fault paths.
5. Return a signed receipt that binds the session id, rates, usage, cost and
   reservation, so the client can check `cost <= usage x signed rates`.

## Where Minerva already matches

- NUT-04/05 quotes carry `expiry` and a state machine (`QuoteState`).
- `melt_quote` already returns `fee_reserve`, which is a reservation ceiling.
- PoL epoch roots are a signed, chained, timestamped record of liabilities.

## Where the pattern applies to the mint (concrete gaps)

These are observations from reading `src/mint_backend.rs` on `main`. They are
noted because the pattern addresses them directly. They are not fixed here.

1. Melt is not reserve-then-settle. `melt()` spends and records burns
   (`verify_and_spend_inputs`, `record_melt_burns`) before
   `ark.pay_lightning_invoice`. If the payout fails, inputs are already burned
   and the quote is not moved back. A reserve step should mark inputs
   `PENDING`, and settle or release after the payout result is known.
2. No change on overpayment. When `input_total > amount + fee_reserve`, the
   difference is not returned. NUT-08 blank outputs are the standard way to
   return the unused part of `fee_reserve`. This is the "settle on actual
   cost" half of the pattern.
3. Melt quotes live in an in-memory `HashMap`. A restart drops any reservation.
   Reservations need to be persisted (SQLite, as the spent store already is).
4. A reserved-but-unresolved state needs a timeout and an explicit release
   path, so funds are never held indefinitely.

## Proposed design (mint side)

State machine for a melt:

`UNPAID -> RESERVED(inputs pending, expiry) -> SETTLED(actual fee, change) | RELEASED(inputs unspent)`

- Reserve: validate inputs, mark them pending in the spent store, persist the
  reservation with an expiry. Do not burn.
- Settle: on confirmed payout, burn the inputs, compute `actual_fee`, sign
  change for the unused `fee_reserve`, move to `SETTLED`.
- Release: if the payout never started or definitively failed, unmark the
  inputs and move to `RELEASED`.
- Unknown outcome (timeout, ambiguous ASP response): keep `RESERVED`, do not
  charge and do not release until reconciled. Never treat an unknown result as
  success or as a clean failure. This mirrors the PASS/FAIL/INCONCLUSIVE
  discipline in the routstr-confidential test rig.
- Never let a missing or unusable measurement turn the full reservation into
  the charge. Routstr states this rule explicitly in its payments docs.

## Signed receipts (mint side)

Add an optional signed receipt for mint and melt operations: quote id, unit,
amount, fee actually charged, reservation outcome, timestamp, keyset id.
Sign it with a key whose public part is published in `/v1/info`. Chain the
receipt hash into the next PoL epoch so a receipt can be checked against a
published root. This extends the existing PoL design rather than adding a
second ledger.

Receipts are evidence of what the mint says it did. They are not an
independent solvency attestation.

## Client and gateway side (out of scope for the mint crate)

The signed-offer and frozen-rate half belongs to a metered service in front of
a mint, not to the mint:

- Signed offer with a short TTL (routstr keeps offers about 10 minutes) and a
  bounded cache, with an explicit `offer_expired` error.
- Reserve `max(min_request, ceil(limit_in x rate_in + max_out x rate_out))`.
- Verify signature, session id and `cost <= usage x signed rates` on the client.

If Minerva ever ships a wallet or gateway, this is the reference. It should be
a separate crate or repo, not part of the mint.

## Category rule

A Cashu token is payment authority. It is bearer value, not caller identity.
A receipt proves a payment event happened, not who the caller is. Any caller
authentication (for example the Membrane's caller proof) must remain a
separate mechanism.

## Deliberately not taken

- The two-party TLS and VOLE-ZK construction in routstr-confidential. It is
  unaudited, designed for a different threat (hiding prompts from a relay),
  and unrelated to mint solvency.
- Any Python code from routstr-core, the SQL-model balance table, and its
  API-key balance model. Minerva is a mint, not a prepaid-key gateway.
- Lightning invoice and top-up flows from routstr-core. The mint already has
  its own quote paths.
- Their "attestation". It is a node-signed session statement, not hardware
  attestation, and the proofs are designated-verifier, so a third party
  cannot check them. Do not reuse the word for Minerva receipts.

## Known seams, noted only

Not addressed here: custodial design, mock defaults, the README and trust
model naming the signatory differently, proofs not yet being an independent
solvency attestation, and the README license badge disagreeing with the
`LICENSE` file and `Cargo.toml` (AGPL-3.0-or-later).

## Suggested next steps

1. Persist melt quotes and add `RESERVED/SETTLED/RELEASED` with tests for
   payout failure and restart.
2. Add NUT-08 change for unused `fee_reserve`.
3. Receipt format and signing key, then chain into PoL epochs.
