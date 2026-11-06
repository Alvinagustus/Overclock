//! Pure game logic for Overclock — no near-sdk dependency, fully unit-testable
//! on any target. The contract crate wraps this with NEAR account state,
//! deposits, payout promises, and events.
//!
//! Design rules for this crate (v2):
//! * **No floating point.** The NEAR runtime rejects wasm that contains float
//!   instructions, so ELO uses an integer lookup table (permille).
//! * **All timestamps are milliseconds** (`env::block_timestamp_ms()`), which
//!   stay below 2^53 and therefore survive a JSON round-trip in JavaScript.
//! * **One generator, one source of truth.** The contract exposes the problem
//!   text through a view, so the web UI / NPC / placement runner no longer
//!   need to mirror this generator.

use sha2::{Digest, Sha256};

// ---------------------------------------------------------------- constants

/// ELO of a brand-new account / floor of the placement band.
pub const START_ELO: u32 = 800;
/// Top of the placement band (20/20).
pub const MAX_ELO: u32 = 1800;
/// Base matchmaking band (RULES §3).
pub const MATCH_BAND: u32 = 150;
/// The band widens by this much every `BAND_STEP_MS` of waiting …
pub const BAND_STEP: u32 = 50;
pub const BAND_STEP_MS: u64 = 10_000;
/// … up to this cap, so the queue can never stall forever in a small pool.
pub const MAX_BAND: u32 = 450;
/// K-factor: provisional (first `PROVISIONAL_GAMES` rated games) vs settled.
pub const K_PROVISIONAL: u32 = 64;
pub const K_SETTLED: u32 = 32;
pub const PROVISIONAL_GAMES: u32 = 10;
/// Rake taken from every pot: 3%. Split 60% treasury / 40% burn.
pub const RAKE_BPS: u128 = 300;
pub const TREASURY_SHARE_BPS: u128 = 6000;
/// Commit window, reveal window (after the commit deadline), placement window.
pub const DUEL_MS: u64 = 30_000;
pub const REVEAL_MS: u64 = 20_000;
pub const PLACEMENT_MS: u64 = 10 * 60_000;
pub const PLACEMENT_SIZE: u32 = 20;
/// How many placement attempts a player gets per season (anti-reroll-fishing).
pub const MAX_PLACEMENT_ATTEMPTS: u32 = 3;
/// A queue entry older than this is refunded and pruned (10 minutes).
pub const QUEUE_MAX_WAIT_MS: u64 = 10 * 60_000;
/// Upper bound the owner may set `season_len` to (one year).
pub const MAX_SEASON_MS: u64 = 365 * 24 * HOUR_MS;
/// Anti-spam.
pub const MAX_DUELS_PER_HOUR: u32 = 10;
pub const HOUR_MS: u64 = 3_600_000;
pub const WRONG_STREAK_LIMIT: u32 = 10;
/// Streak bonus: 0.001 NEAR at 3 consecutive ranked wins (once per season).
pub const STREAK_BONUS: u128 = 1_000_000_000_000_000_000_000;
/// Season payout: this share of the treasury is distributed to the top 5.
pub const SEASON_POOL_BPS: u128 = 5000;
pub const SEASON_WEIGHTS_BPS: [u128; 5] = [4000, 2500, 1700, 1200, 600];

// ------------------------------------------------------------------- tiers

/// One-time refundable storage deposit for a player record. NEAR storage
/// costs 1e19 yocto per byte; 512 bytes → 0.00512 NEAR.
pub const PLAYER_STORAGE_BYTES: u64 = 512;
pub const STORAGE_PRICE_PER_BYTE: u128 = 10_000_000_000_000_000_000;
pub const MIN_STORAGE_DEPOSIT: u128 = PLAYER_STORAGE_BYTES as u128 * STORAGE_PRICE_PER_BYTE;

/// Entry tiers: 1 = 0.01 NEAR, 2 = 0.1 NEAR, 3 = 1 NEAR.
pub fn tier_stake(tier: u8) -> Option<u128> {
    match tier {
        1 => Some(10_000_000_000_000_000_000_000),
        2 => Some(100_000_000_000_000_000_000_000),
        3 => Some(1_000_000_000_000_000_000_000_000),
        _ => None,
    }
}

/// Highest problem kind unlocked by a tier (see [`Problem`]).
pub fn tier_max_kind(tier: u8) -> u8 {
    match tier {
        1 => 2,
        2 => 5,
        _ => 7,
    }
}

/// Numeric scale (1..=3) of a tier: bigger numbers at higher tiers.
pub fn tier_scale(tier: u8) -> u8 {
    tier.clamp(1, 3)
}

// --------------------------------------------------------------------- money

/// Split a pot into (winner payout, treasury share, burn share) after rake.
/// Floors everywhere; dust stays with the winner. Always sums to `pot`.
pub fn split_pot(pot: u128) -> (u128, u128, u128) {
    let rake = pot * RAKE_BPS / 10_000;
    let treasury = rake * TREASURY_SHARE_BPS / 10_000;
    (pot - rake, treasury, rake - treasury)
}

/// Draw: rake is taken on the whole pot, each side absorbs (about) half.
/// Returns (refund_a, refund_b, treasury, burn); always sums to `2*stake`
/// even when the rake is an odd number of yocto.
pub fn draw_split(stake: u128) -> (u128, u128, u128, u128) {
    let pot = stake * 2;
    let rake = pot * RAKE_BPS / 10_000;
    let treasury = rake * TREASURY_SHARE_BPS / 10_000;
    let burn = rake - treasury;
    let back_a = stake - rake / 2;
    let back_b = pot - rake - back_a;
    (back_a, back_b, treasury, burn)
}

/// Season pool split over the top `n` (1..=5) rows. Returns the amounts in
/// rank order; the sum is `<= pool` (dust stays in the treasury).
pub fn season_payouts(pool: u128, n: usize) -> Vec<u128> {
    let n = n.min(5);
    let total_w: u128 = SEASON_WEIGHTS_BPS[..n].iter().sum();
    if n == 0 || total_w == 0 {
        return Vec::new();
    }
    SEASON_WEIGHTS_BPS[..n]
        .iter()
        .map(|w| pool * w / total_w)
        .collect()
}

// ----------------------------------------------------------------------- ELO

/// 1000 * expected score for a player rated `d` points higher, d = 0,50,..,800
/// (round(1000 / (1 + 10^(-d/400)))). Integer table instead of `powf`.
const ELO_TABLE: [u32; 17] = [
    500, 571, 640, 703, 760, 808, 849, 882, 909, 930, 947, 960, 969, 977, 983, 987, 990,
];

/// Expected score of `a` vs `b`, in permille (0..=1000), linear interpolation.
pub fn expected_permille(a: u32, b: u32) -> u32 {
    let diff = a as i64 - b as i64;
    let d = diff.unsigned_abs().min(800) as u32;
    let i = (d / 50) as usize;
    let r = d % 50;
    let lo = ELO_TABLE[i];
    let hi = ELO_TABLE[(i + 1).min(16)];
    let e = lo + (hi - lo) * r / 50;
    if diff >= 0 {
        e
    } else {
        1000 - e
    }
}

/// K-factor for a pairing: provisional if either side is still provisional,
/// so a smurf (low placement, strong player) is corrected quickly.
pub fn k_for(games_a: u32, games_b: u32) -> u32 {
    if games_a < PROVISIONAL_GAMES || games_b < PROVISIONAL_GAMES {
        K_PROVISIONAL
    } else {
        K_SETTLED
    }
}

/// Zero-sum ELO update (one delta, not two independently rounded ones).
/// Returns (winner_elo, loser_elo); ratings never drop below 100. At the
/// floor the winner only gains what the loser can actually lose, so the
/// update stays exactly zero-sum.
pub fn elo_after_win(winner: u32, loser: u32, k: u32) -> (u32, u32) {
    let e_w = expected_permille(winner, loser);
    let delta = (k * (1000 - e_w) + 500) / 1000;
    let loss = delta.min(loser.saturating_sub(100));
    (winner + loss, loser - loss)
}

/// XP for a ranked win: 100 base + up to 100 for beating a stronger player.
pub fn xp_for_win(winner: u32, loser: u32) -> u64 {
    let e_w = expected_permille(winner, loser) as u64;
    100 + 100 * (1000 - e_w) / 1000
}

/// Placement score (0..=20) → seeded ELO (800..=1800).
pub fn elo_from_placement(score: u32) -> u32 {
    START_ELO + (score.min(PLACEMENT_SIZE)) * 50
}

/// Matchmaking band for a candidate that has been waiting `waited_ms`.
pub fn band_for_wait(waited_ms: u64) -> u32 {
    (MATCH_BAND as u64 + BAND_STEP as u64 * (waited_ms / BAND_STEP_MS)).min(MAX_BAND as u64) as u32
}

// ------------------------------------------------------------------ problems

/// An integer problem. `kind` decides how `a`, `b`, `c` are used:
///
/// | kind | meaning                       | unlocked at |
/// |------|-------------------------------|-------------|
/// | 0    | `a + b - c`                   | tier 1      |
/// | 1    | `a * b`                       | tier 1      |
/// | 2    | `a mod b`                     | tier 1      |
/// | 3    | `(a * b) mod c`               | tier 2      |
/// | 4    | `gcd(a, b)`                   | tier 2      |
/// | 5    | `a ^ b mod c`                 | tier 2      |
/// | 6    | modular inverse of `a` mod prime `c` | tier 3 |
/// | 7    | popcount(`a XOR b`)           | tier 3      |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Problem {
    pub kind: u8,
    pub a: i64,
    pub b: i64,
    pub c: i64,
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.abs()
}

fn pow_mod(base: i64, exp: i64, m: i64) -> i64 {
    let (mut r, mut b, mut e, m) = (1i128, base as i128 % m as i128, exp, m as i128);
    while e > 0 {
        if e & 1 == 1 {
            r = r * b % m;
        }
        b = b * b % m;
        e >>= 1;
    }
    r as i64
}

impl Problem {
    pub fn answer(&self) -> i64 {
        match self.kind {
            0 => self.a + self.b - self.c,
            1 => self.a * self.b,
            2 => self.a % self.b,
            3 => ((self.a as i128 * self.b as i128) % self.c as i128) as i64,
            4 => gcd(self.a, self.b),
            5 => pow_mod(self.a, self.b, self.c),
            6 => pow_mod(self.a, self.c - 2, self.c), // Fermat, c prime
            _ => (self.a ^ self.b).count_ones() as i64,
        }
    }

    /// Human / agent readable statement. Clients should show this verbatim.
    pub fn text(&self) -> String {
        match self.kind {
            0 => format!("{} + {} - {} = ?", self.a, self.b, self.c),
            1 => format!("{} * {} = ?", self.a, self.b),
            2 => format!("{} mod {} = ?", self.a, self.b),
            3 => format!("({} * {}) mod {} = ?", self.a, self.b, self.c),
            4 => format!("gcd({}, {}) = ?", self.a, self.b),
            5 => format!("{}^{} mod {} = ?", self.a, self.b, self.c),
            6 => format!(
                "x = ? such that {} * x = 1 (mod {}), 0 < x < {}",
                self.a, self.c, self.c
            ),
            _ => format!("popcount({} XOR {}) = ?", self.a, self.b),
        }
    }
}

/// xorshift64 — deterministic PRNG (state must be non-zero).
fn next_rand(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// uniform-ish value in `lo..=hi`
fn range(state: &mut u64, lo: i64, hi: i64) -> i64 {
    lo + (next_rand(state) % (hi - lo + 1) as u64) as i64
}

const PRIMES: [[i64; 6]; 3] = [
    [101, 103, 107, 109, 113, 127],
    [1009, 1013, 1019, 1021, 1031, 1033],
    [10007, 10009, 10037, 10039, 10061, 10067],
];

/// Build one problem of a given kind and scale (1..=3) from a PRNG state.
pub fn gen_kind(kind: u8, scale: u8, st: &mut u64) -> Problem {
    let sc = scale.clamp(1, 3) as usize;
    let p10 = [1i64, 10, 100][sc - 1];
    match kind {
        0 => Problem {
            kind,
            a: range(st, 100, 9_000 * p10),
            b: range(st, 100, 9_000 * p10),
            c: range(st, 100, 9_000 * p10),
        },
        1 => {
            let (l, h) = [(3, 99), (11, 999), (100, 9_999)][sc - 1];
            Problem { kind, a: range(st, l, h), b: range(st, l.max(2), h / 2 + 5), c: 0 }
        }
        2 => Problem {
            kind,
            a: range(st, 100, 9_000 * p10),
            b: range(st, 3, 80 * sc as i64),
            c: 0,
        },
        3 => Problem {
            kind,
            a: range(st, 100, 9_999 * p10),
            b: range(st, 100, 9_999 * p10),
            c: range(st, 101, 997 * p10),
        },
        4 => {
            let g = range(st, 2, 30 * sc as i64);
            Problem {
                kind,
                a: g * range(st, 2, 200 * sc as i64),
                b: g * range(st, 2, 200 * sc as i64),
                c: 0,
            }
        }
        5 => Problem {
            kind,
            a: range(st, 2, 50),
            b: range(st, 10, 100 * sc as i64),
            c: range(st, 1_000, 9_999 * p10),
        },
        6 => {
            let p = PRIMES[sc - 1][(next_rand(st) % 6) as usize];
            Problem { kind, a: range(st, 2, p - 1), b: 0, c: p }
        }
        _ => {
            let hi = (1i64 << (12 * sc as u32)) - 1;
            Problem { kind: 7, a: range(st, 1, hi), b: range(st, 1, hi), c: 0 }
        }
    }
}

fn seeded_state(seed: u64, idx: u32) -> u64 {
    let mut s = seed ^ 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(idx as u64 + 1);
    s |= 1;
    // warm up: small / structured seeds produce poor first outputs
    for _ in 0..4 {
        next_rand(&mut s);
    }
    s
}

/// The single problem **both sides of a duel** solve. Same problem for both
/// → equal difficulty, so the race is decided by skill + speed, not by luck
/// of the draw. Fairness against copying comes from commit–reveal, not from
/// giving each side different numbers.
pub fn duel_problem(seed: u64, tier: u8) -> Problem {
    let mut st = seeded_state(seed, 0);
    let kind = (next_rand(&mut st) % (tier_max_kind(tier) as u64 + 1)) as u8;
    gen_kind(kind, tier_scale(tier), &mut st)
}

/// Placement problem `idx` (0..20): cycles through all 8 kinds and ramps the
/// scale from 1 to 3, so the score separates weak from strong agents.
pub fn placement_problem(seed: u64, idx: u32) -> Problem {
    let mut st = seeded_state(seed, idx + 1);
    let kind = (idx % 8) as u8;
    let scale = if idx < 7 { 1 } else if idx < 14 { 2 } else { 3 };
    gen_kind(kind, scale, &mut st)
}

/// Score a placement attempt: number of correct answers (missing = wrong).
pub fn placement_score(seed: u64, answers: &[i64]) -> u32 {
    (0..PLACEMENT_SIZE)
        .filter(|&i| answers.get(i as usize) == Some(&placement_problem(seed, i).answer()))
        .count() as u32
}

// ------------------------------------------------------------------- hashing

/// Canonical textual form of an answer: plain decimal, no sign padding, no
/// leading zeros. Clients must hash and reveal exactly this form.
pub fn canonical_answer(answer: i64) -> String {
    answer.to_string()
}

/// Commit hash, bound to the duel and the account so a hash cannot be
/// replayed in another duel or copied by the opponent:
/// `sha256("duel_id:account:answer:salt")` as hex.
pub fn commit_hash(duel_id: u64, account: &str, answer: i64, salt: &str) -> String {
    let mut h = Sha256::new();
    h.update(format!("{}:{}:{}:{}", duel_id, account, canonical_answer(answer), salt).as_bytes());
    hex::encode(h.finalize())
}

pub fn verify_commit(duel_id: u64, account: &str, answer: i64, salt: &str, expected: &str) -> bool {
    commit_hash(duel_id, account, answer, salt) == expected
}

/// Derive a 64-bit seed from entropy bytes (e.g. `env::random_seed()`) plus a
/// domain separator, so two duels in the same block never share a seed.
pub fn derive_seed(entropy: &[u8], domain: &[u8]) -> u64 {
    let mut h = Sha256::new();
    h.update(entropy);
    h.update(domain);
    let d = h.finalize();
    u64::from_le_bytes(d[..8].try_into().unwrap())
}

/// Count duels inside the last hour (anti-spam window).
pub fn in_window(recent: &[u64], now_ms: u64) -> u32 {
    recent.iter().filter(|&&t| now_ms.saturating_sub(t) < HOUR_MS).count() as u32
}

// --------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers() {
        assert_eq!(tier_stake(1), Some(10_000_000_000_000_000_000_000));
        assert_eq!(tier_stake(3), Some(1_000_000_000_000_000_000_000_000));
        assert_eq!(tier_stake(0), None);
        assert_eq!(tier_stake(9), None);
    }

    #[test]
    fn placement_seeding() {
        assert_eq!(elo_from_placement(0), 800);
        assert_eq!(elo_from_placement(10), 1300);
        assert_eq!(elo_from_placement(20), 1800);
        assert_eq!(elo_from_placement(99), 1800);
    }

    #[test]
    fn pot_split_is_exact_for_any_pot() {
        for pot in [0u128, 1, 7, 33, 199, 20_000_000_000_000_000_000_000, 123_456_789_123_456_789_123] {
            let (w, t, b) = split_pot(pot);
            assert_eq!(w + t + b, pot);
        }
        let (w, t, b) = split_pot(tier_stake(2).unwrap() * 2);
        assert_eq!(t * 2, b * 3); // 60/40
        assert_eq!(w, 2 * tier_stake(2).unwrap() * 97 / 100);
    }

    #[test]
    fn draw_split_conserves_funds_even_with_odd_rake() {
        for stake in [1u128, 3, 17, 101, 9_999, tier_stake(1).unwrap(), tier_stake(3).unwrap() + 1] {
            let (a, b, t, bu) = draw_split(stake);
            assert_eq!(a + b + t + bu, stake * 2, "stake {stake}");
            assert!(a <= stake && b <= stake);
        }
    }

    #[test]
    fn season_payouts_never_exceed_pool() {
        for n in 0..=5 {
            let p = season_payouts(1_000_003, n);
            assert_eq!(p.len(), n);
            assert!(p.iter().sum::<u128>() <= 1_000_003);
        }
        let p = season_payouts(10_000, 5);
        assert_eq!(p, vec![4000, 2500, 1700, 1200, 600]);
        assert!(p.windows(2).all(|w| w[0] >= w[1]));
    }

    #[test]
    fn elo_table_matches_float_formula() {
        for d in (-800i64..=800).step_by(7) {
            let a = (1500 + d) as u32;
            let exact = 1000.0 / (1.0 + 10f64.powf(-(d as f64) / 400.0));
            let got = expected_permille(a, 1500) as f64;
            assert!((got - exact).abs() <= 3.0, "d={d} got={got} exact={exact}");
        }
        assert_eq!(expected_permille(1200, 1200), 500);
        assert_eq!(expected_permille(2400, 1000), 990); // clamped
    }

    #[test]
    fn elo_is_zero_sum_and_upsets_pay_more() {
        let (w, l) = elo_after_win(1200, 1200, 32);
        assert_eq!((w, l), (1216, 1184));
        for (a, b) in [(900u32, 1600u32), (1500, 1480), (1000, 1001), (1800, 800)] {
            let (w, l) = elo_after_win(a, b, 32);
            assert_eq!((w - a) as i64, (b - l) as i64, "zero-sum {a} vs {b}");
        }
        let (w_up, _) = elo_after_win(900, 1600, 32);
        assert!(w_up - 900 > 16);
        assert_eq!(elo_after_win(1000, 100, 64).1, 100); // floor
        // at the floor the winner gains exactly what the loser pays
        let (wf, lf) = elo_after_win(1000, 100, 64);
        assert_eq!(lf, 100);
        assert_eq!(wf, 1000);
        assert_eq!((wf - 1000) as i64, (100 - lf) as i64);
    }

    #[test]
    fn canonical_answer_matches_reveal_format() {
        for a in [0i64, -1, 7, 42, -900, i64::MAX] {
            assert_eq!(canonical_answer(a), a.to_string());
            let h = commit_hash(1, "a.testnet", a, "s");
            assert!(verify_commit(1, "a.testnet", a, "s", &h));
        }
        // a different textual encoding of the same number does not verify
        let h = commit_hash(1, "a.testnet", 7, "s");
        assert!(!verify_commit(1, "a.testnet", 70, "s", &h));
    }

    #[test]
    fn provisional_k_applies_if_either_side_is_new() {
        assert_eq!(k_for(0, 50), K_PROVISIONAL);
        assert_eq!(k_for(50, 3), K_PROVISIONAL);
        assert_eq!(k_for(10, 10), K_SETTLED);
    }

    #[test]
    fn band_widens_with_waiting_and_is_capped() {
        assert_eq!(band_for_wait(0), 150);
        assert_eq!(band_for_wait(10_000), 200);
        assert_eq!(band_for_wait(60_000), 450);
        assert_eq!(band_for_wait(10_000_000), MAX_BAND);
    }

    #[test]
    fn every_kind_is_solvable_and_in_range() {
        for seed in 0..400u64 {
            for scale in 1..=3u8 {
                for kind in 0..8u8 {
                    let mut st = seeded_state(seed.wrapping_mul(0xABCDEF1234567), kind as u32);
                    let p = gen_kind(kind, scale, &mut st);
                    let ans = p.answer();
                    assert!(!p.text().is_empty());
                    match kind {
                        2 => assert!(p.b >= 3 && (0..p.b).contains(&ans)),
                        3 | 5 => assert!((0..p.c).contains(&ans), "{p:?}"),
                        4 => assert!(ans >= 2 && p.a % ans == 0 && p.b % ans == 0),
                        6 => {
                            assert!((1..p.c).contains(&ans), "{p:?} -> {ans}");
                            assert_eq!(p.a as i128 * ans as i128 % p.c as i128, 1, "{p:?}");
                        }
                        7 => assert!((0..=36).contains(&ans)),
                        _ => {}
                    }
                }
            }
        }
    }

    #[test]
    fn duel_problem_is_deterministic_and_tier_gated() {
        assert_eq!(duel_problem(42, 2), duel_problem(42, 2));
        for seed in 0..2000u64 {
            assert!(duel_problem(seed, 1).kind <= 2);
            assert!(duel_problem(seed, 2).kind <= 5);
        }
        // tier 3 eventually produces the hard kinds
        assert!((0..2000u64).any(|s| duel_problem(s, 3).kind >= 6));
        // seed 0 must not degenerate (xorshift state 0)
        let _ = duel_problem(0, 1).answer();
    }

    #[test]
    fn placement_scores_correct_answers_only() {
        let seed = 777;
        let all: Vec<i64> = (0..20).map(|i| placement_problem(seed, i).answer()).collect();
        assert_eq!(placement_score(seed, &all), 20);
        let mut half = all.clone();
        for x in half.iter_mut().take(10) {
            *x += 1;
        }
        assert_eq!(placement_score(seed, &half), 10);
        assert_eq!(placement_score(seed, &[]), 0);
        // extra answers are ignored, wrong seed scores ~0
        let mut long = all.clone();
        long.extend([1, 2, 3]);
        assert_eq!(placement_score(seed, &long), 20);
        assert!(placement_score(seed + 1, &all) < 20);
    }

    #[test]
    fn commit_hash_is_bound_to_duel_and_account() {
        let h = commit_hash(7, "alice.testnet", -12, "saltX");
        assert!(verify_commit(7, "alice.testnet", -12, "saltX", &h));
        assert!(!verify_commit(8, "alice.testnet", -12, "saltX", &h)); // other duel
        assert!(!verify_commit(7, "bob.testnet", -12, "saltX", &h)); // copied hash
        assert!(!verify_commit(7, "alice.testnet", -13, "saltX", &h));
        assert!(!verify_commit(7, "alice.testnet", -12, "saltY", &h));
    }

    #[test]
    fn derived_seeds_differ_per_domain() {
        let e = [9u8; 32];
        assert_ne!(derive_seed(&e, b"duel:1"), derive_seed(&e, b"duel:2"));
        assert_eq!(derive_seed(&e, b"duel:1"), derive_seed(&e, b"duel:1"));
    }

    #[test]
    fn cooldown_window() {
        let now = 100 * HOUR_MS;
        let recent = vec![now - 10, now - HOUR_MS / 2, now - 2 * HOUR_MS];
        assert_eq!(in_window(&recent, now), 2);
    }

    #[test]
    fn xp_rewards_upsets() {
        assert!(xp_for_win(900, 1600) > xp_for_win(1600, 900));
        assert!(xp_for_win(1200, 1200) == 150);
    }
}
