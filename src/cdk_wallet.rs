//! The token lane's real wallet: cashubtc/cdk, behind the same `Wallet` seam.
//!
//! This is the only part of the client that needs a mint. Everything above it —
//! discovery, pricing, caps, purchase classification, journaling — is unchanged,
//! which is the whole point of the seam.
//!
//! Two operations matter here:
//!   * `mint_token` — NUT-04: ask the mint for a quote, pay the BOLT11 through a
//!     `LightningPayer`, then mint the proofs into a token the gate will accept.
//!   * `is_spent` — NUT-07: the only honest answer to "did that ambiguous
//!     purchase take my money?". Without it an ambiguous result is a guess.

use std::str::FromStr;
use std::sync::Arc;

use cdk::amount::Amount;
use cdk::amount::SplitTarget;
use cdk::nuts::{CurrencyUnit, KeySetInfo, PaymentMethod, Proofs, State, Token};
use cdk::wallet::{Wallet, WalletBuilder};
use cdk_sqlite::wallet::WalletSqliteDatabase;

use crate::lightning::LightningPayer;

pub struct CdkWallet {
    rt: tokio::runtime::Runtime,
    wallet: Wallet,
    unit: CurrencyUnit,
}

fn currency_unit(unit: &str) -> Result<CurrencyUnit, String> {
    match unit {
        "sat" | "sats" => Ok(CurrencyUnit::Sat),
        "msat" | "msats" => Ok(CurrencyUnit::Msat),
        "usd" => Ok(CurrencyUnit::Usd),
        "eur" => Ok(CurrencyUnit::Eur),
        other => Err(format!(
            "unsupported unit {other:?}: cdk knows sat, msat, usd, eur and auth; a custom unit \
             (e.g. eur-cent) is exactly the gap PLAN-paypal-custom-unit-rail.md tracks"
        )),
    }
}

impl CdkWallet {
    /// `db_path` is the wallet's own SQLite file. It holds seeds and proofs, so it
    /// is `0600` and never in a shared directory.
    pub fn open(mint_url: &str, unit: &str, db_path: &str, seed: [u8; 64]) -> Result<Self, String> {
        let unit = currency_unit(unit)?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;

        let wallet = rt.block_on(async {
            let store = WalletSqliteDatabase::new(db_path)
                .await
                .map_err(|e| format!("wallet db {db_path}: {e}"))?;
            WalletBuilder::new()
                .mint_url(mint_url.parse().map_err(|e| format!("bad mint url: {e}"))?)
                .unit(unit.clone())
                .localstore(Arc::new(store))
                .seed(seed)
                .build()
                .map_err(|e| format!("build wallet: {e}"))
        })?;

        Ok(Self { rt, wallet, unit })
    }

    /// NUT-04: quote → pay → mint. Returns a `cashuB…` token string.
    pub fn mint_token(
        &self,
        amount: u64,
        payer: &mut dyn LightningPayer,
    ) -> Result<String, String> {
        self.rt.block_on(async {
            let quote = self
                .wallet
                .mint_quote(
                    PaymentMethod::BOLT11,
                    Some(Amount::from(amount)),
                    None,
                    None,
                )
                .await
                .map_err(|e| format!("mint quote: {e}"))?;

            payer.pay(&quote.request, amount)?;

            let proofs = self
                .wallet
                .mint(&quote.id, SplitTarget::default(), None)
                .await
                .map_err(|e| format!("mint: {e}"))?;

            let token = Token::new(
                self.wallet.mint_url.clone(),
                proofs,
                None,
                self.unit.clone(),
            );
            Ok(token.to_string())
        })
    }

    /// NUT-07 truth about a token: has the mint already marked these proofs spent?
    pub fn is_spent(&self, token: &str) -> Result<bool, String> {
        self.rt.block_on(async {
            let parsed = Token::from_str(token).map_err(|e| format!("unparseable token: {e}"))?;
            let keysets = self
                .wallet
                .keysets(Default::default())
                .await
                .map_err(|e| format!("keysets: {e}"))?;
            // Token proofs carry *short* keyset ids; expanding them needs the
            // keyset list, and only the id matters for that expansion.
            let infos: Vec<KeySetInfo> = keysets
                .iter()
                .map(|k| KeySetInfo {
                    id: k.id,
                    unit: k.unit.clone(),
                    active: k.active.unwrap_or(true),
                    input_fee_ppk: k.input_fee_ppk,
                    final_expiry: None,
                })
                .collect();
            let proofs: Proofs = parsed
                .proofs(&infos)
                .map_err(|e| format!("token proofs: {e}"))?;
            let states = self
                .wallet
                .check_proofs_spent(proofs)
                .await
                .map_err(|e| format!("checkstate: {e}"))?;
            Ok(states.iter().any(|s| s.state == State::Spent))
        })
    }

    pub fn balance(&self) -> Result<u64, String> {
        self.rt.block_on(async {
            self.wallet
                .total_balance()
                .await
                .map(|a| a.to_u64())
                .map_err(|e| e.to_string())
        })
    }
}

/// Adapter that plugs cdk into the client's existing `Wallet` seam.
///
/// The client asks for "a token worth N of unit U from mint M" and never learns
/// that money had to move first; the payer lives in here. Opening a wallet per
/// call is deliberate — the unit and the mint are the wallet's identity, and a
/// single wallet cannot span two units.
pub struct CdkFunding {
    seed: [u8; 64],
    db_dir: String,
    payer: Box<dyn LightningPayer>,
    margin: u64,
}

impl CdkFunding {
    pub fn new(seed: [u8; 64], db_dir: impl Into<String>, payer: Box<dyn LightningPayer>) -> Self {
        Self {
            seed,
            db_dir: db_dir.into(),
            payer,
            margin: 0,
        }
    }

    /// Extra sats minted above the price. A token worth exactly the price is
    /// worth *less* after the mint's swap fee, and the real gate says so:
    /// `payment-error-below-swap-fee`. Zero by default because it is real money,
    /// not a rounding detail.
    pub fn with_margin(mut self, margin: u64) -> Self {
        self.margin = margin;
        self
    }

    fn db_path(&self, mint_url: &str, unit: &str) -> String {
        let slug: String = mint_url
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        format!("{}/wallet-{slug}-{unit}.sqlite", self.db_dir)
    }
}

impl crate::agent::Wallet for CdkFunding {
    fn mint_token(&mut self, mint_url: &str, unit: &str, amount: u64) -> Result<String, String> {
        let wallet = CdkWallet::open(mint_url, unit, &self.db_path(mint_url, unit), self.seed)?;
        wallet.mint_token(amount + self.margin, self.payer.as_mut())
    }

    fn is_spent(&mut self, token: &str) -> Result<bool, String> {
        let parsed = Token::from_str(token).map_err(|e| format!("unparseable token: {e}"))?;
        let mint_url = parsed
            .mint_url()
            .map_err(|e| format!("token has no single mint: {e}"))?;
        let wallet = CdkWallet::open(
            &mint_url.to_string(),
            &self.unit_name(),
            &self.db_path(&mint_url.to_string(), &self.unit_name()),
            self.seed,
        )?;
        wallet.is_spent(token)
    }
}

impl CdkFunding {
    /// The unit is part of the wallet's identity; the token lane is sat-only in
    /// v1 because the gate prices in sat (see the unit-gap plan for eur-cent).
    fn unit_name(&self) -> String {
        "sat".to_string()
    }
}
