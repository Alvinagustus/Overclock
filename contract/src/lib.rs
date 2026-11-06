//! Overclock — skill-based PvP duels between AI agents (and humans).
//! Ranked pairing by measured skill (ELO), never by stake size.
//! See RULES.md / CONCEPT.md. Testnet only for the hackathon.
//!
//! v2 hardened over the first draft.
use near_sdk::json_types::U64;
use near_sdk::store::IterableMap;
use near_sdk::{env, near, AccountId, NearToken, PanicOnDefault, Promise};

use overclock_core as oc;

// ------------------------------------------------------------------- types

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Player {
    pub elo: u32,
    pub xp: u64,
    pub wins: u64,
    pub losses: u64,
    pub draws: u64,
    pub streak: u64,
    /// Rated (ranked, decisive) games this season — drives the K-factor.
    pub games: u32,
    pub accredited: bool,
    pub accredited_season: u64,
    pub season: u64,
    pub recent_duels: Vec<u64>,
    pub streak_bonus_season: u64,
    /// True while waiting in a queue (one queue at a time).
    pub queued: bool,
    pub wrong_streak: u32,
    pub cooldown_until: u64,
    /// Placement attempt in progress: seed + start (ms). 0 = none.
    pub placement_seed: U64,
    pub placement_started: u64,
    /// Placement attempts used this season (max 3).
    pub placement_attempts: u32,
    /// Refundable storage deposit attached at registration/top-up.
    pub storage_deposit: NearToken,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            elo: oc::START_ELO,
            xp: 0,
            wins: 0,
            losses: 0,
            draws: 0,
            streak: 0,
            games: 0,
            accredited: false,
            accredited_season: 0,
            season: 0,
            recent_duels: Vec::new(),
            streak_bonus_season: u64::MAX,
            queued: false,
            wrong_streak: 0,
            cooldown_until: 0,
            placement_seed: U64(0),
            placement_started: 0,
            placement_attempts: 0,
            storage_deposit: NearToken::from_yoctonear(0),
        }
    }
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct QueueEntry {
    pub account: AccountId,
    pub joined_ms: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Duel {
    pub id: u64,
    pub a: AccountId,
    pub b: AccountId,
    pub tier: u8,
    pub casual: bool,
    pub stake: NearToken,
    /// u64 as a JSON string (JS numbers are only exact below 2^53).
    pub seed: U64,
    pub started_at: u64,
    /// Commits close here.
    pub deadline: u64,
    /// Reveals close here; settlement is always possible after it.
    pub reveal_deadline: u64,
    pub hash_a: Option<String>,
    pub hash_b: Option<String>,
    /// 1 = committed first, 2 = committed second, 0 = not committed.
    pub order_a: u8,
    pub order_b: u8,
    pub revealed_a: bool,
    pub revealed_b: bool,
    pub correct_a: bool,
    pub correct_b: bool,
    /// v1.5 placeholder: judge-signed verdict hash (does not affect outcome).
    pub verdict_hash: Option<String>,
    pub settled: bool,
    pub winner: Option<AccountId>,
    pub draw: bool,
    /// Season whose ladder this duel counts for (snapshot at match time).
    pub season: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct TopRow {
    pub account: AccountId,
    pub elo: u32,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Overclock {
    owner: AccountId,
    /// Account with no access keys; `flush_burn` sends the burn share here.
    burn_account: AccountId,
    players: IterableMap<AccountId, Player>,
    queues: IterableMap<String, Vec<QueueEntry>>,
    duels: IterableMap<u64, Duel>,
    /// Season rewards waiting to be pulled with `claim_reward`.
    claims: IterableMap<AccountId, NearToken>,
    judge: Option<AccountId>,
    season_index: u64,
    season_start: u64,
    season_len: u64,
    treasury: NearToken,
    burn: NearToken,
    duel_seq: u64,
    paused: bool,
    top_season: u64,
    top: Vec<TopRow>,
}

#[near(serializers = [json])]
#[derive(Clone)]
pub struct Config {
    pub judge: Option<AccountId>,
    pub burn_account: AccountId,
    pub treasury: NearToken,
    pub burn: NearToken,
    pub paused: bool,
    pub season: u64,
    pub duel_seq: u64,
}

#[near(serializers = [json])]
#[derive(Clone)]
pub struct LeaderRow {
    pub rank: u64,
    pub account: AccountId,
    pub elo: u32,
    pub xp: u64,
    pub wins: u64,
    pub losses: u64,
}

/// Problem as shown to clients — the contract is the only generator.
#[near(serializers = [json])]
#[derive(Clone)]
pub struct ProblemView {
    pub kind: u8,
    pub text: String,
}

// ---------------------------------------------------------------- contract

#[near]
impl Overclock {
    /// Deploy and call `new` in the same batch transaction (so nobody can
    /// front-run initialization). `PanicOnDefault` makes every other call
    /// fail until `new` has run.
    #[init]
    pub fn new(burn_account: AccountId) -> Self {
        Self {
            owner: env::predecessor_account_id(),
            burn_account,
            players: IterableMap::new(b"p"),
            queues: IterableMap::new(b"q"),
            duels: IterableMap::new(b"d"),
            claims: IterableMap::new(b"c"),
            judge: None,
            season_index: 0,
            season_start: env::block_timestamp_ms(),
            season_len: oc::HOUR_MS * 24 * 14,
            treasury: NearToken::from_yoctonear(0),
            burn: NearToken::from_yoctonear(0),
            duel_seq: 0,
            paused: false,
            top_season: 0,
            top: Vec::new(),
        }
    }

    // ---------- views ----------
    pub fn get_player(&self, account: AccountId) -> Option<Player> {
        self.players.get(&account).cloned()
    }

    pub fn get_duel(&self, duel_id: u64) -> Option<Duel> {
        self.duels.get(&duel_id).cloned()
    }

    /// The problem both sides of a duel must solve.
    pub fn get_problem(&self, duel_id: u64) -> Option<ProblemView> {
        self.duels.get(&duel_id).map(|d| {
            let p = oc::duel_problem(d.seed.0, d.tier);
            ProblemView { kind: p.kind, text: p.text() }
        })
    }

    /// The 20 placement problems of the caller's running attempt.
    pub fn get_placement_problems(&self, account: AccountId) -> Vec<ProblemView> {
        match self.players.get(&account) {
            Some(p) if p.placement_started > 0 => (0..oc::PLACEMENT_SIZE)
                .map(|i| {
                    let pr = oc::placement_problem(p.placement_seed.0, i);
                    ProblemView { kind: pr.kind, text: pr.text() }
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn get_config(&self) -> Config {
        Config {
            judge: self.judge.clone(),
            burn_account: self.burn_account.clone(),
            treasury: self.treasury,
            burn: self.burn,
            paused: self.paused,
            season: self.current_season(),
            duel_seq: self.duel_seq,
        }
    }

    pub fn get_claimable(&self, account: AccountId) -> NearToken {
        let banked = self.claims.get(&account).copied().unwrap_or(NearToken::from_yoctonear(0));
        let cur = self.current_season();
        if cur <= self.top_season {
            return banked;
        }
        // A season is due but no mutating call has closed it yet: show what
        // this account would receive so the UI never reports a stale 0.
        let winners = self.season_winners(self.top_season);
        let pool = self.treasury.as_yoctonear() * oc::SEASON_POOL_BPS / 10_000;
        let amounts = oc::season_payouts(pool, winners.len());
        let pending = winners
            .iter()
            .zip(amounts)
            .find(|(row, _)| row.account == account)
            .map(|(_, amt)| amt)
            .unwrap_or(0);
        NearToken::from_yoctonear(banked.as_yoctonear() + pending)
    }

    /// Cheap top-5 of the season in progress (maintained incrementally).
    pub fn get_season_top(&self) -> Vec<TopRow> {
        if self.top_season == self.current_season() {
            self.top.clone()
        } else {
            Vec::new()
        }
    }

    /// Season leaderboard (current season only). O(players): fine for a
    /// hackathon; production should index `overclock:settled` events.
    pub fn get_leaderboard(&self, limit: Option<u64>) -> Vec<LeaderRow> {
        let n = limit.unwrap_or(10).min(50) as usize;
        let season = self.current_season();
        let mut rows: Vec<(AccountId, Player)> = self
            .players
            .iter()
            .filter(|(_, p)| p.accredited && p.season == season)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        rows.sort_by(|a, b| {
            b.1.elo
                .cmp(&a.1.elo)
                .then_with(|| b.1.xp.cmp(&a.1.xp))
                .then_with(|| b.1.wins.cmp(&a.1.wins))
        });
        rows.into_iter()
            .take(n)
            .enumerate()
            .map(|(i, (account, p))| LeaderRow {
                rank: (i + 1) as u64,
                account,
                elo: p.elo,
                xp: p.xp,
                wins: p.wins,
                losses: p.losses,
            })
            .collect()
    }

    // ---------- registration / accreditation ----------
    /// Idempotent: makes sure a player record exists. Never overwrites.
    /// New records must attach the one-time storage deposit (refundable via
    /// `refund_storage_deposit`).
    #[payable]
    pub fn register_agent(&mut self) -> bool {
        self.close_season_if_needed();
        let who = env::predecessor_account_id();
        let attached = env::attached_deposit();
        if self.players.get(&who).is_some() {
            // Do not trap a stray deposit on an idempotent re-register.
            if attached.as_yoctonear() > 0 {
                Promise::new(who).transfer(attached).detach();
            }
            return false;
        }
        assert!(
            attached.as_yoctonear() >= oc::MIN_STORAGE_DEPOSIT,
            "attach the storage deposit (0.00512 NEAR)"
        );
        let mut p = Player::default();
        p.season = self.current_season();
        p.storage_deposit = attached;
        self.players.insert(who.clone(), p);
        env::log_str(&format!(
            "overclock:registered:{}:storage={}",
            who, attached
        ));
        true
    }

    /// Step 1 of accreditation: the contract draws the 20 problems.
    /// Read them with `get_placement_problems`. If the player record is not
    /// storage-funded yet, attach the (refundable) storage deposit here.
    #[payable]
    pub fn start_placement(&mut self) {
        self.close_season_if_needed();
        let who = env::predecessor_account_id();
        let season = self.current_season();
        let mut p = self.load(&who);
        self.roll_season(&mut p, season);
        p.storage_deposit = p.storage_deposit.saturating_add(env::attached_deposit());
        assert!(
            p.storage_deposit.as_yoctonear() >= oc::MIN_STORAGE_DEPOSIT,
            "attach the storage deposit (0.00512 NEAR)"
        );
        assert!(
            !(p.accredited && p.accredited_season == season),
            "already accredited this season"
        );
        assert!(
            p.placement_attempts < oc::MAX_PLACEMENT_ATTEMPTS,
            "placement attempt limit reached for this season"
        );
        let now = env::block_timestamp_ms();
        let mut dom = b"placement:".to_vec();
        dom.extend_from_slice(who.as_bytes());
        dom.extend_from_slice(&now.to_le_bytes());
        dom.extend_from_slice(&p.placement_attempts.to_le_bytes());
        p.placement_seed = U64(oc::derive_seed(&env::random_seed(), &dom));
        p.placement_started = now;
        p.placement_attempts += 1;
        self.players.insert(who.clone(), p);
        env::log_str(&format!("overclock:placement_started:{}", who));
    }

    /// Step 2: submit all 20 integer answers within `PLACEMENT_MS`.
    /// The contract scores them itself — the caller never reports a score.
    pub fn submit_placement(&mut self, answers: Vec<i64>) {
        self.close_season_if_needed();
        let who = env::predecessor_account_id();
        let season = self.current_season();
        let now = env::block_timestamp_ms();
        let mut p = self.load(&who);
        self.roll_season(&mut p, season);
        assert!(p.placement_started > 0, "call start_placement first");
        assert!(
            now <= p.placement_started + oc::PLACEMENT_MS,
            "placement window closed — call start_placement again"
        );
        assert_eq!(
            answers.len(),
            oc::PLACEMENT_SIZE as usize,
            "submit all 20 answers"
        );
        let score = oc::placement_score(p.placement_seed.0, &answers);
        p.elo = oc::elo_from_placement(score);
        p.accredited = true;
        p.accredited_season = season;
        p.season = season;
        p.xp = 0;
        p.streak = 0;
        p.games = 0;
        p.wrong_streak = 0;
        p.placement_started = 0;
        p.placement_seed = U64(0);
        self.players.insert(who.clone(), p.clone());
        env::log_str(&format!(
            "overclock:placement:{}:score={}:elo={}",
            who, score, p.elo
        ));
    }

    /// Post a judge-signed verdict hash for a duel (v1.5 code-output path).
    /// Stored separately; never changes a duel outcome in v1.
    pub fn judge_verdict(&mut self, duel_id: u64, verdict_hash: String) {
        let who = env::predecessor_account_id();
        assert_eq!(Some(&who), self.judge.as_ref(), "only judge");
        let duel = self.duels.get_mut(&duel_id).expect("duel not found");
        assert!(duel.verdict_hash.is_none(), "verdict already posted");
        duel.verdict_hash = Some(verdict_hash);
        env::log_str(&format!("overclock:judge_verdict:duel={}", duel_id));
    }

    // ---------- matchmaking ----------
    /// Join the queue for a tier. Attached deposit must equal the tier stake.
    /// `casual = true` skips accreditation AND the ELO band (NPC demo play);
    /// casual results never touch ELO.
    #[payable]
    pub fn join_queue(&mut self, tier: u8, casual: bool) {
        assert!(!self.paused, "queues paused");
        let stake = oc::tier_stake(tier).expect("tier must be 1, 2 or 3");
        assert_eq!(
            env::attached_deposit(),
            NearToken::from_yoctonear(stake),
            "attached deposit must equal tier stake"
        );
        self.close_season_if_needed();
        let who = env::predecessor_account_id();
        let now = env::block_timestamp_ms();
        let season = self.current_season();
        let key = Self::queue_key(tier, casual);
        // Drop expired / no-longer-accredited waiters before matching, and
        // refund their stake. This also clears our own stale entry so a
        // timed-out player can rejoin.
        let queue = self.queues.get(&key).cloned().unwrap_or_default();
        let mut queue = self.prune_queue(queue, now, tier, season, casual);
        let mut p = self.load(&who);
        self.roll_season(&mut p, season);
        assert!(!p.queued, "already queued — cancel_queue first");
        if !casual {
            assert!(now >= p.cooldown_until, "cooldown active after repeated wrong answers");
            assert!(
                oc::in_window(&p.recent_duels, now) < oc::MAX_DUELS_PER_HOUR,
                "cooldown: max 10 duels/hour"
            );
            assert!(
                p.accredited && p.accredited_season == season,
                "ranked play requires placement accreditation this season"
            );
        }

        let pos = queue.iter().position(|e| {
            if casual {
                return true;
            }
            // Both sides' bands must be satisfied (M10): the waiter's widened
            // band AND our fresh band (150) — so the effective band is the
            // smaller of the two.
            let their_band = oc::band_for_wait(now.saturating_sub(e.joined_ms));
            let my_band = oc::band_for_wait(0);
            let band = their_band.min(my_band) as i64;
            let their = self.players.get(&e.account).map(|x| x.elo).unwrap_or(oc::START_ELO);
            (their as i64 - p.elo as i64).abs() <= band
        });
        match pos {
            Some(i) => {
                let other = queue.remove(i).account;
                self.queues.insert(key, queue);
                if let Some(op) = self.players.get_mut(&other) {
                    op.queued = false;
                }
                self.players.insert(who.clone(), p);
                self.start_duel(other, who, tier, casual, stake, now);
            }
            None => {
                p.queued = true;
                queue.push(QueueEntry { account: who.clone(), joined_ms: now });
                self.queues.insert(key, queue);
                self.players.insert(who.clone(), p);
                env::log_str(&format!(
                    "overclock:queued:{}:tier={}:casual={}",
                    who, tier, casual
                ));
            }
        }
    }

    /// Leave the queue before being matched; stake refunded in full.
    /// Refunds ONLY if the caller was actually queued.
    pub fn cancel_queue(&mut self, tier: u8, casual: bool) {
        let who = env::predecessor_account_id();
        let stake = oc::tier_stake(tier).expect("tier must be 1, 2 or 3");
        let key = Self::queue_key(tier, casual);
        let mut queue = self.queues.get(&key).cloned().unwrap_or_default();
        let before = queue.len();
        queue.retain(|e| e.account != who);
        assert!(queue.len() < before, "not queued in this tier/mode");
        self.queues.insert(key, queue);
        if let Some(p) = self.players.get_mut(&who) {
            p.queued = false;
        }
        Promise::new(who.clone()).transfer(NearToken::from_yoctonear(stake)).detach();
        env::log_str(&format!("overclock:queue_cancelled:{}", who));
    }

    // ---------- duel ----------
    /// Commit `commit_hash(duel_id, account, answer, salt)` while the commit
    /// window is open. Commit order on-chain decides who was first.
    pub fn submit_answer(&mut self, duel_id: u64, answer_hash: String) {
        let who = env::predecessor_account_id();
        let now = env::block_timestamp_ms();
        let duel = self.duels.get_mut(&duel_id).expect("duel not found");
        assert!(!duel.settled, "duel already settled");
        assert!(now <= duel.deadline, "commit window closed");
        assert!(answer_hash.len() == 64, "hash must be 64 hex chars");
        let order = (duel.order_a != 0) as u8 + (duel.order_b != 0) as u8 + 1;
        if who == duel.a {
            assert!(duel.hash_a.is_none(), "already submitted");
            duel.hash_a = Some(answer_hash);
            duel.order_a = order;
        } else if who == duel.b {
            assert!(duel.hash_b.is_none(), "already submitted");
            duel.hash_b = Some(answer_hash);
            duel.order_b = order;
        } else {
            panic!("not a duel participant");
        }
        env::log_str(&format!(
            "overclock:answer_submitted:duel={}:by={}:order={}",
            duel_id, who, order
        ));
    }

    /// Reveal after committing (until `reveal_deadline`); verified on-chain.
    pub fn reveal(&mut self, duel_id: u64, answer: i64, salt: String) {
        let who = env::predecessor_account_id();
        let now = env::block_timestamp_ms();
        let duel = self.duels.get_mut(&duel_id).expect("duel not found");
        assert!(!duel.settled, "duel already settled");
        assert!(now <= duel.reveal_deadline, "reveal window closed");
        // Premature-reveal guard (H-01): a participant may only reveal while
        // the commit window is still open if the opponent has committed too —
        // otherwise the plaintext answer would be exposed in receipt logs
        // while the opponent can still observe it before committing.
        let both_committed = duel.hash_a.is_some() && duel.hash_b.is_some();
        assert!(
            now > duel.deadline || both_committed,
            "commit window still open for opponent"
        );
        let side_a = who == duel.a;
        assert!(side_a || who == duel.b, "not a duel participant");
        let (hash, revealed) = if side_a {
            (duel.hash_a.clone(), duel.revealed_a)
        } else {
            (duel.hash_b.clone(), duel.revealed_b)
        };
        assert!(!revealed, "already revealed");
        let hash = hash.expect("submit a hash first");
        assert!(
            oc::verify_commit(duel_id, who.as_str(), answer, &salt, &hash),
            "answer does not match submitted hash"
        );
        let correct = answer == oc::duel_problem(duel.seed.0, duel.tier).answer();
        if side_a {
            duel.revealed_a = true;
            duel.correct_a = correct;
        } else {
            duel.revealed_b = true;
            duel.correct_b = correct;
        }
        env::log_str(&format!(
            "overclock:revealed:duel={}:by={}:correct={}",
            duel_id, who, correct
        ));
    }

    /// Settle once. Allowed when (a) the reveal window is over, or (b) every
    /// committed side has revealed and either both committed or the commit
    /// window is over. Anyone may call.
    pub fn settle(&mut self, duel_id: u64) {
        self.close_season_if_needed();
        let mut duel = self.duels.get(&duel_id).expect("duel not found").clone();
        assert!(!duel.settled, "already settled");
        let now = env::block_timestamp_ms();
        let ca = duel.hash_a.is_some();
        let cb = duel.hash_b.is_some();
        let all_revealed = (!ca || duel.revealed_a) && (!cb || duel.revealed_b);
        let can_settle = now > duel.reveal_deadline
            || (all_revealed && ((ca && cb) || now > duel.deadline));
        assert!(can_settle, "duel not ready — waiting for commits/reveals");
        duel.settled = true;
        let stake = duel.stake.as_yoctonear();
        let pot = stake * 2;
        // Rating always applies to the season the duel was matched in (M3),
        // never to whatever season happens to be current at settle time.
        let duel_season = duel.season;

        // Winner: among CORRECT reveals, the one that committed first.
        let winner: Option<AccountId> = match (duel.correct_a, duel.correct_b) {
            (true, true) => Some(if duel.order_a < duel.order_b { duel.a.clone() } else { duel.b.clone() }),
            (true, false) => Some(duel.a.clone()),
            (false, true) => Some(duel.b.clone()),
            (false, false) => None,
        };

        let mut pa = self.load(&duel.a);
        let mut pb = self.load(&duel.b);
        // Only rate the duel if both players are still on the ladder it
        // belongs to. If either has rolled into a new season, settle money
        // but leave the new-season stats untouched.
        let rated = !duel.casual && pa.season == duel_season && pb.season == duel_season;
        if rated {
            // abuse guard: 10 wrong answers in a row → 1h cooldown (no fund loss)
            Self::track_wrong(&mut pa, ca && !duel.correct_a, duel.correct_a, now);
            Self::track_wrong(&mut pb, cb && !duel.correct_b, duel.correct_b, now);
        }

        match winner {
            Some(w) => {
                duel.winner = Some(w.clone());
                let (payout, treasury, burn) = oc::split_pot(pot);
                self.treasury = self.treasury.saturating_add(NearToken::from_yoctonear(treasury));
                self.burn = self.burn.saturating_add(NearToken::from_yoctonear(burn));
                Promise::new(w.clone()).transfer(NearToken::from_yoctonear(payout)).detach();

                let a_wins = w == duel.a;
                let mut bonus = 0u128;
                let mut new_elo = 0u32;
                if rated {
                    let (pw, pl) = if a_wins { (&mut pa, &mut pb) } else { (&mut pb, &mut pa) };
                    // XP first: margin must use the PRE-update ratings.
                    pw.xp += oc::xp_for_win(pw.elo, pl.elo);
                    pl.xp += 10;
                    let k = oc::k_for(pw.games, pl.games);
                    let (ew, el) = oc::elo_after_win(pw.elo, pl.elo, k);
                    pw.elo = ew;
                    pl.elo = el;
                    new_elo = ew;
                    pw.wins += 1;
                    pl.losses += 1;
                    pw.games += 1;
                    pl.games += 1;
                    pw.streak += 1;
                    pl.streak = 0;
                    // Streak bonus: 3+ ranked wins, once per season, treasury-funded.
                    if pw.streak >= 3
                        && pw.streak_bonus_season != duel_season
                        && self.treasury.as_yoctonear() >= oc::STREAK_BONUS
                    {
                        bonus = oc::STREAK_BONUS;
                        pw.streak_bonus_season = duel_season;
                        self.treasury = self.treasury.saturating_sub(NearToken::from_yoctonear(bonus));
                    }
                }
                let (ea, eb) = (pa.elo, pb.elo);
                self.players.insert(duel.a.clone(), pa);
                self.players.insert(duel.b.clone(), pb);
                if rated {
                    self.update_top(&duel.a, ea, duel_season);
                    self.update_top(&duel.b, eb, duel_season);
                }
                if bonus > 0 {
                    Promise::new(w.clone()).transfer(NearToken::from_yoctonear(bonus)).detach();
                }
                env::log_str(&format!(
                    "overclock:settled:duel={}:winner={}:payout={}:treasury={}:burn={}:elo={}:rated={}",
                    duel_id, w, payout, treasury, burn, new_elo, rated
                ));
            }
            None => {
                duel.draw = true;
                let (back_a, back_b, treasury, burn) = oc::draw_split(stake);
                self.treasury = self.treasury.saturating_add(NearToken::from_yoctonear(treasury));
                self.burn = self.burn.saturating_add(NearToken::from_yoctonear(burn));
                Promise::new(duel.a.clone()).transfer(NearToken::from_yoctonear(back_a)).detach();
                Promise::new(duel.b.clone()).transfer(NearToken::from_yoctonear(back_b)).detach();
                if rated {
                    pa.draws += 1;
                    pb.draws += 1;
                    pa.streak = 0;
                    pb.streak = 0;
                }
                let (ea, eb) = (pa.elo, pb.elo);
                self.players.insert(duel.a.clone(), pa);
                self.players.insert(duel.b.clone(), pb);
                if rated {
                    // M9: draws are rated settles too, so keep the top-5 in
                    // sync with the leaderboard ordering.
                    self.update_top(&duel.a, ea, duel_season);
                    self.update_top(&duel.b, eb, duel_season);
                }
                env::log_str(&format!(
                    "overclock:draw:duel={}:refund_a={}:refund_b={}:treasury={}:burn={}:rated={}",
                    duel_id, back_a, back_b, treasury, burn, rated
                ));
            }
        }
        self.duels.insert(duel_id, duel);
    }

    // ---------- rewards / burn (permissionless) ----------
    /// Pull a season reward credited by the season rollover. Closing is lazy,
    /// so run it first: a claimer whose season just ended gets paid here
    /// instead of seeing a stale 0.
    pub fn claim_reward(&mut self) {
        self.close_season_if_needed();
        let who = env::predecessor_account_id();
        let amt = self.claims.remove(&who).expect("nothing to claim");
        Promise::new(who.clone()).transfer(amt).detach();
        env::log_str(&format!("overclock:reward_claimed:{}:{}", who, amt));
    }

    /// Permissionless: close any finished season so top-5 payouts are
    /// credited even if nobody else transacts. Safe to call at any time.
    pub fn close_season(&mut self) {
        self.close_season_if_needed();
    }

    /// Return the caller's refundable storage deposit and delete the record.
    /// Not allowed while queued or with an unsettled duel.
    pub fn refund_storage_deposit(&mut self) {
        let who = env::predecessor_account_id();
        let p = self.players.get(&who).expect("not registered").clone();
        assert!(!p.queued, "cancel_queue before refunding storage");
        assert!(
            !self.duels.iter().any(|(_, d)| {
                !d.settled && (d.a == who || d.b == who)
            }),
            "settle your open duel before refunding storage"
        );
        let amt = p.storage_deposit;
        self.players.remove(&who);
        if amt.as_yoctonear() > 0 {
            Promise::new(who.clone()).transfer(amt).detach();
        }
        env::log_str(&format!("overclock:storage_refunded:{}:{}", who, amt));
    }

    /// Send the accumulated burn share to the key-less burn account.
    pub fn flush_burn(&mut self) {
        let amt = self.burn;
        assert!(amt.as_yoctonear() > 0, "nothing to burn");
        self.burn = NearToken::from_yoctonear(0);
        Promise::new(self.burn_account.clone()).transfer(amt).detach();
        env::log_str(&format!("overclock:burn_flushed:{}", amt));
    }

    // ---------- owner / privileged (the ONLY admin calls; no transfers) ----------
    pub fn set_judge(&mut self, judge: Option<AccountId>) {
        self.only_owner();
        self.judge = judge.clone();
        env::log_str(&format!("overclock:judge_set:{:?}", judge));
    }

    /// Restarts the season clock now, keeping the current season id, so a
    /// length change can never retroactively skip or roll a season.
    pub fn set_season_length(&mut self, length_ms: u64) {
        self.only_owner();
        assert!(length_ms >= 60_000, "length must be at least one minute");
        assert!(length_ms <= oc::MAX_SEASON_MS, "length must be at most one year");
        let s = self.current_season();
        // Lock the past progress (H-03): continue from the current season
        // number with `season_start := now`, never recompute from epoch.
        self.season_index = s;
        self.season_start = env::block_timestamp_ms();
        self.season_len = length_ms;
        env::log_str(&format!("overclock:season_length:{}", length_ms));
    }

    #[payable]
    pub fn seed_treasury(&mut self) {
        self.only_owner();
        let amt = env::attached_deposit();
        self.treasury = self.treasury.saturating_add(amt);
        env::log_str(&format!("overclock:treasury_seeded:{}", amt));
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.only_owner();
        self.paused = paused;
        env::log_str(&format!("overclock:paused:{}", paused));
    }

    /// Emergency stop: refund every queued stake (RULES §7). Does not touch
    /// duels that are already matched.
    pub fn emergency_refund_queues(&mut self) {
        self.only_owner();
        self.paused = true;
        let keys: Vec<String> = self.queues.iter().map(|(k, _)| k.clone()).collect();
        for key in keys {
            let tier: u8 = key.split(':').next().and_then(|t| t.parse().ok()).unwrap_or(0);
            let stake = oc::tier_stake(tier).unwrap_or(0);
            if let Some(queue) = self.queues.get(&key).cloned() {
                for e in queue {
                    if let Some(p) = self.players.get_mut(&e.account) {
                        p.queued = false;
                    }
                    if stake > 0 {
                        Promise::new(e.account).transfer(NearToken::from_yoctonear(stake)).detach();
                    }
                }
            }
            self.queues.remove(&key);
        }
        env::log_str("overclock:queues_refunded");
    }
}

// ------------------------------------------------------------ internal helpers

impl Overclock {
    fn only_owner(&self) {
        assert_eq!(env::predecessor_account_id(), self.owner, "only contract owner");
    }

    fn load(&self, a: &AccountId) -> Player {
        self.players.get(a).cloned().unwrap_or_default()
    }

    /// Season number counts `season_index` (locked whenever the owner changes
    /// the season length) plus elapsed full seasons since that change. This
    /// avoids the classic `saturating_sub(s * len)` underflow that reset the
    /// season to a smaller number after a length change (H-03).
    fn current_season(&self) -> u64 {
        let now = env::block_timestamp_ms();
        let elapsed = now.saturating_sub(self.season_start) / self.season_len;
        self.season_index + elapsed
    }

    /// New season → season stats reset and re-accreditation is required
    /// (ELO is re-seeded by the new placement, so each season is a clean ladder).
    fn roll_season(&self, p: &mut Player, season: u64) {
        if p.season != season {
            p.season = season;
            p.xp = 0;
            p.streak = 0;
            p.games = 0;
            p.accredited = false;
            // Per-season abuse state: an old queue/duel-history/cooldown must
            // not leak into the new ladder.
            p.recent_duels.clear();
            p.wrong_streak = 0;
            p.cooldown_until = 0;
            p.placement_started = 0;
            p.placement_seed = U64(0);
            p.placement_attempts = 0;
        }
    }

    fn track_wrong(p: &mut Player, was_wrong: bool, was_correct: bool, now: u64) {
        if was_correct {
            p.wrong_streak = 0;
        } else if was_wrong {
            p.wrong_streak += 1;
            if p.wrong_streak >= oc::WRONG_STREAK_LIMIT {
                p.wrong_streak = 0;
                p.cooldown_until = now + oc::HOUR_MS;
            }
        }
    }

    fn queue_key(tier: u8, casual: bool) -> String {
        format!("{}:{}", tier, if casual { "casual" } else { "ranked" })
    }

    /// Refund and drop queue entries that waited too long or lost the
    /// accreditation required to play the queued mode (M8 / M2).
    fn prune_queue(
        &mut self,
        queue: Vec<QueueEntry>,
        now: u64,
        tier: u8,
        season: u64,
        casual: bool,
    ) -> Vec<QueueEntry> {
        let stake = oc::tier_stake(tier).unwrap_or(0);
        let mut kept = Vec::with_capacity(queue.len());
        for e in queue {
            let expired = now.saturating_sub(e.joined_ms) > oc::QUEUE_MAX_WAIT_MS;
            let eligible = casual
                || self
                    .players
                    .get(&e.account)
                    .map(|p| p.accredited && p.accredited_season == season)
                    .unwrap_or(false);
            if expired || !eligible {
                if let Some(p) = self.players.get_mut(&e.account) {
                    p.queued = false;
                }
                if stake > 0 {
                    Promise::new(e.account.clone())
                        .transfer(NearToken::from_yoctonear(stake))
                        .detach();
                }
                env::log_str(&format!(
                    "overclock:queue_pruned:{}:expired={}:ineligible={}",
                    e.account,
                    expired,
                    !eligible
                ));
            } else {
                kept.push(e);
            }
        }
        kept
    }

    /// Season top-5 in leaderboard order (ELO, XP, wins). Used both when
    /// closing a season and by the `get_claimable` view.
    fn season_winners(&self, season: u64) -> Vec<TopRow> {
        let mut rows: Vec<(AccountId, Player)> = self
            .players
            .iter()
            .filter(|(_, p)| p.accredited && p.season == season)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        rows.sort_by(|a, b| {
            b.1.elo
                .cmp(&a.1.elo)
                .then_with(|| b.1.xp.cmp(&a.1.xp))
                .then_with(|| b.1.wins.cmp(&a.1.wins))
        });
        rows.into_iter()
            .take(5)
            .map(|(account, p)| TopRow { account, elo: p.elo })
            .collect()
    }

    /// Keep the live top-5 of the current season (ELO desc).
    fn update_top(&mut self, who: &AccountId, elo: u32, season: u64) {
        if self.top_season != season {
            return;
        }
        self.top.retain(|r| &r.account != who);
        self.top.push(TopRow { account: who.clone(), elo });
        self.top.sort_by(|x, y| y.elo.cmp(&x.elo).then_with(|| x.account.cmp(&y.account)));
        // Buffer 20, pay 5 (H-M02 from the user-side audit): a rank-6 player
        // can step back in when someone above them drops, so a falling rank-5
        // cannot be frozen into the payout. Payouts still take only the top 5.
        self.top.truncate(20);
    }

    /// First action in a new season closes the previous one: half of the
    /// treasury is credited to its top 5 (pull via `claim_reward`). The top-5
    /// is rebuilt from the leaderboard ordering so the payout matches
    /// `get_leaderboard` of that season (M9).
    fn close_season_if_needed(&mut self) {
        let cur = self.current_season();
        if cur <= self.top_season {
            return;
        }
        let closing = self.top_season;
        let winners = self.season_winners(closing);
        let pool = self.treasury.as_yoctonear() * oc::SEASON_POOL_BPS / 10_000;
        let amounts = oc::season_payouts(pool, winners.len());
        let mut paid = 0u128;
        for (row, amt) in winners.iter().zip(amounts) {
            let prev = self.claims.get(&row.account).map(|x| x.as_yoctonear()).unwrap_or(0);
            self.claims.insert(row.account.clone(), NearToken::from_yoctonear(prev + amt));
            paid += amt;
        }
        self.treasury = self.treasury.saturating_sub(NearToken::from_yoctonear(paid));
        env::log_str(&format!(
            "overclock:season_closed:{}:paid={}:winners={}",
            closing,
            paid,
            winners.len()
        ));
        self.top.clear();
        self.top_season = cur;
    }

    fn start_duel(&mut self, a: AccountId, b: AccountId, tier: u8, casual: bool, stake: u128, now: u64) {
        self.duel_seq += 1;
        let id = self.duel_seq;
        let seed = oc::derive_seed(&env::random_seed(), format!("duel:{}", id).as_bytes());
        let season = self.current_season();
        // Ranked duels count against the hourly cap as soon as they start;
        // casual duels never do (they are demo/NPC play).
        if !casual {
            for acc in [&a, &b] {
                let mut p = self.load(acc);
                p.recent_duels.push(now);
                p.recent_duels.retain(|&t| now.saturating_sub(t) < oc::HOUR_MS);
                self.players.insert(acc.clone(), p);
            }
        }
        let duel = Duel {
            id,
            a: a.clone(),
            b: b.clone(),
            tier,
            casual,
            stake: NearToken::from_yoctonear(stake),
            seed: U64(seed),
            started_at: now,
            deadline: now + oc::DUEL_MS,
            reveal_deadline: now + oc::DUEL_MS + oc::REVEAL_MS,
            hash_a: None,
            hash_b: None,
            order_a: 0,
            order_b: 0,
            revealed_a: false,
            revealed_b: false,
            correct_a: false,
            correct_b: false,
            verdict_hash: None,
            settled: false,
            winner: None,
            draw: false,
            season,
        };
        self.duels.insert(id, duel);
        env::log_str(&format!(
            "overclock:matched:duel={}:a={}:b={}:tier={}:casual={}",
            id, a, b, tier, casual
        ));
    }
}
