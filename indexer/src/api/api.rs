use {
    super::{
        content::{content_response, AcceptEncoding, ContentError},
        query::{to_hash, to_rune_id},
    },
    crate::{
        bitcoin_rpc::PooledClient,
        index::{Index, IndexError},
        subscription::{self, WebhookSubscriptionManager},
    },
    arch_titan_types_api::{
        query, AddressData, AddressTxOut, BlockTip, MempoolTxEntry, Pagination, PaginationResponse,
        RuneResponse, Status, Subscription,
    },
    arch_titan_types_core::{
        Block, InscriptionId, MempoolEntry, SerializedOutPoint, SerializedTxid, Transaction,
        TransactionStatus, TxOut,
    },
    bitcoin::{consensus, Address, BlockHash},
    bitcoincore_rpc::RpcApi,
    http::HeaderMap,
    rustc_hash::FxHashMap as HashMap,
    std::sync::Arc,
    tracing::{error, warn},
    uuid::Uuid,
};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not found: {0}")]
    IndexError(#[from] IndexError),
    #[error("rpc error: {0}")]
    RpcError(#[from] bitcoincore_rpc::Error),
    #[error("content error: {0}")]
    ContentError(#[from] ContentError),
    #[error("subscription error: {0}")]
    SubscriptionError(#[from] subscription::WebhookSubscriptionError),
    #[error("hex error: {0}")]
    HexError(#[from] hex::FromHexError),
    #[error("consensus error: {0}")]
    ConsensusError(#[from] consensus::encode::Error),
    #[error("bitcoind returned txid {returned} for transaction {expected}")]
    TxidMismatch {
        expected: bitcoin::Txid,
        returned: bitcoin::Txid,
    },
}

impl From<subscription::WebhookStoreError> for ApiError {
    fn from(error: subscription::WebhookStoreError) -> Self {
        Self::SubscriptionError(error.into())
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;

pub fn tip(index: Arc<Index>) -> Result<BlockTip> {
    let block_count = index.get_block_count()?;
    let height = block_count.saturating_sub(1);
    let block_hash = index.get_block_hash(height)?;
    Ok(BlockTip {
        height,
        hash: block_hash.to_string(),
        is_at_tip: index.get_is_at_tip()?,
    })
}

pub fn status(index: Arc<Index>) -> Result<Status> {
    let height = index.get_block_count()?.saturating_sub(1);
    let block_hash = index.get_block_hash(height)?;
    Ok(Status {
        block_tip: BlockTip {
            height,
            hash: block_hash.to_string(),
            is_at_tip: index.get_is_at_tip()?,
        },
        runes_count: index.get_runes_count()?,
        mempool_tx_count: index.get_mempool_count()? as u64,
    })
}

pub fn block(index: Arc<Index>, block: &query::Block) -> Result<Block> {
    let hash = to_hash(block, &index)?;
    Ok(index.get_block_by_hash(&hash)?)
}

pub fn block_hash_by_height(index: Arc<Index>, height: u64) -> Result<String> {
    let hash = index.get_block_hash(height)?;
    Ok(hash.to_string())
}

pub fn block_txids(index: Arc<Index>, block: &query::Block) -> Result<Vec<String>> {
    let hash = to_hash(block, &index)?;
    let block = index.get_block_by_hash(&hash)?;
    Ok(block
        .tx_ids
        .into_iter()
        .map(|txid| txid.to_string())
        .collect())
}

pub fn output(index: Arc<Index>, outpoint: &SerializedOutPoint) -> Result<TxOut> {
    Ok(index.get_tx_out(outpoint)?)
}

pub fn inscription_content(
    index: Arc<Index>,
    inscription_id: &InscriptionId,
    accept_encoding: AcceptEncoding,
    csp_origin: Option<String>,
    decompress: bool,
) -> Result<Option<(HeaderMap, Vec<u8>)>> {
    let inscription = index.get_inscription(inscription_id)?;
    let content_response = content_response(inscription, accept_encoding, csp_origin, decompress)?;
    Ok(content_response)
}

pub fn rune(index: Arc<Index>, rune_query: &query::Rune) -> Result<RuneResponse> {
    let rune_id = to_rune_id(rune_query, &index)?;
    let block_count = index.get_block_count()?;
    let rune_response = index
        .get_rune(&rune_id)?
        .to_rune_response(rune_id, block_count);
    Ok(rune_response)
}

pub fn runes(
    index: Arc<Index>,
    pagination: Pagination,
) -> Result<PaginationResponse<RuneResponse>> {
    let rune_entries = index.get_runes(pagination)?;
    let block_count = index.get_block_count()?;
    let rune_responses: Vec<RuneResponse> = rune_entries
        .items
        .into_iter()
        .map(|(rune_id, rune_entry)| rune_entry.to_rune_response(rune_id, block_count))
        .collect();

    Ok(PaginationResponse {
        items: rune_responses,
        offset: rune_entries.offset,
    })
}

pub fn last_rune_transactions(
    index: Arc<Index>,
    rune_query: &query::Rune,
    pagination: Option<Pagination>,
) -> Result<PaginationResponse<SerializedTxid>> {
    let rune_id = to_rune_id(rune_query, &index)?;
    let transactions = index.get_last_rune_transactions(&rune_id, pagination, None)?;
    Ok(transactions)
}

pub fn broadcast_transaction(
    index: Arc<Index>,
    client: PooledClient,
    hex: &str,
) -> Result<SerializedTxid> {
    let transaction: bitcoin::Transaction = consensus::deserialize(&hex::decode(hex)?)?;
    let txid = transaction.compute_txid();
    let serialized_txid = txid.into();

    index.pre_index_new_submitted_transaction(&serialized_txid)?;
    // Whatever happens below, the txid must leave the pre-index set, otherwise
    // mempool sync would skip this transaction forever.
    let _pre_index_guard = PreIndexGuard {
        index: &index,
        txid: &serialized_txid,
    };

    let new_txid = client.send_raw_transaction(hex)?;
    if new_txid != txid {
        return Err(ApiError::TxidMismatch {
            expected: txid,
            returned: new_txid,
        });
    }

    // The transaction is accepted at this point; if the entry lookup fails
    // (RBF, mined, RPC busy) mempool sync will pick it up later.
    match client.get_mempool_entry(&new_txid) {
        Ok(mempool_entry) => index.index_new_submitted_transaction(
            &client,
            &serialized_txid,
            &transaction,
            MempoolEntry::from(&mempool_entry),
        ),
        Err(e) => warn!("Failed to get mempool entry for broadcast tx {txid}: {e}"),
    }

    Ok(serialized_txid)
}

/// Removes a txid from the updater's pre-index set when dropped.
struct PreIndexGuard<'a> {
    index: &'a Index,
    txid: &'a SerializedTxid,
}

impl Drop for PreIndexGuard<'_> {
    fn drop(&mut self) {
        if let Err(e) = self
            .index
            .remove_pre_index_new_submitted_transaction(self.txid)
        {
            error!("Failed to remove pre-indexed txid {}: {}", self.txid, e);
        }
    }
}

pub fn bitcoin_transaction_raw(
    index: Arc<Index>,
    client: PooledClient,
    txid: &SerializedTxid,
) -> Result<Vec<u8>> {
    if index.is_indexing_bitcoin_transactions() {
        Ok(index.get_transaction_raw(txid)?)
    } else {
        Ok(consensus::serialize(
            &client.get_raw_transaction(&txid.into(), None)?,
        ))
    }
}

pub fn bitcoin_transaction_hex(
    index: Arc<Index>,
    client: PooledClient,
    txid: &SerializedTxid,
) -> Result<String> {
    let transaction = bitcoin_transaction_raw(index, client, txid)?;
    Ok(hex::encode(transaction))
}

pub fn transaction(
    index: Arc<Index>,
    client: PooledClient,
    txid: &SerializedTxid,
) -> Result<Transaction> {
    let transaction = if index.is_indexing_bitcoin_transactions() {
        index.get_transaction(txid)?
    } else {
        let status = index.get_transaction_status(txid)?;
        let transaction = client.get_raw_transaction(&txid.into(), None)?;
        let (inputs, outputs) = index.get_inputs_outputs_from_transaction(&transaction, txid)?;

        Transaction::from((transaction, status, inputs, outputs))
    };

    Ok(transaction)
}

pub fn transaction_status(index: Arc<Index>, txid: &SerializedTxid) -> Result<TransactionStatus> {
    Ok(index.get_transaction_status(txid)?)
}

pub fn transaction_statuses(
    index: Arc<Index>,
    txids: &[SerializedTxid],
    blockhash: &Option<BlockHash>,
) -> Result<HashMap<SerializedTxid, Option<TransactionStatus>>> {
    if let Some(blockhash) = blockhash {
        let block_count = index.get_block_count()?;
        let is_tip = block_count > 0 && *blockhash == index.get_block_hash(block_count - 1)?;
        if !is_tip {
            return Err(ApiError::IndexError(IndexError::InvalidBestBlockHash(
                blockhash.to_string(),
            )));
        }
    }

    Ok(index.get_transactions_statuses(txids)?)
}

pub fn mempool_txids(
    index: Arc<Index>,
    pagination: Pagination,
) -> Result<PaginationResponse<SerializedTxid>> {
    Ok(index.get_mempool_txids(pagination)?)
}

pub fn mempool_tx(index: Arc<Index>, txid: &SerializedTxid) -> Result<MempoolEntry> {
    Ok(index.get_mempool_entry(txid)?)
}

pub fn mempool_entries(
    index: Arc<Index>,
    txids: &[SerializedTxid],
) -> Result<HashMap<SerializedTxid, Option<MempoolEntry>>> {
    Ok(index.get_mempool_entries(txids)?)
}

pub fn mempool_entries_page(
    index: Arc<Index>,
    pagination: Pagination,
) -> Result<PaginationResponse<MempoolTxEntry>> {
    let page = index.get_mempool_entries_page(pagination)?;
    Ok(PaginationResponse {
        items: page
            .items
            .into_iter()
            .map(|(txid, entry)| MempoolTxEntry {
                txid: txid.into(),
                entry,
            })
            .collect(),
        offset: page.offset,
    })
}

pub fn mempool_entries_with_ancestors(
    index: Arc<Index>,
    txids: &[SerializedTxid],
) -> Result<HashMap<SerializedTxid, MempoolEntry>> {
    Ok(index.get_mempool_entries_with_ancestors(txids)?)
}

pub fn address(index: Arc<Index>, address: &Address) -> Result<AddressData> {
    Ok(index.get_address_data(address)?)
}

pub fn address_outputs(
    index: Arc<Index>,
    address: &Address,
    pagination: Pagination,
) -> Result<PaginationResponse<AddressTxOut>> {
    Ok(index.get_address_outputs(address, pagination)?)
}

pub fn subscriptions(subscription_manager: Arc<WebhookSubscriptionManager>) -> Vec<Subscription> {
    subscription_manager.get_subscriptions()
}

/// Body of `POST /subscription`. Accepts a full `Subscription` too: any `id` or
/// `last_success_epoch_secs` sent by the client is ignored.
#[derive(Debug, serde::Deserialize)]
pub struct AddSubscriptionRequest {
    pub endpoint: String,
    pub event_types: Vec<arch_titan_types_core::EventType>,
}

/// Register a webhook. The id and timestamps are assigned by the server.
pub async fn add_subscription(
    subscription_manager: Arc<WebhookSubscriptionManager>,
    subscription: AddSubscriptionRequest,
) -> Result<Subscription> {
    Ok(subscription_manager
        .add_subscription(&subscription.endpoint, subscription.event_types)
        .await?)
}

pub fn delete_subscription(
    subscription_manager: Arc<WebhookSubscriptionManager>,
    id: Uuid,
) -> Result<()> {
    Ok(subscription_manager.delete_subscription(&id)?)
}

pub fn get_subscription(
    subscription_manager: Arc<WebhookSubscriptionManager>,
    id: Uuid,
) -> Result<Subscription> {
    Ok(subscription_manager.get_subscription(&id)?)
}
