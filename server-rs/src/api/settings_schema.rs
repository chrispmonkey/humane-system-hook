//! `GET /api/settings/schema` — the settings as typed, sectioned fields so a
//! client renders the form instead of hardcoding it. Writes go through the
//! typed `PUT /api/settings`.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::api::ApiState;
use crate::config::Config;

pub fn router() -> Router<ApiState> {
    Router::new().route("/api/settings/schema", get(get_schema))
}

async fn get_schema(State(state): State<ApiState>) -> Json<Schema> {
    let config = state.shared_config.read().await;
    Json(describe(&config))
}

/// A config sub-struct that can list its settings fields — implemented by
/// `#[derive(SettingsFields)]`, which reads each field's `#[setting(...)]`.
pub(crate) trait SettingsFields {
    fn settings_fields(&self, prefix: &str) -> Vec<Field>;
}

/// An enum that supplies its dropdown options and selected value — implemented
/// by `#[derive(SettingsOptions)]`.
pub(crate) trait SettingsOptions {
    fn settings_options() -> Vec<EnumOption>;
    fn settings_value(&self) -> &'static str;
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
pub(crate) struct Field {
    pub key: String,
    pub label: &'static str,
    #[serde(flatten)]
    pub kind: FieldKind,
    #[serde(rename = "visibleWhen", skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<BTreeMap<&'static str, &'static str>>,
}

/// The per-type payload; serde tags it as `type` and carries only that type's
/// fields — a value for the scalars, `isSet` for a secret, options for an enum.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum FieldKind {
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
pub(crate) struct EnumOption {
    pub value: &'static str,
    pub label: &'static str,
}

/// The user-facing sections, in display order. Each is one config sub-struct;
/// the fields (and their keys, types, and values) come from its derive.
fn describe(config: &Config) -> Schema {
    Schema {
        sections: vec![
            section("server", "Server", &config.server),
            section("llm", "LLM", &config.llm),
            section("weather", "Weather", &config.weather),
            section("contacts", "Contacts", &config.contacts),
            section("dev", "Developer", &config.dev),
        ],
    }
}

fn section(key: &'static str, label: &'static str, source: &impl SettingsFields) -> Section {
    Section { key, label, fields: source.settings_fields(key) }
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

    /// Locks the whole derived descriptor: every section, every field, its key,
    /// type, and label — so a config change or a wrong `#[setting]` can't quietly
    /// alter what clients render.
    #[test]
    fn descriptor_matches_the_expected_inventory() {
        let schema = descriptor(&default_config());
        let got: Vec<(String, String, String)> = schema["sections"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| {
                let section = s["key"].as_str().unwrap().to_string();
                s["fields"].as_array().unwrap().iter().map(move |f| {
                    (
                        section.clone(),
                        f["key"].as_str().unwrap().to_string(),
                        format!("{}:{}", f["type"].as_str().unwrap(), f["label"].as_str().unwrap()),
                    )
                })
            })
            .collect();

        let expected = [
            ("server", "server.system_prompt", "text:System Prompt"),
            ("server", "server.status_prompt", "text:Status Prompt"),
            ("server", "server.display_name", "string:Display Name"),
            ("llm", "llm.provider", "enum:Provider"),
            ("llm", "llm.model", "string:Model ID"),
            ("llm", "llm.api_key", "secret:API Key"),
            ("llm", "llm.base_url", "string:Base URL"),
            ("llm", "llm.web_search", "bool:Provider web search"),
            ("weather", "weather.pirate_weather_api_key", "secret:PirateWeather API Key"),
            ("contacts", "contacts.trust_all_contacts", "bool:Trust all contacts"),
            ("contacts", "contacts.allow_all_inbound", "bool:Allow all inbound calls and messages"),
            ("dev", "dev.apk_install_enabled", "bool:Remote APK install"),
        ];
        let expected: Vec<_> = expected
            .iter()
            .map(|(s, k, t)| (s.to_string(), k.to_string(), t.to_string()))
            .collect();

        assert_eq!(got, expected);
    }

    /// The prompt fields are backed by `get = "resolved_*"`, not the raw
    /// `Option` — so they render the built-in prompt when unset, while a plain
    /// optional field (display_name) renders empty.
    #[test]
    fn getter_backed_fields_render_resolved_values() {
        let config = default_config();
        let schema = descriptor(&config);

        let system_prompt = find_field(&schema, "server.system_prompt");
        assert_eq!(
            system_prompt["value"].as_str().unwrap(),
            config.server.resolved_system_prompt()
        );
        assert!(!system_prompt["value"].as_str().unwrap().is_empty());

        assert_eq!(find_field(&schema, "server.display_name")["value"], "");
    }
}
