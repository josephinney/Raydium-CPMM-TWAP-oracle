#![allow(unexpected_cfgs)]
#![allow(deprecated)]

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, TokenAccount};
use bytemuck::{Pod, Zeroable};
use spl_tlv_account_resolution::{
    account::ExtraAccountMeta, solana_pubkey::Pubkey as SplPubkey, state::ExtraAccountMetaList,
};
use spl_transfer_hook_interface::instruction::ExecuteInstruction;
use std::mem::size_of;

declare_id!("hMU9ESApomJ8LWL1B7G3yLoGg3D7mSmowZbWoCKEgZb");

#[program]
pub mod twap_hook_cpmm {
    use super::*;

    // Creates / overwrites the list of extra accounts that SPL will inject and initializes the price ring-buffer.
    // Accounts:
    //   0. payer                     – signer who pays
    //   1. extra_account_meta_list   – PDA [*extra-account-metas][mint]
    //   2. mint                      – mint key
    //   3. price_ring                – PDA [*price-ring][mint]
    //   4. system_program            – System
    pub fn initialize_extra_account_meta_list(
        ctx: Context<InitializeExtraAccountMetaList>,
        pool_id: Pubkey,
        token_0_vault: Pubkey,
        token_1_vault: Pubkey,
    ) -> Result<()> {
        // 1. Accounts with SplPubkey format
        let pool_pubkey = SplPubkey::from(pool_id.to_bytes());
        let vault_0_pubkey = SplPubkey::from(token_0_vault.to_bytes());
        let vault_1_pubkey = SplPubkey::from(token_1_vault.to_bytes());

        // 2. Build vec<ExtraAccountMeta>
        // The first account is the pool, the next two are the vaults.
        // All are read-only ('is_readonly' = true).
        let metas = vec![
            ExtraAccountMeta::new_with_pubkey(&pool_pubkey, false, true).unwrap(),
            ExtraAccountMeta::new_with_pubkey(&vault_0_pubkey, false, true).unwrap(),
            ExtraAccountMeta::new_with_pubkey(&vault_1_pubkey, false, true).unwrap(),
        ];

        // 3. Write list to the account
        // The account size must be enough (in this case, for 3 accounts).
        ExtraAccountMetaList::init::<ExecuteInstruction>(
            &mut ctx.accounts.extra_account_meta_list.try_borrow_mut_data()?,
            &metas,
        )
        .unwrap();

        // 4. Initialize ring buffer
        let mut ring = ctx.accounts.price_ring.load_init()?;
        ring.head = 0;
        ring.bump = ctx.bumps.price_ring;
        ring.points = [PricePoint::default(); RING_BUFFER_SIZE];
        msg!("Ring-buffer inicializado con {} slots", RING_BUFFER_SIZE);

        msg!("ExtraAccountMetaList inicializada para CPMM");
        Ok(())
    }

    // Hook executed by SPL-Token-2022 before each transfer.
    // Order of accounts it receives:
    //   0-3  : source, mint, destination, owner (always)
    //   4    : extra_account_meta_list (always)
    //   5... : injected accounts (pool_id, token_0_vault, token_1_vault)
    #[interface(spl_transfer_hook_interface::execute)]
    pub fn transfer_hook(ctx: Context<TransferHookAccounts>) -> Result<()> {
        //    Verify if ExtraAccountMetaList is initialized.
        //    This check allows creating the CPMM pool without failing, since:
        //    - To create the pool, Raydium transfers tokens => invokes this hook
        //    - But to initialize ExtraAccountMetaList we need pool_id (which doesn't exist yet)
        //    - Solution: allow transfers when extra accounts are not configured yet
        if ctx.accounts.extra_account_meta_list.data_is_empty() {
            msg!("Hook no inicializado, permitiendo transfer");
            return Ok(());
        }

        // 1. Validate that the swap is from Raydium CPMM
        let raydium_cpmm_id = pubkey!("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
        if ctx.accounts.owner.key() != raydium_cpmm_id {
            msg!("No es Raydium CPMM; skipping");
            return Ok(());
        }

        // 2. Read vaults
        let vault_0 = &ctx.accounts.token_0_vault;
        let vault_1 = &ctx.accounts.token_1_vault;

        // 3. Verify which of the two mints has the transfer hook. That will be our BASE TOKEN (the one we want the price of)
        let mint_key = ctx.accounts.mint.key();
        let (reserve_base, reserve_quote, base_decimals, quote_decimals) =
            if vault_0.mint == mint_key {
                (
                    vault_0.amount,
                    vault_1.amount,
                    ctx.accounts.mint_0.decimals,
                    ctx.accounts.mint_1.decimals,
                )
            } else if vault_1.mint == mint_key {
                (
                    vault_1.amount,
                    vault_0.amount,
                    ctx.accounts.mint_1.decimals,
                    ctx.accounts.mint_0.decimals,
                )
            } else {
                return err!(ErrorCode::MintNotInPair);
            };

        // 4. Calculate spot price normalized to 6 decimals
        let price = if reserve_base == 0 {
            0
        } else {
            let factor = 10u64.pow(6 + base_decimals as u32 - quote_decimals as u32);
            (reserve_quote as u128)
                .checked_mul(factor as u128)
                .unwrap()
                .checked_div(reserve_base as u128)
                .unwrap() as u64
        };

        let slot = Clock::get()?.slot;

        // 5. Save in ring buffer
        let mut ring = ctx.accounts.price_ring.load_mut()?;
        let idx = ring.head as usize % RING_BUFFER_SIZE;
        ring.points[idx] = PricePoint { slot, price };
        ring.head = (ring.head + 1) % RING_BUFFER_SIZE as u16;

        msg!("Precio guardado: {} (slot: {})", price, slot);
        Ok(())
    }
}

// Accounts to create / overwrite the extra accounts list.
#[derive(Accounts)]
pub struct InitializeExtraAccountMetaList<'info> {
    #[account(mut)]
    payer: Signer<'info>,

    /// CHECK: PDA that will store the extra accounts list.
    #[account(
        init,
        payer = payer,
        space = ExtraAccountMetaList::size_of(3).unwrap(), // Space for 3 accounts
        seeds = [b"extra-account-metas", mint.key().as_ref()],
        bump
    )]
    pub extra_account_meta_list: AccountInfo<'info>,

    /// CHECK: Token mint; we only need its key for the seed.
    pub mint: AccountInfo<'info>,

    /// PDA that will store the price ring-buffer.
    #[account(
        init,
        payer = payer,
        space = 8 + size_of::<PriceRing>(),
        seeds = [b"price-ring", mint.key().as_ref()],
        bump
    )]
    pub price_ring: AccountLoader<'info, PriceRing>,

    pub system_program: Program<'info, System>,
}

// Accounts that SPL ALWAYS sends before calling the hook.
#[derive(Accounts)]
pub struct TransferHookAccounts<'info> {
    /// CHECK: source token account
    pub source: UncheckedAccount<'info>,
    /// CHECK: token mint (the one with transfer hook)
    pub mint: UncheckedAccount<'info>,
    /// CHECK: destination token account
    pub destination: UncheckedAccount<'info>,
    /// CHECK: owner of source (will be the Raydium program)
    pub owner: UncheckedAccount<'info>,

    /// CHECK: list of extra accounts that SPL must inject
    #[account(
        seeds = [b"extra-account-metas", mint.key().as_ref()],
        bump
    )]
    pub extra_account_meta_list: UncheckedAccount<'info>,

    /// CHECK: Raydium CPMM pool
    pub raydium_cpmm_pool: UncheckedAccount<'info>,
    /// CHECK: pool vault 0
    pub token_0_vault: Account<'info, TokenAccount>,
    /// CHECK: pool vault 1
    pub token_1_vault: Account<'info, TokenAccount>,

    // PDA that stores the ring-buffer
    #[account(
        mut,
        seeds = [b"price-ring", mint.key().as_ref()],
        bump = price_ring.load()?.bump
    )]
    pub price_ring: AccountLoader<'info, PriceRing>,

    // /// Mint info to read decimals
    // #[account(
    //     address = mint.key()
    // )]
    // pub mint_info: Account<'info, Mint>,

    /// Mint info of the vaults (for decimals)
    #[account(
        address = token_0_vault.mint
    )]
    pub mint_0: Account<'info, Mint>,

    #[account(
        address = token_1_vault.mint
    )]
    pub mint_1: Account<'info, Mint>,
}

#[account(zero_copy)]
#[derive(Debug)]
pub struct PriceRing {
    pub head: u16,                              // 2 bytes
    pub bump: u8,                               // 1 byte
    pub _pad: [u8; 5],                          // 5 bytes
    pub points: [PricePoint; RING_BUFFER_SIZE], // 512 * 16 bytes
}

const _: () = {
    assert!(size_of::<PriceRing>() == 2 + 1 + 5 + RING_BUFFER_SIZE * 16);
};

#[derive(Copy, Clone, Debug, AnchorSerialize, AnchorDeserialize, Zeroable, Pod, Default)]
#[repr(C)]
pub struct PricePoint {
    pub slot: u64,  // 8 bytes
    pub price: u64, // 8 bytes
}

const RING_BUFFER_SIZE: usize = 512;

#[error_code]
pub enum ErrorCode {
    #[msg("The mint does not belong to the vault pair")]
    MintNotInPair,
}
