use std::collections::BTreeMap;
use std::io::Cursor;

use base64::Engine;
use ciborium::value::Value;
use ciborium::{de::from_reader, ser::into_writer};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::Serialize;
use serde_bytes::ByteBuf;
use sha2::{Digest, Sha256};

const ENVELOPE_V1_SIGNING_DOMAIN: &[u8] = b"AETHOS_ENVELOPE_V1";

pub fn is_valid_wayfarer_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub fn is_valid_payload_b64(value: &str) -> bool {
    !value.contains('=')
        && base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .is_ok()
}

#[derive(Debug, Clone)]
pub struct EnvelopeV1 {
    pub to_wayfarer_id: [u8; 32],
    pub manifest_id: Vec<u8>,
    pub body: Vec<u8>,
    pub author_pubkey: [u8; 32],
    pub author_sig: [u8; 64],
}

#[derive(Debug, Clone)]
pub struct DecodedEnvelopeV1 {
    pub to_wayfarer_id_hex: String,
    pub manifest_id_hex: String,
    pub author_wayfarer_id_hex: String,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
struct DecodedCanonicalMessageV2 {
    body: Vec<u8>,
}

impl EnvelopeV1 {
    pub fn canonical_bytes_v1(&self) -> Result<Vec<u8>, String> {
        let mut payload_fields = BTreeMap::<String, ByteBuf>::new();
        payload_fields.insert(
            "to_wayfarer_id".to_string(),
            ByteBuf::from(self.to_wayfarer_id.to_vec()),
        );
        payload_fields.insert(
            "manifest_id".to_string(),
            ByteBuf::from(self.manifest_id.clone()),
        );
        payload_fields.insert("body".to_string(), ByteBuf::from(self.body.clone()));
        payload_fields.insert(
            "author_pubkey".to_string(),
            ByteBuf::from(self.author_pubkey.to_vec()),
        );
        payload_fields.insert(
            "author_sig".to_string(),
            ByteBuf::from(self.author_sig.to_vec()),
        );
        let value = to_cbor_value(&payload_fields)
            .map_err(|err| format!("failed serializing envelope cbor: {err}"))?;
        encode_cbor_value_deterministic(&value)
    }
}

#[allow(dead_code)]
pub fn build_envelope_payload_b64_from_utf8(
    to_wayfarer_id_hex: &str,
    body_utf8: &str,
    author_signing_key_seed: &[u8; 32],
) -> Result<String, String> {
    build_envelope_payload_b64(
        to_wayfarer_id_hex,
        body_utf8.as_bytes(),
        author_signing_key_seed,
    )
}

pub fn build_wayfarer_chat_envelope_payload_b64(
    to_wayfarer_id_hex: &str,
    chat_text: &str,
    author_signing_key_seed: &[u8; 32],
    created_at_unix_ms: i64,
) -> Result<String, String> {
    if chat_text.trim().is_empty() {
        return Err("chat text must be non-empty".to_string());
    }
    if created_at_unix_ms < 0 {
        return Err("created_at_unix_ms must be non-negative".to_string());
    }

    let signing_key = SigningKey::from_bytes(author_signing_key_seed);
    let author_wayfarer_id_hex = bytes_to_hex_lower(&Sha256::digest(signing_key.verifying_key()));
    let wayfarer_chat_body = encode_cbor_value_deterministic(&Value::Map(vec![
        (
            Value::Text("type".to_string()),
            Value::Text("wayfarer.chat.v1".to_string()),
        ),
        (
            Value::Text("text".to_string()),
            Value::Text(chat_text.to_string()),
        ),
        (
            Value::Text("created_at_unix_ms".to_string()),
            Value::Integer(created_at_unix_ms.into()),
        ),
    ]))?;
    let canonical_message = build_canonical_message_v2(
        &author_wayfarer_id_hex,
        &wayfarer_chat_body,
        created_at_unix_ms,
    )?;

    build_envelope_payload_b64(
        to_wayfarer_id_hex,
        &canonical_message,
        author_signing_key_seed,
    )
}

pub fn build_envelope_payload_b64(
    to_wayfarer_id_hex: &str,
    body: &[u8],
    author_signing_key_seed: &[u8; 32],
) -> Result<String, String> {
    let to_wayfarer_id = parse_wayfarer_id_hex(to_wayfarer_id_hex)?;
    let manifest_id = Sha256::digest(body).to_vec();
    let signing_payload = build_signing_payload_v1(&to_wayfarer_id, &manifest_id, body)?;
    let signing_digest = envelope_signing_digest_v1(&signing_payload);
    let signing_key = SigningKey::from_bytes(author_signing_key_seed);
    let author_pubkey = signing_key.verifying_key().to_bytes();
    let author_sig = signing_key.sign(&signing_digest).to_bytes();

    let envelope = EnvelopeV1 {
        to_wayfarer_id,
        manifest_id,
        body: body.to_vec(),
        author_pubkey,
        author_sig,
    };
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(envelope.canonical_bytes_v1()?))
}

pub fn decode_envelope_payload_b64(payload_b64: &str) -> Result<DecodedEnvelopeV1, String> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|err| format!("failed to decode payload_b64: {err}"))?;

    parse_envelope_cbor(&raw)
}

fn parse_envelope_cbor(raw: &[u8]) -> Result<DecodedEnvelopeV1, String> {
    let fields = decode_cbor_value_exact(raw, "envelope")?;
    let canonical = encode_cbor_value_deterministic(&fields)
        .map_err(|err| format!("envelope cbor canonical re-encode failed: {err}"))?;
    if canonical.as_slice() != raw {
        return Err("envelope is not canonical CBOR encoding".to_string());
    }

    let Value::Map(map_entries) = fields else {
        return Err("envelope cbor root must be a map".to_string());
    };

    let mut field_map = BTreeMap::<String, Vec<u8>>::new();
    for (key, value) in map_entries {
        let Value::Text(key_text) = key else {
            return Err("envelope cbor keys must be UTF-8 strings".to_string());
        };
        let Value::Bytes(bytes) = value else {
            return Err("envelope cbor values must be byte strings".to_string());
        };
        if field_map.insert(key_text, bytes).is_some() {
            return Err("envelope cbor contains duplicate keys".to_string());
        }
    }

    let expected_keys = [
        "to_wayfarer_id",
        "manifest_id",
        "body",
        "author_pubkey",
        "author_sig",
    ];
    if field_map.len() != expected_keys.len()
        || expected_keys
            .iter()
            .any(|required| !field_map.contains_key(*required))
    {
        return Err("envelope cbor must contain exactly required keys".to_string());
    }

    let to_wayfarer_id = field_map
        .get("to_wayfarer_id")
        .cloned()
        .ok_or_else(|| "missing envelope field to_wayfarer_id".to_string())?
        .to_vec();
    let to_wayfarer_id_arr: [u8; 32] = to_wayfarer_id
        .as_slice()
        .try_into()
        .map_err(|_| "invalid to_wayfarer_id length in envelope".to_string())?;

    let manifest_id = field_map
        .get("manifest_id")
        .cloned()
        .ok_or_else(|| "missing envelope field manifest_id".to_string())?
        .to_vec();
    if manifest_id.len() != 32 {
        return Err("invalid manifest_id length in envelope".to_string());
    }

    let body = field_map
        .get("body")
        .cloned()
        .ok_or_else(|| "missing envelope field body".to_string())?
        .to_vec();

    let author_pubkey = field_map
        .get("author_pubkey")
        .cloned()
        .ok_or_else(|| "missing envelope field author_pubkey".to_string())?
        .to_vec();
    let author_pubkey_arr: [u8; 32] = author_pubkey
        .try_into()
        .map_err(|_| "invalid author_pubkey length in envelope".to_string())?;

    let author_sig = field_map
        .get("author_sig")
        .cloned()
        .ok_or_else(|| "missing envelope field author_sig".to_string())?
        .to_vec();
    let author_sig_arr: [u8; 64] = author_sig
        .try_into()
        .map_err(|_| "invalid author_sig length in envelope".to_string())?;

    let signing_payload = build_signing_payload_v1(&to_wayfarer_id_arr, &manifest_id, &body)?;
    let signing_digest = envelope_signing_digest_v1(&signing_payload);

    let verifying_key = VerifyingKey::from_bytes(&author_pubkey_arr)
        .map_err(|err| format!("invalid author_pubkey: {err}"))?;
    let signature = Signature::from_bytes(&author_sig_arr);
    verifying_key
        .verify(&signing_digest, &signature)
        .map_err(|_| "invalid envelope signature".to_string())?;

    let author_wayfarer_id_hex = bytes_to_hex_lower(&Sha256::digest(author_pubkey_arr));

    Ok(DecodedEnvelopeV1 {
        to_wayfarer_id_hex: bytes_to_hex_lower(&to_wayfarer_id_arr),
        manifest_id_hex: bytes_to_hex_lower(&manifest_id),
        author_wayfarer_id_hex,
        body,
    })
}

fn build_signing_payload_v1(
    to_wayfarer_id: &[u8; 32],
    manifest_id: &[u8],
    body: &[u8],
) -> Result<Vec<u8>, String> {
    let mut signing_fields = BTreeMap::<String, ByteBuf>::new();
    signing_fields.insert(
        "to_wayfarer_id".to_string(),
        ByteBuf::from(to_wayfarer_id.to_vec()),
    );
    signing_fields.insert(
        "manifest_id".to_string(),
        ByteBuf::from(manifest_id.to_vec()),
    );
    signing_fields.insert("body".to_string(), ByteBuf::from(body.to_vec()));
    let value = to_cbor_value(&signing_fields)
        .map_err(|err| format!("failed serializing signing payload cbor: {err}"))?;
    encode_cbor_value_deterministic(&value)
}

fn envelope_signing_digest_v1(signing_payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ENVELOPE_V1_SIGNING_DOMAIN);
    hasher.update(signing_payload);
    hasher.finalize().into()
}

pub fn to_cbor_value<T: Serialize>(value: &T) -> Result<Value, String> {
    let mut raw = Vec::new();
    into_writer(value, &mut raw).map_err(|err| format!("CBOR encode failed: {err}"))?;
    from_reader(raw.as_slice()).map_err(|err| format!("CBOR decode failed: {err}"))
}

pub fn decode_cbor_value_exact(raw: &[u8], context: &str) -> Result<Value, String> {
    let mut cursor = Cursor::new(raw);
    let value: Value =
        from_reader(&mut cursor).map_err(|err| format!("{context} cbor decode failed: {err}"))?;
    if cursor.position() as usize != raw.len() {
        return Err(format!(
            "{context} must contain exactly one complete CBOR value"
        ));
    }
    Ok(value)
}

pub fn encode_cbor_value_deterministic(value: &Value) -> Result<Vec<u8>, String> {
    let canonical = canonicalize_cbor_value(value.clone())?;
    let mut out = Vec::new();
    into_writer(&canonical, &mut out)
        .map_err(|err| format!("deterministic CBOR encode failed: {err}"))?;
    Ok(out)
}

fn canonicalize_cbor_value(mut value: Value) -> Result<Value, String> {
    match &mut value {
        Value::Array(items) => {
            for item in items.iter_mut() {
                *item = canonicalize_cbor_value(item.clone())?;
            }
        }
        Value::Map(entries) => {
            for (key, entry_value) in entries.iter_mut() {
                *key = canonicalize_cbor_value(key.clone())?;
                *entry_value = canonicalize_cbor_value(entry_value.clone())?;
            }

            let mut encoded_entries = Vec::with_capacity(entries.len());
            for (key, entry_value) in entries.drain(..) {
                let encoded_key = encode_cbor_value_raw(&key)?;
                encoded_entries.push((encoded_key, key, entry_value));
            }
            encoded_entries.sort_by(|(left_encoded, _, _), (right_encoded, _, _)| {
                left_encoded.cmp(right_encoded)
            });
            *entries = encoded_entries
                .into_iter()
                .map(|(_, key, entry_value)| (key, entry_value))
                .collect();
        }
        _ => {}
    }
    Ok(value)
}

fn encode_cbor_value_raw(value: &Value) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    into_writer(value, &mut out).map_err(|err| format!("CBOR key encode failed: {err}"))?;
    Ok(out)
}

#[allow(dead_code)]
pub fn decode_envelope_payload_utf8_preview(payload_b64: &str) -> Result<String, String> {
    let decoded = decode_envelope_payload_b64(payload_b64)?;
    String::from_utf8(decoded.body).map_err(|_| "envelope body is not valid UTF-8".to_string())
}

pub fn decode_envelope_payload_text_preview(payload_b64: &str) -> Result<String, String> {
    let decoded = decode_envelope_payload_b64(payload_b64)?;
    let message = decode_canonical_message_v2(&decoded.body)
        .map_err(|err| format!("canonical message decode failed: {err}"))?;
    extract_wayfarer_chat_text_from_cbor(&message.body)
        .ok_or_else(|| "payload is not a valid wayfarer.chat.v1 message".to_string())
}

fn decode_canonical_message_v2(raw: &[u8]) -> Result<DecodedCanonicalMessageV2, String> {
    if raw.len() < 2 {
        return Err("canonical message is truncated".to_string());
    }
    if raw[0] != 2 || raw[1] != 3 {
        return Err("canonical message has invalid version or type".to_string());
    }

    let mut offset = 2usize;
    let mut created_at_unix_ms: Option<i64> = None;
    let mut author_wayfarer_id: Option<&[u8]> = None;
    let mut body: Option<Vec<u8>> = None;

    while offset < raw.len() {
        let field_id = raw[offset];
        offset += 1;
        if offset + 4 > raw.len() {
            return Err("canonical message field length is truncated".to_string());
        }
        let field_len = u32::from_be_bytes([
            raw[offset],
            raw[offset + 1],
            raw[offset + 2],
            raw[offset + 3],
        ]) as usize;
        offset += 4;
        if offset + field_len > raw.len() {
            return Err("canonical message field payload is truncated".to_string());
        }
        let field_raw = &raw[offset..offset + field_len];
        offset += field_len;

        match field_id {
            1 => {
                if field_raw.len() != 8 {
                    return Err(
                        "canonical message created_at_unix_ms has invalid length".to_string()
                    );
                }
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(field_raw);
                created_at_unix_ms = Some(i64::from_be_bytes(bytes));
            }
            2 => {
                if field_raw.len() != 32 {
                    return Err(
                        "canonical message author_wayfarer_id has invalid length".to_string()
                    );
                }
                author_wayfarer_id = Some(field_raw);
            }
            3 => {
                body = Some(field_raw.to_vec());
            }
            4 => {}
            _ => {
                return Err(format!(
                    "canonical message contains unknown field {field_id}"
                ))
            }
        }
    }

    if created_at_unix_ms.is_none() {
        return Err("canonical message missing created_at_unix_ms".to_string());
    }
    if author_wayfarer_id.is_none() {
        return Err("canonical message missing author_wayfarer_id".to_string());
    }
    let body = body.ok_or_else(|| "canonical message missing body".to_string())?;

    Ok(DecodedCanonicalMessageV2 { body })
}

fn extract_wayfarer_chat_text_from_cbor(body: &[u8]) -> Option<String> {
    let value = decode_cbor_value_exact(body, "wayfarer payload").ok()?;
    let Value::Map(entries) = value else {
        return None;
    };

    let mut type_string: Option<String> = None;
    let mut text: Option<String> = None;
    let mut created_at_present = false;

    for (key, value) in entries {
        let Value::Text(key_text) = key else {
            return None;
        };
        match (key_text.as_str(), value) {
            ("type", Value::Text(value)) => type_string = Some(value),
            ("text", Value::Text(value)) if !value.is_empty() => text = Some(value),
            ("created_at_unix_ms", Value::Integer(_)) => created_at_present = true,
            _ => {}
        }
    }

    if type_string.as_deref() != Some("wayfarer.chat.v1") || !created_at_present {
        return None;
    }

    text
}

fn build_canonical_message_v2(
    author_wayfarer_id_hex: &str,
    body: &[u8],
    created_at_unix_ms: i64,
) -> Result<Vec<u8>, String> {
    let author_wayfarer_id = parse_wayfarer_id_hex(author_wayfarer_id_hex)?;

    let mut out = vec![2u8, 3u8];
    out.push(1);
    out.extend_from_slice(&(8u32).to_be_bytes());
    out.extend_from_slice(&created_at_unix_ms.to_be_bytes());
    out.push(2);
    out.extend_from_slice(&(32u32).to_be_bytes());
    out.extend_from_slice(&author_wayfarer_id);
    out.push(3);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    Ok(out)
}

fn parse_wayfarer_id_hex(hex_lower: &str) -> Result<[u8; 32], String> {
    if !is_valid_wayfarer_id(hex_lower) {
        return Err("invalid wayfarer_id; expected 64 lowercase hex chars".to_string());
    }

    let mut out = [0u8; 32];
    for (idx, slot) in out.iter_mut().enumerate() {
        let start = idx * 2;
        let end = start + 2;
        let byte = u8::from_str_radix(&hex_lower[start..end], 16)
            .map_err(|err| format!("failed to parse wayfarer_id hex byte: {err}"))?;
        *slot = byte;
    }
    Ok(out)
}

pub fn bytes_to_hex_lower(input: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(input.len() * 2);
    for byte in input {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::aethos_core::vectors::load_envelope_vectors;

    use super::{
        build_envelope_payload_b64, build_envelope_payload_b64_from_utf8, bytes_to_hex_lower,
        decode_envelope_payload_b64, decode_envelope_payload_text_preview,
        encode_cbor_value_deterministic, parse_envelope_cbor,
    };
    use base64::Engine;
    use ciborium::value::Value;
    use ed25519_dalek::SigningKey;
    use sha2::{Digest, Sha256};

    const VECTOR_TO_WAYFARER_ID: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";
    const VECTOR_BODY: &str = "hello";
    const TEST_SIGNING_KEY_SEED: [u8; 32] = [7u8; 32];

    fn expected_author_wayfarer_id() -> String {
        let verifying_key = SigningKey::from_bytes(&TEST_SIGNING_KEY_SEED).verifying_key();
        bytes_to_hex_lower(&Sha256::digest(verifying_key.to_bytes()))
    }

    fn build_test_canonical_message(
        author_byte: u8,
        body: &[u8],
        created_at_unix_ms: i64,
    ) -> Vec<u8> {
        let mut out = vec![2u8, 3u8];
        out.push(1);
        out.extend_from_slice(&(8u32).to_be_bytes());
        out.extend_from_slice(&created_at_unix_ms.to_be_bytes());
        out.push(2);
        out.extend_from_slice(&(32u32).to_be_bytes());
        out.extend_from_slice(&[author_byte; 32]);
        out.push(3);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn envelope_v1_contains_signed_author_fields() {
        let envelope_b64 = build_envelope_payload_b64_from_utf8(
            VECTOR_TO_WAYFARER_ID,
            VECTOR_BODY,
            &TEST_SIGNING_KEY_SEED,
        )
        .expect("build vector envelope");
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&envelope_b64)
            .expect("decode envelope bytes");

        let decoded = decode_envelope_payload_b64(&envelope_b64).expect("decode envelope");
        assert_eq!(decoded.to_wayfarer_id_hex, VECTOR_TO_WAYFARER_ID);
        assert_eq!(
            decoded.author_wayfarer_id_hex,
            expected_author_wayfarer_id()
        );
        assert_eq!(decoded.body, VECTOR_BODY.as_bytes());
        assert!(!raw.is_empty());
    }

    #[test]
    fn envelope_decode_rejects_noncanonical_map_order() {
        let to_wayfarer = vec![0x11u8; 32];
        let manifest = Sha256::digest(VECTOR_BODY.as_bytes()).to_vec();
        let body = VECTOR_BODY.as_bytes().to_vec();
        let signing_key = SigningKey::from_bytes(&TEST_SIGNING_KEY_SEED);
        let author_pubkey = signing_key.verifying_key().to_bytes().to_vec();
        let author_sig = vec![0u8; 64];
        let noncanonical = Value::Map(vec![
            (
                Value::Text("to_wayfarer_id".to_string()),
                Value::Bytes(to_wayfarer),
            ),
            (
                Value::Text("manifest_id".to_string()),
                Value::Bytes(manifest),
            ),
            (Value::Text("body".to_string()), Value::Bytes(body)),
            (
                Value::Text("author_pubkey".to_string()),
                Value::Bytes(author_pubkey),
            ),
            (
                Value::Text("author_sig".to_string()),
                Value::Bytes(author_sig),
            ),
        ]);
        let noncanonical_raw = {
            let mut out = Vec::new();
            ciborium::ser::into_writer(&noncanonical, &mut out).expect("encode noncanonical");
            out
        };
        assert!(parse_envelope_cbor(&noncanonical_raw)
            .expect_err("must reject noncanonical ordering")
            .contains("not canonical"));
    }

    #[test]
    fn deterministic_encoder_orders_map_keys_by_encoded_bytes() {
        let value = Value::Map(vec![
            (
                Value::Text("payload".to_string()),
                Value::Integer(1u8.into()),
            ),
            (
                Value::Text("type".to_string()),
                Value::Text("HELLO".to_string()),
            ),
        ]);
        let raw = encode_cbor_value_deterministic(&value).expect("encode deterministic");
        let expected = vec![
            0xa2, 0x64, b't', b'y', b'p', b'e', 0x65, b'H', b'E', b'L', b'L', b'O', 0x67, b'p',
            b'a', b'y', b'l', b'o', b'a', b'd', 0x01,
        ];
        assert_eq!(raw, expected);
    }

    #[test]
    fn envelope_payload_decoder_accepts_vector() {
        let envelope_b64 = build_envelope_payload_b64_from_utf8(
            VECTOR_TO_WAYFARER_ID,
            VECTOR_BODY,
            &TEST_SIGNING_KEY_SEED,
        )
        .expect("build vector envelope");
        let decoded = decode_envelope_payload_b64(&envelope_b64).expect("decode envelope");
        assert_eq!(decoded.to_wayfarer_id_hex, VECTOR_TO_WAYFARER_ID);
        assert_eq!(decoded.body, VECTOR_BODY.as_bytes());
        assert_eq!(
            decoded.author_wayfarer_id_hex,
            expected_author_wayfarer_id()
        );
    }

    #[test]
    fn envelope_text_preview_rejects_non_contract_legacy_chat_json_body() {
        let destination = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let from = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let payload = format!(
            "\u{0002}\u{0003}\u{0001}\u{0000}\u{0000}\u{0000}\u{0008}\u{0000}\u{0000}\u{0001}\u{009d}\u{0000}\u{0000}\u{0000}\u{0020}{from}\u{0003}\u{0000}\u{0000}\u{0000}\u{001f}{{\"text\":\"interop marker\",\"type\":\"chat\",\"v\":2}}"
        );
        let envelope_b64 =
            build_envelope_payload_b64_from_utf8(destination, &payload, &TEST_SIGNING_KEY_SEED)
                .expect("build envelope with canonical message body payload");

        assert!(decode_envelope_payload_text_preview(&envelope_b64)
            .expect_err("legacy json body should not pass strict contract preview")
            .contains("canonical message decode failed"));
    }

    #[test]
    fn envelope_text_preview_extracts_wayfarer_chat_text_from_canonical_message_body() {
        let destination = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let wayfarer_chat_body = encode_cbor_value_deterministic(&Value::Map(vec![
            (
                Value::Text("text".to_string()),
                Value::Text("hello wayfarer".to_string()),
            ),
            (
                Value::Text("type".to_string()),
                Value::Text("wayfarer.chat.v1".to_string()),
            ),
            (
                Value::Text("created_at_unix_ms".to_string()),
                Value::Integer(1_735_689_600_000u64.into()),
            ),
        ]))
        .expect("encode wayfarer chat body");
        let canonical_message =
            build_test_canonical_message(0xbb, &wayfarer_chat_body, 1_735_689_600_000);
        let envelope_b64 =
            build_envelope_payload_b64(destination, &canonical_message, &TEST_SIGNING_KEY_SEED)
                .expect("build envelope with wayfarer canonical message body");

        let preview = decode_envelope_payload_text_preview(&envelope_b64)
            .expect("decode wayfarer chat preview");
        assert_eq!(preview, "hello wayfarer");
    }

    #[test]
    fn envelope_decode_rejects_tampered_signature() {
        let envelope_b64 = build_envelope_payload_b64_from_utf8(
            VECTOR_TO_WAYFARER_ID,
            VECTOR_BODY,
            &TEST_SIGNING_KEY_SEED,
        )
        .expect("build vector envelope");
        let mut raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&envelope_b64)
            .expect("decode envelope bytes");
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let tampered_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);

        assert!(decode_envelope_payload_b64(&tampered_b64)
            .expect_err("tampered signature must fail")
            .contains("invalid envelope signature"));
    }

    #[test]
    fn cross_client_vectors_decode_and_verify() {
        let vector_set = load_envelope_vectors();

        for vector in vector_set.vectors {
            let envelope_raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&vector.payload_b64)
                .expect("decode payload_b64");
            assert_eq!(
                bytes_to_hex_lower(&envelope_raw),
                vector.canonical_envelope_cbor_hex
            );
            assert_eq!(
                bytes_to_hex_lower(&Sha256::digest(&envelope_raw)),
                vector.item_id_hex
            );

            let decoded =
                decode_envelope_payload_b64(&vector.payload_b64).expect("decode envelope payload");
            assert_eq!(
                decoded.to_wayfarer_id_hex,
                vector.expected_decoded.to_wayfarer_id
            );
            assert_eq!(decoded.manifest_id_hex, vector.expected_decoded.manifest_id);
            assert_eq!(
                String::from_utf8(decoded.body).expect("body is utf8"),
                vector.expected_decoded.body_utf8_preview
            );
            assert_eq!(
                vector.expected_decoded.manifest_id,
                bytes_to_hex_lower(&Sha256::digest(
                    vector.expected_decoded.body_utf8_preview.as_bytes()
                ))
            );
        }
    }
}
