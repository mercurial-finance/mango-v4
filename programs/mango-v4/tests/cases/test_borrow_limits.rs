use super::*;

#[tokio::test]
async fn test_bank_utilization_based_borrow_limit() -> Result<(), TransportError> {
    let context = TestContext::new().await;
    let solana = &context.solana.clone();

    let admin = TestKeypair::new();
    let owner = context.users[0].key;
    let payer = context.users[1].key;
    let mints = &context.mints[0..2];
    let payer_mint_accounts = &context.users[1].token_accounts[0..=2];

    let initial_token_deposit = 10_000;

    //
    // SETUP: Create a group and an account
    //

    let GroupWithTokens { group, .. } = GroupWithTokensConfig {
        admin,
        payer,
        mints: mints.to_vec(),
        ..GroupWithTokensConfig::default()
    }
    .create(solana)
    .await;

    //
    // SETUP: Prepare accounts
    //
    let account_0 = create_funded_account(
        &solana,
        group,
        owner,
        0,
        &context.users[1],
        &mints[0..1],
        initial_token_deposit,
        0,
    )
    .await;
    let account_1 = create_funded_account(
        &solana,
        group,
        owner,
        1,
        &context.users[1],
        &mints[1..2],
        initial_token_deposit * 10,
        1,
    )
    .await;

    {
        let deposit_amount = initial_token_deposit;

        // account_1 tries to borrow all existing deposits on mint_0
        // should fail because borrow limit would be reached
        let res = send_tx(
            solana,
            TokenWithdrawInstruction {
                amount: deposit_amount,
                allow_borrow: true,
                account: account_1,
                owner,
                token_account: payer_mint_accounts[0],
                bank_index: 0,
            },
        )
        .await;
        assert!(res.is_err());
        solana.advance_clock().await;

        // account_1 tries to borrow < limit on mint_0
        // should succeed because borrow limit won't be reached
        send_tx(
            solana,
            TokenWithdrawInstruction {
                amount: deposit_amount / 10 * 7,
                allow_borrow: true,
                account: account_1,
                owner,
                token_account: payer_mint_accounts[0],
                bank_index: 0,
            },
        )
        .await
        .unwrap();
        solana.advance_clock().await;

        // account_0 tries to withdraw all remaining on mint_0
        // should succeed because withdraws without borrows are not limited
        send_tx(
            solana,
            TokenWithdrawInstruction {
                amount: deposit_amount / 10 * 3,
                allow_borrow: false,
                account: account_0,
                owner,
                token_account: payer_mint_accounts[0],
                bank_index: 0,
            },
        )
        .await
        .unwrap();
    }

    Ok(())
}

#[tokio::test]
async fn test_bank_net_borrows_based_borrow_limit() -> Result<(), TransportError> {
    let context = TestContext::new().await;
    let solana = &context.solana.clone();

    let admin = TestKeypair::new();
    let owner = context.users[0].key;
    let payer = context.users[1].key;
    let mints = &context.mints[0..2];
    let payer_mint_accounts = &context.users[1].token_accounts[0..=2];

    //
    // SETUP: Create a group and an account
    //

    let GroupWithTokens { group, tokens, .. } = GroupWithTokensConfig {
        admin,
        payer,
        mints: mints.to_vec(),
        ..GroupWithTokensConfig::default()
    }
    .create(solana)
    .await;

    let reset_net_borrows = || {
        let mint = tokens[0].mint.pubkey;
        async move {
            send_tx(
                solana,
                TokenResetNetBorrows {
                    group,
                    admin,
                    mint,
                    // we want to test net borrow limits in isolation
                    min_vault_to_deposits_ratio_opt: Some(0.0),
                    net_borrow_limit_per_window_quote_opt: Some(6000),
                    net_borrow_limit_window_size_ts_opt: Some(1000),
                },
            )
            .await
            .unwrap();
        }
    };

    reset_net_borrows().await;

    //
    // SETUP: Prepare accounts
    //
    let account_0 = create_funded_account(
        &solana,
        group,
        owner,
        0,
        &context.users[1],
        &mints[0..1],
        100_000,
        0,
    )
    .await;
    let account_1 = create_funded_account(
        &solana,
        group,
        owner,
        1,
        &context.users[1],
        &mints[1..2],
        1_000_000,
        1,
    )
    .await;

    reset_net_borrows().await;

    {
        // succeeds because borrow is less than net borrow limit
        send_tx(
            solana,
            TokenWithdrawInstruction {
                amount: 5000,
                allow_borrow: true,
                account: account_1,
                owner,
                token_account: payer_mint_accounts[0],
                bank_index: 0,
            },
        )
        .await
        .unwrap();

        // fails because borrow is greater than remaining margin in net borrow limit
        // (requires the test to be quick enough to avoid accidentally going to the next borrow limit window!)
        let res = send_tx(
            solana,
            TokenWithdrawInstruction {
                amount: 4000,
                allow_borrow: true,
                account: account_1,
                owner,
                token_account: payer_mint_accounts[0],
                bank_index: 0,
            },
        )
        .await;
        assert!(res.is_err());

        // succeeds because is not a borrow
        send_tx(
            solana,
            TokenWithdrawInstruction {
                amount: 4000,
                allow_borrow: false,
                account: account_0,
                owner,
                token_account: payer_mint_accounts[0],
                bank_index: 0,
            },
        )
        .await
        .unwrap();
    }

    reset_net_borrows().await;

    //
    // TEST: If the price goes up, the borrow limit is hit more quickly - it's in USD
    //
    {
        // succeeds because borrow is less than net borrow limit in a fresh window
        {
            send_tx(
                solana,
                TokenWithdrawInstruction {
                    amount: 1000,
                    allow_borrow: true,
                    account: account_1,
                    owner,
                    token_account: payer_mint_accounts[0],
                    bank_index: 0,
                },
            )
            .await
            .unwrap();
        }

        set_bank_stub_oracle_price(solana, group, &tokens[0], admin, 10.0).await;

        // cannot borrow anything: net borrowed 1000 * price 10.0 > limit 6000
        let res = send_tx(
            solana,
            TokenWithdrawInstruction {
                amount: 1,
                allow_borrow: true,
                account: account_1,
                owner,
                token_account: payer_mint_accounts[0],
                bank_index: 0,
            },
        )
        .await;
        assert!(res.is_err());

        set_bank_stub_oracle_price(solana, group, &tokens[0], admin, 5.0).await;

        // cannot borrow this much: (net borrowed 1000 + new borrow 201) * price 5.0 > limit 6000
        let res = send_tx(
            solana,
            TokenWithdrawInstruction {
                amount: 201,
                allow_borrow: true,
                account: account_1,
                owner,
                token_account: payer_mint_accounts[0],
                bank_index: 0,
            },
        )
        .await;
        assert!(res.is_err());

        // can borrow smaller amounts: (net borrowed 1000 + new borrow 199) * price 5.0 < limit 6000
        send_tx(
            solana,
            TokenWithdrawInstruction {
                amount: 199,
                allow_borrow: true,
                account: account_1,
                owner,
                token_account: payer_mint_accounts[0],
                bank_index: 0,
            },
        )
        .await
        .unwrap();
    }

    Ok(())
}
