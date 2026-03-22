use crate::RootState;
use lattice_store_base::{FieldFormat, Introspectable, MethodKind, MethodMeta};
use prost_reflect::DescriptorPool;
use std::collections::HashMap;

static DESCRIPTOR_POOL_STATE: std::sync::OnceLock<DescriptorPool> = std::sync::OnceLock::new();

pub(crate) fn get_descriptor_pool() -> &'static DescriptorPool {
    DESCRIPTOR_POOL_STATE.get_or_init(|| {
        DescriptorPool::decode(crate::ROOTSTORE_DESCRIPTOR_BYTES)
            .expect("Invalid embedded descriptors")
    })
}

impl Introspectable for RootState {
    fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        get_descriptor_pool()
            .get_service_by_name("lattice.rootstore.RootStore")
            .expect("Service definition missing")
    }

    fn decode_payload(
        &self,
        payload: &[u8],
    ) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        // Migrated stores contain both RootPayload (version > 0) and legacy
        // KvPayload intentions. This is not a fallback — both formats
        // physically exist in the intention log and must be decoded for display.
        use prost::Message as _;
        if let Ok(rp) = crate::proto::RootPayload::decode(payload) {
            if rp.version > 0 {
                let pool = get_descriptor_pool();
                let msg_desc = pool
                    .get_message_by_name("lattice.rootstore.RootPayload")
                    .ok_or("RootPayload descriptor not found")?;
                let mut dynamic = prost_reflect::DynamicMessage::new(msg_desc);
                prost_reflect::prost::Message::merge(&mut dynamic, payload)?;
                return Ok(dynamic);
            }
        }

        let kv_pool = prost_reflect::DescriptorPool::decode(lattice_kvstore::KV_DESCRIPTOR_BYTES)?;
        let msg_desc = kv_pool
            .get_message_by_name("lattice.kv.KvPayload")
            .ok_or("KvPayload descriptor not found")?;
        let mut dynamic = prost_reflect::DynamicMessage::new(msg_desc);
        prost_reflect::prost::Message::merge(&mut dynamic, payload)?;
        Ok(dynamic)
    }

    fn method_meta(&self) -> HashMap<String, MethodMeta> {
        HashMap::from([
            (
                "RegisterApp".into(),
                MethodMeta {
                    description: "Register an app at a subdomain".into(),
                    kind: MethodKind::Command,
                },
            ),
            (
                "RemoveApp".into(),
                MethodMeta {
                    description: "Remove an app definition".into(),
                    kind: MethodKind::Command,
                },
            ),
            (
                "UploadBundle".into(),
                MethodMeta {
                    description: "Upload an app bundle (zip)".into(),
                    kind: MethodKind::Command,
                },
            ),
            (
                "RemoveBundle".into(),
                MethodMeta {
                    description: "Remove an app bundle".into(),
                    kind: MethodKind::Command,
                },
            ),
            (
                "GetApp".into(),
                MethodMeta {
                    description: "Get app definition by subdomain".into(),
                    kind: MethodKind::Query,
                },
            ),
            (
                "ListApps".into(),
                MethodMeta {
                    description: "List all registered apps".into(),
                    kind: MethodKind::Query,
                },
            ),
            (
                "GetBundle".into(),
                MethodMeta {
                    description: "Get bundle data by app_id".into(),
                    kind: MethodKind::Query,
                },
            ),
            (
                "ListBundles".into(),
                MethodMeta {
                    description: "List all uploaded bundles".into(),
                    kind: MethodKind::Query,
                },
            ),
        ])
    }

    fn field_formats(&self) -> HashMap<String, FieldFormat> {
        HashMap::from([
            ("RegisterAppOp.store_id".into(), FieldFormat::Hex),
            ("GetAppResponse.store_id".into(), FieldFormat::Hex),
            ("AppEntry.store_id".into(), FieldFormat::Hex),
        ])
    }

    fn summarize_payload(
        &self,
        payload: &prost_reflect::DynamicMessage,
    ) -> Vec<lattice_model::SExpr> {
        payload_summary::summarize(payload)
    }
}

mod payload_summary {
    use lattice_model::SExpr;
    use prost_reflect::{DynamicMessage, ReflectMessage, Value};

    pub fn summarize(payload: &DynamicMessage) -> Vec<SExpr> {
        let Some(Value::List(ops)) = payload.get_field_by_name("ops").map(|v| v.into_owned())
        else {
            return vec![];
        };
        ops.iter()
            .filter_map(|v| summarize_root_op(v).or_else(|| summarize_kv_op(v)))
            .collect()
    }

    // Native RootPayload ops
    fn summarize_root_op(val: &Value) -> Option<SExpr> {
        let Value::Message(m) = val else { return None };
        if let Some(r) = get_active_oneof(m, "register_app") {
            return Some(SExpr::list(vec![
                SExpr::sym("register-app"),
                SExpr::str(get_string(&r, "subdomain")?),
                SExpr::str(get_string(&r, "app_id")?),
            ]));
        }
        if let Some(r) = get_active_oneof(m, "remove_app") {
            return Some(SExpr::list(vec![
                SExpr::sym("remove-app"),
                SExpr::str(get_string(&r, "subdomain")?),
            ]));
        }
        if let Some(r) = get_active_oneof(m, "upload_bundle") {
            let size = get_bytes(&r, "data").map(|b| b.len()).unwrap_or(0);
            return Some(SExpr::list(vec![
                SExpr::sym("upload-bundle"),
                SExpr::str(get_string(&r, "app_id")?),
                SExpr::str(format!("<{} bytes>", size)),
            ]));
        }
        if let Some(r) = get_active_oneof(m, "remove_bundle") {
            return Some(SExpr::list(vec![
                SExpr::sym("remove-bundle"),
                SExpr::str(get_string(&r, "app_id")?),
            ]));
        }
        None
    }

    fn summarize_kv_op(val: &Value) -> Option<SExpr> {
        let Value::Message(m) = val else { return None };
        if let Some(d) = get_active_oneof(m, "delete") {
            let k = String::from_utf8_lossy(&get_bytes(&d, "key")?).to_string();
            return Some(SExpr::list(vec![SExpr::sym("del"), SExpr::str(k)]));
        }
        if let Some(p) = get_active_oneof(m, "put") {
            let k = String::from_utf8_lossy(&get_bytes(&p, "key")?).to_string();
            let val_bytes = get_bytes(&p, "value").unwrap_or_default();
            let v = if val_bytes.len() > 128 {
                format!("<{} bytes>", val_bytes.len())
            } else {
                String::from_utf8_lossy(&val_bytes).to_string()
            };
            return Some(SExpr::list(vec![
                SExpr::sym("put"),
                SExpr::str(k),
                SExpr::str(v),
            ]));
        }
        None
    }

    fn get_active_oneof(msg: &DynamicMessage, field_name: &str) -> Option<DynamicMessage> {
        let field = msg.descriptor().get_field_by_name(field_name)?;
        if !msg.has_field(&field) {
            return None;
        }
        match msg.get_field(&field).into_owned() {
            Value::Message(m) => Some(m),
            _ => None,
        }
    }

    fn get_string(msg: &DynamicMessage, field: &str) -> Option<String> {
        match msg.get_field_by_name(field)?.as_ref() {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            _ => None,
        }
    }

    fn get_bytes(msg: &DynamicMessage, field: &str) -> Option<Vec<u8>> {
        match msg.get_field_by_name(field)?.as_ref() {
            Value::Bytes(b) if !b.is_empty() => Some(b.to_vec()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use lattice_store_base::Introspectable;
    use prost::Message;

    fn make_state() -> crate::RootState {
        lattice_mockkernel::TestHarness::<crate::RootState>::new().store
    }

    #[test]
    fn decode_native_register_app() {
        let state = make_state();
        let payload = crate::proto::RootPayload {
            version: 1,
            ops: vec![crate::proto::RootOperation {
                op: Some(crate::proto::root_operation::Op::RegisterApp(
                    crate::proto::RegisterAppOp {
                        subdomain: "inventory".into(),
                        app_id: "inv-app".into(),
                        store_id: vec![1; 16],
                    },
                )),
            }],
        };
        let bytes = payload.encode_to_vec();
        let decoded = state.decode_payload(&bytes).expect("decode failed");
        let summary = state.summarize_payload(&decoded);
        assert!(!summary.is_empty(), "summary should not be empty");
        let text = format!("{:?}", summary);
        assert!(
            text.contains("register-app"),
            "should contain register-app, got: {text}"
        );
        assert!(
            text.contains("inventory"),
            "should contain subdomain, got: {text}"
        );
    }

    #[test]
    fn decode_native_upload_bundle() {
        let state = make_state();
        let payload = crate::proto::RootPayload {
            version: 1,
            ops: vec![crate::proto::RootOperation {
                op: Some(crate::proto::root_operation::Op::UploadBundle(
                    crate::proto::UploadBundleOp {
                        app_id: "my-app".into(),
                        data: vec![0xDE, 0xAD],
                        manifest: Vec::new(),
                    },
                )),
            }],
        };
        let bytes = payload.encode_to_vec();
        let decoded = state.decode_payload(&bytes).expect("decode failed");
        let summary = state.summarize_payload(&decoded);
        assert!(!summary.is_empty(), "summary should not be empty");
        let text = format!("{:?}", summary);
        assert!(
            text.contains("upload-bundle"),
            "should contain upload-bundle, got: {text}"
        );
        assert!(
            text.contains("my-app"),
            "should contain app_id, got: {text}"
        );
    }

    #[test]
    fn decode_native_remove_app() {
        let state = make_state();
        let payload = crate::proto::RootPayload {
            version: 1,
            ops: vec![crate::proto::RootOperation {
                op: Some(crate::proto::root_operation::Op::RemoveApp(
                    crate::proto::RemoveAppOp {
                        subdomain: "old-app".into(),
                    },
                )),
            }],
        };
        let bytes = payload.encode_to_vec();
        let decoded = state.decode_payload(&bytes).expect("decode failed");
        let summary = state.summarize_payload(&decoded);
        let text = format!("{:?}", summary);
        assert!(text.contains("remove-app"), "got: {text}");
        assert!(text.contains("old-app"), "got: {text}");
    }

    #[test]
    fn decode_legacy_kv_put() {
        let state = make_state();
        let payload = lattice_kvstore::KvPayload {
            ops: vec![lattice_kvstore::proto::Operation::put(
                b"apps/crm/app-id",
                b"crm-app",
            )],
        };
        let bytes = payload.encode_to_vec();
        let decoded = state.decode_payload(&bytes).expect("decode failed");
        let summary = state.summarize_payload(&decoded);
        assert!(!summary.is_empty(), "summary should not be empty");
        let text = format!("{:?}", summary);
        assert!(text.contains("put"), "should contain put, got: {text}");
        assert!(
            text.contains("apps/crm/app-id"),
            "should contain key, got: {text}"
        );
    }

    #[test]
    fn decode_legacy_kv_delete() {
        let state = make_state();
        let payload = lattice_kvstore::KvPayload {
            ops: vec![lattice_kvstore::proto::Operation::delete(
                b"appbundles/old/bundle.zip",
            )],
        };
        let bytes = payload.encode_to_vec();
        let decoded = state.decode_payload(&bytes).expect("decode failed");
        let summary = state.summarize_payload(&decoded);
        let text = format!("{:?}", summary);
        assert!(text.contains("del"), "should contain del, got: {text}");
        assert!(text.contains("appbundles/old/bundle.zip"), "got: {text}");
    }

    #[test]
    fn decode_legacy_kv_bundle_upload() {
        let state = make_state();
        let payload = lattice_kvstore::KvPayload {
            ops: vec![lattice_kvstore::proto::Operation::put(
                b"appbundles/myapp/bundle.zip",
                b"\x50\x4b\x03\x04fake-zip-data-here",
            )],
        };
        let bytes = payload.encode_to_vec();
        let decoded = state.decode_payload(&bytes).expect("decode failed");
        let summary = state.summarize_payload(&decoded);
        assert!(!summary.is_empty(), "legacy bundle upload should decode");
        let text = format!("{:?}", summary);
        assert!(text.contains("put"), "got: {text}");
        assert!(text.contains("appbundles/myapp/bundle.zip"), "got: {text}");
    }
}
