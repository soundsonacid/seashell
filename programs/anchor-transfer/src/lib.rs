use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};

declare_id!("Bench11111111111111111111111111111111111111");

#[program]
pub mod anchor_transfer {
    use super::*;

    pub fn transfer_tokens(ctx: Context<TransferTokens>, amount: u64) -> Result<()> {
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.source.to_account_info(),
                    to: ctx.accounts.destination.to_account_info(),
                    authority: ctx.accounts.authority.to_account_info(),
                },
            ),
            amount,
        )?;
        msg!("transfer complete");
        Ok(())
    }
}

#[derive(Accounts)]
pub struct TransferTokens<'info> {
    #[account(mut)]
    pub source: Account<'info, TokenAccount>,
    #[account(mut)]
    pub destination: Account<'info, TokenAccount>,
    pub authority: Signer<'info>,
    pub token_program: Program<'info, Token>,
}
