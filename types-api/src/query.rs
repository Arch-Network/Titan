use {
    arch_titan_types_core::{RuneId, SpacedRune},
    bitcoin::BlockHash,
    std::{
        fmt::{self, Display},
        str::FromStr,
    },
};

#[derive(Debug, thiserror::Error)]
pub enum BlockParseError {
    #[error("invalid block height")]
    InvalidHeight,
    #[error("invalid block hash")]
    InvalidHash,
}

#[derive(Debug)]
pub enum Block {
    Height(u64),
    Hash(BlockHash),
}

impl FromStr for Block {
    type Err = BlockParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(if s.len() == 64 {
            Self::Hash(s.parse().map_err(|_| BlockParseError::InvalidHash)?)
        } else {
            Self::Height(s.parse().map_err(|_| BlockParseError::InvalidHeight)?)
        })
    }
}

impl Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Block::Height(height) => write!(f, "{}", height),
            Block::Hash(hash) => write!(f, "{}", hash),
        }
    }
}

impl From<Block> for String {
    fn from(val: Block) -> Self {
        match val {
            Block::Height(height) => height.to_string(),
            Block::Hash(hash) => hash.to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuneParseError {
    #[error("invalid rune id")]
    InvalidId,
    #[error("invalid spaced rune")]
    InvalidSpacedRune,
}

#[derive(Debug)]
pub enum Rune {
    Spaced(SpacedRune),
    Id(RuneId),
}

impl FromStr for Rune {
    type Err = RuneParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.contains(':') {
            Ok(Self::Id(s.parse().map_err(|_| RuneParseError::InvalidId)?))
        } else {
            Ok(Self::Spaced(
                s.parse().map_err(|_| RuneParseError::InvalidSpacedRune)?,
            ))
        }
    }
}

impl Display for Rune {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spaced(rune) => write!(f, "{}", rune),
            Self::Id(id) => write!(f, "{}", id),
        }
    }
}

impl From<Rune> for String {
    fn from(val: Rune) -> Self {
        match val {
            Rune::Spaced(rune) => rune.to_string(),
            Rune::Id(id) => id.to_string(),
        }
    }
}
