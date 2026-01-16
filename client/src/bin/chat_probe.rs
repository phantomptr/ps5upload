use std::time::{SystemTime, UNIX_EPOCH};

use aes_gcm::{Aes256Gcm, Key, Nonce};
use aes_gcm::aead::{Aead, KeyInit};
use base64::{engine::general_purpose, Engine as _};
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use secp256k1::{Message, Secp256k1, Keypair, XOnlyPublicKey};
use serde::Deserialize;
use serde_json::json;
use sha2::Digest;
use tokio_tungstenite::connect_async;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

const DEFAULT_RELAY: &str = "wss://relay.damus.io";

#[derive(Deserialize)]
struct NostrEvent {
    id: String,
    pubkey: String,
    tags: Vec<Vec<String>>,
    content: String,
}

#[derive(Deserialize)]
struct ChatPayload {
    name: String,
    text: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut relay = DEFAULT_RELAY.to_string();
    let mut message = "Hello from chat_probe".to_string();

    let mut idx = 1;
    while idx < args.len() {
        match args[idx].as_str() {
            "--relay" => {
                if let Some(val) = args.get(idx + 1) {
                    relay = val.clone();
                    idx += 1;
                }
            }
            "--message" => {
                if let Some(val) = args.get(idx + 1) {
                    message = val.clone();
                    idx += 1;
                }
            }
            _ => {}
        }
        idx += 1;
    }

    let key_hex = std::env::var("PS5UPLOAD_CHAT_KEY")
        .ok()
        .or_else(|| std::fs::read_to_string("ps5upload_chat.key").ok())
        .or_else(|| std::fs::read_to_string("client/ps5upload_chat.key").ok())
        .unwrap_or_default();
    let key = parse_key(&key_hex).ok_or_else(|| anyhow::anyhow!("Missing/invalid key"))?;
    let room = room_id(&key_hex);
    println!("Relay: {}", relay);
    println!("Room: {}", room);

    let secp = Secp256k1::new();
    let keypair = Keypair::new(&secp, &mut rand::thread_rng());
    let pubkey_hex = xonly_pubkey_hex(&keypair);

    let (ws, _) = connect_async(&relay).await?;
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 - 600;
    let sub_id = format!("ps5upload-{}", rand::random::<u32>());
    let req = json!(["REQ", sub_id, {"kinds":[1], "since": since, "limit": 50}]).to_string();
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let relay_clone = relay.clone();
    let req_clone = req.clone();

    ws_tx.send(tokio_tungstenite::tungstenite::Message::Text(req)).await?;

    let payload = json!({"name":"probe","text":message}).to_string();
    let _ = out_tx.send(payload);

    let self_req = json!(["REQ", format!("self-{}", rand::random::<u32>()), {"authors":[pubkey_hex], "kinds":[1], "since": since, "limit": 5}]).to_string();
    let _ = ws_tx.send(tokio_tungstenite::tungstenite::Message::Text(self_req)).await;

    println!("Interactive mode: type messages and press Enter.");
    println!("Waiting for events...");

    let input_tx = out_tx.clone();
    tokio::spawn(async move {
        let stdin = BufReader::new(tokio::io::stdin());
        let mut lines = stdin.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let _ = input_tx.send(trimmed.to_string());
        }
    });

    loop {
        tokio::select! {
            maybe_line = out_rx.recv() => {
                let Some(line) = maybe_line else { continue; };
                if let Some(content) = encrypt_message(&key, &line) {
                    if let Some(event) = build_event(&secp, &keypair, &pubkey_hex, content) {
                        let msg = json!(["EVENT", event]).to_string();
                        let _ = ws_tx.send(tokio_tungstenite::tungstenite::Message::Text(msg)).await;
                    }
                }
            }
            maybe_msg = ws_rx.next() => {
                let Some(msg) = maybe_msg else { break; };
                let Ok(msg) = msg else { continue; };
                let Ok(text) = msg.to_text() else { continue; };
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
                    if let Some(challenge) = parse_auth_challenge(&val) {
                        println!("AUTH challenge received.");
                        if let Some(event) = build_auth_event(&secp, &keypair, &pubkey_hex, &relay_clone, &challenge) {
                            let msg = json!(["AUTH", event]).to_string();
                            let _ = ws_tx.send(tokio_tungstenite::tungstenite::Message::Text(msg)).await;
                            let _ = ws_tx.send(tokio_tungstenite::tungstenite::Message::Text(req_clone.clone())).await;
                        }
                        continue;
                    }
                    if let Some((ok, reason)) = parse_ok(val.clone()) {
                        println!("OK: {} {:?}", ok, reason);
                        continue;
                    }
                    if is_notice(&val) {
                        println!("NOTICE: {}", text);
                        continue;
                    }
                    if let Some(event) = parse_event(val) {
                        if !event_has_tag(&event, "ps5upload-chat") {
                            continue;
                        }
                        if let Some(plain) = decrypt_message(&key, &event.content) {
                            if let Ok(parsed) = serde_json::from_str::<ChatPayload>(&plain) {
                                println!("EVENT {} {} {}: {}", event.id, event.pubkey, parsed.name, parsed.text);
                            } else {
                                println!("EVENT {} {} {}", event.id, event.pubkey, plain);
                            }
                        } else {
                            println!("EVENT {} <decrypt failed>", event.id);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn parse_key(hex_str: &str) -> Option<[u8; 32]> {
    let cleaned = hex_str.trim();
    let bytes = hex::decode(cleaned).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Some(key)
}

fn room_id(key_hex: &str) -> String {
    let bytes = hex::decode(key_hex.trim()).unwrap_or_else(|_| key_hex.as_bytes().to_vec());
    let digest = sha2::Sha256::digest(&bytes);
    let full = hex::encode(digest);
    full.chars().take(8).collect()
}

fn xonly_pubkey_hex(keypair: &Keypair) -> String {
    let (xonly, _) = XOnlyPublicKey::from_keypair(keypair);
    hex::encode(xonly.serialize())
}

fn build_event(
    secp: &Secp256k1<secp256k1::All>,
    keypair: &Keypair,
    pubkey_hex: &str,
    content: String,
) -> Option<serde_json::Value> {
    let created_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let tags = vec![vec!["t".to_string(), "ps5upload-chat".to_string()]];
    let event_data = json!([0, pubkey_hex, created_at, 1, tags, content]);
    let id_bytes = sha2::Sha256::digest(event_data.to_string().as_bytes());
    let id_hex = hex::encode(id_bytes);
    let id_raw = hex::decode(&id_hex).ok()?;
    let msg = Message::from_digest_slice(&id_raw).ok()?;
    let sig = secp.sign_schnorr(&msg, keypair);
    Some(json!({
        "id": id_hex,
        "pubkey": pubkey_hex,
        "created_at": created_at,
        "kind": 1,
        "tags": vec![vec!["t", "ps5upload-chat"]],
        "content": content,
        "sig": sig.to_string(),
    }))
}

fn parse_event(val: serde_json::Value) -> Option<NostrEvent> {
    let arr = val.as_array()?;
    if arr.len() < 3 {
        return None;
    }
    if arr.get(0)?.as_str()? != "EVENT" {
        return None;
    }
    serde_json::from_value(arr.get(2)?.clone()).ok()
}

fn event_has_tag(event: &NostrEvent, tag: &str) -> bool {
    event.tags.iter().any(|t| t.get(0).map(|s| s == "t").unwrap_or(false) && t.get(1).map(|s| s == tag).unwrap_or(false))
}

fn parse_ok(val: serde_json::Value) -> Option<(bool, Option<String>)> {
    let arr = val.as_array()?;
    if arr.len() < 3 {
        return None;
    }
    if arr.get(0)?.as_str()? != "OK" {
        return None;
    }
    let ok = arr.get(2)?.as_bool()?;
    let reason = arr.get(3).and_then(|v| v.as_str()).map(|s| s.to_string());
    Some((ok, reason))
}

fn parse_auth_challenge(val: &serde_json::Value) -> Option<String> {
    let arr = val.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    if arr.get(0)?.as_str()? != "AUTH" {
        return None;
    }
    arr.get(1)?.as_str().map(|s| s.to_string())
}

fn is_notice(val: &serde_json::Value) -> bool {
    let arr = match val.as_array() {
        Some(arr) => arr,
        None => return false,
    };
    if arr.len() < 2 {
        return false;
    }
    arr.get(0).and_then(|v| v.as_str()) == Some("NOTICE")
}

fn encrypt_message(key: &[u8; 32], plaintext: &str) -> Option<String> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher.encrypt(nonce, plaintext.as_bytes()).ok()?;
    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Some(general_purpose::STANDARD.encode(out))
}

fn decrypt_message(key: &[u8; 32], payload: &str) -> Option<String> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let data = general_purpose::STANDARD.decode(payload).ok()?;
    if data.len() < 13 {
        return None;
    }
    let (nonce_bytes, ciphertext) = data.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);
    let plain = cipher.decrypt(nonce, ciphertext).ok()?;
    String::from_utf8(plain).ok()
}

fn build_auth_event(
    secp: &Secp256k1<secp256k1::All>,
    keypair: &Keypair,
    pubkey_hex: &str,
    relay: &str,
    challenge: &str,
) -> Option<serde_json::Value> {
    let created_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let tags = vec![
        vec!["relay".to_string(), relay.to_string()],
        vec!["challenge".to_string(), challenge.to_string()],
    ];
    let event_data = json!([0, pubkey_hex, created_at, 22242, tags, ""]);
    let id_bytes = sha2::Sha256::digest(event_data.to_string().as_bytes());
    let id_hex = hex::encode(id_bytes);
    let id_raw = hex::decode(&id_hex).ok()?;
    let msg = Message::from_digest_slice(&id_raw).ok()?;
    let sig = secp.sign_schnorr(&msg, keypair);
    Some(json!({
        "id": id_hex,
        "pubkey": pubkey_hex,
        "created_at": created_at,
        "kind": 22242,
        "tags": tags,
        "content": "",
        "sig": sig.to_string(),
    }))
}
