//! `GET /api/settings/schema` — the settings as typed, sectioned fields so a
//! client renders the form instead of hardcoding it. Writes go through the
//! typed `PUT /api/settings`.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use strum::VariantArray;

use crate::api::ApiState;
use crate::config::{Config, LlmProvider};

pub fn router() -> Router<ApiState> {
    Router::new().route("/api/settings/schema", get(get_schema))
}

async fn get_schema(State(state): State<ApiState>) -> Json<Schema> {
    let config = state.shared_config.read().await;
    Json(describe(&config))
}

#[derive(Serialize)]
struct Schema {
    sections: Vec<Section>,
}

#[derive(Serialize)]
struct Section {
    key: &'static str,
    label: &'static str,
    fields: Vec<Field>,
}

#[derive(Serialize)]
struct Field {
    key: String,
    label: &'static str,
    #[serde(flatten)]
    kind: FieldKind,
    #[serde(rename = "visibleWhen", skip_serializing_if = "Option::is_none")]
    visible_when: Option<BTreeMap<&'static str, &'static str>>,
}

/// The per-type payload; serde tags it as `type` and carries only that type's
/// fields — a value for the scalars, `isSet` for a secret, options for an enum.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum FieldKind {
    String { value: String },
    Text { value: String },
    Bool { value: bool },
    Secret {
        #[serde(rename = "isSet")]
        is_set: bool,
    },
    Enum {
        value: &'static str,
        options: Vec<EnumOption>,
    },
}

#[derive(Serialize)]
struct EnumOption {
    value: &'static str,
    label: &'static str,
}

fn describe(config: &Config) -> Schema {
    Schema {
        sections: vec![
            section("server", "Server", vec![
                field("display_name", "Display Name",
                    FieldKind::String { value: config.server.display_name.clone().unwrap_or_default() }),
                field("system_prompt", "System Prompt",
                    FieldKind::Text { value: config.server.resolved_system_prompt() }),
                field("status_prompt", "Status Prompt",
                    FieldKind::Text { value: config.server.resolved_status_prompt() }),
            ]),
            section("llm", "LLM", vec![
                field("provider", "Provider",
                    FieldKind::Enum { value: config.llm.provider.as_str(), options: provider_options() }),
                field("model", "Model ID", FieldKind::String { value: config.llm.model.clone() }),
                field("api_key", "API Key", FieldKind::Secret { is_set: config.llm.api_key.is_some() }),
                field("base_url", "Base URL",
                    FieldKind::String { value: config.llm.base_url.clone().unwrap_or_default() })
                    .visible_when("llm.provider", "openai-compatible"),
                field("web_search", "Provider web search", FieldKind::Bool { value: config.llm.web_search }),
            ]),
            section("weather", "Weather", vec![
                field("pirate_weather_api_key", "PirateWeather API Key",
                    FieldKind::Secret { is_set: config.weather.pirate_weather_api_key.is_some() }),
            ]),
            section("contacts", "Contacts", vec![
                field("trust_all_contacts", "Trust all contacts",
                    FieldKind::Bool { value: config.contacts.trust_all_contacts }),
                field("allow_all_inbound", "Allow all inbound calls and messages",
                    FieldKind::Bool { value: config.contacts.allow_all_inbound }),
            ]),
            section("dev", "Developer", vec![
                field("apk_install_enabled", "Remote APK install",
                    FieldKind::Bool { value: config.dev.apk_install_enabled }),
            ]),
        ],
    }
}

/// Prefix each field's key with the section key, so the `server.display_name`
/// config paths are joined here rather than spelled out per field.
fn section(key: &'static str, label: &'static str, mut fields: Vec<Field>) -> Section {
    for f in &mut fields {
        f.key = format!("{key}.{}", f.key);
    }
    Section { key, label, fields }
}

fn field(name: &'static str, label: &'static str, kind: FieldKind) -> Field {
    Field { key: name.to_string(), label, kind, visible_when: None }
}

impl Field {
    fn visible_when(mut self, key: &'static str, value: &'static str) -> Field {
        self.visible_when = Some(BTreeMap::from([(key, value)]));
        self
    }
}

fn provider_options() -> Vec<EnumOption> {
    LlmProvider::VARIANTS
        .iter()
        .map(|p| EnumOption { value: p.as_str(), label: p.label() })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// A defaults-only config (no file present → serde defaults), like the
    /// config module's own tests.
    fn default_config() -> Config {
        let dir = tempfile::tempdir().unwrap();
        Config::load(&dir.path().join("config.toml")).unwrap()
    }

    fn descriptor(config: &Config) -> Value {
        serde_json::to_value(describe(config)).unwrap()
    }

    fn find_field<'a>(schema: &'a Value, key: &str) -> &'a Value {
        schema["sections"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["fields"].as_array().unwrap())
            .find(|f| f["key"] == key)
            .unwrap_or_else(|| panic!("field {key} not in descriptor"))
    }

    #[test]
    fn secret_fields_report_is_set_not_the_value() {
        let mut config = default_config();
        config.llm.api_key = Some("sk-secret".to_string());
        let schema = descriptor(&config);

        let api_key = find_field(&schema, "llm.api_key");
        assert_eq!(api_key["type"], "secret");
        assert_eq!(api_key["isSet"], true);
        // The value must never appear.
        assert!(api_key.get("value").is_none());
        assert!(!schema.to_string().contains("sk-secret"));
    }

    #[test]
    fn enum_and_conditional_fields_are_described() {
        let schema = descriptor(&default_config());

        let provider = find_field(&schema, "llm.provider");
        assert_eq!(provider["type"], "enum");
        // Options come from LlmProvider's variants, so every one — including the
        // serde-renamed `openai-compatible` — is present with its label.
        let values: Vec<&str> = provider["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["value"].as_str().unwrap())
            .collect();
        assert_eq!(values, ["echo", "gemini", "anthropic", "openai", "openai-compatible"]);
        let openai_compatible = provider["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["value"] == "openai-compatible")
            .unwrap();
        assert_eq!(openai_compatible["label"], "OpenAI-compatible");

        // base_url is gated on the openai-compatible provider.
        let base_url = find_field(&schema, "llm.base_url");
        assert_eq!(base_url["visibleWhen"]["llm.provider"], "openai-compatible");
    }
}
