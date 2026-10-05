use bitcoin::ScriptBuf;
use std::convert::TryInto;

use arch_titan_types_core::{RuneId, SerializedOutPoint};

/// Creates an `OutPoint` from a 36-byte slice.
/// Returns an error if the slice is not exactly 36 bytes long.
pub fn outpoint_from_bytes(bytes: &[u8]) -> Result<SerializedOutPoint, &'static str> {
    if bytes.len() != 36 {
        return Err("Invalid length for OutPoint, expected 36 bytes");
    }

    // Extract Txid (first 32 bytes) and vout (last 4 bytes)
    let vout = u32::from_le_bytes(bytes[32..36].try_into().unwrap());

    Ok(SerializedOutPoint::new(&bytes[0..32], vout))
}

pub fn script_pubkey_outpoint_to_bytes(
    script_pubkey: &ScriptBuf,
    outpoint: &SerializedOutPoint,
) -> Vec<u8> {
    let prefix = script_pubkey_search_key(script_pubkey);
    let mut buffer: Vec<u8> = Vec::with_capacity(prefix.len() + 36);
    buffer.extend_from_slice(&prefix);
    buffer.extend_from_slice(outpoint.as_ref());
    buffer
}

pub fn script_pubkey_search_key(script_pubkey: &ScriptBuf) -> Vec<u8> {
    let mut buffer: Vec<u8> = Vec::with_capacity(script_pubkey.len() + 1);
    buffer.extend_from_slice(script_pubkey.as_bytes());
    buffer.push(b':');
    buffer
}

/// Parses the outpoint out of a `script_pubkey || ':' || outpoint` key, where `prefix_len` is
/// the length of the `script_pubkey || ':'` search prefix the key was found with.
///
/// The key must be exactly `prefix_len + 36` bytes long: a longer key belongs to a different
/// script that merely starts with the queried script followed by `':'`.
pub fn parse_outpoint_from_script_pubkey_key(
    key: &[u8],
    prefix_len: usize,
) -> Option<SerializedOutPoint> {
    if prefix_len == 0 || key.len() != prefix_len + 36 || key[prefix_len - 1] != b':' {
        return None;
    }

    outpoint_from_bytes(&key[prefix_len..]).ok()
}

pub fn rune_index_key(rune_id: &RuneId) -> Vec<u8> {
    let rune_id_bytes = rune_id.to_bytes();
    let mut v = Vec::with_capacity(rune_id_bytes.len() + 11);
    v.extend_from_slice(b"rune_index:");
    v.extend_from_slice(&rune_id_bytes);
    v
}

/// `"rune:<id>:"`, the common prefix of every rune transaction key of `rune_id`.
pub fn rune_transaction_prefix(rune_id: &RuneId) -> Vec<u8> {
    let rune_id_bytes = rune_id.to_bytes();
    let mut v = Vec::with_capacity(rune_id_bytes.len() + 6 + 8);
    v.extend_from_slice(b"rune:");
    v.extend_from_slice(&rune_id_bytes);
    v.push(b':');
    v
}

/// Exclusive upper bound of every key starting with `rune_transaction_prefix(rune_id)`.
pub fn rune_transaction_prefix_end(rune_id: &RuneId) -> Vec<u8> {
    let mut v = rune_transaction_prefix(rune_id);
    // ':' + 1 == ';', so "rune:<id>;" sorts right after every "rune:<id>:..." key.
    *v.last_mut().expect("prefix is not empty") = b':' + 1;
    v
}

/// `"rune:<id>:<index as 8 big-endian bytes>"`. Big-endian so RocksDB's bytewise key order
/// matches numeric index order.
pub fn rune_transaction_key(rune_id: &RuneId, index: u64) -> Vec<u8> {
    let mut v = rune_transaction_prefix(rune_id);
    v.extend_from_slice(&index.to_be_bytes());
    v
}

/// Extracts the index from a key built by `rune_transaction_key`, given the length of the
/// `rune_transaction_prefix` it was found with.
pub fn rune_transaction_index(key: &[u8], prefix_len: usize) -> Option<u64> {
    if key.len() != prefix_len + 8 {
        return None;
    }
    Some(u64::from_be_bytes(key[prefix_len..].try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rune_transaction_keys_sort_numerically() {
        let rune_id = RuneId::new(840_000, 1);
        let prefix = rune_transaction_prefix(&rune_id);
        let end = rune_transaction_prefix_end(&rune_id);

        let mut previous = rune_transaction_key(&rune_id, 0);
        for index in [1u64, 255, 256, 257, 65_535, 65_536, u64::MAX] {
            let key = rune_transaction_key(&rune_id, index);
            assert!(
                key > previous,
                "key for {index} must sort after its predecessor"
            );
            assert!(key.starts_with(&prefix));
            assert!(key < end);
            assert_eq!(rune_transaction_index(&key, prefix.len()), Some(index));
            previous = key;
        }
    }

    #[test]
    fn script_pubkey_key_requires_exact_length() {
        let script = ScriptBuf::from_bytes(vec![0x51]);
        let outpoint = SerializedOutPoint::new(&[7u8; 32], 3);
        let prefix_len = script_pubkey_search_key(&script).len();

        let key = script_pubkey_outpoint_to_bytes(&script, &outpoint);
        assert_eq!(
            parse_outpoint_from_script_pubkey_key(&key, prefix_len),
            Some(outpoint)
        );

        // A longer script that starts with `script || ':'` must not match.
        let longer = ScriptBuf::from_bytes(vec![0x51, b':', 0x52]);
        let longer_key = script_pubkey_outpoint_to_bytes(&longer, &outpoint);
        assert!(longer_key.starts_with(&script_pubkey_search_key(&script)));
        assert_eq!(
            parse_outpoint_from_script_pubkey_key(&longer_key, prefix_len),
            None
        );
    }
}
