//! Fan curve editor over FTX2.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

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
    let mut c = Connection::connect(addr)?;
    let body = serde_json::json!({ "points": points });
    c.send_frame(FrameType::HwFanCurveSet, &serde_json::to_vec(&body)?)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected HW_FAN_CURVE_SET: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::HwFanCurveSetAck {
        bail!("expected HW_FAN_CURVE_SET_ACK, got {ft:?}");
    }
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
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::HwFanCurveGet, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected HW_FAN_CURVE_GET: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::HwFanCurveGetAck {
        bail!("expected HW_FAN_CURVE_GET_ACK, got {ft:?}");
    }
    let parsed: FanCurveGetResult = serde_json::from_slice(&resp)?;
    Ok(parsed.points)
}
