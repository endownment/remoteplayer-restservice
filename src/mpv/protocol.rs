//! Wire format of the mpv JSON IPC: one JSON object per line in both directions.
//! See `man mpv`, section "JSON IPC".

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MpvReply {
    pub error: String,
    #[serde(default)]
    pub data: Value,
}

impl MpvReply {
    pub fn is_success(&self) -> bool {
        self.error == "success"
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MpvEvent {
    pub name: String,
    pub fields: Map<String, Value>,
}

impl MpvEvent {
    /// For `property-change` events: the property name and its new value
    /// (`null` when the property became unavailable).
    pub fn property_change(&self) -> Option<(&str, &Value)> {
        if self.name != "property-change" {
            return None;
        }
        let property: &str = self.fields.get("name").and_then(Value::as_str)?;
        let value: &Value = self.fields.get("data").unwrap_or(&Value::Null);
        Some((property, value))
    }

    pub fn to_json(&self) -> Value {
        let mut object: Map<String, Value> = self.fields.clone();
        object.insert("event".to_string(), Value::String(self.name.clone()));
        Value::Object(object)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum IncomingMessage {
    Reply { request_id: u64, reply: MpvReply },
    Event(MpvEvent),
    Unrecognized,
}

pub fn parse_incoming_line(line: &str) -> Result<IncomingMessage, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let Value::Object(mut object): Value = value else {
        return Ok(IncomingMessage::Unrecognized);
    };
    if let Some(Value::String(name)) = object.remove("event") {
        return Ok(IncomingMessage::Event(MpvEvent { name, fields: object }));
    }
    let request_id: Option<u64> = object.get("request_id").and_then(Value::as_u64);
    match request_id {
        Some(request_id) => {
            let reply: MpvReply = serde_json::from_value(Value::Object(object))?;
            Ok(IncomingMessage::Reply { request_id, reply })
        }
        None => Ok(IncomingMessage::Unrecognized),
    }
}

pub fn encode_command(request_id: u64, command: &[Value]) -> String {
    let payload: Value = json!({ "command": command, "request_id": request_id });
    let mut line: String = payload.to_string();
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_is_matched_by_request_id() {
        let message: IncomingMessage =
            parse_incoming_line(r#"{"data":42.5,"request_id":7,"error":"success"}"#).expect("parse");
        assert_eq!(
            message,
            IncomingMessage::Reply {
                request_id: 7,
                reply: MpvReply { error: "success".to_string(), data: json!(42.5) }
            }
        );
    }

    #[test]
    fn property_change_event_exposes_name_and_value() {
        let message: IncomingMessage =
            parse_incoming_line(r#"{"event":"property-change","id":1,"name":"pause","data":true}"#).expect("parse");
        let IncomingMessage::Event(event): IncomingMessage = message else {
            panic!("expected an event");
        };
        assert_eq!(event.property_change(), Some(("pause", &json!(true))));
    }

    #[test]
    fn command_is_one_line_of_json() {
        let line: String = encode_command(3, &[json!("get_property"), json!("volume")]);
        assert_eq!(line, "{\"command\":[\"get_property\",\"volume\"],\"request_id\":3}\n");
    }
}
