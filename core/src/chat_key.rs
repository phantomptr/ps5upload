use rand::Rng;
use sha2::Digest;

include!(concat!(env!("OUT_DIR"), "/chat_key.rs"));

pub fn shared_key_hex() -> &'static str {
    CHAT_SHARED_KEY_HEX
}

pub fn chat_room_id_for_key(key_hex: &str) -> String {
    let bytes = hex::decode(key_hex).unwrap_or_else(|_| key_hex.as_bytes().to_vec());
    let digest = sha2::Sha256::digest(&bytes);
    let full = hex::encode(digest);
    full.chars().take(8).collect()
}

pub fn generate_chat_display_name(key_hex: &str) -> String {
    let mut rng = rand::thread_rng();
    let rand_part: u32 = rng.gen();
    let mut tag = String::new();
    let trimmed = key_hex.trim();
    if !trimmed.is_empty() {
        let digest = sha2::Sha256::digest(trimmed.as_bytes());
        tag = hex::encode(digest);
    }
    let tag_short = if tag.is_empty() {
        "chat".to_string()
    } else {
        tag.chars().take(4).collect()
    };
    let time = chrono::Utc::now().timestamp() as u64;
    let suffix = format!("{:08x}{:08x}", time as u32, rand_part);
    format!("Player-{}-{}", tag_short, &suffix[..6])
}
