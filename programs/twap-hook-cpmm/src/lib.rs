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

    // Crea / sobrescribe la lista de cuentas extras que el SPL inyectara y inicializa el ring-buffer de precios.
    // Accounts:
    //   0. payer                     – signer que paga
    //   1. extra_account_meta_list   – PDA [*extra-account-metas][mint]
    //   2. mint                      – key del mint
    //   3. price_ring                – PDA [*price-ring][mint]
    //   4. system_program            – System
    pub fn initialize_extra_account_meta_list(
        ctx: Context<InitializeExtraAccountMetaList>,
        pool_id: Pubkey,
        token_0_vault: Pubkey,
        token_1_vault: Pubkey,
    ) -> Result<()> {
        // 1. Cuentas con formato SplPubkey
        let pool_pubkey = SplPubkey::from(pool_id.to_bytes());
        let vault_0_pubkey = SplPubkey::from(token_0_vault.to_bytes());
        let vault_1_pubkey = SplPubkey::from(token_1_vault.to_bytes());

        // 2. Construir vec<ExtraAccountMeta>
        // La primera cuenta es el pool, las siguientes dos son las bóvedas (vaults).
        // Todas son de solo lectura ('is_readonly' = true).
        let metas = vec![
            ExtraAccountMeta::new_with_pubkey(&pool_pubkey, false, true).unwrap(),
            ExtraAccountMeta::new_with_pubkey(&vault_0_pubkey, false, true).unwrap(),
            ExtraAccountMeta::new_with_pubkey(&vault_1_pubkey, false, true).unwrap(),
        ];

        // 3. Escribir lista a la cuenta
        // El tamaño de la cuenta debe ser suficiente (en este caso, para 3 cuentas).
        ExtraAccountMetaList::init::<ExecuteInstruction>(
            &mut ctx.accounts.extra_account_meta_list.try_borrow_mut_data()?,
            &metas,
        )
        .unwrap();

        // 4. Inicializar ring buffer
        let mut ring = ctx.accounts.price_ring.load_init()?;
        ring.head = 0;
        ring.bump = ctx.bumps.price_ring;
        ring.points = [PricePoint::default(); RING_BUFFER_SIZE];
        msg!("Ring-buffer inicializado con {} slots", RING_BUFFER_SIZE);

        msg!("ExtraAccountMetaList inicializada para CPMM");
        Ok(())
    }

    // Hook ejecutado por SPL-Token-2022 antes de cada transferencia.
    // Orden de cuentas que recibe:
    //   0-3  : source, mint, destination, owner (siempre)
    //   4    : extra_account_meta_list (siempre)
    //   5... : cuentas inyectadas (pool_id, token_0_vault, token_1_vault)
    #[interface(spl_transfer_hook_interface::execute)]
    pub fn transfer_hook(ctx: Context<TransferHookAccounts>) -> Result<()> {

        //   Verificar si ExtraAccountMetaList está inicializado.
        //    Este check permite crear el pool CPMM sin fallar, ya que:
        //    - Para crear el pool, Raydium transfiere tokens => invoca este hook
        //    - Pero para inicializar ExtraAccountMetaList necesitamos pool_id (que aún no existe)
        //    - Solución: permitir transfers cuando las cuentas extra no están configuradas todavía
        if ctx.accounts.extra_account_meta_list.data_is_empty() {
            msg!("Hook no inicializado, permitiendo transfer");
            return Ok(());
        }

        // 1. Validar que el swap es de Raydium CPMM
        let raydium_cpmm_id = pubkey!("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
        //let raydium_cpmm_id = pubkey!("AXz2trx51zUQ35W7gonmLiQUVPSdrM82VG1KJNWdym4x"); 
        if ctx.accounts.owner.key() != raydium_cpmm_id {
            msg!("No es Raydium CPMM; skipping");
            return Ok(());
        }

        // 2. Leer vaults
        let vault_0 = &ctx.accounts.token_0_vault;
        let vault_1 = &ctx.accounts.token_1_vault;

        // 3. Detectar cuál de los dos mints es el que tiene transfer hook. Ese será nuestro TOKEN BASE (el que queremos precio de)
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

        // 4. Calcular precio spot normalizado a 6 decimales
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

        // 5. Guardar en ring buffer
        let mut ring = ctx.accounts.price_ring.load_mut()?;
        let idx = ring.head as usize % RING_BUFFER_SIZE;
        ring.points[idx] = PricePoint { slot, price };
        ring.head = (ring.head + 1) % RING_BUFFER_SIZE as u16;

        msg!("Precio guardado: {} (slot: {})", price, slot);
        Ok(())
    }
}

// Accounts para crear / sobrescribir la lista de cuentas extras.
#[derive(Accounts)]
pub struct InitializeExtraAccountMetaList<'info> {
    #[account(mut)]
    payer: Signer<'info>,

    /// CHECK: PDA que almacenará la lista de cuentas extras.
    #[account(
        init,
        payer = payer,
        space = ExtraAccountMetaList::size_of(3).unwrap(), // Espacio para 3 cuentas
        seeds = [b"extra-account-metas", mint.key().as_ref()],
        bump
    )]
    pub extra_account_meta_list: AccountInfo<'info>,

    /// CHECK: Mint del token; solo necesitamos su key para la semilla.
    pub mint: AccountInfo<'info>,

    /// PDA que almacenará el ring-buffer de precios.
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

// Cuentas que SIEMPRE manda el SPL antes de llamar al hook.
#[derive(Accounts)]
pub struct TransferHookAccounts<'info> {
    /// CHECK: token account origen
    pub source: UncheckedAccount<'info>,
    /// CHECK: mint del token (el que tiene transfer hook)
    pub mint: UncheckedAccount<'info>,
    /// CHECK: token account destino
    pub destination: UncheckedAccount<'info>,
    /// CHECK: dueño de source (será el programa de Raydium)
    pub owner: UncheckedAccount<'info>,

    /// CHECK: lista de cuentas extras que el SPL debe inyectar
    #[account(
        seeds = [b"extra-account-metas", mint.key().as_ref()],
        bump
    )]
    pub extra_account_meta_list: UncheckedAccount<'info>,

    /// CHECK: pool de Raydium CPMM
    pub raydium_cpmm_pool: UncheckedAccount<'info>,
    /// CHECK: vault 0 del pool
    pub token_0_vault: Account<'info, TokenAccount>,
    /// CHECK: vault 1 del pool
    pub token_1_vault: Account<'info, TokenAccount>,

    // PDA que almacena el ring-buffer
    #[account(
        mut,
        seeds = [b"price-ring", mint.key().as_ref()],
        bump = price_ring.load()?.bump
    )]
    pub price_ring: AccountLoader<'info, PriceRing>,

    /// Mint info para leer decimales
    #[account(
        address = mint.key()
    )]
    pub mint_info: Account<'info, Mint>,

    /// Mint info de los vaults (para decimales)
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
    #[msg("El mint no pertenece al par de vaults")]
    MintNotInPair,
}
