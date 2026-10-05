use arch_titan_types_api::*;
use arch_titan_types_core::*;
use bitcoin::{OutPoint, Txid};
use reqwest::{header::HeaderMap, Client as AsyncReqwestClient, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use std::{collections::HashMap, str::FromStr};

use crate::Error;

use super::TitanApiAsync;

#[derive(Clone)]
pub struct AsyncClient {
    /// The async HTTP client from `reqwest`.
    http_client: AsyncReqwestClient,
    /// The base URL for all endpoints (e.g. http://localhost:3030).
    base_url: String,
    /// Optional admin token sent as `Authorization: Bearer <token>` on webhook
    /// subscription endpoints.
    admin_token: Option<String>,
}

impl AsyncClient {
    /// Creates a new `AsyncClient` for the given `base_url`.
    pub fn new(base_url: &str) -> Self {
        Self {
            http_client: AsyncReqwestClient::new(),
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
    async fn send(&self, req: RequestBuilder) -> Result<Response, Error> {
        let response = req.send().await?;
        let status = response.status();
        if status.is_success() {
            Ok(response)
        } else {
            Err(Error::TitanError(
                status,
                response.text().await.unwrap_or_default(),
            ))
        }
    }

    fn authorized(&self, req: RequestBuilder) -> RequestBuilder {
        match &self.admin_token {
            Some(token) => req.bearer_auth(token),
            None => req,
        }
    }

    async fn call_text(&self, path: &str) -> Result<String, Error> {
        Ok(self
            .send(self.http_client.get(self.url(path)))
            .await?
            .text()
            .await?)
    }

    async fn call_json<T: DeserializeOwned>(&self, req: RequestBuilder) -> Result<T, Error> {
        let text = self.send(req).await?.text().await?;
        serde_json::from_str(&text).map_err(Error::from)
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.call_json(self.http_client.get(self.url(path))).await
    }

    async fn call_bytes(&self, path: &str) -> Result<Vec<u8>, Error> {
        Ok(self
            .send(self.http_client.get(self.url(path)))
            .await?
            .bytes()
            .await?
            .to_vec())
    }
}

#[async_trait::async_trait]
impl TitanApiAsync for AsyncClient {
    async fn get_status(&self) -> Result<Status, Error> {
        self.get_json("/status").await
    }

    async fn get_tip(&self) -> Result<BlockTip, Error> {
        self.get_json("/tip").await
    }

    async fn get_block(&self, query: &query::Block) -> Result<Block, Error> {
        self.get_json(&format!("/block/{}", query)).await
    }

    async fn get_block_hash_by_height(&self, height: u64) -> Result<String, Error> {
        self.get_json(&format!("/block/{}/hash", height)).await
    }

    async fn get_block_txids(&self, query: &query::Block) -> Result<Vec<String>, Error> {
        self.get_json(&format!("/block/{}/txids", query)).await
    }

    async fn get_address(&self, address: &str) -> Result<AddressData, Error> {
        self.get_json(&format!("/address/{}", address)).await
    }

    async fn get_address_outputs(
        &self,
        address: &str,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<AddressTxOut>, Error> {
        let req = self
            .http_client
            .get(self.url(&format!("/address/{}/outputs", address)));
        self.call_json(with_pagination(req, pagination)).await
    }

    async fn get_transaction(&self, txid: &Txid) -> Result<Transaction, Error> {
        self.get_json(&format!("/tx/{}", txid)).await
    }

    async fn get_transaction_raw(&self, txid: &Txid) -> Result<Vec<u8>, Error> {
        self.call_bytes(&format!("/tx/{}/raw", txid)).await
    }

    async fn get_transaction_hex(&self, txid: &Txid) -> Result<String, Error> {
        self.call_text(&format!("/tx/{}/hex", txid)).await
    }

    async fn get_transaction_status(&self, txid: &Txid) -> Result<TransactionStatus, Error> {
        self.get_json(&format!("/tx/{}/status", txid)).await
    }

    async fn send_transaction(&self, tx_hex: String) -> Result<Txid, Error> {
        let req = self
            .http_client
            .post(self.url("/tx/broadcast"))
            .body(tx_hex);
        let text = self.send(req).await?.text().await?;
        Txid::from_str(&text).map_err(Error::from)
    }

    async fn get_output(&self, outpoint: &OutPoint) -> Result<TxOut, Error> {
        self.get_json(&format!("/output/{}", outpoint)).await
    }

    async fn get_inscription(
        &self,
        inscription_id: &InscriptionId,
    ) -> Result<(HeaderMap, Vec<u8>), Error> {
        let resp = self
            .send(
                self.http_client
                    .get(self.url(&format!("/inscription/{}", inscription_id))),
            )
            .await?;
        let headers = resp.headers().clone();
        let bytes = resp.bytes().await?.to_vec();
        Ok((headers, bytes))
    }

    async fn get_runes(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<RuneResponse>, Error> {
        let req = self.http_client.get(self.url("/runes"));
        self.call_json(with_pagination(req, pagination)).await
    }

    async fn get_rune(&self, rune: &query::Rune) -> Result<RuneResponse, Error> {
        self.get_json(&format!("/rune/{}", rune)).await
    }

    async fn get_rune_transactions(
        &self,
        rune: &query::Rune,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<Txid>, Error> {
        let req = self
            .http_client
            .get(self.url(&format!("/rune/{}/transactions", rune)));
        self.call_json(with_pagination(req, pagination)).await
    }

    async fn get_mempool_txids(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<Txid>, Error> {
        let req = self.http_client.get(self.url("/mempool/txids"));
        self.call_json(with_pagination(req, pagination)).await
    }

    async fn get_mempool_entry(&self, txid: &Txid) -> Result<MempoolEntry, Error> {
        self.get_json(&format!("/mempool/entry/{}", txid)).await
    }

    async fn get_mempool_entries(
        &self,
        txids: &[Txid],
    ) -> Result<HashMap<Txid, Option<MempoolEntry>>, Error> {
        self.call_json(
            self.http_client
                .post(self.url("/mempool/entries"))
                .json(txids),
        )
        .await
    }

    async fn list_mempool_entries(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<PaginationResponse<MempoolTxEntry>, Error> {
        let req = self.http_client.get(self.url("/mempool/entries"));
        self.call_json(with_pagination(req, pagination)).await
    }

    async fn get_mempool_entries_with_ancestors(
        &self,
        txids: &[Txid],
    ) -> Result<HashMap<Txid, MempoolEntry>, Error> {
        self.call_json(
            self.http_client
                .post(self.url("/mempool/entries/ancestors"))
                .json(txids),
        )
        .await
    }

    async fn get_subscription(&self, id: &str) -> Result<Subscription, Error> {
        self.call_json(
            self.authorized(
                self.http_client
                    .get(self.url(&format!("/subscription/{}", id))),
            ),
        )
        .await
    }

    async fn list_subscriptions(&self) -> Result<Vec<Subscription>, Error> {
        self.call_json(self.authorized(self.http_client.get(self.url("/subscriptions"))))
            .await
    }

    async fn add_subscription(&self, subscription: &Subscription) -> Result<Subscription, Error> {
        self.call_json(
            self.authorized(self.http_client.post(self.url("/subscription")))
                .json(subscription),
        )
        .await
    }

    async fn delete_subscription(&self, id: &str) -> Result<(), Error> {
        self.send(
            self.authorized(
                self.http_client
                    .delete(self.url(&format!("/subscription/{}", id))),
            ),
        )
        .await?;
        Ok(())
    }
}

fn with_pagination(req: RequestBuilder, pagination: Option<Pagination>) -> RequestBuilder {
    match pagination {
        Some(p) => req.query(&[("skip", p.skip), ("limit", p.limit)]),
        None => req,
    }
}
