use {
    crate::{
        api::{content::ContentError, ApiError},
        bitcoin_rpc::{RpcClientError, RpcClientPoolError},
        index::{IndexError, StoreError},
        subscription::{WebhookStoreError, WebhookSubscriptionError},
    },
    axum::response::{IntoResponse, Response},
    http::{header, HeaderValue, StatusCode},
    std::fmt::Write,
    tracing::{error, warn},
};

#[derive(Debug, thiserror::Error)]
pub(super) enum ServerError {
    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("content error: {0}")]
    ContentError(#[from] ContentError),

    #[error("rpc client error: {0}")]
    RpcClientError(#[from] RpcClientError),

    #[error("rpc client pool error: {0}")]
    RpcClientPoolError(#[from] RpcClientPoolError),

    #[error("api error: {0}")]
    ApiError(#[from] ApiError),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("unauthorized")]
    Unauthorized,
}

pub(super) type ServerResult<T = Response> = Result<T, ServerError>;

/// bitcoind JSON-RPC codes that mean the submitted transaction was rejected:
/// RPC_VERIFY_ERROR, RPC_VERIFY_REJECTED, RPC_VERIFY_ALREADY_IN_CHAIN.
const RPC_REJECT_CODES: [i32; 3] = [-25, -26, -27];
/// RPC_INVALID_ADDRESS_OR_KEY: e.g. "No such mempool or blockchain transaction".
const RPC_INVALID_ADDRESS_OR_KEY: i32 = -5;

fn plain(status: StatusCode, message: impl Into<String>) -> Response {
    (status, message.into()).into_response()
}

fn generic(status: StatusCode) -> Response {
    plain(status, status.canonical_reason().unwrap_or_default())
}

/// Map a bitcoind RPC error: transaction rejections are the client's fault
/// (400 with bitcoind's message); anything else is an upstream failure whose
/// details are logged but not exposed.
fn rpc_error_response(error: &bitcoincore_rpc::Error) -> Response {
    use bitcoincore_rpc::{jsonrpc, Error};

    match error {
        Error::JsonRpc(jsonrpc::Error::Rpc(rpc)) if RPC_REJECT_CODES.contains(&rpc.code) => {
            plain(StatusCode::BAD_REQUEST, rpc.message.clone())
        }
        Error::JsonRpc(jsonrpc::Error::Rpc(rpc)) if rpc.code == RPC_INVALID_ADDRESS_OR_KEY => {
            plain(StatusCode::NOT_FOUND, rpc.message.clone())
        }
        Error::JsonRpc(jsonrpc::Error::Transport(_)) | Error::Io(_) => {
            error!("bitcoind unavailable: {error}");
            generic(StatusCode::SERVICE_UNAVAILABLE)
        }
        _ => {
            error!("bitcoind rpc error: {error}");
            generic(StatusCode::BAD_GATEWAY)
        }
    }
}

fn store_error_response(error: StoreError) -> Response {
    match error {
        StoreError::NotFound(message) => plain(StatusCode::NOT_FOUND, message),
        error => {
            error!("error serving request: {error}");
            generic(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn index_error_response(error: IndexError) -> Response {
    match error {
        IndexError::StoreError(error) => store_error_response(error),
        IndexError::InvalidBestBlockHash(_) => plain(StatusCode::BAD_REQUEST, error.to_string()),
        IndexError::RpcApiError(error) => rpc_error_response(&error),
        error => {
            error!("error serving request: {error}");
            generic(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn content_error_response(error: ContentError) -> Response {
    match error {
        ContentError::NotAcceptable {
            accept_encoding,
            content_encoding,
        } => {
            let mut message = format!(
                "inscription content encoding `{}` is not acceptable.",
                String::from_utf8_lossy(content_encoding.as_bytes())
            );

            if let Some(accept_encoding) = accept_encoding.0 {
                let _ = write!(message, " `Accept-Encoding` header: `{accept_encoding}`");
            } else {
                let _ = write!(message, " `Accept-Encoding` header not present");
            };

            plain(StatusCode::NOT_ACCEPTABLE, message)
        }
        ContentError::InvalidCspOrigin(_) => plain(StatusCode::BAD_REQUEST, error.to_string()),
        error => {
            error!("content error: {error}");
            generic(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn pool_error_response(error: RpcClientPoolError) -> Response {
    match error {
        RpcClientPoolError::Exhausted => {
            warn!("rpc client pool exhausted");
            generic(StatusCode::SERVICE_UNAVAILABLE)
        }
        error => {
            error!("rpc client pool error: {error}");
            generic(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => plain(StatusCode::BAD_REQUEST, message),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))],
                "unauthorized",
            )
                .into_response(),
            Self::ApiError(ApiError::IndexError(error)) => index_error_response(error),
            Self::ApiError(ApiError::RpcError(error)) => rpc_error_response(&error),
            Self::ApiError(ApiError::ContentError(error)) => content_error_response(error),
            Self::ApiError(error @ (ApiError::HexError(_) | ApiError::ConsensusError(_))) => {
                plain(StatusCode::BAD_REQUEST, error.to_string())
            }
            Self::ApiError(ApiError::SubscriptionError(error)) => match error {
                WebhookSubscriptionError::Store(WebhookStoreError::NotFound(message)) => {
                    plain(StatusCode::NOT_FOUND, message)
                }
                WebhookSubscriptionError::InvalidEndpoint(_)
                | WebhookSubscriptionError::LimitReached(_) => {
                    plain(StatusCode::BAD_REQUEST, error.to_string())
                }
                error => {
                    error!("subscription error: {error}");
                    generic(StatusCode::INTERNAL_SERVER_ERROR)
                }
            },
            Self::ApiError(error @ ApiError::TxidMismatch { .. }) => {
                error!("error serving request: {error}");
                generic(StatusCode::BAD_GATEWAY)
            }
            Self::RpcClientError(error) => {
                error!("rpc client error: {error}");
                generic(StatusCode::SERVICE_UNAVAILABLE)
            }
            Self::RpcClientPoolError(error) => pool_error_response(error),
            Self::ContentError(error) => content_error_response(error),
            Self::NotFound(message) => (
                StatusCode::NOT_FOUND,
                [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
                message,
            )
                .into_response(),
        }
    }
}

pub(super) trait OptionExt<T> {
    fn ok_or_not_found<F: FnOnce() -> S, S: Into<String>>(self, f: F) -> ServerResult<T>;
}

impl<T> OptionExt<T> for Option<T> {
    fn ok_or_not_found<F: FnOnce() -> S, S: Into<String>>(self, f: F) -> ServerResult<T> {
        match self {
            Some(value) => Ok(value),
            None => Err(ServerError::NotFound(f().into() + " not found")),
        }
    }
}
