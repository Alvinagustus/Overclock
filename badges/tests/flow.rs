#[cfg(test)]
mod tests {
    use near_sdk::{testing_env, test_utils::{accounts, VMContextBuilder}, AccountId, NearToken, VMContext};
    use overclock_badges::OverclockBadges;

    fn context(predecessor: AccountId, deposit: u128) -> VMContext {
        VMContextBuilder::new()
            .predecessor_account_id(predecessor)
            .attached_deposit(NearToken::from_yoctonear(deposit))
            .build()
    }

    #[test]
    fn mint_then_transfer_and_views() {
        let owner = accounts(0); // alice
        testing_env!(context(owner.clone(), 0));
        let mut c = OverclockBadges::new(owner.clone());
        let winner = "winner.testnet".to_string().parse::<AccountId>().unwrap();
        c.mint_badge(winner.clone(), "champion".to_string(), Some(1));

        assert_eq!(c.mt_balance_of(winner.clone(), "champion".to_string()), "1");
        assert_eq!(c.mt_supply("champion".to_string()).as_deref(), Some("1"));
        assert_eq!(c.badges_of(winner.clone()), vec![("champion".to_string(), "1".to_string())]);

        let recipient = "recipient.testnet".to_string().parse::<AccountId>().unwrap();
        testing_env!(context(winner.clone(), 1));
        c.mt_transfer(recipient.clone(), "champion".to_string(), "1".to_string(), None, None);

        assert_eq!(c.mt_balance_of(winner, "champion".to_string()), "0");
        assert_eq!(c.mt_balance_of(recipient, "champion".to_string()), "1");
    }

    #[test]
    #[should_panic(expected = "only owner")]
    fn only_owner_can_mint() {
        let owner = accounts(0);
        testing_env!(context(owner.clone(), 0));
        let mut c = OverclockBadges::new(owner);
        testing_env!(context("intruder.testnet".parse::<AccountId>().unwrap(), 0));
        c.mint_badge("x.testnet".parse::<AccountId>().unwrap(), "champion".to_string(), None);
    }

    #[test]
    #[should_panic(expected = "attach exactly 1 yoctoNEAR")]
    fn transfer_requires_one_yocto() {
        let owner = accounts(0);
        testing_env!(context(owner.clone(), 0));
        let mut c = OverclockBadges::new(owner);
        c.mint_badge("w.testnet".parse::<AccountId>().unwrap(), "champion".to_string(), None);
        testing_env!(context("w.testnet".parse::<AccountId>().unwrap(), 0));
        c.mt_transfer("r.testnet".parse::<AccountId>().unwrap(), "champion".to_string(), "1".to_string(), None, None);
    }

    #[test]
    #[should_panic(expected = "insufficient balance")]
    fn cannot_overspend() {
        let owner = accounts(0);
        testing_env!(context(owner.clone(), 0));
        let mut c = OverclockBadges::new(owner);
        testing_env!(context("w.testnet".parse::<AccountId>().unwrap(), 1));
        c.mt_transfer("r.testnet".parse::<AccountId>().unwrap(), "champion".to_string(), "1".to_string(), None, None);
    }

    #[test]
    fn uninitialised_badges_cannot_be_captured() {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        testing_env!(context(accounts(0), 0));
        // PanicOnDefault: a separate-tx init capture must fail.
        assert!(catch_unwind(AssertUnwindSafe(|| {
            let _: OverclockBadges = <OverclockBadges as Default>::default();
        }))
        .is_err());
    }
}
