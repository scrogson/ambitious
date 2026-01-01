//! Integration tests for ambitious-macros.

use ambitious::message::{Message, decode_tag};
use ambitious_macros::Message as DeriveMessage;

// =========================================================================
// Message derive macro tests
// =========================================================================

// Unit struct with derive
#[derive(DeriveMessage)]
struct Ping;

#[test]
fn test_derive_unit_struct() {
    let msg = Ping;
    let bytes = msg.encode_local();

    let (tag, payload) = decode_tag(&bytes).unwrap();
    assert_eq!(tag, "Ping");
    assert!(payload.is_empty());

    let _decoded = Ping::decode_local(payload).unwrap();
}

// Newtype struct with derive
#[derive(DeriveMessage)]
struct Add(i64);

#[test]
fn test_derive_newtype_struct() {
    let msg = Add(42);
    let bytes = msg.encode_local();

    let (tag, payload) = decode_tag(&bytes).unwrap();
    assert_eq!(tag, "Add");

    let decoded = Add::decode_local(payload).unwrap();
    assert_eq!(decoded.0, 42);
}

// Named struct with derive
#[derive(DeriveMessage, Debug, PartialEq)]
struct Login {
    username: String,
    password: String,
}

#[test]
fn test_derive_named_struct() {
    let msg = Login {
        username: "alice".to_string(),
        password: "secret".to_string(),
    };
    let bytes = msg.encode_local();

    let (tag, payload) = decode_tag(&bytes).unwrap();
    assert_eq!(tag, "Login");

    let decoded = Login::decode_local(payload).unwrap();
    assert_eq!(decoded.username, "alice");
    assert_eq!(decoded.password, "secret");
}

// Custom tag
#[derive(DeriveMessage)]
#[message(tag = "increment")]
struct Inc;

#[test]
fn test_derive_custom_tag() {
    let msg = Inc;
    let bytes = msg.encode_local();

    let (tag, _) = decode_tag(&bytes).unwrap();
    assert_eq!(tag, "increment");
}

// =========================================================================
// ETF (Erlang Term Format) encoding tests
// =========================================================================

#[test]
fn test_derive_unit_struct_etf() {
    let msg = Ping;
    let bytes = msg.encode_remote();

    // Should be decodable
    let decoded = Ping::decode_remote(&bytes).unwrap();
    // Unit structs have no data to compare, just verify no panic
    let _ = decoded;

    // Verify it's valid ETF (atom)
    let term = erltf::decode(&bytes).unwrap();
    match term {
        erltf::OwnedTerm::Atom(atom) => assert_eq!(atom.as_str(), "Ping"),
        _ => panic!("expected atom, got {:?}", term),
    }
}

#[test]
fn test_derive_newtype_struct_etf() {
    let msg = Add(42);
    let bytes = msg.encode_remote();

    // Should roundtrip
    let decoded = Add::decode_remote(&bytes).unwrap();
    assert_eq!(decoded.0, 42);

    // Verify it's valid ETF (tagged tuple)
    let term = erltf::decode(&bytes).unwrap();
    match term {
        erltf::OwnedTerm::Tuple(elems) => {
            assert_eq!(elems.len(), 2);
            match &elems[0] {
                erltf::OwnedTerm::Atom(atom) => assert_eq!(atom.as_str(), "Add"),
                _ => panic!("expected atom tag"),
            }
        }
        _ => panic!("expected tuple, got {:?}", term),
    }
}

#[test]
fn test_derive_named_struct_etf() {
    let msg = Login {
        username: "alice".to_string(),
        password: "secret123".to_string(),
    };
    let bytes = msg.encode_remote();

    // Should roundtrip
    let decoded = Login::decode_remote(&bytes).unwrap();
    assert_eq!(decoded, msg);

    // Verify it's valid ETF (tagged tuple)
    let term = erltf::decode(&bytes).unwrap();
    match term {
        erltf::OwnedTerm::Tuple(elems) => {
            assert_eq!(elems.len(), 2);
            match &elems[0] {
                erltf::OwnedTerm::Atom(atom) => assert_eq!(atom.as_str(), "Login"),
                _ => panic!("expected atom tag"),
            }
        }
        _ => panic!("expected tuple, got {:?}", term),
    }
}

#[test]
fn test_derive_custom_tag_etf() {
    let msg = Inc;
    let bytes = msg.encode_remote();

    // Should be decodable
    let decoded = Inc::decode_remote(&bytes).unwrap();
    let _ = decoded;

    // Verify custom tag in ETF
    let term = erltf::decode(&bytes).unwrap();
    match term {
        erltf::OwnedTerm::Atom(atom) => assert_eq!(atom.as_str(), "increment"),
        _ => panic!("expected atom, got {:?}", term),
    }
}

// Enum test
#[derive(DeriveMessage, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
enum CounterMsg {
    Get,
    Add(i64),
    Reset { to: i64 },
}

#[test]
fn test_derive_enum_etf() {
    // Test Get variant
    let msg = CounterMsg::Get;
    let bytes = msg.encode_remote();
    let decoded = CounterMsg::decode_remote(&bytes).unwrap();
    assert_eq!(decoded, msg);

    // Test Add variant
    let msg = CounterMsg::Add(42);
    let bytes = msg.encode_remote();
    let decoded = CounterMsg::decode_remote(&bytes).unwrap();
    assert_eq!(decoded, msg);

    // Test Reset variant
    let msg = CounterMsg::Reset { to: 100 };
    let bytes = msg.encode_remote();
    let decoded = CounterMsg::decode_remote(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
