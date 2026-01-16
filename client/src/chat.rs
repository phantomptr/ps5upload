use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aes_gcm::{Aes256Gcm, Key, Nonce};
use aes_gcm::aead::{Aead, KeyInit};
use base64::{engine::general_purpose, Engine as _};
use chrono::Local;
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use secp256k1::{Message, Secp256k1, Keypair, XOnlyPublicKey};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Digest;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_tungstenite::connect_async;

use crate::{AppMessage, ChatStatusEvent};

const CHAT_RELAYS: [&str; 3] = [
    "wss://relay.damus.io",
    "wss://relay.nostr.band",
    "wss://nostr.wine",
];

const CHAT_TAG: &str = "ps5upload-chat";
const DEDUP_MAX: usize = 4096;

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub time: String,
    pub sender: String,
    pub text: String,
    pub local: bool,
}

#[derive(Deserialize)]
struct NostrEvent {
    id: String,
    pubkey: String,
    #[serde(rename = "created_at")]
    _created_at: i64,
    #[serde(rename = "kind")]
    _kind: u64,
    tags: Vec<Vec<String>>,
    content: String,
    #[serde(rename = "sig")]
    _sig: String,
}

#[derive(Serialize)]
struct OutEvent {
    id: String,
    pubkey: String,
    created_at: i64,
    kind: u64,
    tags: Vec<Vec<String>>,
    content: String,
    sig: String,
}

#[derive(Deserialize)]
struct ChatPayload {
    name: String,
    text: String,
}

struct Dedup {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl Dedup {
    fn new() -> Self {
        Self { set: HashSet::new(), order: VecDeque::new() }
    }

    fn insert(&mut self, id: &str) -> bool {
        if self.set.contains(id) {
            return false;
        }
        self.set.insert(id.to_string());
        self.order.push_back(id.to_string());
        if self.order.len() > DEDUP_MAX {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

pub fn start_chat_worker(
    app_tx: std::sync::mpsc::Sender<AppMessage>,
    key_hex: String,
    handle: tokio::runtime::Handle,
) -> mpsc::UnboundedSender<String> {
    let (out_tx, out_rx) = mpsc::unbounded_channel::<String>();
    let (broadcast_tx, _) = broadcast::channel::<String>(256);
    let key = parse_key(&key_hex);

    if key.is_none() {
        let _ = app_tx.send(AppMessage::ChatStatus(ChatStatusEvent::Disconnected));
        return out_tx;
    }

    let key = match key {
        Some(value) => value,
        None => {
            let _ = app_tx.send(AppMessage::ChatStatus(ChatStatusEvent::Disconnected));
            return out_tx;
        }
    };
    let secp = Secp256k1::new();
    let keypair = Keypair::new(&secp, &mut rand::thread_rng());
    let pubkey_hex = xonly_pubkey_hex(&keypair);
    let relay_count = std::sync::Arc::new(AtomicUsize::new(0));

    let dedup = std::sync::Arc::new(Mutex::new(Dedup::new()));

    // Outgoing fan-out
    {
        let broadcast_tx = broadcast_tx.clone();
        let app_tx = app_tx.clone();
        handle.spawn(async move {
            let mut rx = out_rx;
            while let Some(msg) = rx.recv().await {
                if broadcast_tx.send(msg).is_err() {
                    let _ = app_tx.send(AppMessage::ChatStatus(ChatStatusEvent::Disconnected));
                }
            }
        });
    }

    for relay in CHAT_RELAYS {
        let relay = relay.to_string();
        let app_tx = app_tx.clone();
        let key = key.clone();
        let secp = secp.clone();
        let keypair = keypair.clone();
        let pubkey_hex = pubkey_hex.clone();
        let dedup = dedup.clone();
        let mut out_rx = broadcast_tx.subscribe();
        let handle = handle.clone();
        let relay_count = relay_count.clone();
        handle.spawn(async move {
            let mut connected = false;
            loop {
                match connect_async(&relay).await {
                    Ok((mut ws, _)) => {
                        if !connected {
                            let prev = relay_count.fetch_add(1, Ordering::SeqCst);
                            if prev == 0 {
                                let _ = app_tx.send(AppMessage::ChatStatus(ChatStatusEvent::Connected));
                            }
                            connected = true;
                        }
                        let sub_id = format!("ps5upload-{}", rand::random::<u32>());
                        let since = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64 - 600;
                        let req = json!(["REQ", sub_id, {"kinds":[1], "since": since, "limit": 50}]).to_string();
                        let _ = ws.send(tokio_tungstenite::tungstenite::Message::Text(req.clone())).await;
                        let _ = app_tx.send(AppMessage::ChatStatus(ChatStatusEvent::Connected));

                        loop {
                            tokio::select! {
                                Ok(out) = out_rx.recv() => {
                                    if let Some(content) = encrypt_message(&key, &out) {
                                        if let Some(event) = build_event(&secp, &keypair, &pubkey_hex, content) {
                                            let msg = json!(["EVENT", event]).to_string();
                                            if ws.send(tokio_tungstenite::tungstenite::Message::Text(msg)).await.is_err() {
                                                break;
                                            }
                                        }
                                    }
                                }
                                Some(msg) = ws.next() => {
                                    let Ok(msg) = msg else { break; };
                                    let Ok(text) = msg.to_text() else { continue; };
                                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
                                        if let Some(challenge) = parse_auth_challenge(&val) {
                                            let _ = app_tx.send(AppMessage::Log("Chat: auth challenge received.".to_string()));
                                            if let Some(event) = build_auth_event(&secp, &keypair, &pubkey_hex, &relay, &challenge) {
                                                let msg = json!(["AUTH", event]).to_string();
                                                let _ = ws.send(tokio_tungstenite::tungstenite::Message::Text(msg)).await;
                                                let _ = ws.send(tokio_tungstenite::tungstenite::Message::Text(req.clone())).await;
                                            }
                                            continue;
                                        }
                                        if let Some((ok, reason)) = parse_ok(val.clone()) {
                                            let _ = app_tx.send(AppMessage::ChatAck { ok, reason });
                                            continue;
                                        }
                                        if is_notice(&val) {
                                            let _ = app_tx.send(AppMessage::ChatAck { ok: false, reason: None });
                                            continue;
                                        }
                                        if let Some(event) = parse_event(val) {
                                            let mut dedup_guard = dedup.lock().await;
                                            if !dedup_guard.insert(&event.id) {
                                                continue;
                                            }
                                            drop(dedup_guard);

                                            if event.pubkey == pubkey_hex {
                                                continue;
                                            }
                                            if !event_has_tag(&event, CHAT_TAG) {
                                                continue;
                                            }
                                            if let Some(plain) = decrypt_message(&key, &event.content) {
                                                let fallback_sender = format!("{}…", &event.pubkey[..8.min(event.pubkey.len())]);
                                                let (sender, text) = parse_chat_payload(&plain, &fallback_sender);
                                                let time = Local::now().format("%H:%M").to_string();
                                                let chat = ChatMessage {
                                                    time,
                                                    sender,
                                                    text,
                                                    local: false,
                                                };
                                                let _ = app_tx.send(AppMessage::ChatMessage(chat));
                                            } else {
                                                let _ = app_tx.send(AppMessage::Log("Chat: failed to decrypt message.".to_string()));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => {
                        // fall through to disconnect handling
                    }
                }
                if connected {
                    let prev = relay_count.fetch_sub(1, Ordering::SeqCst);
                    if prev == 1 {
                        let _ = app_tx.send(AppMessage::ChatStatus(ChatStatusEvent::Disconnected));
                    }
                    connected = false;
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        });
    }

    out_tx
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

fn xonly_pubkey_hex(keypair: &Keypair) -> String {
    let (xonly, _) = XOnlyPublicKey::from_keypair(keypair);
    hex::encode(xonly.serialize())
}

fn build_event(
    secp: &Secp256k1<secp256k1::All>,
    keypair: &Keypair,
    pubkey_hex: &str,
    content: String,
) -> Option<OutEvent> {
    let created_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let tags = vec![vec!["t".to_string(), CHAT_TAG.to_string()]];
    let event_data = json!([0, pubkey_hex, created_at, 1, tags, content]);
    let id_bytes = sha2::Sha256::digest(event_data.to_string().as_bytes());
    let id_hex = hex::encode(id_bytes);
    let id_raw = hex::decode(&id_hex).ok()?;
    let msg = Message::from_digest_slice(&id_raw).ok()?;
    let sig = secp.sign_schnorr(&msg, keypair);
    Some(OutEvent {
        id: id_hex,
        pubkey: pubkey_hex.to_string(),
        created_at,
        kind: 1,
        tags: vec![vec!["t".to_string(), CHAT_TAG.to_string()]],
        content,
        sig: sig.to_string(),
    })
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

fn event_has_tag(event: &NostrEvent, tag: &str) -> bool {
    event.tags.iter().any(|t| t.get(0).map(|s| s == "t").unwrap_or(false) && t.get(1).map(|s| s == tag).unwrap_or(false))
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

fn parse_chat_payload(payload: &str, fallback_sender: &str) -> (String, String) {
    if let Ok(parsed) = serde_json::from_str::<ChatPayload>(payload) {
        let name = parsed.name.trim();
        let text = parsed.text.trim();
        if !name.is_empty() && !text.is_empty() {
            return (name.to_string(), text.to_string());
        }
    }
    (fallback_sender.to_string(), payload.to_string())
}

fn build_auth_event(
    secp: &Secp256k1<secp256k1::All>,
    keypair: &Keypair,
    pubkey_hex: &str,
    relay: &str,
    challenge: &str,
) -> Option<OutEvent> {
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
    Some(OutEvent {
        id: id_hex,
        pubkey: pubkey_hex.to_string(),
        created_at,
        kind: 22242,
        tags,
        content: String::new(),
        sig: sig.to_string(),
    })
}
