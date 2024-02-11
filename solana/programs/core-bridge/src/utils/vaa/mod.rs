// mod zero_copy;
// pub use zero_copy::*;

use anchor_lang::{prelude::*, system_program::CreateAccount};
use wormhole_solana_vaas::zero_copy::VaaAccount;

#[derive(Accounts)]
pub struct ClaimVaa<'info> {
    /// Claim account, which acts as replay protection after consuming data from the VAA
    /// account.
    ///
    /// PDA address can either be:
    /// * \[emitter_address, emitter_chain, sequence\]
    /// * \[claim_seed_prefix, emitter_address, emitter_chain, sequence\]
    ///
    /// We encourage the integrator to use a claim seed prefix for his own program. And be aware
    /// that apps like Token Bridge do not do this.
    ///
    /// CHECK: Only this account's PDA bump will be saved to this account.
    pub claim: AccountInfo<'info>,

    /// Payer.
    ///
    /// CHECK: This account's lamports will be used to create the new account.
    pub payer: AccountInfo<'info>,
}

/// This method provides a way to prevent replay attacks on VAAs. It creates a PDA for your program
/// using seeds \[emitter_address, emitter_chain, sequence\]. By calling this method, it creates an
/// account of one byte (storing the bump of this PDA address). If your instruction handler is
/// called again, this step will fail because the account will already exist.
pub fn claim_vaa<'info>(
    ctx: CpiContext<'_, '_, '_, 'info, ClaimVaa<'info>>,
    program_id: &Pubkey,
    vaa: &VaaAccount,
    claim_seed_prefix: Option<&[u8]>,
) -> Result<()> {
    let emitter = vaa.emitter_info();

    // First make sure the claim address is derived as what we expect.
    match claim_seed_prefix {
        Some(prefix_seed) => handle_claim_vaa_prefixed(
            ctx,
            program_id,
            prefix_seed,
            emitter.address,
            emitter.chain.to_be_bytes(),
            emitter.sequence.to_be_bytes(),
        ),
        None => handle_claim_vaa(
            ctx,
            program_id,
            emitter.address,
            emitter.chain.to_be_bytes(),
            emitter.sequence.to_be_bytes(),
        ),
    }
}

fn handle_claim_vaa_prefixed<'info>(
    ctx: CpiContext<'_, '_, '_, 'info, ClaimVaa<'info>>,
    program_id: &Pubkey,
    prefix_seed: &[u8],
    emitter_address_seed: [u8; 32],
    emitter_chain_seed: [u8; 2],
    sequence_seed: [u8; 8],
) -> Result<()> {
    let (expected_addr, bump) = Pubkey::find_program_address(
        &[
            prefix_seed,
            emitter_address_seed.as_ref(),
            emitter_chain_seed.as_ref(),
            sequence_seed.as_ref(),
        ],
        program_id,
    );

    // Make sure the claim account key is what we expect.
    require_keys_eq!(
        ctx.accounts.claim.key(),
        expected_addr,
        ErrorCode::ConstraintSeeds
    );

    wormhole_solana_utils::cpi::system_program::create_account_safe(
        CpiContext::new_with_signer(
            ctx.program,
            CreateAccount {
                from: ctx.accounts.payer,
                to: ctx.accounts.claim.to_account_info(),
            },
            &[&[
                prefix_seed,
                emitter_address_seed.as_ref(),
                emitter_chain_seed.as_ref(),
                sequence_seed.as_ref(),
                &[bump],
            ]],
        ),
        1,
        program_id,
    )?;

    // In the legacy implementation, claim accounts stored a boolean (1 byte). Instead, we repurpose
    // this account to store something a little more useful: the bump of the PDA address.
    ctx.accounts.claim.data.borrow_mut()[0] = bump;

    // Done.
    Ok(())
}

fn handle_claim_vaa<'info>(
    ctx: CpiContext<'_, '_, '_, 'info, ClaimVaa<'info>>,
    program_id: &Pubkey,
    emitter_address_seed: [u8; 32],
    emitter_chain_seed: [u8; 2],
    sequence_seed: [u8; 8],
) -> Result<()> {
    let (expected_addr, bump) = Pubkey::find_program_address(
        &[
            emitter_address_seed.as_ref(),
            emitter_chain_seed.as_ref(),
            sequence_seed.as_ref(),
        ],
        program_id,
    );

    // Make sure the claim account key is what we expect.
    require_keys_eq!(
        ctx.accounts.claim.key(),
        expected_addr,
        ErrorCode::ConstraintSeeds
    );

    wormhole_solana_utils::cpi::system_program::create_account_safe(
        CpiContext::new_with_signer(
            ctx.program,
            CreateAccount {
                from: ctx.accounts.payer,
                to: ctx.accounts.claim.to_account_info(),
            },
            &[&[
                emitter_address_seed.as_ref(),
                emitter_chain_seed.as_ref(),
                sequence_seed.as_ref(),
                &[bump],
            ]],
        ),
        1,
        program_id,
    )?;

    // In the legacy implementation, claim accounts stored a boolean (1 byte). Instead, we repurpose
    // this account to store something a little more useful: the bump of the PDA address.
    ctx.accounts.claim.data.borrow_mut()[0] = bump;

    // Done.
    Ok(())
}
