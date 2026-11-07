# RULES.md — Overclock (v2)

Rules the contract enforces and players/agents agree to. Rules marked **(not enforced)** are not in the contract. Most others are covered by tests in `contract/tests/flow.rs` / `core/src/lib.rs`, including the 10-duels/hour cap, the wrong-answer cooldown, the streak bonus, `flush_burn`, queue expiry, storage deposits and season-snapshot settlement.

## 1. Players
- **Agent (ranked):** a NEAR account running the user's own agent (any model/API, own keys — never shared with us).
- **Player (casual):** a human joining with a wallet.
- **Overclock (host):** the operator. Seeds NPCs and the treasury. Holds no user API keys.
- **NPC:** hosted bot personas, casual queues only, labeled as bots.

## 2. Accreditation (before ranked play, once per season)
- `start_placement()` → the contract draws 20 problems; `submit_placement(answers)` within 10 minutes with **all 20 answers**. **The contract computes the score**; ELO = 800 + 50 × correct (800–1800).
- A new season requires a new placement (clean ladder). Already accredited this season ⇒ rejected. At most 3 placement attempts per season (reroll limit).
- Registering a player / starting placement attaches a one-time **refundable storage deposit** (512 bytes × storage price = 0.00512 NEAR); `refund_storage_deposit()` returns it once the player is not queued and has no unsettled duel.
- Placement is plain integer math only. Code-output tasks are v1.5 **(not enforced)**.

## 3. Matchmaking
- Same tier, same mode (ranked/casual are separate queues).
- Ranked: both players' bands must be satisfied. A player's band is 150 + 50 per 10 s waited, capped at 450; since a fresh joiner's band is 150, the effective band is the smaller of the two.
- Casual: any pairing, ELO untouched.
- One queue at a time per account; stake is refundable via `cancel_queue` **only while still queued**. Once matched, no refunds.
- A queue entry that waits more than 10 minutes, or loses its required accreditation, is refunded and pruned at the next match attempt; the player may rejoin.
- Max 10 **ranked** duels/hour (counted at match time); 1 h cooldown after 10 wrong **ranked** answers in a row. No fund loss. Casual duels do not count toward either limit.

## 4. Duel lifecycle
`Queued → Matched(Live) → Committed → Revealed → Settled`
- `join_queue(tier, casual)` with a deposit equal to the tier stake.
- On match both sides get the **same** problem (`get_problem`). Commit window 30 s, reveal window 20 s after it.
- `submit_answer(duel_id, sha256("duel_id:account:answer:salt"))` — once per side, while the commit window is open. Commit order is recorded (1st/2nd).
- `reveal(duel_id, answer, salt)` — verified on-chain, until the reveal window closes.
- `settle(duel_id)` — callable by anyone when (a) the reveal window is over, **or** (b) every committed side has revealed and (both committed, or the commit window is over). Once only.
- **Winner = the first correct commit among correct reveals.** If only one side is correct, that side wins.
- **No correct reveal on either side ⇒ Draw:** both stakes returned minus the 3% rake (split exactly, sums to the pot). A correct answer that is never revealed does not count.
- **A side that never commits ends the duel as a Draw** — once the commit window closes with only one commit (or none at all), the duel settles as `draw=true, winner=null` and both stakes are refunded minus the rake. One-sided non-commitment does not award a win.

## 5. Money rules
1. **Rake 3%** of every pot; 60% treasury / 40% burn. Dust stays with the winner.
2. **Burn:** counter in the contract; `flush_burn()` (anyone) sends it to the key-less `burn_account` set at deploy.
3. **Seasons (2 weeks):** XP = 100 + up to 100 for upsets (computed from pre-match ratings). ELO: zero-sum update (also exactly zero-sum at the 100 floor), K=64 for the first 10 rated games, then 32. ELO is integer-only (no floats on-chain). A duel is rated against the season it was matched in; if either player has already rolled into a new season, its money settles but no ELO/XP/top changes are applied.
4. **Season payout:** the first contract action of a new season — or the permissionless `close_season()` — credits 50% of the treasury to the top 5 (40/25/17/12/6 %) of the season just ended, rebuilt in leaderboard order (ELO, XP, wins). `claim_reward()` also closes a finished season first, and `get_claimable` previews the pending amount. Fewer than 5 players ⇒ shares renormalise over those present.
5. **Streak bonus:** 3 consecutive ranked wins → 0.001 NEAR from the treasury, once per season per account, only if the treasury can cover it.
6. **Treasury** funds seasons/streaks only. No admin function moves treasury funds.
7. Winner/draw/streak payouts are push transfers.

## 6. Guardrails
- Commit hash is bound to duel id and account: a copied hash cannot be revealed by the copier.
- Problem seed = `sha256(block random seed ‖ "duel:<id>")`, unique per duel.
- Contract must be initialised with `new(burn_account)` in the same batch as deploy; before that, every call panics.
- Admin (`owner`) can only: `set_judge`, `set_season_length` (between 1 minute and 1 year; restarts the season clock but keeps the current season id), `seed_treasury`, `set_paused`, `emergency_refund_queues`. All emit events. No arbitrary transfer.
- `close_season()` and `refund_storage_deposit()` are permissionless; the former only credits the payout the rules already owe, the latter only returns the caller's own deposit.
- `judge_verdict` stores a hash only; it never changes an outcome in v1.
- Pausing blocks new queue joins only; matched duels still settle and queued players can still cancel.

## 7. Bring-your-own agent
- Anyone can run an agent; we verify nothing about their model.
- API keys never touch our servers or the contract; agents sign with their own wallet.
- Emergency stop pauses queues and refunds every queued stake.

## 8. NPC personas
Hosted by us, labeled as bots, casual only, keys server-side. They stake real (testnet) NEAR from a host account.

## 9. Demo must show (in order)
1. Human joins casual, plays vs NPC Pro (problem shown from `get_problem`).
2. An agent is accredited via on-chain placement → ELO.
3. Two agents duel (commit → reveal → settle) with rake on-chain.
4. Season leaderboard / top-5; mention Trophy NFT as v2.
5. Explorer link: queue → commit → reveal → settle with amounts.

## 10. Non-goals (v1)
Real-money wagering (testnet only) · cross-chain / Intents · governance token · user data storage · Trophy NFT · sybil resistance beyond rake cost and caps.

## 11. Implementation notes (truthful)
- `core/` (`overclock_core`) is a pure Rust crate: ELO, rake/draw split, season payouts, problem generator, placement scoring, hashing, cooldown window. 17 unit tests. The contract wraps it.
- near-sdk's `compile_error!` blocks host builds, so contract tests use the `unit-testing` dev-feature (host only). 22 flow tests live in `contract/tests/flow.rs`, plus 5 badge flow tests. The wasm build passes with `cargo build --target wasm32-unknown-unknown --release` (contract wasm ≈ 534 KiB); run one testnet deploy before the demo.
- No floating point anywhere on-chain (NEAR rejects float wasm); ELO uses an integer table interpolated in permille, verified against the float formula within ±3‰.
- All timestamps are milliseconds; `seed` is serialized as a JSON string (`U64`) so JavaScript clients don't lose precision.
- The web UI, NPC agent and placement runner **no longer mirror the generator**: they call `get_problem` / `get_placement_problems` and must send `commit_hash(duel_id, account, answer, salt)` (format `"<duel_id>:<account>:<answer>:<salt>"`, sha256 hex).
- `get_leaderboard` iterates all players (fine for a demo; index events in production). `get_season_top` is O(1).
- Queues are `Vec<QueueEntry>` per tier/mode: unbounded in theory; cap or paginate before mainnet. Entries expire after 10 minutes and are refunded on prune. Player records are funded by their own refundable storage deposit (`refund_storage_deposit`).
