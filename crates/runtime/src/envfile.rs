use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const KNOWN_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "GROQ_API_KEY",
    "GROQ_BASE_URL",
    "QWEN_API_KEY",
    "QWEN_BASE_URL",
    "ZAI_API_KEY",
    "ZAI_BASE_URL",
    "MINIMAX_API_KEY",
    "MINIMAX_BASE_URL",
    "DEEPINFRA_API_KEY",
    "DEEPINFRA_BASE_URL",
    "OLLAMA_HOST",
    "HARNESS_TEST_OLLAMA_MODEL",
    "HARNESS_TEST_OLLAMA_GEMMA_MODEL",
    "HARNESS_TEST_OLLAMA_QWEN_MODEL",
    "HARNESS_TEST_SAVED_PROFILE_ALIAS",
    "HARNESS_TEST_SAVED_PROFILE_ROUTE",
    "HARNESS_TEST_SAVED_PROFILE_MODEL",
    "HARNESS_TEST_SAVED_PROFILE_BASE_URL",
    "HARNESS_TEST_SAVED_PROFILE_API_KEY",
    "HARNESS_RUN_LIVE_PROVIDER_TESTS",
    "HARNESS_RUN_AUTH_ADAPTER_TESTS",
    "HARNESS_TEST_OPENAI_MODEL",
    "HARNESS_TEST_ANTHROPIC_MODEL",
    "HARNESS_TEST_CLAUDE_CODE_MODEL",
    "HARNESS_TEST_CODEX_MODEL",
    "HARNESS_TEST_ZAI_MODEL",
    "HARNESS_TEST_MINIMAX_MODEL",
    "HARNESS_TEST_GROQ_MODEL",
    "HARNESS_TEST_QWEN_API_MODEL",
    "HARNESS_TEST_DEEPINFRA_MODEL",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FreeformSection {
    Anthropic,
    OpenAi,
    ZAi,
    MiniMax,
    Groq,
    Qwen,
    DeepInfra,
}

pub fn load_workspace_env(workspace_root: &Path) -> Result<Vec<String>, String> {
    let mut loaded = Vec::new();
    for path in env_candidates(workspace_root) {
        if !path.is_file() {
            continue;
        }
        let values = parse_env_file(&path)?;
        for (key, value) in values {
            if value.trim().is_empty() || std::env::var(&key).is_ok() {
                continue;
            }
            std::env::set_var(&key, value);
            loaded.push(key);
        }
    }
    loaded.sort();
    loaded.dedup();
    Ok(loaded)
}

fn env_candidates(workspace_root: &Path) -> Vec<PathBuf> {
    vec![workspace_root.join(".env"), workspace_root.join(".env.local")]
}

fn parse_env_file(path: &Path) -> Result<HashMap<String, String>, String> {
    let contents = fs::read_to_string(path).map_err(|err| err.to_string())?;
    let mut values = HashMap::new();
    let mut section = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((key, value)) = parse_standard_assignment(line) {
            if KNOWN_KEYS.contains(&key.as_str()) && !value.is_empty() {
                values.entry(key).or_insert(value);
            }
            continue;
        }

        section = detect_section(line).or(section);

        if let Some(url) = normalize_url(line) {
            match section {
                Some(FreeformSection::ZAi) => {
                    values.entry("ZAI_BASE_URL".to_string()).or_insert(url);
                }
                Some(FreeformSection::Qwen) => {
                    values.entry("QWEN_BASE_URL".to_string()).or_insert(url);
                }
                Some(FreeformSection::MiniMax) => {
                    values.entry("MINIMAX_BASE_URL".to_string()).or_insert(url);
                }
                Some(FreeformSection::Groq) => {
                    values.entry("GROQ_BASE_URL".to_string()).or_insert(url);
                }
                Some(FreeformSection::DeepInfra) => {
                    values.entry("DEEPINFRA_BASE_URL".to_string()).or_insert(url);
                }
                _ => {}
            }
            continue;
        }

        if let Some((key, value)) = infer_freeform_value(section, line) {
            values.entry(key.to_string()).or_insert(value.to_string());
        }
    }

    values
        .entry("GROQ_BASE_URL".to_string())
        .or_insert_with(|| "https://api.groq.com/openai/v1".to_string());
    values
        .entry("MINIMAX_BASE_URL".to_string())
        .or_insert_with(|| "https://api.minimax.io/v1".to_string());
    values
        .entry("DEEPINFRA_BASE_URL".to_string())
        .or_insert_with(|| "https://api.deepinfra.com/v1/openai".to_string());

    if let Some(base_url) = values.get("QWEN_BASE_URL").cloned() {
        values.insert("QWEN_BASE_URL".to_string(), strip_chat_completions(&base_url));
    }
    if let Some(base_url) = values.get("OPENAI_BASE_URL").cloned() {
        values.insert("OPENAI_BASE_URL".to_string(), strip_chat_completions(&base_url));
    }

    Ok(values)
}

fn parse_standard_assignment(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if !key
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
    {
        return None;
    }
    Some((key.to_string(), value.trim().to_string()))
}

fn detect_section(line: &str) -> Option<FreeformSection> {
    let lower = line.to_ascii_lowercase();
    if lower == "claude" {
        return Some(FreeformSection::Anthropic);
    }
    if lower == "open-ai" || lower == "openai" {
        return Some(FreeformSection::OpenAi);
    }
    if lower.contains("z.ai") && lower.contains("api") {
        return Some(FreeformSection::ZAi);
    }
    if lower.contains("minimax") && lower.contains("api") {
        return Some(FreeformSection::MiniMax);
    }
    if lower.contains("groq") && lower.contains("api") {
        return Some(FreeformSection::Groq);
    }
    if lower.contains("qwen") && lower.contains("api") {
        return Some(FreeformSection::Qwen);
    }
    if lower.contains("deepinfra") && lower.contains("api") {
        return Some(FreeformSection::DeepInfra);
    }
    None
}

fn normalize_url(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return None;
    }
    Some(strip_chat_completions(trimmed))
}

fn strip_chat_completions(url: &str) -> String {
    url.trim_end_matches("/chat/completions")
        .trim_end_matches('/')
        .to_string()
}

fn infer_freeform_value(section: Option<FreeformSection>, line: &str) -> Option<(&'static str, &str)> {
    let trimmed = line.trim();
    if trimmed.contains(' ') {
        return None;
    }

    match section {
        Some(FreeformSection::Anthropic) if trimmed.starts_with("sk-ant-") => {
            Some(("ANTHROPIC_API_KEY", trimmed))
        }
        Some(FreeformSection::OpenAi)
            if trimmed.starts_with("sk-proj-") || trimmed.starts_with("sk-") =>
        {
            Some(("OPENAI_API_KEY", trimmed))
        }
        Some(FreeformSection::ZAi) if trimmed.contains('.') && trimmed.len() > 20 => {
            Some(("ZAI_API_KEY", trimmed))
        }
        Some(FreeformSection::MiniMax) if trimmed.starts_with("sk-") => {
            Some(("MINIMAX_API_KEY", trimmed))
        }
        Some(FreeformSection::Groq) if trimmed.starts_with("gsk_") => {
            Some(("GROQ_API_KEY", trimmed))
        }
        Some(FreeformSection::Qwen)
            if trimmed.starts_with("sk-or-v1-") || trimmed.starts_with("sk-") =>
        {
            Some(("QWEN_API_KEY", trimmed))
        }
        Some(FreeformSection::DeepInfra)
            if trimmed.len() >= 20
                && trimmed
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') =>
        {
            Some(("DEEPINFRA_API_KEY", trimmed))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::parse_env_file;

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
    fn parses_standard_assignments_and_freeform_notes() {
        let workspace = temp_workspace("envfile-parse");
        let env_path = workspace.join(".env");
        fs::write(
            &env_path,
            r#"
OPENAI_BASE_URL=https://api.openai.com/v1

claude
sk-ant-test

open-ai
sk-proj-test

• Z.AI API key와 쓸 base URL
https://api.z.ai/api/coding/paas/v4
zai.secret.value

• MiniMax API key
sk-cp-minimax

• Groq API key
gsk_demo

• Qwen API key
https://openrouter.ai/api/v1/chat/completions
sk-or-v1-demo

• DeepInfra API key
https://api.deepinfra.com/v1/openai
KIGa6N9N38UFeOj8L8kftPjzxOEsxhGB
"#,
        )
        .unwrap();

        let parsed = parse_env_file(&env_path).unwrap();
        assert_eq!(
            parsed.get("OPENAI_BASE_URL").map(String::as_str),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            parsed.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-ant-test")
        );
        assert_eq!(
            parsed.get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-proj-test")
        );
        assert_eq!(
            parsed.get("ZAI_BASE_URL").map(String::as_str),
            Some("https://api.z.ai/api/coding/paas/v4")
        );
        assert_eq!(
            parsed.get("QWEN_BASE_URL").map(String::as_str),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(
            parsed.get("MINIMAX_BASE_URL").map(String::as_str),
            Some("https://api.minimax.io/v1")
        );
        assert_eq!(
            parsed.get("DEEPINFRA_BASE_URL").map(String::as_str),
            Some("https://api.deepinfra.com/v1/openai")
        );
        assert_eq!(
            parsed.get("DEEPINFRA_API_KEY").map(String::as_str),
            Some("KIGa6N9N38UFeOj8L8kftPjzxOEsxhGB")
        );
        cleanup(&workspace);
    }
}
