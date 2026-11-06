//! OverClock Badges — NEP-245 Multi Token (minimal, badge-style).
//! Season rewards for Overclock; owner (the game contract or operator) mints
//! badge tokens (amount 1 per owner per badge, transferable).
//!
//! Implements the core MT Interface subset needed to be usable/valid:
//!   mt_transfer, mt_batch_transfer, mt_token, mt_balance_of,
//!   mt_batch_balance_of, mt_supply, mt_batch_supply, + mint_badge (owner).
//! mt_transfer attaches a required 1 yoctoNEAR deposit, like NEP-245.
//! Events: nep245 mt_mint / mt_transfer.
use near_sdk::store::IterableMap;
use near_sdk::{env, near, require, AccountId, NearToken, PanicOnDefault};

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Token {
    pub token_id: String,
    pub owner_id: Option<AccountId>,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct OverclockBadges {
    owner: AccountId,
    // (account, token_id) -> amount
    balances: IterableMap<(AccountId, String), u128>,
    // token_id -> total supply
    supply: IterableMap<String, u128>,
    // known token ids (for views)
    tokens: Vec<String>,
}

#[near]
impl OverclockBadges {
    /// Deploy flow: deploy -> init `new(owner)` in the same batch. Before
    /// `new` has run every call panics (PanicOnDefault), so nobody can
    /// capture an uninitialised contract.
    #[init]
    pub fn new(owner: AccountId) -> Self {
        Self {
            owner,
            balances: IterableMap::new(b"b"),
            supply: IterableMap::new(b"s"),
            tokens: Vec::new(),
        }
    }

    /// Mint a badge to `account`. Owner only (game contract / operator).
    /// Badges are single-owned (amount = 1) unless `amount > 1`.
    #[payable]
    pub fn mint_badge(&mut self, account: AccountId, token_id: String, amount: Option<u128>) {
        self.only_owner();
        let amount = amount.unwrap_or(1);
        require!(amount > 0, "amount must be positive");
        let key = (account.clone(), token_id.clone());
        let current = self.balances.get(&key).copied().unwrap_or(0);
        self.balances.insert(key, current + amount);
        let supply = self.supply.get(&token_id).copied().unwrap_or(0);
        self.supply.insert(token_id.clone(), supply + amount);
        if !self.tokens.contains(&token_id) {
            self.tokens.push(token_id.clone());
        }
        Self::mt_log_mint(&account, &[token_id.clone()], &[amount.to_string()]);
        env::log_str(&format!("overclock-badges:mint:{}:{}:x{}", account, token_id, amount));
    }

    /// NEP-245 mt_transfer. Requires 1 yoctoNEAR attached deposit.
    #[payable]
    pub fn mt_transfer(
        &mut self,
        receiver_id: AccountId,
        token_id: String,
        amount: String,
        approval: Option<(AccountId, u64)>,
        memo: Option<String>,
    ) {
        require!(
            env::attached_deposit() == NearToken::from_yoctonear(1),
            "attach exactly 1 yoctoNEAR"
        );
        let _ = approval; // no approval management in this minimal impl
        let sender = env::predecessor_account_id();
        let amount_u = Self::parse_amount(&amount);
        self.debit(&sender, &token_id, amount_u);
        self.credit(&receiver_id, &token_id, amount_u);
        Self::mt_log_transfer(&sender, &receiver_id, &[token_id], &[amount], memo.as_deref());
    }

    /// NEP-245 mt_batch_transfer. Requires 1 yoctoNEAR attached deposit.
    #[payable]
    pub fn mt_batch_transfer(
        &mut self,
        receiver_id: AccountId,
        token_ids: Vec<String>,
        amounts: Vec<String>,
        approvals: Option<Vec<Option<(AccountId, u64)>>>,
        memo: Option<String>,
    ) {
        require!(
            env::attached_deposit() == NearToken::from_yoctonear(1),
            "attach exactly 1 yoctoNEAR"
        );
        require!(token_ids.len() == amounts.len(), "token_ids and amounts length mismatch");
        let _ = approvals;
        let sender = env::predecessor_account_id();
        for i in 0..token_ids.len() {
            let amount_u = Self::parse_amount(&amounts[i]);
            self.debit(&sender, &token_ids[i], amount_u);
            self.credit(&receiver_id, &token_ids[i], amount_u);
        }
        Self::mt_log_transfer(&sender, &receiver_id, &token_ids, &amounts, memo.as_deref());
    }

    // ---------- views ----------
    pub fn mt_token(&self, token_ids: Vec<String>) -> Vec<Option<Token>> {
        token_ids
            .into_iter()
            .map(|id| {
                if self.supply.contains_key(&id) {
                    Some(Token { token_id: id, owner_id: None })
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn mt_balance_of(&self, account_id: AccountId, token_id: String) -> String {
        self.balances.get(&(account_id, token_id)).map(|v| v.to_string()).unwrap_or_else(|| "0".to_string())
    }

    pub fn mt_batch_balance_of(&self, account_id: AccountId, token_ids: Vec<String>) -> Vec<String> {
        token_ids
            .into_iter()
            .map(|id| self.mt_balance_of(account_id.clone(), id))
            .collect()
    }

    pub fn mt_supply(&self, token_id: String) -> Option<String> {
        self.supply.get(&token_id).map(|s| s.to_string())
    }

    pub fn mt_batch_supply(&self, token_ids: Vec<String>) -> Vec<Option<String>> {
        token_ids
            .into_iter()
            .map(|id| self.supply.get(&id).map(|s| s.to_string()))
            .collect()
    }

    /// Extra view: everything this contract knows about one account.
    pub fn badges_of(&self, account_id: AccountId) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for id in &self.tokens {
            let b = self.balances.get(&(account_id.clone(), id.clone())).copied().unwrap_or(0);
            if b > 0 {
                out.push((id.clone(), b.to_string()));
            }
        }
        out
    }

    pub fn list_tokens(&self) -> Vec<String> {
        self.tokens.clone()
    }

    // ---------- internals ----------
    fn only_owner(&self) {
        require!(env::predecessor_account_id() == self.owner, "only owner");
    }

    fn parse_amount(s: &str) -> u128 {
        require!(s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty(), "amount must be a non-negative integer string");
        s.parse::<u128>().expect("amount overflow")
    }

    fn debit(&mut self, from: &AccountId, token_id: &str, amount: u128) {
        let key = (from.clone(), token_id.to_string());
        let current = self.balances.get(&key).copied().unwrap_or(0);
        require!(current >= amount, "insufficient balance");
        if current == amount {
            self.balances.remove(&key);
        } else {
            self.balances.insert(key, current - amount);
        }
    }

    fn credit(&mut self, to: &AccountId, token_id: &str, amount: u128) {
        let key = (to.clone(), token_id.to_string());
        let current = self.balances.get(&key).copied().unwrap_or(0);
        self.balances.insert(key, current + amount);
    }

    fn mt_log_mint(owner_id: &AccountId, token_ids: &[String], amounts: &[String]) {
        env::log_str(&format!(
            r#"EVENT_JSON:{{"standard":"nep245","version":"1.0.0","event":"mt_mint","data":[{{"owner_id":"{}","token_ids":{:?},"amounts":{:?}}}]}}"#,
            owner_id, token_ids, amounts
        ));
    }

    fn mt_log_transfer(
        sender_id: &AccountId,
        new_owner_id: &AccountId,
        token_ids: &[String],
        amounts: &[String],
        memo: Option<&str>,
    ) {
        let memo_part = memo.map(|m| format!(",\"memo\":\"{}\"", m)).unwrap_or_default();
        env::log_str(&format!(
            r#"EVENT_JSON:{{"standard":"nep245","version":"1.0.0","event":"mt_transfer","data":[{{"old_owner_id":"{}","new_owner_id":"{}","token_ids":{:?},"amounts":{:?}{}}}]}}"#,
            sender_id, new_owner_id, token_ids, amounts, memo_part
        ));
    }
}

