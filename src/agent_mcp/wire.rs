use hbb_common::{message_proto::Message, protobuf::UnknownValueRef};
use rustdesk_agent_mcp::automation::{MAX_WIRE_BYTES, WIRE_FIELD, WIRE_VERSION};
use serde_json::Value;

// Private, versioned protobuf extension: old peers ignore this unknown field.
// Keep it outside the upstream oneof; no hbb_common submodule modification is needed.
pub(super) fn encode(value: &Value) -> Result<Message, String> {
    let data = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if data.len() > MAX_WIRE_BYTES {
        return Err("UIA message exceeds 1 MiB".into());
    }
    let mut msg = Message::new();
    msg.special_fields
        .mut_unknown_fields()
        .add_length_delimited(WIRE_FIELD, data);
    Ok(msg)
}

pub(super) fn decode(msg: &Message) -> Option<Result<Value, String>> {
    let field = msg.special_fields.unknown_fields().get(WIRE_FIELD)?;
    Some((|| {
        if msg.union.is_some() {
            return Err("UIA envelope cannot contain another message".into());
        }
        let UnknownValueRef::LengthDelimited(data) = field else {
            return Err("Invalid UIA wire type".into());
        };
        if data.len() > MAX_WIRE_BYTES {
            return Err("UIA message exceeds 1 MiB".into());
        }
        let value: Value = serde_json::from_slice(data).map_err(|_| "Invalid UIA JSON")?;
        if value.get("protocol").and_then(Value::as_str) != Some(WIRE_VERSION) {
            return Err("Unsupported UIA protocol".into());
        }
        Ok(value)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hbb_common::protobuf::Message as _;
    use serde_json::json;
    #[test]
    fn round_trip_without_changing_upstream_protocol() {
        let value = json!({"protocol":WIRE_VERSION,"id":"test","operation":"tree"});
        let encoded = encode(&value).unwrap().write_to_bytes().unwrap();
        let parsed = Message::parse_from_bytes(&encoded).unwrap();
        assert!(parsed.union.is_none());
        assert_eq!(decode(&parsed).unwrap().unwrap(), value);
        assert!(decode(&Message::new()).is_none());
        let mut mixed = encode(&value).unwrap();
        mixed.set_key_event(Default::default());
        assert!(decode(&mixed).unwrap().is_err());
    }
}
