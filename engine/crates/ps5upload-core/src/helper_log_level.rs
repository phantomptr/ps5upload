//! The helper's log levels (`log.level`, method 197): the engine hands each helper the level
//! setting (`crate::log`) as `{"level": n, "cats": {"rp": 5}, "until_s": N}`, numbers 1 (error) to
//! 5 (trace). The helper counts `until_s` down on its own clock and falls back to info. An empty
//! body only reads what the helper has.
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::console_files::is_unsupported;
use crate::mgmt::{self, m};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Sends `config` (or reads, with `Value::Null`) and returns the helper's answer
/// `{ok, level, cats, until_s}`; `Ok(None)` from a helper older than the method.
pub fn set(addr: &str, config: &Value) -> Result<Option<Value>> {
    let body = if config.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(config)?
    };
    let bytes = match mgmt::call_with(addr, m::LOG_LEVEL, m::LOG_LEVEL.label, &body, Some(TIMEOUT))
    {
        Ok(b) => b,
        Err(e) if is_unsupported(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    let reply: Value = serde_json::from_slice(&bytes).context("LOG_LEVEL reply")?;
    Ok(Some(reply))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_method_has_its_number() {
        assert_eq!(super::m::LOG_LEVEL.id, 197);
    }
}
