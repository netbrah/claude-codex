use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Controls how the model selects tools during a turn.
///
/// - `Auto` (default): the model decides whether to call a tool.
/// - `Required`: the model must call at least one tool (Anthropic `any` / OpenAI `required`).
/// - `None`: the model must not call any tools — forced pure-text synthesis.
/// - `Specific { name }`: the model must call the named tool.
#[derive(Debug, Default, Serialize, Deserialize, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ToolChoice {
    /// Let the model decide whether to use tools.
    #[default]
    Auto,
    /// Force the model to call at least one tool.
    Required,
    /// Prevent the model from calling any tools.
    None,
    /// Force the model to call a specific tool by name.
    Specific {
        /// The name of the tool the model must call.
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn default_is_auto() {
        assert_eq!(ToolChoice::default(), ToolChoice::Auto);
    }

    #[test]
    fn serialize_auto_json() {
        let val = serde_json::to_value(&ToolChoice::Auto).unwrap();
        assert_eq!(val, serde_json::json!({"type": "auto"}));
    }

    #[test]
    fn serialize_required_json() {
        let val = serde_json::to_value(&ToolChoice::Required).unwrap();
        assert_eq!(val, serde_json::json!({"type": "required"}));
    }

    #[test]
    fn serialize_none_json() {
        let val = serde_json::to_value(&ToolChoice::None).unwrap();
        assert_eq!(val, serde_json::json!({"type": "none"}));
    }

    #[test]
    fn serialize_specific_json() {
        let val = serde_json::to_value(&ToolChoice::Specific {
            name: "shell".to_string(),
        })
        .unwrap();
        assert_eq!(
            val,
            serde_json::json!({"type": "specific", "name": "shell"})
        );
    }

    #[test]
    fn deserialize_auto_json() {
        let tc: ToolChoice = serde_json::from_str(r#"{"type": "auto"}"#).unwrap();
        assert_eq!(tc, ToolChoice::Auto);
    }

    #[test]
    fn deserialize_required_json() {
        let tc: ToolChoice = serde_json::from_str(r#"{"type": "required"}"#).unwrap();
        assert_eq!(tc, ToolChoice::Required);
    }

    #[test]
    fn deserialize_none_json() {
        let tc: ToolChoice = serde_json::from_str(r#"{"type": "none"}"#).unwrap();
        assert_eq!(tc, ToolChoice::None);
    }

    #[test]
    fn deserialize_specific_json() {
        let tc: ToolChoice =
            serde_json::from_str(r#"{"type": "specific", "name": "apply_patch"}"#).unwrap();
        assert_eq!(
            tc,
            ToolChoice::Specific {
                name: "apply_patch".to_string()
            }
        );
    }

    #[test]
    fn round_trip_all_variants() {
        let variants = vec![
            ToolChoice::Auto,
            ToolChoice::Required,
            ToolChoice::None,
            ToolChoice::Specific {
                name: "my_tool".to_string(),
            },
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let deserialized: ToolChoice = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, deserialized);
        }
    }

    #[test]
    fn deserialize_rejects_unknown_type() {
        let result = serde_json::from_str::<ToolChoice>(r#"{"type": "invalid"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_specific_requires_name() {
        let result = serde_json::from_str::<ToolChoice>(r#"{"type": "specific"}"#);
        assert!(result.is_err());
    }
}
