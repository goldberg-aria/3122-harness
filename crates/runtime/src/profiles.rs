use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ProviderRoute;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedProviderProfile {
    pub alias: String,
    pub route: String,
    pub base_url: String,
    pub api_key: String,
    pub source: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderRegistry {
    #[serde(default)]
    pub profiles: Vec<SavedProviderProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDetection {
    pub route: ProviderRoute,
    pub provider_name: String,
    pub base_url: String,
    pub confidence: DetectionConfidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionConfidence {
    High,
    Medium,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderPreset {
    pub name: &'static str,
    pub route: ProviderRoute,
    pub base_url: &'static str,
    pub description: &'static str,
}

pub fn provider_registry_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".harness").join("providers.json")
}

pub fn load_provider_registry(workspace_root: &Path) -> Result<ProviderRegistry, String> {
    let path = provider_registry_path(workspace_root);
    if !path.is_file() {
        return Ok(ProviderRegistry::default());
    }
    let contents = fs::read_to_string(&path).map_err(|err| err.to_string())?;
    serde_json::from_str(&contents).map_err(|err| err.to_string())
}

pub fn save_provider_registry(
    workspace_root: &Path,
    registry: &ProviderRegistry,
) -> Result<PathBuf, String> {
    let path = provider_registry_path(workspace_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let contents = serde_json::to_string_pretty(registry).map_err(|err| err.to_string())?;
    fs::write(&path, contents).map_err(|err| err.to_string())?;
    Ok(path)
}

pub fn upsert_provider_profile(
    workspace_root: &Path,
    profile: SavedProviderProfile,
) -> Result<PathBuf, String> {
    let mut registry = load_provider_registry(workspace_root)?;
    if let Some(existing) = registry
        .profiles
        .iter_mut()
        .find(|existing| existing.alias == profile.alias)
    {
        *existing = profile;
    } else {
        registry.profiles.push(profile);
        registry
            .profiles
            .sort_by(|left, right| left.alias.cmp(&right.alias));
    }
    save_provider_registry(workspace_root, &registry)
}

pub fn remove_provider_profile(workspace_root: &Path, alias: &str) -> Result<bool, String> {
    let mut registry = load_provider_registry(workspace_root)?;
    let before = registry.profiles.len();
    registry.profiles.retain(|profile| profile.alias != alias);
    if registry.profiles.len() == before {
        return Ok(false);
    }
    save_provider_registry(workspace_root, &registry)?;
    Ok(true)
}

pub fn find_provider_profile<'a>(
    registry: &'a ProviderRegistry,
    alias: &str,
) -> Option<&'a SavedProviderProfile> {
    registry
        .profiles
        .iter()
        .find(|profile| profile.alias == alias)
}

pub fn detect_provider_key(api_key: &str) -> Option<ProviderDetection> {
    let trimmed = api_key.trim();
    if trimmed.starts_with("sk-or-v1-") {
        return Some(ProviderDetection {
            route: ProviderRoute::OpenAiCompat,
            provider_name: "openrouter".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            confidence: DetectionConfidence::High,
        });
    }
    if trimmed.starts_with("sk-proj-") {
        return Some(ProviderDetection {
            route: ProviderRoute::OpenAiCompat,
            provider_name: "openai".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            confidence: DetectionConfidence::Medium,
        });
    }
    None
}

pub fn provider_presets() -> Vec<ProviderPreset> {
    vec![
        ProviderPreset {
            name: "openai",
            route: ProviderRoute::OpenAiCompat,
            base_url: "https://api.openai.com/v1",
            description: "OpenAI-compatible Chat Completions",
        },
        ProviderPreset {
            name: "openrouter",
            route: ProviderRoute::OpenAiCompat,
            base_url: "https://openrouter.ai/api/v1",
            description: "OpenRouter OpenAI-compatible gateway",
        },
        ProviderPreset {
            name: "deepseek",
            route: ProviderRoute::OpenAiCompat,
            base_url: "https://api.deepseek.com/v1",
            description: "DeepSeek OpenAI-compatible API",
        },
        ProviderPreset {
            name: "dashscope-cn",
            route: ProviderRoute::OpenAiCompat,
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
            description: "Alibaba Model Studio China OpenAI-compatible API",
        },
        ProviderPreset {
            name: "dashscope-intl",
            route: ProviderRoute::OpenAiCompat,
            base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            description: "Alibaba Model Studio International OpenAI-compatible API",
        },
        ProviderPreset {
            name: "siliconflow",
            route: ProviderRoute::OpenAiCompat,
            base_url: "https://api.siliconflow.com/v1",
            description: "SiliconFlow OpenAI-compatible API",
        },
        ProviderPreset {
            name: "anthropic",
            route: ProviderRoute::Anthropic,
            base_url: "https://api.anthropic.com",
            description: "Anthropic Messages API",
        },
    ]
}

pub fn provider_preset(name: &str) -> Option<ProviderPreset> {
    provider_presets()
        .into_iter()
        .find(|preset| preset.name == name.trim())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::ProviderRoute;

    use super::{
        detect_provider_key, find_provider_profile, load_provider_registry, provider_preset,
        remove_provider_profile, upsert_provider_profile, DetectionConfidence,
        SavedProviderProfile,
    };

    fn temp_workspace(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn detects_openrouter_and_openai_keys() {
        let openrouter = detect_provider_key("sk-or-v1-test").unwrap();
        assert_eq!(openrouter.route, ProviderRoute::OpenAiCompat);
        assert_eq!(openrouter.provider_name, "openrouter");
        assert_eq!(openrouter.confidence, DetectionConfidence::High);

        let openai = detect_provider_key("sk-proj-test").unwrap();
        assert_eq!(openai.provider_name, "openai");
        assert_eq!(openai.confidence, DetectionConfidence::Medium);
    }

    #[test]
    fn persists_profiles_to_registry() {
        let workspace = temp_workspace("provider-registry");
        upsert_provider_profile(
            &workspace,
            SavedProviderProfile {
                alias: "router".to_string(),
                route: "openai-compat".to_string(),
                base_url: "https://openrouter.ai/api/v1".to_string(),
                api_key: "sk-or-v1-test".to_string(),
                source: "detected".to_string(),
            },
        )
        .unwrap();

        let registry = load_provider_registry(&workspace).unwrap();
        let profile = find_provider_profile(&registry, "router").unwrap();
        assert_eq!(profile.base_url, "https://openrouter.ai/api/v1");

        assert!(remove_provider_profile(&workspace, "router").unwrap());
        let registry = load_provider_registry(&workspace).unwrap();
        assert!(find_provider_profile(&registry, "router").is_none());

        cleanup(&workspace);
    }

    #[test]
    fn loads_presets() {
        let preset = provider_preset("deepseek").unwrap();
        assert_eq!(preset.route, ProviderRoute::OpenAiCompat);
        assert!(preset.base_url.contains("api.deepseek.com"));
    }
}
