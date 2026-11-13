//! Stateless MCP Streamable HTTP endpoint, protocol version 2025-11-25.
use crate::model::Core;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub const VERSION: &str = "2025-11-25";

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message}})
}

/// Returns None for notifications, including initialized and cancelled.
pub fn handle(body: &[u8], core: &Arc<Mutex<Core>>) -> Option<Value> {
    let request: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return Some(error(Value::Null, -32700, "Parse error")),
    };
    let Some(obj) = request.as_object() else {
        return Some(error(Value::Null, -32600, "Invalid Request"));
    };
    let id = obj.get("id").cloned();
    if obj.get("jsonrpc") != Some(&json!("2.0"))
        || obj.get("method").and_then(Value::as_str).is_none()
        || id
            .as_ref()
            .is_some_and(|v| !(v.is_string() || v.is_i64() || v.is_u64()))
        || obj.get("params").is_some_and(|v| !v.is_object())
    {
        return Some(error(id.unwrap_or(Value::Null), -32600, "Invalid Request"));
    }
    let method = obj["method"].as_str().unwrap_or("");
    let Some(id) = id else {
        return None;
    };
    let params = obj.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method {
        "initialize" => {
            if params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .is_none()
                || !params.get("capabilities").is_some_and(Value::is_object)
                || !params.get("clientInfo").is_some_and(Value::is_object)
            {
                return Some(error(id, -32602, "Invalid params"));
            }
            json!({"protocolVersion":VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"padgent","version":env!("CARGO_PKG_VERSION")},"instructions":"Claim keys to obtain a lease token, then set native or custom scenes. Clear blanks selected or all leased keys while retaining the lease; release restores the normal display."})
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools":tools()}),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(error(id, -32602, "Invalid params"));
            };
            let Some(op) = name.strip_prefix("display_").filter(|op| {
                matches!(
                    *op,
                    "state" | "claim" | "set" | "clear" | "renew" | "release" | "events"
                )
            }) else {
                return Some(error(id, -32602, "Unknown tool"));
            };
            let mut args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let Some(args_obj) = args.as_object_mut() else {
                return Some(error(id, -32602, "Invalid arguments"));
            };
            args_obj.insert("op".into(), json!(op));
            let command: crate::display::DisplayCommand = match serde_json::from_value(args) {
                Ok(c) => c,
                Err(e) => {
                    return Some(
                        json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":format!("Invalid arguments: {e}")}],"isError":true}}),
                    );
                }
            };
            let result = match core.lock() {
                Ok(mut c) => c.display_command(command).map_err(|e| e.message),
                Err(_) => Err("controller unavailable".into()),
            };
            match result {
                Ok(value) => {
                    json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value})
                }
                Err(message) => json!({"content":[{"type":"text","text":message}],"isError":true}),
            }
        }
        _ => return Some(error(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn tools() -> Vec<Value> {
    let mut tools = Vec::new();
    for (name, description, properties, required) in [
        (
            "state",
            "Read display state without lease tokens",
            json!({}),
            json!([]),
        ),
        (
            "claim",
            "Claim exclusive display keys for a bounded lease",
            json!({"owner":{"type":"string","minLength":1,"maxLength":80},"keys":{"type":"array","items":{"type":"integer","minimum":0,"maximum":8},"minItems":1,"maxItems":9,"uniqueItems":true},"ttl_seconds":{"type":"number","minimum":1,"maximum":3600}}),
            json!(["owner", "keys"]),
        ),
        (
            "set",
            "Update scenes for keys held by a lease",
            json!({"token":{"type":"string"},"keys":{"type":"object","minProperties":1,"maxProperties":9,"propertyNames":{"pattern":"^[0-8]$"},"additionalProperties":{"anyOf":[{"type":"null"},scene_schema()]}}}),
            json!(["token", "keys"]),
        ),
        (
            "clear",
            "Blank selected or all leased keys while retaining the lease",
            json!({"token":{"type":"string"},"keys":{"type":["array","null"],"items":{"type":"integer","minimum":0,"maximum":8},"minItems":1,"maxItems":9,"uniqueItems":true}}),
            json!(["token"]),
        ),
        (
            "renew",
            "Renew a display lease",
            json!({"token":{"type":"string"},"ttl_seconds":{"type":"number","minimum":1,"maximum":3600}}),
            json!(["token"]),
        ),
        (
            "release",
            "Release a display lease",
            json!({"token":{"type":"string"}}),
            json!(["token"]),
        ),
        (
            "events",
            "Poll bounded key press events for a lease",
            json!({"token":{"type":"string"}}),
            json!(["token"]),
        ),
    ] {
        tools.push(json!({"name":format!("display_{name}"),"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}));
    }
    tools
}

fn scene_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "kind":{"enum":["native","custom"]},"title":{"type":"string","maxLength":64},"text":{"type":"string","maxLength":160},
        "activity":{"enum":["empty","starting","idle","thinking","tool","streaming","waiting_permission","waiting_question","done","error","compacting","offline"]},"hue":{"type":"number","minimum":0,"maximum":360},"style":{"enum":["orb","dots"]},
        "background":{"type":"string","pattern":"^#?[0-9a-fA-F]{6}$","description":"Hex RGB color"},"foreground":{"type":"string","pattern":"^#?[0-9a-fA-F]{6}$","description":"Hex RGB color"},"accent":{"type":"string","pattern":"^#?[0-9a-fA-F]{6}$","description":"Hex RGB color"},
        "animation":{"enum":["none","pulse","drift","gradient"]},"period_seconds":{"type":"number","minimum":1,"maximum":120},
        "layers":{"type":"array","maxItems":16,"items":{"type":"object","additionalProperties":false,"properties":{
            "kind":{"enum":["rect","circle","ellipse","line","text"]},"x":{"type":"number","minimum":0,"maximum":1},"y":{"type":"number","minimum":0,"maximum":1},"width":{"type":"number","minimum":0,"maximum":1},"height":{"type":"number","minimum":0,"maximum":1},"size":{"type":"number","minimum":0,"maximum":1},"color":{"type":"string","pattern":"^#?[0-9a-fA-F]{6}$"},"text":{"type":"string","maxLength":100}
        }}}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(core: &Arc<Mutex<Core>>, id: u64, method: &str, params: Value) -> Value {
        let body = json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}).to_string();
        handle(body.as_bytes(), core).expect("request with id returns a response")
    }

    fn tool(core: &Arc<Mutex<Core>>, id: u64, name: &str, arguments: Value) -> Value {
        rpc(
            core,
            id,
            "tools/call",
            json!({"name":name,"arguments":arguments}),
        )
    }

    #[test]
    fn initialize_lists_display_tools_and_accepts_initialized_notification() {
        let core = Arc::new(Mutex::new(Core::new(crate::model::Settings::default())));
        let initialized = rpc(
            &core,
            1,
            "initialize",
            json!({
                "protocolVersion":VERSION,
                "capabilities":{},
                "clientInfo":{"name":"test-client","version":"1"}
            }),
        );
        assert_eq!(initialized["result"]["protocolVersion"], VERSION);
        assert_eq!(initialized["result"]["serverInfo"]["name"], "padgent");

        let notification =
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string();
        assert!(handle(notification.as_bytes(), &core).is_none());

        let listed = rpc(&core, 2, "tools/list", json!({}));
        let names: Vec<_> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"display_claim"));
        assert!(names.contains(&"display_set"));
        assert!(names.contains(&"display_events"));
    }

    #[test]
    fn display_tools_share_controller_and_do_not_create_provider_sessions() {
        let core = Arc::new(Mutex::new(Core::new(crate::model::Settings::default())));
        let before = core.lock().unwrap().visible().len();

        let claimed = tool(
            &core,
            1,
            "display_claim",
            json!({"owner":"mcp-test","keys":[0],"ttl_seconds":30}),
        );
        assert_ne!(claimed["result"]["isError"], true);
        let token = claimed["result"]["structuredContent"]["token"]
            .as_str()
            .unwrap()
            .to_owned();

        let set = tool(
            &core,
            2,
            "display_set",
            json!({
                "token":token,
                "keys":{"0":{"kind":"custom","title":"MCP scene","animation":"none"}}
            }),
        );
        assert_eq!(set["result"]["structuredContent"]["ok"], true);
        let state = tool(&core, 3, "display_state", json!({}));
        let leases = state["result"]["structuredContent"]["leases"]
            .as_array()
            .unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0]["owner"], "mcp-test");
        assert!(leases[0].get("token").is_none());

        let released = tool(&core, 4, "display_release", json!({"token":token}));
        assert_eq!(released["result"]["structuredContent"]["ok"], true);
        assert_eq!(
            tool(&core, 5, "display_state", json!({}))["result"]["structuredContent"]["leases"],
            json!([])
        );
        assert_eq!(
            core.lock().unwrap().visible().len(),
            before,
            "display tools must not mutate provider tracking"
        );
    }
}
