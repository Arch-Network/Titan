use arch_titan_types_api::*;
use arch_titan_types_core::*;
use bitcoin::{OutPoint, Txid};
use reqwest::{
    blocking::{Client as BlockingReqwestClient, RequestBuilder, Response},
    header::HeaderMap,
};
use serde::de::DeserializeOwned;
use std::{collections::HashMap, str::FromStr};

use crate::Error;

use super::TitanApiSync;

#[derive(Clone)]
pub struct SyncClient {
    /// The **blocking** HTTP client from `reqwest::blocking`.
    http_client: BlockingReqwestClient,
    /// The base URL for all endpoints (e.g. http://localhost:3030).
    base_url: String,
    /// Optional admin token sent as `Authorization: Bearer <token>` on webhook
    /// subscription endpoints.
    admin_token: Option<String>,
}

impl SyncClient {
    /// Creates a new `SyncClient` for the given `base_url`.
    pub fn new(base_url: &str) -> Self {
        Self {
            http_client: BlockingReqwestClient::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            admin_token: None,
        }
    }

    /// Sets the admin token used to authenticate webhook subscription requests.
    pub fn with_admin_token(mut self, token: impl Into<String>) -> Self {
        self.admin_token = Some(token.into());
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Sends the request and maps non-success statuses to `Error::TitanError`.
    fn send(&self, req: RequestBuilder) -> Result<Response, Error> {
        let response = req.send()?;
        let status = response.status();
        if status.is_success() {
            Ok(response)
        } else {
            Err(Error::TitanError(
                status,
                response.text().unwrap_or_default(),
            ))
        }
    }

    fn authorized(&self, req: RequestBuilder) -> RequestBuilder {
        match &self.admin_token {
            Some(token) => req.bearer_auth(token),
            None => req,
        }
    }

    fn call_text(&self, path: &str) -> Result<String, Error> {
        Ok(self.send(self.http_client.get(self.url(path)))?.text()?)
    }

    fn call_json<T: DeserializeOwned>(&self, req: RequestBuilder) -> Result<T, Error> {
        let text = self.send(req)?.text()?;
        serde_json::from_str(&text).map_err(Error::from)
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.call_json(self.http_client.get(self.url(path)))
    }

    fn call_bytes(&self, path: &str) -> Result<Vec<u8>, Error> {
        Ok(self
            .send(self.http_client.get(self.url(path)))?
            .bytes()?
            .to_vec())
    }
}

impl TitanApiSync for SyncClient {
    fn get_status(&self) -> Result<Status, Error> {
        self.get_json("/status")
    }

    fn get_tip(&self) -> Result<BlockTip, Error> {
        self.get_json("/tip")
    }

    fn get_block(&self, query: &query::Block) -> Result<Block, Error> {
        self.get_json(&format!("/block/{}", query))
    }

    fn get_block_hash_by_height(&self, height: u64) -> Result<String, Error> {
        self.get_json(&format!("/block/{}/hash", height))
    }

    fn get_block_txids(&self, query: &query::Block) -> Result<Vec<String>, Error> {
        self.get_json(&format!("/block/{}/txids", query))
    }

    fn get_address(&self, address: &str) -> Result<AddressData, Error> {
        self.get_json(&format!("/address/{}", address))
    }

    fn get_address_outputs(
        &self,
        address: &str,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<AddressTxOut>, Error> {
        let req = self
            .http_client
            .get(self.url(&format!("/address/{}/outputs", address)));
        self.call_json(with_pagination(req, pagination))
    }

    fn get_transaction(&self, txid: &Txid) -> Result<Transaction, Error> {
        self.get_json(&format!("/tx/{}", txid))
    }

    fn get_transaction_raw(&self, txid: &Txid) -> Result<Vec<u8>, Error> {
        self.call_bytes(&format!("/tx/{}/raw", txid))
    }

    fn get_transaction_hex(&self, txid: &Txid) -> Result<String, Error> {
        self.call_text(&format!("/tx/{}/hex", txid))
    }

    fn get_transaction_status(&self, txid: &Txid) -> Result<TransactionStatus, Error> {
        self.get_json(&format!("/tx/{}/status", txid))
    }

    fn send_transaction(&self, tx_hex: String) -> Result<Txid, Error> {
        let req = self
            .http_client
            .post(self.url("/tx/broadcast"))
            .body(tx_hex);
        let text = self.send(req)?.text()?;
        Txid::from_str(&text).map_err(Error::from)
    }

    fn get_output(&self, outpoint: &OutPoint) -> Result<TxOut, Error> {
        self.get_json(&format!("/output/{}", outpoint))
    }

    fn get_inscription(
        &self,
        inscription_id: &InscriptionId,
    ) -> Result<(HeaderMap, Vec<u8>), Error> {
        let resp = self.send(
            self.http_client
                .get(self.url(&format!("/inscription/{}", inscription_id))),
        )?;
        let headers = resp.headers().clone();
        let bytes = resp.bytes()?.to_vec();
        Ok((headers, bytes))
    }

    fn get_runes(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<RuneResponse>, Error> {
        let req = self.http_client.get(self.url("/runes"));
        self.call_json(with_pagination(req, pagination))
    }

    fn get_rune(&self, rune: &query::Rune) -> Result<RuneResponse, Error> {
        self.get_json(&format!("/rune/{}", rune))
    }

    fn get_rune_transactions(
        &self,
        rune: &query::Rune,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<Txid>, Error> {
        let req = self
            .http_client
            .get(self.url(&format!("/rune/{}/transactions", rune)));
        self.call_json(with_pagination(req, pagination))
    }

    fn get_mempool_txids(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<Txid>, Error> {
        let req = self.http_client.get(self.url("/mempool/txids"));
        self.call_json(with_pagination(req, pagination))
    }

    fn get_mempool_entry(&self, txid: &Txid) -> Result<MempoolEntry, Error> {
        self.get_json(&format!("/mempool/entry/{}", txid))
    }

    fn get_mempool_entries(
        &self,
        txids: &[Txid],
    ) -> Result<HashMap<Txid, Option<MempoolEntry>>, Error> {
        self.call_json(
            self.http_client
                .post(self.url("/mempool/entries"))
                .json(txids),
        )
    }

    fn list_mempool_entries(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<MempoolTxEntry>, Error> {
        let req = self.http_client.get(self.url("/mempool/entries"));
        self.call_json(with_pagination(req, pagination))
    }

    fn get_mempool_entries_with_ancestors(
        &self,
        txids: &[Txid],
    ) -> Result<HashMap<Txid, MempoolEntry>, Error> {
        self.call_json(
            self.http_client
                .post(self.url("/mempool/entries/ancestors"))
                .json(txids),
        )
    }

    fn get_subscription(&self, id: &str) -> Result<Subscription, Error> {
        self.call_json(
            self.authorized(
                self.http_client
                    .get(self.url(&format!("/subscription/{}", id))),
            ),
        )
    }

    fn list_subscriptions(&self) -> Result<Vec<Subscription>, Error> {
        self.call_json(self.authorized(self.http_client.get(self.url("/subscriptions"))))
    }

    fn add_subscription(&self, subscription: &Subscription) -> Result<Subscription, Error> {
        self.call_json(
            self.authorized(self.http_client.post(self.url("/subscription")))
                .json(subscription),
        )
    }

    fn delete_subscription(&self, id: &str) -> Result<(), Error> {
        self.send(
            self.authorized(
                self.http_client
                    .delete(self.url(&format!("/subscription/{}", id))),
            ),
        )?;
        Ok(())
    }
}

fn with_pagination(req: RequestBuilder, pagination: Option<Pagination>) -> RequestBuilder {
    match pagination {
        Some(p) => req.query(&[("skip", p.skip), ("limit", p.limit)]),
        None => req,
    }
}
