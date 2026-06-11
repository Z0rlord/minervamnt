# Legal disclaimer and regulatory notice

**Read this document before downloading, building, deploying, or operating
Minerva Mint or any derivative of this software.**

This document is provided for **informational purposes only**. It is **not
legal advice**. Laws and regulatory interpretations differ by country, state,
province, and municipality, and they change over time. **Consult qualified
legal counsel** in every jurisdiction where you intend to operate before
running a mint, accepting deposits, or offering ecash to third parties.

---

## 1. Experimental software — no warranty

Minerva Mint is **research and development software**. It is provided **"AS
IS"**, without warranty of any kind, express or implied, including but not
limited to warranties of merchantability, fitness for a particular purpose,
non-infringement, security, or regulatory compliance.

The authors and contributors:

- Do **not** guarantee that the software is correct, complete, or safe for
  production use.
- Do **not** guarantee that operating a mint based on this code will satisfy
  any licensing, registration, or reporting obligation.
- Are **not** responsible for losses, fines, penalties, criminal charges, or
  civil liability arising from your use or misuse of the software.

**Do not deploy this software to serve real users or real funds** until you
have completed independent security review, legal review, and operational
readiness checks appropriate to your jurisdiction and risk tolerance.

---

## 2. Money transmission and similar regulated activity

Operating a Cashu mint — software that **issues**, **redeems**, **transfers
value in exchange for consideration**, or **holds bitcoin on behalf of
others** — may be classified as one or more of the following depending on
facts, jurisdiction, and regulator interpretation:

| Concept (examples) | Why it may apply |
| ------------------ | ---------------- |
| **Money transmission** / **money services business (MSB)** | Custodial issuance and redemption of ecash backed by bitcoin |
| **Virtual asset service provider (VASP)** | Exchange, transfer, or safekeeping of virtual assets |
| **E-money / payment institution** | Stored-value instruments redeemable for fiat or crypto |
| **Securities / commodities** | If tokens or operations are deemed investment contracts or commodities |
| **Consumer protection / AML–CFT** | Handling customer funds triggers KYC, SAR, and record-keeping regimes |

**Using or deploying this software does not grant any license, exemption, or
regulatory approval.** Whether you need registration, licensing, bonding,
capital requirements, AML/KYC programs, MSB registration (e.g. with FinCEN in
the United States), state money-transmitter licenses, EU MiCA authorization,
FCA registration, or equivalent obligations depends entirely on **how** you
operate the mint, **who** your customers are, **where** they are located, and
**what** assets flow through the system.

### United States (non-exhaustive)

FinCEN and state regulators have historically treated custodial exchange and
transmission of convertible virtual currency as **money transmission** in
many fact patterns. Operating without required federal or state registration
can result in **civil and criminal penalties**. Some activities may also implicate
state **BitLicense**-style regimes, **securities** law, or **commodities**
regulation depending on structure.

### European Union and United Kingdom (non-exhaustive)

The **Markets in Crypto-Assets Regulation (MiCA)** and national implementations
may require authorization for crypto-asset services including custody and
exchange. The UK **Financial Conduct Authority** registers cryptoasset firms
for certain activities. PoL/PoR transparency features in this repository **do
not** substitute for regulatory authorization.

### Other jurisdictions

Many countries regulate digital assets, payment services, and foreign exchange.
**Assume regulation applies until counsel confirms otherwise.**

---

## 3. Operator responsibilities

If you run a mint (whether for yourself, a community, or the public), **you**
are solely responsible for:

1. **Determining** whether your activities require licenses, registrations, or
   exemptions.
2. **Implementing** AML/KYC, sanctions screening, transaction monitoring, and
   record retention if required.
3. **Tax reporting** for yourself and, where applicable, for users.
4. **Consumer disclosures** — ecash is **custodial**; users rely on the
   operator to honor redemption.
5. **Security** — key management, infrastructure hardening, incident response,
   and backup of VTXO exit material.
6. **Honest marketing** — do not represent PoL, PoR, OTS anchoring, or
   signatory separation as eliminating legal obligations or all custodial
   risk.

The trust features documented in [trust-model.md](trust-model.md) are
**technical transparency tools**. They help third parties **detect** certain
classes of misbehavior; they do **not** make the operator non-custodial in a
legal sense, and they do **not** guarantee solvency or redemption.

---

## 4. Ecash is custodial

Cashu ecash tokens represent a **claim against the mint operator**. Even with:

- Proof of Liabilities (PoL) epoch roots,
- Proof of Reserves (PoR) reconciliation,
- V-PACK verification,
- Remote signatory separation, and
- OpenTimestamps anchoring,

users still depend on the operator (and the Ark ASP) to **honor melts** and
maintain backing. Unilateral exit paths depend on correct VTXO construction,
timely refresh, and available on-chain fees. **Total loss is possible.**

---

## 5. Third-party services

Minerva Mint is designed to integrate with **Ark ASPs**, **Bitcoin Core**,
**OpenTimestamps calendars**, and optionally the **Cashu Development Kit
(CDK)**. Your use of those services is subject to **their** terms, privacy
policies, and applicable law. The Minerva Mint project does not operate those
services and is not responsible for their availability, security, or legal
status.

---

## 6. No endorsement of illegal use

This software must not be used to evade sanctions, launder money, finance
terrorism, defraud users, or violate applicable law. The authors and
contributors **condemn** such use and provide the software for lawful research
and development only.

---

## 7. Limitation of liability

To the maximum extent permitted by applicable law, the authors, contributors,
and copyright holders shall not be liable for any direct, indirect,
incidental, special, consequential, or punitive damages, or any loss of
profits, data, funds, or goodwill, arising from or related to the software or
this documentation, even if advised of the possibility of such damages.

---

## 8. Changes

This disclaimer may be updated without notice. The version in the repository
at the time you clone or deploy governs your relationship to the project
documentation; your **legal obligations** are governed by applicable law and
your own counsel's advice.

---

## 9. Summary (plain language)

| Statement | Meaning |
| --------- | ------- |
| This is not legal advice | Hire a lawyer before going live |
| Experimental software | Bugs and rug vectors may remain |
| Money transmission risk | Running a public mint may require licenses |
| Custodial ecash | Users trust the operator to redeem |
| Transparency ≠ compliance | PoL/PoR help auditors; they don't replace MSB/VASP rules |
| You are the operator | Compliance, security, and user harm are your problem |

If you are unsure whether you may operate this software in your jurisdiction,
**do not operate it until you obtain professional legal guidance.**
