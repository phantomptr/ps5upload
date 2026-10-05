//! Fan curve editor over AVA1 management.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanCurvePoint {
    pub temp_c: i32,
    pub duty_pct: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanCurveSetResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub err: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanCurveGetResult {
    #[serde(default)]
    pub points: Vec<FanCurvePoint>,
}

pub fn fan_curve_set(addr: &str, points: &[FanCurvePoint]) -> Result<()> {
    if points.is_empty() {
        bail!("fan curve must have at least one point");
    }
    for p in points {
        if p.temp_c < 0 || p.temp_c > 120 {
            bail!("temperature must be 0-120°C");
        }
        if p.duty_pct < 0 || p.duty_pct > 100 {
            bail!("duty must be 0-100%");
        }
    }
    let body = serde_json::json!({ "points": points });
    let resp = mgmt::call_keep(
        addr,
        m::HW_FAN_CURVE_SET,
        "HW_FAN_CURVE_SET",
        &serde_json::to_vec(&body)?,
    )?;
    let parsed: FanCurveSetResult = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "fan curve set failed: {}",
            if parsed.err.is_empty() {
                "unknown error"
            } else {
                &parsed.err
            }
        );
    }
    Ok(())
}

pub fn fan_curve_get(addr: &str) -> Result<Vec<FanCurvePoint>> {
    let resp = mgmt::call_keep(addr, m::HW_FAN_CURVE_GET, "HW_FAN_CURVE_GET", &[])?;
    let parsed: FanCurveGetResult = serde_json::from_slice(&resp)?;
    Ok(parsed.points)
}
