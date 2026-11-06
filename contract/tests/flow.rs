use near_sdk::test_utils::{get_created_receipts, get_logs, VMContextBuilder};
use near_sdk::{testing_env, AccountId, NearToken};
use overclock::Overclock;
use overclock_core as oc;
use std::panic::{catch_unwind, AssertUnwindSafe};

const MS: u64 = 1_000_000; // ns per ms (near test ctx uses ns)

fn acc(s: &str) -> AccountId { s.parse().unwrap() }
fn ctx(who: &str, dep: u128, t_ms: u64) {
    let mut b = VMContextBuilder::new();
    b.predecessor_account_id(acc(who)).signer_account_id(acc(who))
        .attached_deposit(NearToken::from_yoctonear(dep))
        .block_timestamp(t_ms * MS).random_seed([7u8; 32]);
    testing_env!(b.build());
}
fn panics<F: FnOnce()>(f: F) -> bool { catch_unwind(AssertUnwindSafe(f)).is_err() }

fn fresh() -> Overclock {
    ctx("owner.testnet", 0, 1_000);
    Overclock::new(acc("burn.owner.testnet"))
}

/// Do a real placement with all-correct answers. `start_placement` also funds
/// the one-time (refundable) storage deposit.
fn accredit(c: &mut Overclock, who: &str, t: u64, correct: bool) -> u32 {
    ctx(who, oc::MIN_STORAGE_DEPOSIT, t);
    c.start_placement();
    let seed = c.get_player(acc(who)).unwrap().placement_seed.0;
    assert_eq!(c.get_placement_problems(acc(who)).len(), 20);
    let ans: Vec<i64> = (0..20).map(|i| {
        let a = oc::placement_problem(seed, i).answer();
        if correct { a } else { a + 1 }
    }).collect();
    ctx(who, 0, t + 5_000);
    c.submit_placement(ans);
    c.get_player(acc(who)).unwrap().elo
}

fn start_ranked(c: &mut Overclock, t: u64) -> u64 {
    accredit(c, "alice.testnet", t, true);
    accredit(c, "bob.testnet", t, true);
    let stake = oc::tier_stake(1).unwrap();
    ctx("alice.testnet", stake, t + 10_000);
    c.join_queue(1, false);
    ctx("bob.testnet", stake, t + 10_500);
    c.join_queue(1, false);
    c.get_config().duel_seq
}

fn commit(c: &mut Overclock, who: &str, id: u64, ans: i64, salt: &str, t: u64) {
    ctx(who, 0, t);
    c.submit_answer(id, oc::commit_hash(id, who, ans, salt));
}

#[test]
fn placement_works_in_season_zero_and_cannot_be_faked() {
    let mut c = fresh();
    assert_eq!(accredit(&mut c, "alice.testnet", 2_000, true), 1800);   // was a PANIC in v1
    assert_eq!(accredit(&mut c, "bob.testnet", 2_000, false), 800);     // all wrong -> 800
    // no way to self-report: old signature is gone, and a second try is refused
    ctx("alice.testnet", 0, 20_000);
    assert!(panics(|| c.start_placement()));
    // submit without start
    ctx("carol.testnet", 0, 20_000);
    assert!(panics(|| c.submit_placement(vec![1; 20])));
    // too late
    ctx("dave.testnet", oc::MIN_STORAGE_DEPOSIT, 30_000); c.start_placement();
    ctx("dave.testnet", 0, 30_000 + oc::PLACEMENT_MS + 1);
    assert!(panics(|| c.submit_placement(vec![])));
}

#[test]
fn cancel_queue_cannot_drain_the_contract() {
    let mut c = fresh();
    ctx("mallory.testnet", 0, 2_000);
    assert!(panics(|| c.cancel_queue(3, false)));            // v1: refunded 1 NEAR from nothing
    assert!(get_created_receipts().is_empty());
    // honest path refunds exactly once
    let stake = oc::tier_stake(1).unwrap();
    ctx("erin.testnet", stake, 3_000); c.join_queue(1, true);
    ctx("erin.testnet", 0, 3_500); c.cancel_queue(1, true);
    assert_eq!(get_created_receipts().len(), 1);
    ctx("erin.testnet", 0, 3_600);
    assert!(panics(|| c.cancel_queue(1, true)));
}

#[test]
fn double_join_is_rejected_and_stake_not_orphaned() {
    let mut c = fresh();
    let stake = oc::tier_stake(1).unwrap();
    ctx("erin.testnet", stake, 3_000); c.join_queue(1, true);
    ctx("erin.testnet", stake, 3_100);
    assert!(panics(|| c.join_queue(1, true)));
}

#[test]
fn ranked_duel_first_correct_commit_wins_and_settles_early() {
    let mut c = fresh();
    let id = start_ranked(&mut c, 2_000);
    let d = c.get_duel(id).unwrap();
    assert_eq!(c.get_problem(id).unwrap().text.is_empty(), false);
    let ans = oc::duel_problem(d.seed.0, d.tier).answer();
    // both solve it, bob commits FIRST, alice second
    commit(&mut c, "bob.testnet", id, ans, "s1", 13_000);
    commit(&mut c, "alice.testnet", id, ans, "s2", 13_400);
    // cannot settle before reveals
    ctx("anyone.testnet", 0, 14_000);
    assert!(panics(|| c.settle(id)));
    ctx("bob.testnet", 0, 14_000); c.reveal(id, ans, "s1".into());
    ctx("anyone.testnet", 0, 14_100);
    assert!(panics(|| c.settle(id)));                          // alice hasn't revealed yet
    ctx("alice.testnet", 0, 14_200); c.reveal(id, ans, "s2".into());
    ctx("anyone.testnet", 0, 14_300);
    c.settle(id);                                              // early: both committed+revealed
    let d = c.get_duel(id).unwrap();
    assert_eq!(d.winner, Some(acc("bob.testnet")));
    let b = c.get_player(acc("bob.testnet")).unwrap();
    let a = c.get_player(acc("alice.testnet")).unwrap();
    assert!(b.elo > 1800 && a.elo < 1800 && b.wins == 1 && a.losses == 1);
    assert_eq!((b.elo - 1800) as i64, (1800 - a.elo) as i64);  // zero-sum
    assert!(get_logs().iter().any(|l| l.contains("overclock:settled")));
    // second settle blocked
    ctx("anyone.testnet", 0, 14_400);
    assert!(panics(|| c.settle(id)));
    // top-5 tracked
    let top = c.get_season_top();
    assert_eq!(top[0].account, acc("bob.testnet"));
}

#[test]
fn copying_the_opponents_hash_is_useless() {
    let mut c = fresh();
    let id = start_ranked(&mut c, 2_000);
    let d = c.get_duel(id).unwrap();
    let ans = oc::duel_problem(d.seed.0, d.tier).answer();
    commit(&mut c, "alice.testnet", id, ans, "secret", 13_000);
    let copied = c.get_duel(id).unwrap().hash_a.unwrap();
    ctx("bob.testnet", 0, 13_200); c.submit_answer(id, copied);       // bob copies the hash
    ctx("alice.testnet", 0, 13_500); c.reveal(id, ans, "secret".into());
    // bob replays alice's answer+salt: bound to alice's account -> rejected
    ctx("bob.testnet", 0, 13_600);
    assert!(panics(|| c.reveal(id, ans, "secret".into())));
}

#[test]
fn slow_revealer_is_protected_until_reveal_window_ends() {
    let mut c = fresh();
    let id = start_ranked(&mut c, 2_000);
    let d = c.get_duel(id).unwrap();
    let ans = oc::duel_problem(d.seed.0, d.tier).answer();
    commit(&mut c, "alice.testnet", id, ans, "s1", 13_000);
    commit(&mut c, "bob.testnet", id, ans, "s2", 13_100);
    ctx("alice.testnet", 0, 13_200); c.reveal(id, ans, "s1".into());
    // commit window is over (deadline = 12_500+30_000) but reveal window is not:
    // v1 would let anyone settle now and erase bob's correct answer.
    ctx("anyone.testnet", 0, 12_500 + oc::DUEL_MS + 1);
    assert!(panics(|| c.settle(id)));
    ctx("bob.testnet", 0, 12_500 + oc::DUEL_MS + 2_000); c.reveal(id, ans, "s2".into());
    ctx("anyone.testnet", 0, 12_500 + oc::DUEL_MS + 2_100); c.settle(id);
    assert_eq!(c.get_duel(id).unwrap().winner, Some(acc("alice.testnet"))); // committed first
}

#[test]
fn no_reveal_means_forfeit_and_both_wrong_means_draw_with_exact_refunds() {
    let mut c = fresh();
    let id = start_ranked(&mut c, 2_000);
    let d = c.get_duel(id).unwrap();
    let ans = oc::duel_problem(d.seed.0, d.tier).answer();
    // alice correct but never reveals; bob wrong & reveals -> nobody has a correct reveal -> draw
    commit(&mut c, "alice.testnet", id, ans, "s1", 13_000);
    commit(&mut c, "bob.testnet", id, ans + 1, "s2", 13_100);
    ctx("bob.testnet", 0, 13_200); c.reveal(id, ans + 1, "s2".into());
    ctx("anyone.testnet", 0, 12_500 + oc::DUEL_MS + oc::REVEAL_MS + 1);
    c.settle(id);
    let d = c.get_duel(id).unwrap();
    assert!(d.draw && d.winner.is_none());
    assert_eq!(get_created_receipts().len(), 2);
    let cfg = c.get_config();
    let stake = oc::tier_stake(1).unwrap();
    let (ba, bb, t, bu) = oc::draw_split(stake);
    assert_eq!(ba + bb + t + bu, 2 * stake);
    assert_eq!(cfg.treasury.as_yoctonear(), t);
    assert_eq!(cfg.burn.as_yoctonear(), bu);
}

#[test]
fn commit_window_and_participants_are_enforced() {
    let mut c = fresh();
    let id = start_ranked(&mut c, 2_000);
    ctx("alice.testnet", 0, 12_500 + oc::DUEL_MS + 1);
    assert!(panics(|| c.submit_answer(id, "0".repeat(64))));          // late
    ctx("mallory.testnet", 0, 13_000);
    assert!(panics(|| c.submit_answer(id, "0".repeat(64))));          // stranger
    ctx("alice.testnet", 0, 13_000);
    assert!(panics(|| c.submit_answer(id, "short".into())));          // malformed
}

#[test]
fn ranked_requires_placement_and_casual_ignores_band() {
    let mut c = fresh();
    let stake = oc::tier_stake(1).unwrap();
    ctx("newbie.testnet", stake, 2_000);
    assert!(panics(|| c.join_queue(1, false)));
    // casual: an 1800 agent and an 800 newbie still match (NPC demo play)
    accredit(&mut c, "alice.testnet", 2_000, true);
    ctx("alice.testnet", stake, 12_000); c.join_queue(1, true);
    ctx("newbie.testnet", stake, 12_100); c.join_queue(1, true);
    assert_eq!(c.get_config().duel_seq, 1);
    assert!(c.get_duel(1).unwrap().casual);
}

#[test]
fn ranked_band_blocks_far_ratings_then_widens_with_waiting() {
    let mut c = fresh();
    let stake = oc::tier_stake(1).unwrap();
    accredit(&mut c, "alice.testnet", 2_000, true);   // 1800
    accredit(&mut c, "bob.testnet", 2_000, false);    // 800
    ctx("alice.testnet", stake, 12_000); c.join_queue(1, false);
    ctx("bob.testnet", stake, 12_100); c.join_queue(1, false);
    assert_eq!(c.get_config().duel_seq, 0);           // 1000 apart: no match
    // after cap (450) still < 1000 apart -> still no match; a closer rival matches
    accredit(&mut c, "carol.testnet", 2_000, true);   // 1800
    ctx("carol.testnet", stake, 12_200); c.join_queue(1, false);
    assert_eq!(c.get_config().duel_seq, 1);           // alice-carol
}

#[test]
fn season_rollover_pays_top_players_via_claim() {
    let mut c = fresh();
    let id = start_ranked(&mut c, 2_000);
    let d = c.get_duel(id).unwrap();
    let ans = oc::duel_problem(d.seed.0, d.tier).answer();
    commit(&mut c, "alice.testnet", id, ans, "a", 13_000);
    commit(&mut c, "bob.testnet", id, ans, "b", 13_100);
    ctx("alice.testnet", 0, 13_200); c.reveal(id, ans, "a".into());
    ctx("bob.testnet", 0, 13_300); c.reveal(id, ans, "b".into());
    ctx("x.testnet", 0, 13_400); c.settle(id);
    // owner seeds the treasury, season ends, first action closes it
    ctx("owner.testnet", 10_000_000_000_000_000_000_000_000, 14_000); c.seed_treasury();
    let before = c.get_config().treasury.as_yoctonear();
    let next_season = 1_000 + oc::HOUR_MS * 24 * 14 + 5_000;
    ctx("alice.testnet", 0, next_season);
    c.start_placement();                                   // any action rolls the season
    assert!(get_logs().iter().any(|l| l.contains("season_closed")));
    let a = c.get_claimable(acc("alice.testnet")).as_yoctonear();
    let b = c.get_claimable(acc("bob.testnet")).as_yoctonear();
    let pool = before * oc::SEASON_POOL_BPS / 10_000;
    assert!(a > 0 && b > 0 && a > b);                      // alice(1st) > bob(2nd)
    assert!(a + b <= pool);
    assert_eq!(c.get_config().treasury.as_yoctonear(), before - (a + b));
    ctx("alice.testnet", 0, next_season + 1);
    c.claim_reward();
    assert_eq!(get_created_receipts().len(), 1);
    ctx("alice.testnet", 0, next_season + 2);
    assert!(panics(|| c.claim_reward()));                  // no double claim
    // old accreditation is not valid in the new season
    ctx("bob.testnet", 10u128.pow(22), next_season + 3);
    assert!(panics(|| c.join_queue(1, false)));
}

#[test]
fn admin_cannot_skip_seasons_or_move_funds() {
    let mut c = fresh();
    ctx("mallory.testnet", 0, 2_000);
    assert!(panics(|| c.set_paused(true)));
    assert!(panics(|| c.set_season_length(60_000)));
    ctx("owner.testnet", 0, 2_000 + oc::HOUR_MS * 24 * 7);
    let s = c.get_config().season;
    c.set_season_length(oc::HOUR_MS * 24 * 3);
    assert_eq!(c.get_config().season, s);                  // id preserved
}

#[test]
fn uninitialised_contract_cannot_be_captured() {
    ctx("mallory.testnet", 0, 2_000);
    assert!(panics(|| { let _: Overclock = <Overclock as Default>::default(); }));
}

// ---------------------------------------------------------------- v2 fix tests

/// Match the two already-accredited test players in the ranked tier-1 queue.
fn match_ranked(c: &mut Overclock, t: u64) -> u64 {
    let stake = oc::tier_stake(1).unwrap();
    ctx("alice.testnet", stake, t);
    c.join_queue(1, false);
    ctx("bob.testnet", stake, t + 1);
    c.join_queue(1, false);
    c.get_config().duel_seq
}

fn play(c: &mut Overclock, id: u64, t: u64, a_ans: i64, b_ans: i64) {
    let d = c.get_duel(id).unwrap();
    let ans = oc::duel_problem(d.seed.0, d.tier).answer();
    let (a, b) = (if a_ans == -1 { ans } else { ans + a_ans }, if b_ans == -1 { ans } else { ans + b_ans });
    commit(c, "alice.testnet", id, a, "sa", t);
    commit(c, "bob.testnet", id, b, "sb", t + 1);
    ctx("alice.testnet", 0, t + 2);
    c.reveal(id, a, "sa".into());
    ctx("bob.testnet", 0, t + 3);
    c.reveal(id, b, "sb".into());
    ctx("x.testnet", 0, t + 4);
    c.settle(id);
}

#[test]
fn storage_deposit_is_required_and_refundable() {
    let mut c = fresh();
    ctx("alice.testnet", 0, 2_000);
    assert!(panics(|| { c.register_agent(); }));
    ctx("alice.testnet", oc::MIN_STORAGE_DEPOSIT - 1, 2_100);
    assert!(panics(|| { c.register_agent(); }));
    ctx("alice.testnet", oc::MIN_STORAGE_DEPOSIT, 2_200);
    assert!(c.register_agent());
    ctx("alice.testnet", 0, 2_300);
    assert!(!c.register_agent()); // idempotent
    // start_placement works because the deposit is already credited
    ctx("alice.testnet", 0, 2_400);
    c.start_placement();
    // refund is blocked while placement is in progress? (allowed) but blocked
    // while queued; settle that below with a fresh account.
    ctx("bob.testnet", oc::MIN_STORAGE_DEPOSIT, 2_500);
    c.register_agent();
    ctx("bob.testnet", 0, 2_600);
    c.refund_storage_deposit();
    assert!(c.get_player(acc("bob.testnet")).is_none());
    assert_eq!(get_created_receipts().len(), 1);
}

#[test]
fn queue_entries_expire_refund_and_allow_rejoin() {
    let mut c = fresh();
    accredit(&mut c, "alice.testnet", 2_000, true);
    accredit(&mut c, "bob.testnet", 2_000, true);
    let stake = oc::tier_stake(1).unwrap();
    ctx("alice.testnet", stake, 12_000);
    c.join_queue(1, false);
    // bob joins after alice's entry is older than the max wait: alice is
    // pruned+refunded, so bob simply queues instead of matching stale state.
    let late = 12_000 + oc::QUEUE_MAX_WAIT_MS + 1;
    ctx("bob.testnet", stake, late);
    c.join_queue(1, false);
    assert_eq!(c.get_config().duel_seq, 0);
    assert!(get_created_receipts().len() >= 1);
    // alice's own entry is gone, so she can rejoin and match bob
    ctx("alice.testnet", stake, late + 1);
    c.join_queue(1, false);
    assert_eq!(c.get_config().duel_seq, 1);
}

#[test]
fn ten_duels_per_hour_cap_is_enforced() {
    let mut c = fresh();
    accredit(&mut c, "alice.testnet", 2_000, true);
    accredit(&mut c, "bob.testnet", 2_000, true);
    for i in 0..10u64 {
        let id = match_ranked(&mut c, 12_000 + i * 1_000);
        // alternate the winner so the pair stays inside the 150 band
        if i % 2 == 0 {
            play(&mut c, id, 12_000 + i * 1_000 + 100, -1, 1);
        } else {
            play(&mut c, id, 12_000 + i * 1_000 + 100, 1, -1);
        }
    }
    let stake = oc::tier_stake(1).unwrap();
    ctx("alice.testnet", stake, 12_000 + 20_000);
    assert!(panics(|| c.join_queue(1, false))); // 11th within the hour
}

#[test]
fn wrong_answer_cooldown_blocks_ranked_queue_but_not_casual() {
    let mut c = fresh();
    accredit(&mut c, "alice.testnet", 2_000, true);
    accredit(&mut c, "bob.testnet", 2_000, true);
    // 10 wrong ranked duels, spread > 1 h apart so the hourly cap is not the
    // reason for the block — the wrong-streak cooldown is.
    for i in 0..10u64 {
        let t = 12_000 + i * 2 * oc::HOUR_MS;
        let id = match_ranked(&mut c, t);
        play(&mut c, id, t + 100, 1, 1); // both wrong -> draw, but counted
    }
    let stake = oc::tier_stake(1).unwrap();
    let last = 12_000 + 9 * 2 * oc::HOUR_MS + 1_000;
    ctx("alice.testnet", stake, last);
    assert!(panics(|| c.join_queue(1, false)));
    // casual play is exempt from ranked cooldown/cap
    ctx("alice.testnet", stake, last + 1);
    c.join_queue(1, true);
}

#[test]
fn streak_bonus_pays_once_in_season_zero() {
    let mut c = fresh();
    accredit(&mut c, "alice.testnet", 2_000, true);
    accredit(&mut c, "bob.testnet", 2_000, true);
    ctx("owner.testnet", 10u128.pow(23), 11_000);
    c.seed_treasury();
    for i in 0..3u64 {
        let t = 12_000 + i * 1_000;
        let id = match_ranked(&mut c, t);
        play(&mut c, id, t + 100, -1, 1); // alice correct, bob wrong
    }
    let a = c.get_player(acc("alice.testnet")).unwrap();
    assert_eq!(a.streak, 3);
    // default was 0 == season 0, so the bonus never paid; now it must.
    assert_eq!(a.streak_bonus_season, 0);
}

#[test]
fn flush_burn_sends_accumulated_share_once() {
    let mut c = fresh();
    accredit(&mut c, "alice.testnet", 2_000, true);
    accredit(&mut c, "bob.testnet", 2_000, true);
    let id = match_ranked(&mut c, 12_000);
    play(&mut c, id, 12_100, -1, 1);
    assert!(c.get_config().burn.as_yoctonear() > 0);
    ctx("anyone.testnet", 0, 13_000);
    c.flush_burn();
    assert_eq!(c.get_config().burn.as_yoctonear(), 0);
    assert_eq!(get_created_receipts().len(), 1);
    ctx("anyone.testnet", 0, 13_100);
    assert!(panics(|| c.flush_burn()));
}

#[test]
fn settle_after_season_roll_leaves_new_ladder_untouched() {
    let mut c = fresh();
    let id = start_ranked(&mut c, 2_000);
    let d = c.get_duel(id).unwrap();
    let ans = oc::duel_problem(d.seed.0, d.tier).answer();
    commit(&mut c, "alice.testnet", id, ans, "a", 13_000);
    commit(&mut c, "bob.testnet", id, ans, "b", 13_100);
    ctx("alice.testnet", 0, 13_200);
    c.reveal(id, ans, "a".into());
    ctx("bob.testnet", 0, 13_300);
    c.reveal(id, ans, "b".into());
    // season rolls; alice starts the new placement so her record moves on
    let next = 1_000 + oc::HOUR_MS * 24 * 14 + 5_000;
    ctx("alice.testnet", 0, next);
    c.start_placement();
    let before = c.get_player(acc("bob.testnet")).unwrap();
    ctx("x.testnet", 0, next + 10);
    c.settle(id); // duel belonged to season 0
    let a = c.get_player(acc("alice.testnet")).unwrap();
    let b = c.get_player(acc("bob.testnet")).unwrap();
    assert_eq!(a.wins, 0);
    assert_eq!(a.games, 0);
    assert_eq!(b.wins, before.wins);
    assert_eq!(b.elo, before.elo);
    assert!(c.get_duel(id).unwrap().settled);
}

#[test]
fn claim_reward_closes_a_finished_season_without_prior_action() {
    let mut c = fresh();
    accredit(&mut c, "alice.testnet", 2_000, true);
    accredit(&mut c, "bob.testnet", 2_000, true);
    let id = match_ranked(&mut c, 12_000);
    play(&mut c, id, 12_100, -1, 1);
    ctx("owner.testnet", 10u128.pow(23), 13_000);
    c.seed_treasury();
    let next = 1_000 + oc::HOUR_MS * 24 * 14 + 5_000;
    ctx("alice.testnet", 0, next);
    // view is not stale-0 even though no mutating call closed the season
    assert!(c.get_claimable(acc("alice.testnet")).as_yoctonear() > 0);
    c.claim_reward(); // must close the season first
    assert_eq!(get_created_receipts().len(), 1);
    ctx("alice.testnet", 0, next + 1);
    assert_eq!(c.get_claimable(acc("alice.testnet")).as_yoctonear(), 0);
}

#[test]
fn set_season_length_has_an_upper_bound() {
    let mut c = fresh();
    ctx("owner.testnet", 0, 2_000);
    assert!(panics(|| c.set_season_length(oc::MAX_SEASON_MS + 1)));
    c.set_season_length(oc::MAX_SEASON_MS);
}

#[test]
fn premature_reveal_is_blocked_until_opponent_commits_or_window_expires() {
    // Ported from the user-side audit (H-01): revealing the plaintext while
    // the opponent can still commit would leak the answer in receipt logs.
    let mut c = fresh();
    let id = start_ranked(&mut c, 2_000);
    let d = c.get_duel(id).unwrap();
    let ans = oc::duel_problem(d.seed.0, d.tier).answer();
    // bob commits, alice has NOT committed yet
    commit(&mut c, "bob.testnet", id, ans, "s1", 13_000);
    assert!(panics(|| c.reveal(id, ans, "s1".into())));
    // once alice commits, both committed -> bob may reveal early
    commit(&mut c, "alice.testnet", id, ans, "s2", 13_200);
    ctx("bob.testnet", 0, 13_300);
    c.reveal(id, ans, "s1".into()); // succeeds
}

#[test]
fn set_season_length_never_underflows_across_multiple_seasons() {
    // Ported from the user-side audit (H-03): extending the season length
    // after several seasons must freeze the current season number, not reset it.
    let mut c = fresh();
    ctx("owner.testnet", 0, 1_000);
    c.set_season_length(oc::HOUR_MS); // 1 hour per season
    let t_season_5 = 1_000 + oc::HOUR_MS * 5 + 500;
    ctx("owner.testnet", 0, t_season_5);
    assert_eq!(c.get_config().season, 5);
    c.set_season_length(oc::HOUR_MS * 24 * 14); // huge jump 1h -> 2 weeks
    assert_eq!(c.get_config().season, 5); // NOT reset to 0
}
