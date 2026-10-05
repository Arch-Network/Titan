use std::collections::HashMap;

use crate::Error;
use arch_titan_types_api::{
    query, AddressData, AddressTxOut, BlockTip, MempoolTxEntry, Pagination, PaginationResponse,
    RuneResponse, Status, Subscription,
};
use arch_titan_types_core::{
    Block, InscriptionId, MempoolEntry, Transaction, TransactionStatus, TxOut,
};
use async_trait::async_trait;
use bitcoin::{OutPoint, Txid};
use reqwest::header::HeaderMap;

/// Trait for all **async** methods.
#[async_trait]
pub trait TitanApiAsync: Send + Sync + Clone + 'static {
    /// Returns the node's status (e.g., network info, block height).
    async fn get_status(&self) -> Result<Status, Error>;

    /// Returns the current best block tip (height + hash).
    async fn get_tip(&self) -> Result<BlockTip, Error>;

    /// Fetches a block (by height or hash) using `query::Block`.
    async fn get_block(&self, query: &query::Block) -> Result<Block, Error>;

    /// Given a block height, returns the block hash.
    async fn get_block_hash_by_height(&self, height: u64) -> Result<String, Error>;

    /// Returns a list of transaction IDs in a particular block.
    async fn get_block_txids(&self, query: &query::Block) -> Result<Vec<String>, Error>;

    /// Fetches an address's balance summary (value, runes and output count).
    async fn get_address(&self, address: &str) -> Result<AddressData, Error>;

    /// Returns one page of an address's outputs. Follow `offset` as the next `skip`;
    /// an empty page means the end. At most 50 per page.
    async fn get_address_outputs(
        &self,
        address: &str,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<AddressTxOut>, Error>;

    /// Returns a higher-level transaction object (including Runes info) by `txid`.
    async fn get_transaction(&self, txid: &Txid) -> Result<Transaction, Error>;

    /// Returns raw transaction bytes (binary).
    async fn get_transaction_raw(&self, txid: &Txid) -> Result<Vec<u8>, Error>;

    /// Returns raw transaction hex.
    async fn get_transaction_hex(&self, txid: &Txid) -> Result<String, Error>;

    /// Returns the status of a transaction by `txid`.
    async fn get_transaction_status(&self, txid: &Txid) -> Result<TransactionStatus, Error>;

    /// Broadcasts a transaction (raw hex) to the network and returns the resulting `Txid`.
    async fn send_transaction(&self, tx_hex: String) -> Result<Txid, Error>;

    /// Fetches a specific output by outpoint (`<txid>:<vout>`).
    async fn get_output(&self, outpoint: &OutPoint) -> Result<TxOut, Error>;

    /// Returns `(HTTP Headers, Bytes)` for an inscription by its `inscription_id`.
    async fn get_inscription(
        &self,
        inscription_id: &InscriptionId,
    ) -> Result<(HeaderMap, Vec<u8>), Error>;

    /// Lists existing runes, supporting pagination.
    async fn get_runes(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<RuneResponse>, Error>;

    /// Fetches data about a specific rune.
    async fn get_rune(&self, rune: &query::Rune) -> Result<RuneResponse, Error>;

    /// Returns a paginated list of `Txid` for all transactions involving a given `rune`.
    async fn get_rune_transactions(
        &self,
        rune: &query::Rune,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<Txid>, Error>;

    /// Returns one page of mempool txids in txid byte order (at most 1000 per page).
    async fn get_mempool_txids(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<Txid>, Error>;

    /// Returns a single mempool entry by `txid`.
    async fn get_mempool_entry(&self, txid: &Txid) -> Result<MempoolEntry, Error>;

    /// Returns multiple mempool entries by their `txid`s.
    async fn get_mempool_entries(
        &self,
        txids: &[Txid],
    ) -> Result<HashMap<Txid, Option<MempoolEntry>>, Error>;

    /// Returns one page of mempool entries in txid byte order (at most 50 per page).
    async fn list_mempool_entries(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<MempoolTxEntry>, Error>;

    /// Returns the specified mempool entries along with all their ancestors.
    async fn get_mempool_entries_with_ancestors(
        &self,
        txids: &[Txid],
    ) -> Result<HashMap<Txid, MempoolEntry>, Error>;

    /// Fetches a single subscription by `id`.
    async fn get_subscription(&self, id: &str) -> Result<Subscription, Error>;

    /// Lists all subscriptions currently known.
    async fn list_subscriptions(&self) -> Result<Vec<Subscription>, Error>;

    /// Adds (creates) a subscription.
    async fn add_subscription(&self, subscription: &Subscription) -> Result<Subscription, Error>;

    /// Deletes a subscription by `id`.
    async fn delete_subscription(&self, id: &str) -> Result<(), Error>;
}

/// Trait for all **blocking** (synchronous) methods.
pub trait TitanApiSync: Send + Sync + Clone + 'static {
    /// Returns the node's status in a **blocking** manner.
    fn get_status(&self) -> Result<Status, Error>;

    /// Returns the block tip in a **blocking** manner.
    fn get_tip(&self) -> Result<BlockTip, Error>;

    /// Fetches a block by height/hash in a **blocking** manner.
    fn get_block(&self, query: &query::Block) -> Result<Block, Error>;

    /// Given a block height, returns the block hash in a **blocking** manner.
    fn get_block_hash_by_height(&self, height: u64) -> Result<String, Error>;

    /// Returns txids for a block in a **blocking** manner.
    fn get_block_txids(&self, query: &query::Block) -> Result<Vec<String>, Error>;

    /// Returns an address's balance summary in a **blocking** manner.
    fn get_address(&self, address: &str) -> Result<AddressData, Error>;

    /// Returns one page of an address's outputs in a **blocking** manner.
    fn get_address_outputs(
        &self,
        address: &str,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<AddressTxOut>, Error>;

    /// Returns a transaction (with runic info) by `txid` in a **blocking** manner.
    fn get_transaction(&self, txid: &Txid) -> Result<Transaction, Error>;

    /// Returns raw tx bytes in a **blocking** manner.
    fn get_transaction_raw(&self, txid: &Txid) -> Result<Vec<u8>, Error>;

    /// Returns raw tx hex in a **blocking** manner.
    fn get_transaction_hex(&self, txid: &Txid) -> Result<String, Error>;

    /// Returns the status of a transaction by `txid` in a **blocking** manner.
    fn get_transaction_status(&self, txid: &Txid) -> Result<TransactionStatus, Error>;

    /// Broadcasts a raw-hex transaction in a **blocking** manner.
    fn send_transaction(&self, tx_hex: String) -> Result<Txid, Error>;

    /// Fetches a specific output (outpoint) in a **blocking** manner.
    fn get_output(&self, outpoint: &OutPoint) -> Result<TxOut, Error>;

    /// Fetches an inscription (headers + bytes) by `inscription_id`, blocking.
    fn get_inscription(
        &self,
        inscription_id: &InscriptionId,
    ) -> Result<(HeaderMap, Vec<u8>), Error>;

    /// Returns paginated runes in a **blocking** manner.
    fn get_runes(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<RuneResponse>, Error>;

    /// Fetches data for a specific rune in a **blocking** manner.
    fn get_rune(&self, rune: &query::Rune) -> Result<RuneResponse, Error>;

    /// Returns transactions for a given rune in a **blocking** manner.
    fn get_rune_transactions(
        &self,
        rune: &query::Rune,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<Txid>, Error>;

    /// Returns one page of mempool txids in a **blocking** manner.
    fn get_mempool_txids(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<Txid>, Error>;

    /// Returns a single mempool entry by `txid`.
    fn get_mempool_entry(&self, txid: &Txid) -> Result<MempoolEntry, Error>;

    /// Returns multiple mempool entries by their `txid`s
    fn get_mempool_entries(
        &self,
        txids: &[Txid],
    ) -> Result<HashMap<Txid, Option<MempoolEntry>>, Error>;

    /// Returns one page of mempool entries in a **blocking** manner.
    fn list_mempool_entries(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<MempoolTxEntry>, Error>;

    /// Returns the specified mempool entries along with all their ancestors.
    fn get_mempool_entries_with_ancestors(
        &self,
        txids: &[Txid],
    ) -> Result<HashMap<Txid, MempoolEntry>, Error>;

    /// Fetches a single subscription by `id`, blocking.
    fn get_subscription(&self, id: &str) -> Result<Subscription, Error>;

    /// Lists all subscriptions, blocking.
    fn list_subscriptions(&self) -> Result<Vec<Subscription>, Error>;

    /// Adds a new subscription, blocking.
    fn add_subscription(&self, subscription: &Subscription) -> Result<Subscription, Error>;

    /// Deletes a subscription by `id`, blocking.
    fn delete_subscription(&self, id: &str) -> Result<(), Error>;
}
