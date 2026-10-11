use {
    super::{
        deserialize_from_str::DeserializeFromStr,
        error::{OptionExt, ServerError, ServerResult},
        ServerConfig,
    },
    crate::{
        api::{self, content::AcceptEncoding},
        bitcoin_rpc::RpcClientPool,
        index::Index,
        subscription::WebhookSubscriptionManager,
        util::ServiceExitSender,
    },
    arch_titan_types_api::{query, Pagination},
    arch_titan_types_core::{InscriptionId, SerializedOutPoint, SerializedTxid},
    axum::{
        body::Bytes,
        extract::{DefaultBodyLimit, Extension, FromRef, Json, Path, Query, Request, State},
        middleware::{self, Next},
        response::{IntoResponse, Response},
        routing::{get, post},
        Router,
    },
    axum_server::Handle,
    bitcoin::{address::NetworkUnchecked, Address, BlockHash},
    http::{header, StatusCode},
    serde::Deserialize,
    std::{io, net::ToSocketAddrs, sync::Arc, time::Duration},
    tokio::task,
    tower_http::{
        compression::CompressionLayer,
        cors::{Any, CorsLayer},
    },
    tracing::info,
    uuid::Uuid,
};

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("failed to bind to address: {0}")]
    BindError(#[from] std::io::Error),
    #[error("no socket address found")]
    NoSocketAddr,
}

type SpawnResult<T> = std::result::Result<T, SpawnError>;

/// Body limit for routes that take no body.
const DEFAULT_BODY_LIMIT: usize = 16 * 1024;
/// Hex of the largest standard transaction (400k weight units = 100 kB) is
/// 200 kB; allow twice that for non-standard but relay-able transactions.
const BROADCAST_BODY_LIMIT: usize = 400 * 1024;
/// Maximum txids accepted by batch routes.
const MAX_TXIDS_PER_REQUEST: usize = 1000;
/// A JSON-quoted txid is 66 bytes plus separator; leave room for whitespace.
const BATCH_BODY_LIMIT: usize = MAX_TXIDS_PER_REQUEST * 80 + 1024;
/// How long an HTTP request waits for a free bitcoind RPC client.
const RPC_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
struct TransactionStatusesQuery {
    blockhash: Option<BlockHash>,
}

/// `skip`/`limit` for `GET /mempool/txids`. Txids are small, so pages default to and
/// are clamped at `MAX_TXIDS_PER_REQUEST` instead of `Pagination`'s 50.
#[derive(Debug, Deserialize)]
struct TxidPagination {
    #[serde(default)]
    skip: u64,
    #[serde(default = "max_txids_per_page")]
    limit: u64,
}

fn max_txids_per_page() -> u64 {
    MAX_TXIDS_PER_REQUEST as u64
}

impl From<TxidPagination> for Pagination {
    fn from(query: TxidPagination) -> Self {
        Pagination {
            skip: query.skip,
            limit: query.limit.min(max_txids_per_page()),
        }
    }
}

fn check_txid_count(txids: &[SerializedTxid]) -> ServerResult<()> {
    if txids.len() > MAX_TXIDS_PER_REQUEST {
        return Err(ServerError::BadRequest(format!(
            "too many txids: {} (max {MAX_TXIDS_PER_REQUEST})",
            txids.len()
        )));
    }
    Ok(())
}

/// Constant-time comparison so the token cannot be recovered by timing.
fn token_matches(provided: &[u8], expected: &[u8]) -> bool {
    provided.len() == expected.len()
        && provided
            .iter()
            .zip(expected)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// Guards the webhook subscription routes: 400 if webhooks are disabled,
/// 401 unless `Authorization: Bearer <webhook_admin_token>` is sent.
async fn require_webhook_admin(
    State(config): State<Arc<ServerConfig>>,
    request: Request,
    next: Next,
) -> Response {
    if !config.enable_webhook_subscriptions {
        return ServerError::BadRequest("subscriptions are not enabled".to_string())
            .into_response();
    }

    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    match (provided, config.webhook_admin_token.as_deref()) {
        (Some(provided), Some(expected))
            if !expected.is_empty() && token_matches(provided.as_bytes(), expected.as_bytes()) =>
        {
            next.run(request).await
        }
        _ => ServerError::Unauthorized.into_response(),
    }
}

pub struct Server;

impl Server {
    /// Bind the HTTP listener (failing synchronously on bind errors) and serve
    /// the API in a background task. `exit_tx` is notified when the server stops.
    pub fn start(
        &self,
        index: Arc<Index>,
        webhook_subscription_manager: Option<Arc<WebhookSubscriptionManager>>,
        bitcoin_rpc_pool: RpcClientPool,
        config: Arc<ServerConfig>,
        handle: Handle,
        exit_tx: ServiceExitSender,
    ) -> SpawnResult<task::JoinHandle<io::Result<()>>> {
        let router = Self::router(
            index,
            webhook_subscription_manager,
            bitcoin_rpc_pool,
            config.clone(),
        );
        let listener = Self::bind(&config)?;

        Ok(tokio::spawn(async move {
            let result = axum_server::from_tcp(listener)
                .handle(handle)
                .serve(router.into_make_service())
                .await;
            let _ = exit_tx.send("HTTP server");
            result
        }))
    }

    fn bind(config: &ServerConfig) -> SpawnResult<std::net::TcpListener> {
        let addr = config
            .http_listen
            .to_socket_addrs()?
            .next()
            .ok_or(SpawnError::NoSocketAddr)?;

        let listener = std::net::TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        info!("Listening on http://{}", listener.local_addr()?);
        Ok(listener)
    }

    fn router(
        index: Arc<Index>,
        webhook_subscription_manager: Option<Arc<WebhookSubscriptionManager>>,
        bitcoin_rpc_pool: RpcClientPool,
        config: Arc<ServerConfig>,
    ) -> Router {
        let public = Router::new()
            // Health check
            .route("/", get(Self::health_check))
            // Status
            .route("/status", get(Self::status))
            // Blocks
            .route("/tip", get(Self::tip))
            .route("/block/{query}", get(Self::block))
            .route("/block/{height}/hash", get(Self::block_hash_by_height))
            .route("/block/{query}/txids", get(Self::block_txids))
            // Addresses
            .route("/address/{address}", get(Self::address))
            .route("/address/{address}/outputs", get(Self::address_outputs))
            // Transactions
            .route(
                "/tx/broadcast",
                post(Self::broadcast_transaction)
                    .layer(DefaultBodyLimit::max(BROADCAST_BODY_LIMIT)),
            )
            .route(
                "/txs/statuses",
                post(Self::transaction_statuses).layer(DefaultBodyLimit::max(BATCH_BODY_LIMIT)),
            )
            .route("/tx/{txid}", get(Self::transaction))
            .route("/tx/{txid}/raw", get(Self::transaction_raw))
            .route("/tx/{txid}/hex", get(Self::transaction_hex))
            .route("/tx/{txid}/status", get(Self::transaction_status))
            .route("/output/{outpoint}", get(Self::output))
            // Inscriptions
            .route("/inscription/{inscription_id}", get(Self::inscription))
            // Runes
            .route("/runes", get(Self::runes))
            .route("/rune/{rune}", get(Self::rune))
            .route("/rune/{rune}/transactions", get(Self::rune_transactions))
            // Mempool
            .route("/mempool/txids", get(Self::mempool_txids))
            // Mempool entries
            .route("/mempool/entry/{txid}", get(Self::mempool_tx))
            .route(
                "/mempool/entries",
                get(Self::mempool_entries_page)
                    .post(Self::mempool_entries)
                    .layer(DefaultBodyLimit::max(BATCH_BODY_LIMIT)),
            )
            .route(
                "/mempool/entries/ancestors",
                post(Self::mempool_entries_with_ancestors)
                    .layer(DefaultBodyLimit::max(BATCH_BODY_LIMIT)),
            )
            .layer(
                CorsLayer::new()
                    .allow_methods([http::Method::GET, http::Method::POST])
                    .allow_headers([header::CONTENT_TYPE])
                    .allow_origin(Any),
            );

        // Subscription admin routes: bearer-token protected and not exposed via CORS.
        let admin = Router::new()
            .route(
                "/subscription/{id}",
                get(Self::get_subscription).delete(Self::delete_subscription),
            )
            .route("/subscription", post(Self::add_subscription))
            .route("/subscriptions", get(Self::subscriptions))
            .route_layer(middleware::from_fn_with_state(
                config.clone(),
                require_webhook_admin,
            ));

        public
            .merge(admin)
            .layer(Extension(index))
            .layer(Extension(webhook_subscription_manager))
            .layer(Extension(config.clone()))
            .layer(Extension(bitcoin_rpc_pool))
            .layer(DefaultBodyLimit::max(DEFAULT_BODY_LIMIT))
            .layer(CompressionLayer::new())
            .with_state(config)
    }

    async fn health_check() -> ServerResult {
        Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            Json(serde_json::json!({"status": "ok"})),
        )
            .into_response())
    }

    async fn tip(Extension(index): Extension<Arc<Index>>) -> ServerResult {
        task::block_in_place(|| Ok(Json(api::tip(index)?).into_response()))
    }

    async fn status(Extension(index): Extension<Arc<Index>>) -> ServerResult {
        task::block_in_place(|| Ok(Json(api::status(index)?).into_response()))
    }

    async fn block(
        Extension(index): Extension<Arc<Index>>,
        Path(DeserializeFromStr(query)): Path<DeserializeFromStr<query::Block>>,
    ) -> ServerResult {
        task::block_in_place(|| Ok(Json(api::block(index, &query)?).into_response()))
    }

    async fn block_hash_by_height(
        Extension(index): Extension<Arc<Index>>,
        Path(DeserializeFromStr(height)): Path<DeserializeFromStr<u64>>,
    ) -> ServerResult {
        task::block_in_place(|| Ok(Json(api::block_hash_by_height(index, height)?).into_response()))
    }

    async fn block_txids(
        Extension(index): Extension<Arc<Index>>,
        Path(DeserializeFromStr(query)): Path<DeserializeFromStr<query::Block>>,
    ) -> ServerResult {
        task::block_in_place(|| Ok(Json(api::block_txids(index, &query)?).into_response()))
    }

    async fn broadcast_transaction(
        Extension(index): Extension<Arc<Index>>,
        Extension(bitcoin_rpc_pool): Extension<RpcClientPool>,
        hex: String,
    ) -> ServerResult {
        task::block_in_place(|| {
            let txid = api::broadcast_transaction(
                index,
                bitcoin_rpc_pool.get_timeout(RPC_ACQUIRE_TIMEOUT)?,
                &hex,
            )?;

            Ok((
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain")],
                txid.to_string(),
            )
                .into_response())
        })
    }

    async fn transaction(
        Extension(index): Extension<Arc<Index>>,
        Extension(bitcoin_rpc_pool): Extension<RpcClientPool>,
        Path(txid): Path<SerializedTxid>,
    ) -> ServerResult {
        task::block_in_place(|| {
            let transaction = api::transaction(
                index,
                bitcoin_rpc_pool.get_timeout(RPC_ACQUIRE_TIMEOUT)?,
                &txid,
            )?;
            Ok(Json(transaction).into_response())
        })
    }

    async fn transaction_raw(
        Extension(index): Extension<Arc<Index>>,
        Extension(bitcoin_rpc_pool): Extension<RpcClientPool>,
        Path(txid): Path<SerializedTxid>,
    ) -> ServerResult {
        task::block_in_place(|| {
            let raw_tx = api::bitcoin_transaction_raw(
                index,
                bitcoin_rpc_pool.get_timeout(RPC_ACQUIRE_TIMEOUT)?,
                &txid,
            )?;

            Ok((
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/octet-stream")],
                Bytes::from(raw_tx),
            )
                .into_response())
        })
    }

    async fn transaction_hex(
        Extension(index): Extension<Arc<Index>>,
        Extension(bitcoin_rpc_pool): Extension<RpcClientPool>,
        Path(txid): Path<SerializedTxid>,
    ) -> ServerResult {
        task::block_in_place(|| {
            let hex_string = api::bitcoin_transaction_hex(
                index,
                bitcoin_rpc_pool.get_timeout(RPC_ACQUIRE_TIMEOUT)?,
                &txid,
            )?;

            Ok((
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain")],
                hex_string,
            )
                .into_response())
        })
    }

    async fn transaction_status(
        Extension(index): Extension<Arc<Index>>,
        Path(txid): Path<SerializedTxid>,
    ) -> ServerResult {
        task::block_in_place(|| Ok(Json(api::transaction_status(index, &txid)?).into_response()))
    }

    async fn transaction_statuses(
        Extension(index): Extension<Arc<Index>>,
        Query(query): Query<TransactionStatusesQuery>,
        Json(txids): Json<Vec<SerializedTxid>>,
    ) -> ServerResult {
        check_txid_count(&txids)?;
        task::block_in_place(|| {
            let result = api::transaction_statuses(index, &txids, &query.blockhash)?;

            Ok((
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                Json(result),
            )
                .into_response())
        })
    }

    async fn output(
        Extension(index): Extension<Arc<Index>>,
        Path(DeserializeFromStr(outpoint)): Path<DeserializeFromStr<SerializedOutPoint>>,
    ) -> ServerResult {
        task::block_in_place(|| Ok(Json(api::output(index, &outpoint)?).into_response()))
    }

    async fn runes(
        Extension(index): Extension<Arc<Index>>,
        Query(pagination): Query<Pagination>,
    ) -> ServerResult {
        task::block_in_place(|| Ok(Json(api::runes(index, pagination)?).into_response()))
    }

    async fn rune(
        Extension(index): Extension<Arc<Index>>,
        Path(DeserializeFromStr(rune)): Path<DeserializeFromStr<query::Rune>>,
    ) -> ServerResult {
        task::block_in_place(|| Ok(Json(api::rune(index, &rune)?).into_response()))
    }

    async fn rune_transactions(
        Extension(index): Extension<Arc<Index>>,
        Path(DeserializeFromStr(rune)): Path<DeserializeFromStr<query::Rune>>,
        Query(pagination): Query<Pagination>,
    ) -> ServerResult {
        task::block_in_place(|| {
            Ok(Json(api::last_rune_transactions(index, &rune, Some(pagination))?).into_response())
        })
    }

    async fn inscription(
        Extension(index): Extension<Arc<Index>>,
        Extension(config): Extension<Arc<ServerConfig>>,
        Path(DeserializeFromStr(inscription_id)): Path<DeserializeFromStr<InscriptionId>>,
        accept_encoding: AcceptEncoding,
    ) -> ServerResult {
        task::block_in_place(|| {
            Ok(api::inscription_content(
                index,
                &inscription_id,
                accept_encoding,
                config.csp_origin.clone(),
                config.decompress,
            )?
            .ok_or_not_found(|| format!("inscription {inscription_id} content"))?
            .into_response())
        })
    }

    async fn mempool_txids(
        Extension(index): Extension<Arc<Index>>,
        Query(pagination): Query<TxidPagination>,
    ) -> ServerResult {
        task::block_in_place(|| {
            Ok(Json(api::mempool_txids(index, pagination.into())?).into_response())
        })
    }

    async fn mempool_tx(
        Extension(index): Extension<Arc<Index>>,
        Path(txid): Path<SerializedTxid>,
    ) -> ServerResult {
        task::block_in_place(|| Ok(Json(api::mempool_tx(index, &txid)?).into_response()))
    }

    async fn mempool_entries(
        Extension(index): Extension<Arc<Index>>,
        Json(txids): Json<Vec<SerializedTxid>>,
    ) -> ServerResult {
        check_txid_count(&txids)?;
        task::block_in_place(|| Ok(Json(api::mempool_entries(index, &txids)?).into_response()))
    }

    async fn mempool_entries_page(
        Extension(index): Extension<Arc<Index>>,
        Query(pagination): Query<Pagination>,
    ) -> ServerResult {
        task::block_in_place(|| {
            Ok(Json(api::mempool_entries_page(index, pagination)?).into_response())
        })
    }

    async fn mempool_entries_with_ancestors(
        Extension(index): Extension<Arc<Index>>,
        Json(txids): Json<Vec<SerializedTxid>>,
    ) -> ServerResult {
        check_txid_count(&txids)?;
        task::block_in_place(|| {
            Ok(Json(api::mempool_entries_with_ancestors(index, &txids)?).into_response())
        })
    }

    async fn address(
        Extension(index): Extension<Arc<Index>>,
        Extension(config): Extension<Arc<ServerConfig>>,
        Path(address): Path<Address<NetworkUnchecked>>,
    ) -> ServerResult {
        let address = Self::indexed_address(&config, address)?;
        task::block_in_place(|| Ok(Json(api::address(index, &address)?).into_response()))
    }

    async fn address_outputs(
        Extension(index): Extension<Arc<Index>>,
        Extension(config): Extension<Arc<ServerConfig>>,
        Path(address): Path<Address<NetworkUnchecked>>,
        Query(pagination): Query<Pagination>,
    ) -> ServerResult {
        let address = Self::indexed_address(&config, address)?;
        task::block_in_place(|| {
            Ok(Json(api::address_outputs(index, &address, pagination)?).into_response())
        })
    }

    fn indexed_address(
        config: &ServerConfig,
        address: Address<NetworkUnchecked>,
    ) -> ServerResult<Address> {
        if !config.index_addresses {
            return Err(ServerError::BadRequest(
                "addresses are not indexed. Enable --index-addresses to index addresses"
                    .to_string(),
            ));
        }

        address
            .require_network(config.chain.network())
            .map_err(|err| ServerError::BadRequest(err.to_string()))
    }

    // Subscription handlers run behind `require_webhook_admin`, which rejects
    // requests when webhooks are disabled, so the manager is always present.
    fn subscription_manager(
        manager: Option<Arc<WebhookSubscriptionManager>>,
    ) -> ServerResult<Arc<WebhookSubscriptionManager>> {
        manager.ok_or_else(|| ServerError::BadRequest("subscriptions are not enabled".to_string()))
    }

    async fn subscriptions(
        Extension(manager): Extension<Option<Arc<WebhookSubscriptionManager>>>,
    ) -> ServerResult {
        let manager = Self::subscription_manager(manager)?;
        Ok(Json(api::subscriptions(manager)).into_response())
    }

    async fn add_subscription(
        Extension(manager): Extension<Option<Arc<WebhookSubscriptionManager>>>,
        Json(subscription): Json<api::AddSubscriptionRequest>,
    ) -> ServerResult {
        let manager = Self::subscription_manager(manager)?;
        Ok(Json(api::add_subscription(manager, subscription).await?).into_response())
    }

    async fn delete_subscription(
        Extension(manager): Extension<Option<Arc<WebhookSubscriptionManager>>>,
        Path(id): Path<Uuid>,
    ) -> ServerResult {
        let manager = Self::subscription_manager(manager)?;
        task::block_in_place(|| Ok(Json(api::delete_subscription(manager, id)?).into_response()))
    }

    async fn get_subscription(
        Extension(manager): Extension<Option<Arc<WebhookSubscriptionManager>>>,
        Path(id): Path<Uuid>,
    ) -> ServerResult {
        let manager = Self::subscription_manager(manager)?;
        Ok(Json(api::get_subscription(manager, id)?).into_response())
    }
}

impl<S> axum::extract::FromRequestParts<S> for AcceptEncoding
where
    Arc<ServerConfig>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(parts.headers.get("accept-encoding").map(|value| {
            value.to_str().unwrap_or_default().to_owned()
        })))
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            db::RocksDB,
            index::{Chain, Settings},
            models::BatchUpdate,
        },
        arch_titan_types_api::{MempoolTxEntry, PaginationResponse},
        arch_titan_types_core::{MempoolEntry, MempoolEntryFee},
        bitcoincore_rpc::Auth,
        std::path::PathBuf,
    };

    struct TestServer {
        base_url: String,
        data_dir: PathBuf,
        index: Arc<Index>,
        db: Arc<RocksDB>,
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.index.shutdown();
            let _ = std::fs::remove_dir_all(&self.data_dir);
        }
    }

    async fn start_test_server(webhook_admin_token: Option<&str>) -> TestServer {
        let data_dir = std::env::temp_dir().join(format!("titan-server-test-{}", Uuid::new_v4()));
        let db = Arc::new(RocksDB::open(data_dir.to_str().unwrap()).unwrap());

        let settings = Settings {
            data_dir: data_dir.clone(),
            zmq_endpoint: "tcp://127.0.0.1:1".to_string(),
            bitcoin_rpc_limit: 4,
            bitcoin_rpc_url: "http://127.0.0.1:1".to_string(),
            bitcoin_rpc_auth: Auth::None,
            chain: Chain::Regtest,
            no_index_inscriptions: true,
            index_bitcoin_transactions: true,
            index_spent_outputs: true,
            index_addresses: false,
            commit_interval: 50,
            main_loop_interval: 500,
        };
        let config = Arc::new(ServerConfig {
            chain: Chain::Regtest,
            csp_origin: None,
            decompress: false,
            http_listen: "127.0.0.1:0".to_string(),
            bitcoin_rpc_url: settings.bitcoin_rpc_url.clone(),
            bitcoin_rpc_auth: Auth::None,
            index_addresses: false,
            enable_webhook_subscriptions: webhook_admin_token.is_some(),
            webhook_admin_token: webhook_admin_token.map(str::to_string),
        });

        let pool = RpcClientPool::new(Arc::new(settings.clone()), 1, 4);
        let webhook_manager = webhook_admin_token
            .map(|_| Arc::new(WebhookSubscriptionManager::new(db.clone(), false).unwrap()));
        let index = Arc::new(Index::new(db.clone(), pool.clone(), settings, None));

        let router = Server::router(index.clone(), webhook_manager, pool, config);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await });

        TestServer {
            base_url,
            data_dir,
            index,
            db,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mempool_routes_are_paginated() {
        let server = start_test_server(None).await;
        let mut update = BatchUpdate::new(0, 0, 0);
        for n in 0..1_200u32 {
            let mut bytes = [0u8; 32];
            bytes[..4].copy_from_slice(&n.to_be_bytes());
            update.mempool_txs.insert(
                SerializedTxid::from(bytes),
                MempoolEntry {
                    vsize: 100,
                    weight: Some(400),
                    descendant_count: 1,
                    descendant_size: 100,
                    ancestor_count: 1,
                    ancestor_size: 100,
                    fees: MempoolEntryFee {
                        base: 1,
                        descendant: 1,
                        ancestor: 1,
                    },
                    depends: vec![],
                    spent_by: vec![],
                },
            );
        }
        server.db.batch_update(&update, true).unwrap();

        let client = reqwest::Client::new();
        let get = |path: &str| client.get(format!("{}{path}", server.base_url)).send();

        // Txid pages are clamped at MAX_TXIDS_PER_REQUEST, and `offset` continues the walk.
        let page: PaginationResponse<SerializedTxid> = get("/mempool/txids?limit=5000")
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(page.items.len(), MAX_TXIDS_PER_REQUEST);
        assert_eq!(page.offset, MAX_TXIDS_PER_REQUEST as u64);
        let rest: PaginationResponse<SerializedTxid> =
            get(&format!("/mempool/txids?skip={}", page.offset))
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        assert_eq!(rest.items.len(), 200);

        // Entry pages use the shared 50-item clamp and carry their txid.
        let entries: PaginationResponse<MempoolTxEntry> = get("/mempool/entries?skip=10&limit=500")
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(entries.items.len(), 50);
        assert_eq!(entries.offset, 60);
        assert_eq!(SerializedTxid::from(entries.items[0].txid), page.items[10]);

        let removed = get("/mempool/entries/all").await.unwrap();
        assert_eq!(removed.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn transaction_statuses_route() {
        let server = start_test_server(None).await;
        let client = reqwest::Client::new();
        let url = format!("{}/txs/statuses", server.base_url);
        let txid = "0".repeat(64);

        // Unknown txids map to null; no query string is required.
        let response = client
            .post(&url)
            .json(&vec![txid.clone()])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body, serde_json::json!({ txid.clone(): null }));

        // A well-formed blockhash that is not the tip is a client error.
        let response = client
            .post(format!("{url}?blockhash={txid}"))
            .json(&Vec::<String>::new())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("invalid best block hash"));

        // Malformed blockhash.
        let response = client
            .post(format!("{url}?blockhash=nothex"))
            .json(&Vec::<String>::new())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // Too many txids.
        let response = client
            .post(&url)
            .json(&vec![txid; MAX_TXIDS_PER_REQUEST + 1])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn webhook_routes_require_admin_token() {
        let server = start_test_server(Some("secret")).await;
        let client = reqwest::Client::new();
        let url = format!("{}/subscriptions", server.base_url);

        let response = client.get(&url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = client.get(&url).bearer_auth("wrong").send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = client
            .post(format!("{}/subscription", server.base_url))
            .json(&serde_json::json!({ "endpoint": "https://example.com", "event_types": [] }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = client.get(&url).bearer_auth("secret").send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "[]");

        // Authorized, but webhooks may not target internal addresses.
        let response = client
            .post(format!("{}/subscription", server.base_url))
            .bearer_auth("secret")
            .json(&serde_json::json!({
                "id": Uuid::nil(),
                "endpoint": "http://127.0.0.1:8332/",
                "event_types": [],
                "last_success_epoch_secs": 0
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn webhook_routes_disabled() {
        let server = start_test_server(None).await;
        let response = reqwest::Client::new()
            .get(format!("{}/subscriptions", server.base_url))
            .bearer_auth("anything")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
