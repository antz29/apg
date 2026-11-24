//! Unit tests for the canonical fact-body codec — pure in-memory, no
//! filesystem, database, git or process.

use std::collections::{BTreeMap, BTreeSet};

use crate::cache::{CacheKey, FileFragment, FragEdge, FragNode, Manifest, ModuleScaffolding};
use crate::delta::ScanRecord;
use crate::impact::{OverloadIndex, Signature};

use super::*;

fn sample_cache_key() -> CacheKey {
    CacheKey {
        binary_version: "0.0.0-test".into(),
        jsonl_schema: "jsonl-test".into(),
        projection: "projection-test".into(),
        config: "config-digest".into(),
    }
}

fn sample_manifest() -> Manifest {
    Manifest {
        root: "/repo".into(),
        entries: BTreeMap::from([
            ("src/a.rs".to_string(), "oid-a".to_string()),
            ("src/b.rs".to_string(), "oid-b".to_string()),
        ]),
    }
}

fn sample_frag_node() -> FragNode {
    FragNode {
        fqn: "rust.apg.cache.body".into(),
        kind: "struct".into(),
        path: "/repo/src/cache/body.rs".into(),
        start: 0,
        end: 10,
        start_line: 1,
        end_line: 10,
        code_type: "src".into(),
        params: vec!["u32".into()],
        category: None,
        status: None,
    }
}

fn sample_frag_edge() -> FragEdge {
    FragEdge {
        kind: "calls".into(),
        from: "rust.apg.a".into(),
        to: "rust.apg.b".into(),
        target_type: String::new(),
        properties: BTreeMap::new(),
    }
}

fn sample_file_fragment() -> FileFragment {
    FileFragment {
        rel_path: "src/cache/body.rs".into(),
        blob_oid: "deadbeef".into(),
        lang: "rust".into(),
        modules: vec!["rust.apg.cache".into()],
        nodes: vec![sample_frag_node()],
        edges: vec![sample_frag_edge()],
        inputs: BTreeSet::from(["rust.apg.cache.body".to_string()]),
    }
}

fn sample_module_scaffolding() -> ModuleScaffolding {
    ModuleScaffolding {
        modules: vec!["rust.apg".into(), "rust.apg.cache".into()],
        edges: vec![("rust.apg".into(), "rust.apg.cache".into())],
        languages: vec!["rust".into()],
        language_edges: vec![("rust".into(), "rust.apg".into())],
        files: BTreeMap::from([(
            "rust.apg.cache".to_string(),
            vec!["src/cache/body.rs".to_string()],
        )]),
    }
}

fn sample_scan_record() -> ScanRecord {
    ScanRecord {
        sha: "abc123".into(),
        cache_key: sample_cache_key(),
        manifest: sample_manifest(),
        content_key: Some("tree-identity".into()),
    }
}

fn sample_signature_map() -> BTreeMap<String, Vec<Signature>> {
    BTreeMap::from([(
        "src/lib.rs".to_string(),
        vec![
            Signature {
                kind: "function".into(),
                fqn: "rust.apg.f".into(),
                params: vec!["u32".into()],
                parent: "rust.apg".into(),
            },
            Signature {
                kind: "struct".into(),
                fqn: "rust.apg.S".into(),
                params: Vec::new(),
                parent: "rust.apg.S".into(),
            },
        ],
    )])
}

fn sample_overload_index() -> OverloadIndex {
    OverloadIndex {
        scopes: BTreeMap::from([("rust.apg".to_string(), vec!["rust.apg.f".to_string()])]),
        file_scopes: BTreeMap::from([(
            "src/lib.rs".to_string(),
            BTreeSet::from(["rust.apg".to_string()]),
        )]),
    }
}

fn sample_dep_edges() -> Vec<(String, String)> {
    vec![
        ("src/a.rs".to_string(), "src/b.rs".to_string()),
        ("src/b.rs".to_string(), "src/c.rs".to_string()),
    ]
}

/// Encodes, decodes and re-encodes `value`, requiring a byte-identical
/// round-trip and agreeing ids. Comparing the re-encoded bytes proves the
/// decode is the inverse of the canonical encoding even where a type has no
/// `PartialEq`.
fn assert_roundtrips<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let encoded = encode(value).expect("encode");
    let decoded: T = decode(&encoded.bytes).expect("decode");
    let reencoded = encode(&decoded).expect("re-encode");
    assert_eq!(
        encoded.bytes, reencoded.bytes,
        "payload must round-trip byte-identically"
    );
    assert_eq!(encoded.id, body_id(&encoded.bytes));
    assert_eq!(encoded.id, reencoded.id);
}

/// unit tier — pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

    #[test]
    fn encoded_body_starts_with_the_postcard_codec_header() {
        let encoded = encode(&sample_cache_key()).expect("encode");
        assert_eq!(encoded.bytes.first(), Some(&CODEC_POSTCARD));
        assert_eq!(encoded.bytes[0], CODEC_POSTCARD);
        assert_eq!(CODEC_POSTCARD, 1);
    }

    #[test]
    fn every_stored_body_shape_round_trips() {
        assert_roundtrips(&sample_file_fragment());
        assert_roundtrips(&sample_module_scaffolding());
        assert_roundtrips(&sample_scan_record());
        assert_roundtrips(&sample_manifest());
        assert_roundtrips(&sample_cache_key());
        assert_roundtrips(&sample_signature_map());
        assert_roundtrips(&sample_overload_index());
        assert_roundtrips(&sample_dep_edges());
    }

    #[test]
    fn body_id_is_the_blake3_digest_and_is_stable() {
        let encoded = encode(&sample_cache_key()).expect("encode");
        assert_eq!(encoded.id, body_id(&encoded.bytes));
        assert_eq!(
            body_id(&encoded.bytes),
            *blake3::hash(&encoded.bytes).as_bytes()
        );
        assert_eq!(encoded.id, body_id(&encoded.bytes));
    }

    #[test]
    fn identical_bodies_share_one_id_and_a_changed_body_gets_another() {
        let a = sample_cache_key();
        let b = sample_cache_key();
        let encoded_a = encode(&a).expect("encode a");
        let encoded_b = encode(&b).expect("encode b");
        assert_eq!(encoded_a.bytes, encoded_b.bytes);
        assert_eq!(encoded_a.id, encoded_b.id);

        let again = encode(&a).expect("re-encode a");
        assert_eq!(encoded_a.bytes, again.bytes);
        assert_eq!(encoded_a.id, again.id);

        let mut changed = sample_cache_key();
        changed.config = "a different config digest".into();
        let encoded_changed = encode(&changed).expect("encode changed");
        assert_ne!(encoded_a.bytes, encoded_changed.bytes);
        assert_ne!(encoded_a.id, encoded_changed.id);
    }

    #[test]
    fn decode_rejects_a_missing_or_unknown_header() {
        let empty = decode::<CacheKey>(&[]).expect_err("an empty slice has no header");
        let message = format!("{empty:#}");
        assert!(message.contains("empty"), "unexpected error: {message}");
        assert!(message.contains("header"), "unexpected error: {message}");

        let unknown = decode::<CacheKey>(&[0x7f]).expect_err("an unknown header is rejected");
        let message = format!("{unknown:#}");
        assert!(message.contains("unknown"), "unexpected error: {message}");
        assert!(message.contains("127"), "unexpected error: {message}");
    }
}
