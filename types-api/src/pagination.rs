use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Pagination {
    #[serde(default = "default_skip")]
    pub skip: u64,
    #[serde(default = "default_limit", deserialize_with = "clamp_limit")]
    pub limit: u64,
}

impl Default for Pagination {
    fn default() -> Self {
        Pagination {
            skip: 0,
            limit: default_limit(),
        }
    }
}

fn default_skip() -> u64 {
    0
}

fn default_limit() -> u64 {
    50
}

fn clamp_limit<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let limit = u64::deserialize(deserializer)?;

    // Clamp the limit to a maximum of 50 entries.
    Ok(limit.min(default_limit()))
}

impl From<(u64, u64)> for Pagination {
    fn from(val: (u64, u64)) -> Self {
        Pagination {
            skip: val.0,
            limit: val.1,
        }
    }
}

impl From<Pagination> for (u64, u64) {
    fn from(val: Pagination) -> Self {
        (val.skip, val.limit)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginationResponse<T> {
    pub items: Vec<T>,
    pub offset: u64,
}
