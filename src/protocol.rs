use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Version of the protocol shared by the Hawk frontend and compiler driver.
///
/// Increment this whenever the serialized graph or fix-plan schema changes.
pub const VERSION: u32 = 5;

pub const VERSION_ARGUMENT: &str = "--hawk-protocol-version";

pub const VERSION_ENV: &str = "HAWK_PROTOCOL_VERSION";
pub const OUTPUT_DIR_ENV: &str = "HAWK_OUTPUT_DIR";
pub const ROOT_CRATE_ENV: &str = "HAWK_ROOT_CRATE";
pub const CONSUMER_MODE_ENV: &str = "HAWK_CONSUMER_MODE";
pub const COLLECTION_OPTIONS_ENV: &str = "HAWK_COLLECTION_OPTIONS";
pub const RUN_ID_ENV: &str = "HAWK_RUN_ID";
pub const FIX_PLAN_ENV: &str = "HAWK_FIX_PLAN";
pub const RUSTC_PROBE_ENV: &str = "HAWK_RUSTC_PROBE";
pub const RUSTC_PROBE_TOKEN_ENV: &str = "HAWK_RUSTC_PROBE_TOKEN";

pub const ENVIRONMENT_VARIABLES: &[&str] = &[
    VERSION_ENV,
    OUTPUT_DIR_ENV,
    ROOT_CRATE_ENV,
    CONSUMER_MODE_ENV,
    COLLECTION_OPTIONS_ENV,
    RUN_ID_ENV,
    FIX_PLAN_ENV,
    RUSTC_PROBE_ENV,
    RUSTC_PROBE_TOKEN_ENV,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsumerMode {
    Production,
    NonProduction,
}

impl ConsumerMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::NonProduction => "non-production",
        }
    }

    pub fn from_env_value(value: &str) -> Option<Self> {
        match value {
            "production" => Some(Self::Production),
            "non-production" => Some(Self::NonProduction),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolVersion;

impl Serialize for ProtocolVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(VERSION)
    }
}

impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = u32::deserialize(deserializer)?;
        if version == VERSION {
            Ok(Self)
        } else {
            Err(D::Error::custom(format_args!(
                "unsupported Hawk protocol version {version}; expected {VERSION}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConsumerMode, ProtocolVersion};

    #[test]
    fn consumer_modes_round_trip() {
        for mode in [ConsumerMode::Production, ConsumerMode::NonProduction] {
            assert_eq!(ConsumerMode::from_env_value(mode.as_str()), Some(mode));
        }
        assert_eq!(ConsumerMode::from_env_value(""), None);
        assert_eq!(ConsumerMode::from_env_value("invalid"), None);
    }

    #[test]
    fn rejects_mismatched_serialized_version() {
        let error = serde_json::from_str::<ProtocolVersion>("1")
            .expect_err("mismatched protocol version should fail");

        assert_eq!(
            error.to_string(),
            "unsupported Hawk protocol version 1; expected 5"
        );
    }
}
