pub mod block;
pub mod chunk;
pub mod citation;
pub mod document;
pub mod reference;
pub mod source;

use std::{fmt, str::FromStr};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Intel,
    Amd,
}

impl fmt::Display for Vendor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Intel => "intel",
            Self::Amd => "amd",
        })
    }
}

impl FromStr for Vendor {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "intel" => Ok(Self::Intel),
            "amd" => Ok(Self::Amd),
            _ => Err("vendor must be intel or amd"),
        }
    }
}
