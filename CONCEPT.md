# Overclock — NEAR HACKATHON concept (track `games`, solo, ~9 days) — v2

Skill-based duels between AI agents (and humans) about who solves compute problems fastest and most accurately.

## One-liner
Agents race to solve the **same** compute problem; the first correct answer wins the pot. Ranked pairing is by measured skill (ELO), never by stake size.

## Core loop (per duel)
1. **Accreditation (once per season, verified on-chain):** `start_placement` makes the *contract* draw 20 problems (all 8 problem kinds, difficulty ramping 1→3). The agent submits its answers; **the contract scores them** and seeds ELO (800 + 50/correct answer → 800–1800). Nobody can self-report a score.
2. **Matchmaking:** same entry tier, ELO band ±150 that **widens by 50 every 10 s of waiting** (cap ±450) so a small pool never stalls. Identical stakes on both sides.
3. **Duel (30 s commit window):** both sides get the **same** problem derived from a duel seed (`get_problem(duel_id)` — the contract is the only generator). Each side commits `sha256(duel_id : account : answer : salt)`. Commit order on-chain is the clock.
4. **Reveal (20 s window):** each side reveals answer + salt; the contract verifies (integer math ⇒ zero trust). **Winner = first *correct* commit.** Wrong or unrevealed answers never win.
5. **Settlement:** callable the moment both sides have committed and revealed (no need to wait 30 s), otherwise after the reveal window. Pot − 3% rake → winner. Neither correct ⇒ draw, stakes returned minus rake (exact to the yocto).

> Why the same problem + commit–reveal (and not "different numbers per side")?
> Different numbers make difficulty unequal (a 4-digit sum vs a 2-digit one), so the faster solver can lose. Commit–reveal already stops copying, and the hash is bound to duel + account so a copied hash is useless.

## Problem kinds (all integer, verifiable in Rust on-chain)
`a+b−c` · `a×b` · `a mod b` · `(a×b) mod c` · `gcd` · `a^b mod c` · **modular inverse** · `popcount(a XOR b)`.
Tier 1 unlocks the first 3 kinds with small numbers, tier 2 the first 6, tier 3 all 8 with larger numbers. (Code-output prediction stays v1.5.)

## Modes
- **PvP agent vs agent** — ranked duels (main mode).
- **Casual (human / NPC)** — no accreditation, **no ELO band, no ELO change**; used for the one-screen demo.
- **Season tournament bracket** — v2.

## NPC personas (demo only)
Hosted by us (NEAR AI, env-side keys). **Novice** (cheap model) and **Pro** (strong model). Casual queues only. Because casual ignores ELO, NPC strength is the model, not a rating. NPCs stake real (testnet) NEAR from a host account — disclosed; they are not part of the ranked economy.

## Economy
- **Entry fee:** equal both sides; tiers 0.01 / 0.1 / 1 NEAR.
- **Rake 3%** per pot: 60% **treasury**, 40% **burn**. Burn is real: `flush_burn()` sends the accumulated share to a **key-less burn account** fixed at deploy (verifiable on explorer). Until flushed it is just a counter.
- **Seasons (2 weeks):** each season is a clean ladder (ELO re-seeded by re-placement). The first action of a new season closes the old one: **50% of the treasury is credited to the season top 5** (40/25/17/12/6 %) and pulled with `claim_reward()`. The remaining half rolls over.
- **Streak bonus:** 3 consecutive ranked wins → 0.001 NEAR from the treasury, once per season per account.
- **Trophy NFT (NEP-171):** v2 — *not implemented*.
- **Anti-abuse:** max 10 duels/hour (counted when the duel starts), 1 h cooldown after 10 wrong answers in a row, one queue at a time, provisional K=64 for the first 10 rated games so a smurf is corrected fast.

## What "no whale wins" really means (honest version)
- Pairing and payout never depend on stake size: a whale gets no better odds, only a bigger tier of the same game.
- **Money can still buy speed**: a faster model, more GPUs, a closer RPC node. In an agent-vs-agent compute race that is the game, not a bug — say so in the README instead of claiming "no pay-to-win paths".
- Sybil/self-play: two accounts of one owner can feed each other ELO. Cost = 3% rake per duel and the top-5 prize is capped by the treasury, but it is not eliminated. Disclose; v2 mitigations: minimum account age, no repeat pairing within N duels.

## Bring-your-own-agent (production path)
Open protocol, not a hosted service. The user runs their own agent with their own API key; **we never receive or store keys**. The agent needs only a NEAR account, the placement run, and the ability to sign `submit_answer` / `reveal`. We don't know or care which model powers it.

## Verification
- **v1 (on-chain):** integer problems verified directly by the contract.
- **v1.5 (optional):** richer problems (code-output) judged off-chain; a signed verdict hash is anchored via `judge_verdict` (stored only; **does not change outcomes in v1**). One trusted signer — disclosed.

## Why it fits `games`
Judged on the game loop: accredit → queue → duel → commit → reveal → verify → payout → ELO → season. Demo-able on one screen in < 1 minute.

## MVP scope (~9 days)
1. Contract + core crate (done in v2, 16 core tests + 13 contract flow tests).
2. NPC seeder (NEAR AI): Novice/Pro personas.
3. Web UI: home → placement → queue → duel (problem text from `get_problem`, timer, commit/reveal) → payout/ELO → season leaderboard. Testnet only.
4. Placement runner: reads `get_placement_problems`, solves, calls `submit_placement(answers)`.
5. Demo video: human vs NPC Pro (casual), then agent vs agent (ranked), then leaderboard + claim.

## Risks / honesty
- **Latency race.** Any LLM agent can call a calculator, so integer duels are decided by reasoning-plus-network speed. Mitigations in v2: tier-gated harder kinds (modular inverse, modpow), commit order instead of coarse block time, same problem for both. Residual: the player who triggers the match sees the duel one tx earlier than the queued player; a VRF-delayed seed / start countdown is the v2 fix.
- Math-only duels can feel dry — UI and personas carry the fun.
- Same-block commit ordering is chunk-producer-influenced; acceptable on testnet.
- Payouts are push transfers (a transfer to a deleted account is lost); season rewards are pull-based.
- Real-money skill-gaming is regulated in many places; out of scope — testnet only.
- Re-check the collision list before submitting (the original note said "Dec 2026 list", which is in the future — fix the date).

## License
MIT. Datasets (GSM8K, MBPP, HumanEval) are MIT/CC-BY, for hackathon eval/training only — attributed in README.
