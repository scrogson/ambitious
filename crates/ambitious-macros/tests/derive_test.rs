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
