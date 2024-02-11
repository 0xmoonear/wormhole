use crate::{
    constants::CUSTODY_AUTHORITY_SEED_PREFIX, error::TokenBridgeError,
    legacy::instruction::EmptyArgs, state::RegisteredEmitter,
};
use anchor_lang::prelude::*;
use anchor_spl::token;
use core_bridge_program::sdk as core_bridge;
use wormhole_raw_vaas::token_bridge::TokenBridgeMessage;
use wormhole_solana_vaas::zero_copy::VaaAccount;

#[derive(Accounts)]
pub struct CompleteTransferNative<'info> {
    #[account(mut)]
    payer: Signer<'info>,

    /// Previously needed config account.
    ///
    /// CHECK: This account is unchecked.
    _config: UncheckedAccount<'info>,

    /// VAA account, which may either be the new EncodedVaa account or legacy PostedVaaV1
    /// account.
    ///
    /// CHECK: This account will be read via zero-copy deserialization in the instruction
    /// handler, which will determine which type of VAA account is being used. If this account
    /// is the legacy PostedVaaV1 account, its PDA address will be verified by this zero-copy
    /// reader.
    #[account(owner = core_bridge::id())]
    vaa: AccountInfo<'info>,

    /// Claim account (mut), which acts as replay protection after consuming data from the VAA
    /// account.
    ///
    /// Seeds: [emitter_address, emitter_chain, sequence],
    /// seeds::program = token_bridge_program.
    ///
    /// CHECK: This account is created via [claim_vaa](core_bridge_program::sdk::claim_vaa).
    /// This account can only be created once for this VAA.
    #[account(mut)]
    claim: AccountInfo<'info>,

    /// This account is a foreign token Bridge and is created via the Register Chain governance
    /// decree.
    ///
    /// NOTE: The seeds of this account are insane because they include the emitter address, which
    /// allows registering multiple emitter addresses for the same chain ID. These seeds are not
    /// checked via Anchor macro, but will be checked in the access control function instead.
    ///
    /// See the `require_valid_token_bridge_vaa` instruction handler for more details.
    registered_emitter: Account<'info, core_bridge::legacy::LegacyAnchorized<RegisteredEmitter>>,

    /// Recipient token account.
    #[account(
        mut,
        token::mint = mint
    )]
    recipient_token: Account<'info, token::TokenAccount>,

    /// CHECK: Payer (relayer) token account. Because we check the mint of the custody token
    /// account, we can be sure that this token account is the same mint since the Token Program
    /// transfer instruction handler checks that the mints of these two accounts must be the same.
    ///
    /// NOTE: We will check that the owner of this account belongs to the payer of this transaction.
    #[account(
        mut,
        token::mint = mint,
        token::authority = payer
    )]
    payer_token: Account<'info, token::TokenAccount>,

    /// Custody token account.
    ///
    /// CHECK: Because we are deriving this PDA's address, we ensure that this account is the Token
    /// Bridge's custody token account. And because this account can only be created on a native
    /// mint's outbound transfer (since these tokens originated from Solana), this account should
    /// already be created.
    #[account(
        mut,
        seeds = [mint.key().as_ref()],
        bump,
    )]
    custody_token: AccountInfo<'info>,

    /// Native mint. We know this mint does not belong to the Token Bridge because a custody token
    /// account exists for it.
    mint: Account<'info, token::Mint>,

    /// CHECK: This account is the authority that can move tokens from the custody account.
    #[account(
        seeds = [CUSTODY_AUTHORITY_SEED_PREFIX],
        bump,
    )]
    custody_authority: AccountInfo<'info>,

    /// Previously needed sysvar.
    ///
    /// CHECK: This account is unchecked.
    _rent: UncheckedAccount<'info>,

    system_program: Program<'info, System>,
    token_program: Program<'info, token::Token>,
}

impl<'info> core_bridge::legacy::ProcessLegacyInstruction<'info, EmptyArgs>
    for CompleteTransferNative<'info>
{
    const LOG_IX_NAME: &'static str = "LegacyCompleteTransferNative";

    const ANCHOR_IX_FN: fn(Context<Self>, EmptyArgs) -> Result<()> = complete_transfer_native;

    fn order_account_infos<'a>(
        account_infos: &'a [AccountInfo<'info>],
    ) -> Result<Vec<AccountInfo<'info>>> {
        super::order_complete_transfer_account_infos(account_infos)
    }
}

impl<'info> CompleteTransferNative<'info> {
    fn constraints(ctx: &Context<Self>) -> Result<()> {
        let (token_chain, token_address) = super::validate_token_transfer_vaa(
            &ctx.accounts.vaa,
            &ctx.accounts.registered_emitter,
            &ctx.accounts.recipient_token,
        )?;

        // For native transfers, this mint must have been created on Solana.
        require_eq!(
            token_chain,
            wormhole_solana_consts::SOLANA_CHAIN,
            TokenBridgeError::WrappedAsset
        );

        // Mint account must agree with the encoded token address.
        require_keys_eq!(
            ctx.accounts.mint.key(),
            Pubkey::from(token_address),
            TokenBridgeError::InvalidMint
        );

        // Done.
        Ok(())
    }
}

#[access_control(CompleteTransferNative::constraints(&ctx))]
fn complete_transfer_native(ctx: Context<CompleteTransferNative>, _args: EmptyArgs) -> Result<()> {
    let vaa = VaaAccount::load(&ctx.accounts.vaa);

    // Create the claim account to provide replay protection. Because this instruction creates this
    // account every time it is executed, this account cannot be created again with this emitter
    // address, chain and sequence combination.
    core_bridge::claim_vaa(
        CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            core_bridge::ClaimVaa {
                claim: ctx.accounts.claim.to_account_info(),
                payer: ctx.accounts.payer.to_account_info(),
            },
        ),
        &crate::ID,
        &vaa,
        None,
    )?;

    let transfer = TokenBridgeMessage::try_from(vaa.payload())
        .unwrap()
        .to_transfer_unchecked();

    let decimals = ctx.accounts.mint.decimals;

    // Denormalize transfer transfer_amount and relayer payouts based on this mint's decimals. When these
    // transfers were made outbound, the amounts were normalized, so it is safe to unwrap these
    // operations.
    let mut transfer_amount = transfer
        .encoded_amount()
        .denorm(decimals)
        .try_into()
        .expect("Solana token amounts are u64");
    let relayer_payout = transfer
        .encoded_relayer_fee()
        .denorm(decimals)
        .try_into()
        .unwrap();

    // Save references to these accounts to be used later.
    let recipient_token = &ctx.accounts.recipient_token;
    let payer_token = &ctx.accounts.payer_token;
    let custody_token = &ctx.accounts.custody_token;

    // Custody authority is who has the authority to transfer tokens from the custody account.
    let custody_authority_seeds = &[
        CUSTODY_AUTHORITY_SEED_PREFIX,
        &[ctx.bumps["custody_authority"]],
    ];

    // If there is a payout to the relayer and the relayer's token account differs from the transfer
    // recipient's, we have to make an extra transfer.
    if relayer_payout > 0 && recipient_token.key() != payer_token.key() {
        // NOTE: This math operation is safe because the relayer payout is always <= to the
        // total outbound transfer transfer_amount.
        transfer_amount -= relayer_payout;

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                token::Transfer {
                    from: custody_token.to_account_info(),
                    to: payer_token.to_account_info(),
                    authority: ctx.accounts.custody_authority.to_account_info(),
                },
                &[custody_authority_seeds],
            ),
            relayer_payout,
        )?;
    }

    // Finally transfer remaining transfer_amount to recipient.
    token::transfer(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            token::Transfer {
                from: custody_token.to_account_info(),
                to: recipient_token.to_account_info(),
                authority: ctx.accounts.custody_authority.to_account_info(),
            },
            &[custody_authority_seeds],
        ),
        transfer_amount,
    )
}
