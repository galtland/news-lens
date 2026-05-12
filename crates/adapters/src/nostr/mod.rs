//! Nostr publishing adapter

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use bech32::Hrp;
use futures_util::{SinkExt, StreamExt};
use k256::schnorr::SigningKey;
use news_lens_domain::{PublishError, PublishResult, Publisher, RenderedPost};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use time::OffsetDateTime;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const NOSTR_TEXT_NOTE_KIND: u32 = 1;
const SCHNORR_ZERO_AUX_RANDOMNESS: [u8; 32] = [0; 32];
const RELAY_ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// Parse a Nostr secret key from either hex or `nsec` Bech32 format.
pub fn parse_secret_key(raw: &str) -> Result<SigningKey> {
    let trimmed = raw.trim();

    if trimmed.is_empty() {
        bail!("Nostr secret key is empty");
    }

    let lower = trimmed.to_ascii_lowercase();
    let secret_bytes = if lower.starts_with('n') && lower.contains('1') {
        parse_nsec_key_bytes(&lower)?
    } else {
        parse_hex_key_bytes(trimmed)?
    };

    SigningKey::from_bytes(&secret_bytes).map_err(|_| {
        anyhow!("Invalid Nostr secret key scalar (must be non-zero and < curve order)")
    })
}

fn parse_nsec_key_bytes(raw: &str) -> Result<[u8; 32]> {
    let (hrp, data) =
        bech32::decode(raw).map_err(|e| anyhow!("Invalid nsec Bech32 encoding: {}", e))?;

    if hrp != Hrp::parse("nsec")? {
        bail!(
            "Invalid nsec human-readable part: expected 'nsec', got '{}'",
            hrp
        );
    }

    if data.len() != 32 {
        bail!(
            "Invalid nsec payload length: expected 32 bytes, got {}",
            data.len()
        );
    }

    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&data);
    Ok(bytes)
}

fn parse_hex_key_bytes(raw: &str) -> Result<[u8; 32]> {
    if raw.len() != 64 {
        bail!(
            "Invalid hex secret key length: expected 64 characters, got {}",
            raw.len()
        );
    }

    if !raw.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("Invalid hex secret key: contains non-hex characters");
    }

    let mut bytes = [0u8; 32];
    for (idx, chunk) in raw.as_bytes().chunks_exact(2).enumerate() {
        let hex_pair = std::str::from_utf8(chunk)
            .map_err(|_| anyhow!("Invalid hex secret key: contains invalid UTF-8"))?;

        bytes[idx] = u8::from_str_radix(hex_pair, 16)
            .map_err(|_| anyhow!("Invalid hex secret key: failed to decode at byte {}", idx))?;
    }

    Ok(bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }

    output
}

#[cfg(test)]
fn decode_hex_fixed<const N: usize>(raw: &str) -> Result<[u8; N]> {
    if raw.len() != N * 2 {
        bail!("Expected {} hex characters, got {}", N * 2, raw.len());
    }

    if !raw.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("Input contains non-hex characters");
    }

    let mut bytes = [0u8; N];
    for (idx, chunk) in raw.as_bytes().chunks_exact(2).enumerate() {
        let hex_pair = std::str::from_utf8(chunk).map_err(|_| anyhow!("Invalid UTF-8"))?;
        bytes[idx] =
            u8::from_str_radix(hex_pair, 16).map_err(|_| anyhow!("Invalid hex at byte {}", idx))?;
    }

    Ok(bytes)
}

/// Nostr publisher for creating notes
pub struct NostrPublisher {
    signing_key: Option<SigningKey>,
    relays: Vec<String>,
    enabled: bool,
}

impl NostrPublisher {
    pub fn new(secret_key: impl AsRef<str>, relays: Vec<String>) -> Result<Self> {
        let signing_key = parse_secret_key(secret_key.as_ref())?;

        Ok(Self {
            signing_key: Some(signing_key),
            relays,
            enabled: true,
        })
    }

    /// Create a disabled publisher (for testing/dry-run)
    pub fn disabled() -> Self {
        Self {
            signing_key: None,
            relays: vec![],
            enabled: false,
        }
    }

    /// Generate a Nostr event (NIP-01)
    fn create_event(&self, content: &str, created_at: i64) -> Result<NostrEvent> {
        let pubkey = self.derive_pubkey()?;
        let tags: Vec<Vec<String>> = vec![];

        let serialized =
            serde_json::to_string(&(0, &pubkey, created_at, NOSTR_TEXT_NOTE_KIND, &tags, content))
                .map_err(|e| anyhow!("Failed to canonicalize Nostr event: {}", e))?;

        let id_bytes: [u8; 32] = Sha256::digest(serialized.as_bytes()).into();
        let id = hex_encode(&id_bytes);

        let signing_key = self
            .signing_key
            .as_ref()
            .ok_or_else(|| anyhow!("Nostr publisher is disabled"))?;

        let signature = signing_key
            .sign_prehash_with_aux_rand(&id_bytes, &SCHNORR_ZERO_AUX_RANDOMNESS)
            .map_err(|e| anyhow!("Failed to sign Nostr event: {}", e))?;

        Ok(NostrEvent {
            id,
            pubkey,
            created_at,
            kind: NOSTR_TEXT_NOTE_KIND,
            tags,
            content: content.to_string(),
            sig: hex_encode(&signature.to_bytes()),
        })
    }

    /// Derive x-only public key from secret key
    fn derive_pubkey(&self) -> Result<String> {
        let signing_key = self
            .signing_key
            .as_ref()
            .ok_or_else(|| anyhow!("Nostr publisher is disabled"))?;

        Ok(hex_encode(&signing_key.verifying_key().to_bytes()))
    }

    /// Publish event to a relay via NIP-01 WebSocket.
    async fn publish_to_relay(&self, relay: &str, event: &NostrEvent) -> Result<(), PublishError> {
        let (mut socket, _) = connect_async(relay)
            .await
            .map_err(|e| PublishError::Api(format!("Failed to connect to relay: {}", e)))?;

        let request = serde_json::json!(["EVENT", event]);
        socket
            .send(Message::Text(request.to_string().into()))
            .await
            .map_err(|e| PublishError::Api(format!("Failed to send event to relay: {}", e)))?;

        let mut last_notice = None;

        loop {
            let message = timeout(RELAY_ACK_TIMEOUT, socket.next())
                .await
                .map_err(|_| PublishError::Api("Timed out waiting for relay OK".to_string()))?
                .ok_or_else(|| {
                    PublishError::Api(
                        last_notice
                            .clone()
                            .unwrap_or_else(|| "Relay closed before OK".to_string()),
                    )
                })?
                .map_err(|e| PublishError::Api(format!("Relay websocket error: {}", e)))?;

            match message {
                Message::Text(text) => {
                    match handle_relay_text(&text, &event.id, &mut last_notice)? {
                        RelayAck::Accepted => return Ok(()),
                        RelayAck::Rejected(message) => {
                            return Err(PublishError::Api(format!(
                                "Relay rejected event: {}",
                                message
                            )));
                        }
                        RelayAck::Wait => {}
                    }
                }
                Message::Ping(payload) => socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|e| PublishError::Api(format!("Failed to pong relay: {}", e)))?,
                Message::Close(_) => {
                    return Err(PublishError::Api(
                        last_notice.unwrap_or_else(|| "Relay closed before OK".to_string()),
                    ));
                }
                Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }
    }
}

enum RelayAck {
    Accepted,
    Rejected(String),
    Wait,
}

fn handle_relay_text(
    text: &str,
    event_id: &str,
    last_notice: &mut Option<String>,
) -> Result<RelayAck, PublishError> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| PublishError::Api(format!("Relay sent invalid JSON: {}", e)))?;
    let Some(items) = value.as_array() else {
        return Ok(RelayAck::Wait);
    };

    match items.first().and_then(|kind| kind.as_str()) {
        Some("OK") if items.get(1).and_then(|id| id.as_str()) == Some(event_id) => {
            if items.get(2).and_then(|accepted| accepted.as_bool()) == Some(true) {
                Ok(RelayAck::Accepted)
            } else {
                let message = items
                    .get(3)
                    .and_then(|message| message.as_str())
                    .unwrap_or("relay rejected event");
                Ok(RelayAck::Rejected(message.to_string()))
            }
        }
        Some("NOTICE") => {
            *last_notice = items
                .get(1)
                .and_then(|message| message.as_str())
                .map(str::to_string);
            Ok(RelayAck::Wait)
        }
        _ => Ok(RelayAck::Wait),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NostrEvent {
    id: String,
    pubkey: String,
    created_at: i64,
    kind: u32,
    tags: Vec<Vec<String>>,
    content: String,
    sig: String,
}

#[async_trait]
impl Publisher for NostrPublisher {
    async fn publish(&self, post: &RenderedPost) -> Result<PublishResult, PublishError> {
        if !self.enabled {
            return Err(PublishError::Api("Publisher is disabled".to_string()));
        }

        if self.relays.is_empty() {
            return Err(PublishError::Api("No relays configured".to_string()));
        }

        let created_at = OffsetDateTime::now_utc().unix_timestamp();
        let event = self
            .create_event(&post.text, created_at)
            .map_err(|e| PublishError::Api(format!("Failed to create Nostr event: {}", e)))?;
        let event_id = event.id.clone();

        // Try to publish to at least one relay
        let mut last_error = None;
        let mut success_count = 0;

        for relay in &self.relays {
            match self.publish_to_relay(relay, &event).await {
                Ok(()) => {
                    tracing::info!(relay = %relay, event_id = %event_id, "Published to Nostr relay");
                    success_count += 1;
                }
                Err(e) => {
                    tracing::warn!(relay = %relay, error = %e, "Failed to publish to relay");
                    last_error = Some(e);
                }
            }
        }

        if success_count == 0 {
            return Err(last_error.unwrap_or_else(|| {
                PublishError::Api("Failed to publish to any relay".to_string())
            }));
        }

        Ok(PublishResult {
            id: Some(event_id),
            url: None, // Nostr doesn't have a canonical URL
        })
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn platform(&self) -> &'static str {
        "nostr"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bech32::Bech32;
    use k256::schnorr::{Signature, VerifyingKey};
    use serde_json::Value;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    fn sample_post() -> RenderedPost {
        RenderedPost {
            text: "Narrative analysis of @user\n\nTags: test_tag (0.85)\n\nOriginal: https://x.com/user/status/123".to_string(),
            source_post_id: "123".to_string(),
            source_post_url: "https://x.com/user/status/123".to_string(),
        }
    }

    fn sample_secret_hex() -> &'static str {
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
    }

    fn sample_secret() -> &'static str {
        sample_secret_hex()
    }

    fn nsec_from_bytes(bytes: &[u8]) -> String {
        bech32::encode::<Bech32>(Hrp::parse("nsec").expect("valid hrp"), bytes)
            .expect("valid bech32 encoding")
    }

    async fn spawn_relay(accepted: bool) -> (String, oneshot::Receiver<Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind relay");
        let addr = listener.local_addr().expect("relay addr");
        let (tx, rx) = oneshot::channel();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept relay client");
            let mut socket = accept_async(stream).await.expect("websocket handshake");
            let message = socket
                .next()
                .await
                .expect("event message")
                .expect("valid websocket message");
            let text = message.into_text().expect("text message");
            let value: Value = serde_json::from_str(&text).expect("event JSON");
            let event_id = value[1]["id"].as_str().expect("event id").to_string();
            tx.send(value).ok();

            let ack = serde_json::json!([
                "OK",
                event_id,
                accepted,
                if accepted { "" } else { "blocked" }
            ]);
            socket
                .send(Message::Text(ack.to_string().into()))
                .await
                .expect("send ack");
        });

        (format!("ws://{}", addr), rx)
    }

    #[tokio::test]
    async fn test_disabled_publisher() {
        let publisher = NostrPublisher::disabled();

        assert!(!publisher.is_enabled());
        assert_eq!(publisher.platform(), "nostr");

        let result = publisher.publish(&sample_post()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_no_relays_configured() {
        let publisher = NostrPublisher::new(sample_secret(), vec![]).expect("valid publisher");

        let result = publisher.publish(&sample_post()).await;
        assert!(matches!(result, Err(PublishError::Api(_))));
    }

    #[tokio::test]
    async fn test_publish_success() {
        let (relay, request_rx) = spawn_relay(true).await;
        let publisher = NostrPublisher::new(sample_secret(), vec![relay]).expect("valid publisher");

        let result = publisher.publish(&sample_post()).await.unwrap();

        assert_eq!(result.id.as_deref().expect("event id").len(), 64);

        let request = request_rx.await.expect("relay request");
        assert_eq!(request[0], "EVENT");
        assert_eq!(request[1]["id"].as_str(), result.id.as_deref());
        assert_eq!(
            request[1]["kind"].as_u64(),
            Some(NOSTR_TEXT_NOTE_KIND as u64)
        );
    }

    #[tokio::test]
    async fn test_publish_relay_rejects_event() {
        let (relay, _request_rx) = spawn_relay(false).await;
        let publisher = NostrPublisher::new(sample_secret(), vec![relay]).expect("valid publisher");

        let error = publisher
            .publish(&sample_post())
            .await
            .expect_err("relay rejection");

        assert!(matches!(error, PublishError::Api(message) if message.contains("blocked")));
    }

    #[test]
    fn test_create_event() {
        let publisher = NostrPublisher::new(sample_secret(), vec![]).expect("valid publisher");

        let event = publisher
            .create_event("Test content", 1_704_000_000)
            .expect("event creation should succeed");

        assert_eq!(event.id.len(), 64);
        assert_eq!(event.pubkey.len(), 64);
        assert_eq!(event.sig.len(), 128);
        assert_eq!(event.kind, 1);
        assert_eq!(event.content, "Test content");
    }

    #[test]
    fn test_parse_secret_key_hex() {
        let key = parse_secret_key(sample_secret_hex()).expect("hex key should parse");

        assert_eq!(key.verifying_key().to_bytes().len(), 32);
    }

    #[test]
    fn test_parse_secret_key_nsec() {
        let secret_bytes = decode_hex_fixed::<32>(sample_secret_hex()).expect("valid hex");
        let nsec = nsec_from_bytes(&secret_bytes);

        let key = parse_secret_key(&nsec).expect("nsec key should parse");
        let hex_key = parse_secret_key(sample_secret_hex()).expect("hex key should parse");

        assert_eq!(
            key.verifying_key().to_bytes(),
            hex_key.verifying_key().to_bytes()
        );
    }

    #[test]
    fn test_parse_secret_key_uppercase_nsec() {
        let secret_bytes = decode_hex_fixed::<32>(sample_secret_hex()).expect("valid hex");
        let nsec = nsec_from_bytes(&secret_bytes).to_uppercase();

        let key = parse_secret_key(&nsec).expect("uppercase nsec key should parse");
        let hex_key = parse_secret_key(sample_secret_hex()).expect("hex key should parse");

        assert_eq!(
            key.verifying_key().to_bytes(),
            hex_key.verifying_key().to_bytes()
        );
    }

    #[test]
    fn test_parse_secret_key_hex_and_nsec_equivalence() {
        let secret_bytes = decode_hex_fixed::<32>(sample_secret_hex()).expect("valid hex");
        let nsec = nsec_from_bytes(&secret_bytes);

        let hex_key = parse_secret_key(sample_secret_hex()).expect("hex key should parse");
        let nsec_key = parse_secret_key(&nsec).expect("nsec key should parse");

        assert_eq!(
            hex_key.verifying_key().to_bytes(),
            nsec_key.verifying_key().to_bytes()
        );
    }

    #[test]
    fn test_parse_secret_key_invalid_matrix() {
        let valid_secret_bytes = decode_hex_fixed::<32>(sample_secret_hex()).expect("valid hex");

        let mut short_payload = [0u8; 31];
        short_payload.copy_from_slice(&valid_secret_bytes[..31]);

        let mut long_payload = [0u8; 33];
        long_payload[..32].copy_from_slice(&valid_secret_bytes);
        long_payload[32] = 0x42;

        let cases = vec![
            ("".to_string(), "empty"),
            ("a".repeat(63), "length"),
            ("a".repeat(65), "length"),
            ("g".repeat(64), "non-hex"),
            (
                bech32::encode::<Bech32>(
                    Hrp::parse("npub").expect("valid hrp"),
                    &valid_secret_bytes,
                )
                .expect("valid bech32"),
                "human-readable part",
            ),
            (nsec_from_bytes(&short_payload), "payload length"),
            (nsec_from_bytes(&long_payload), "payload length"),
            (
                "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
                "scalar",
            ),
        ];

        for (input, expected) in cases {
            let err = match parse_secret_key(&input) {
                Ok(_) => panic!("input should be rejected"),
                Err(err) => err,
            };
            assert!(
                err.to_string().to_lowercase().contains(expected),
                "error '{}' should contain '{}' for input '{}'",
                err,
                expected,
                input
            );
        }
    }

    #[test]
    fn test_event_fields_are_consistent() {
        let publisher = NostrPublisher::new(sample_secret(), vec![]).expect("valid publisher");

        let event = publisher
            .create_event("hello nostr", 1_700_000_000)
            .expect("event creation should succeed");

        let serialized = serde_json::to_string(&(
            0,
            &event.pubkey,
            event.created_at,
            event.kind,
            &event.tags,
            &event.content,
        ))
        .expect("event canonical serialization");

        let expected_id = hex_encode(&Sha256::digest(serialized.as_bytes()));
        assert_eq!(event.id, expected_id);

        let pubkey_bytes = decode_hex_fixed::<32>(&event.pubkey).expect("valid pubkey hex");
        let id_bytes = decode_hex_fixed::<32>(&event.id).expect("valid id hex");
        let sig_bytes = decode_hex_fixed::<64>(&event.sig).expect("valid sig hex");

        let verifying_key = VerifyingKey::from_bytes(&pubkey_bytes).expect("valid pubkey");
        let signature = Signature::try_from(sig_bytes.as_slice()).expect("valid signature");

        use k256::schnorr::signature::hazmat::PrehashVerifier;
        verifying_key
            .verify_prehash(&id_bytes, &signature)
            .expect("signature should verify against event id");
    }

    #[test]
    fn test_json_escaping_canonicalization() {
        let publisher = NostrPublisher::new(sample_secret(), vec![]).expect("valid publisher");
        let content = "Quote: \"hello\"\\nPath: C:\\\\tmp\\\\file\\nUnicode: café";

        let event = publisher
            .create_event(content, 1_700_000_111)
            .expect("event creation should succeed");

        let serialized = serde_json::to_string(&(
            0,
            &event.pubkey,
            event.created_at,
            event.kind,
            &event.tags,
            &event.content,
        ))
        .expect("event canonical serialization");

        let expected_id = hex_encode(&Sha256::digest(serialized.as_bytes()));
        assert_eq!(event.id, expected_id);
    }

    #[test]
    fn test_bip340_reference_vector_0() {
        // Official test vector index 0 from BIP-340.
        // Source: https://github.com/bitcoin/bips/blob/master/bip-0340/test-vectors.csv
        let signing_key =
            parse_secret_key("0000000000000000000000000000000000000000000000000000000000000003")
                .expect("reference key should parse");

        let expected_pubkey = "f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9";
        let expected_sig = "e907831f80848d1069a5371b402410364bdf1c5f8307b0084c55f1ce2dca821525f66a4a85ea8b71e482a74f382d2ce5ebeee8fdb2172f477df4900d310536c0";

        let message = [0u8; 32];
        let signature = signing_key
            .sign_raw(&message, &SCHNORR_ZERO_AUX_RANDOMNESS)
            .expect("reference signature should be computable");

        assert_eq!(
            hex_encode(&signing_key.verifying_key().to_bytes()),
            expected_pubkey
        );
        assert_eq!(hex_encode(&signature.to_bytes()), expected_sig);
    }
}
