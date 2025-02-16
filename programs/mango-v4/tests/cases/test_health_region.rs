use super::*;

#[tokio::test]
async fn test_health_wrap() -> Result<(), TransportError> {
    let mut test_builder = TestContextBuilder::new();
    test_builder.test().set_compute_max_units(100000);
    let context = test_builder.start_default().await;
    let solana = &context.solana.clone();

    let admin = TestKeypair::new();
    let owner = context.users[0].key;
    let payer = context.users[1].key;
    let mints = &context.mints[0..2];
    let payer_mint_accounts = &context.users[1].token_accounts;

    //
    // SETUP: Create a group, account, register a token (mint0)
    //

    let GroupWithTokens { group, tokens, .. } = GroupWithTokensConfig {
        admin,
        payer,
        mints: mints.to_vec(),
        ..GroupWithTokensConfig::default()
    }
    .create(solana)
    .await;

    // SETUP: Create an account with deposits, so the second account can borrow more than it has
    create_funded_account(&solana, group, owner, 0, &context.users[1], mints, 2000, 0).await;

    // SETUP: Make a second account
    let account = send_tx(
        solana,
        AccountCreateInstruction {
            account_num: 1,
            token_count: 8,
            serum3_count: 0,
            perp_count: 0,
            perp_oo_count: 0,
            group,
            owner,
            payer,
        },
    )
    .await
    .unwrap()
    .account;

    // SETUP: deposit something, so only one new token position needs to be created
    // simply because the test code can't deal with two affected banks right now
    send_tx(
        solana,
        TokenDepositInstruction {
            amount: 1,
            reduce_only: false,
            account,
            owner,
            token_account: payer_mint_accounts[0],
            token_authority: payer.clone(),
            bank_index: 0,
        },
    )
    .await
    .unwrap();

    let send_test_tx = |repay_amount| {
        let tokens = tokens.clone();
        async move {
            let mut tx = ClientTransaction::new(solana);
            tx.add_instruction(HealthRegionBeginInstruction { account })
                .await;
            tx.add_instruction(TokenWithdrawInstruction {
                amount: 1000, // more than the 1 token that's on the account
                allow_borrow: true,
                account,
                owner,
                token_account: payer_mint_accounts[0],
                bank_index: 0,
            })
            .await;
            tx.add_instruction(TokenDepositInstruction {
                amount: repay_amount,
                reduce_only: false,
                account,
                owner,
                token_account: payer_mint_accounts[1],
                token_authority: payer.clone(),
                bank_index: 0,
            })
            .await;
            tx.add_instruction(HealthRegionEndInstruction {
                account,
                affected_bank: Some(tokens[1].bank),
            })
            .await;
            tx.send().await
        }
    };

    //
    // TEST: Borrow a lot of token0 without collateral, but repay too little
    //
    {
        send_test_tx(1000).await.unwrap_err();
        let logs = solana.program_log();
        // reaches the End instruction
        assert!(logs
            .iter()
            .any(|line| line.contains("Instruction: HealthRegionEnd")));
        // errors due to health
        assert!(logs
            .iter()
            .any(|line| line.contains("Error Code: HealthMustBePositiveOrIncrease")));
    }

    //
    // TEST: Borrow a lot of token0 without collateral, and repay in token1 in the same tx
    //
    {
        let start_payer_mint0 = solana.token_account_balance(payer_mint_accounts[0]).await;
        let start_payer_mint1 = solana.token_account_balance(payer_mint_accounts[1]).await;

        send_test_tx(3000).await.unwrap();

        assert_eq!(
            solana.token_account_balance(payer_mint_accounts[0]).await - start_payer_mint0,
            1000
        );
        assert_eq!(
            start_payer_mint1 - solana.token_account_balance(payer_mint_accounts[1]).await,
            3000
        );
        assert_eq!(
            account_position(solana, account, tokens[0].bank).await,
            -999
        );
        assert_eq!(
            account_position(solana, account, tokens[1].bank).await,
            3000
        );
        let account_data: MangoAccount = solana.get_account(account).await;
        assert_eq!(account_data.in_health_region, 0);
    }

    Ok(())
}
