#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

mask() {
  local value="${1:-}"
  if [[ -z "$value" ]]; then
    printf '%s' "-"
    return
  fi
  local length=${#value}
  if (( length <= 8 )); then
    printf '********'
    return
  fi
  printf '%s…%s' "${value:0:4}" "${value: -4}"
}

pick_ollama_model() {
  local pattern="$1"
  if ! command -v ollama >/dev/null 2>&1; then
    return 0
  fi
  ollama list 2>/dev/null | awk -v pattern="$pattern" 'tolower($1) ~ pattern { print $1; exit }'
}

export HARNESS_RUN_LIVE_PROVIDER_TESTS="${HARNESS_RUN_LIVE_PROVIDER_TESTS:-1}"
if command -v claude >/dev/null 2>&1 && command -v codex >/dev/null 2>&1; then
  export HARNESS_RUN_AUTH_ADAPTER_TESTS="${HARNESS_RUN_AUTH_ADAPTER_TESTS:-1}"
fi

export HARNESS_TEST_OPENAI_MODEL="${HARNESS_TEST_OPENAI_MODEL:-gpt-4.1-mini}"
export HARNESS_TEST_ANTHROPIC_MODEL="${HARNESS_TEST_ANTHROPIC_MODEL:-claude-sonnet-4-6}"
export HARNESS_TEST_CLAUDE_CODE_MODEL="${HARNESS_TEST_CLAUDE_CODE_MODEL:-sonnet}"
export HARNESS_TEST_CODEX_MODEL="${HARNESS_TEST_CODEX_MODEL:-gpt-5}"
export HARNESS_TEST_ZAI_MODEL="${HARNESS_TEST_ZAI_MODEL:-5.1}"
export HARNESS_TEST_MINIMAX_MODEL="${HARNESS_TEST_MINIMAX_MODEL:-2.7}"
export HARNESS_TEST_GROQ_MODEL="${HARNESS_TEST_GROQ_MODEL:-openai/gpt-oss-20b}"
export HARNESS_TEST_QWEN_API_MODEL="${HARNESS_TEST_QWEN_API_MODEL:-qwen/qwen3.6-plus}"
export HARNESS_TEST_DEEPINFRA_MODEL="${HARNESS_TEST_DEEPINFRA_MODEL:-nvidia/Nemotron-3-Nano-30B-A3B}"
export HARNESS_TEST_OLLAMA_QWEN_MODEL="${HARNESS_TEST_OLLAMA_QWEN_MODEL:-$(pick_ollama_model "qwen")}"
export HARNESS_TEST_OLLAMA_GEMMA_MODEL="${HARNESS_TEST_OLLAMA_GEMMA_MODEL:-$(pick_ollama_model "gemma")}"
export HARNESS_TEST_OLLAMA_MODEL="${HARNESS_TEST_OLLAMA_MODEL:-${HARNESS_TEST_OLLAMA_QWEN_MODEL:-}}"

echo "syncing env-backed provider profiles"
cargo run -p cli -- providers sync-env

echo
echo "matrix configuration"
echo "- anthropic model: ${HARNESS_TEST_ANTHROPIC_MODEL:-"-"}"
echo "- openai model: ${HARNESS_TEST_OPENAI_MODEL:-"-"}"
echo "- zai model: ${HARNESS_TEST_ZAI_MODEL:-"-"}"
echo "- minimax model: ${HARNESS_TEST_MINIMAX_MODEL:-"-"}"
echo "- groq model: ${HARNESS_TEST_GROQ_MODEL:-"-"}"
echo "- qwen api model: ${HARNESS_TEST_QWEN_API_MODEL:-"-"}"
echo "- deepinfra model: ${HARNESS_TEST_DEEPINFRA_MODEL:-"-"}"
echo "- ollama qwen model: ${HARNESS_TEST_OLLAMA_QWEN_MODEL:-"-"}"
echo "- ollama gemma model: ${HARNESS_TEST_OLLAMA_GEMMA_MODEL:-"-"}"
echo "- auth adapter tests: ${HARNESS_RUN_AUTH_ADAPTER_TESTS:-0}"
echo "- anthropic key: $(mask "${ANTHROPIC_API_KEY:-}")"
echo "- openai key: $(mask "${OPENAI_API_KEY:-}")"
echo "- zai key: $(mask "${ZAI_API_KEY:-}")"
echo "- minimax key: $(mask "${MINIMAX_API_KEY:-}")"
echo "- groq key: $(mask "${GROQ_API_KEY:-}")"
echo "- qwen key: $(mask "${QWEN_API_KEY:-}")"
echo "- deepinfra key: $(mask "${DEEPINFRA_API_KEY:-}")"

echo
echo "running cargo test --workspace"
cargo test --workspace

profile_field() {
  local alias="$1"
  local field="$2"
  if [[ ! -f ".harness/providers.json" ]]; then
    return 1
  fi
  jq -r --arg alias "$alias" --arg field "$field" \
    '.profiles[]? | select(.alias == $alias) | .[$field] // empty' \
    .harness/providers.json | head -n 1
}

run_saved_profile_smoke() {
  local alias="$1"
  local route="$2"
  local model="$3"
  local base_url
  local api_key

  base_url="$(profile_field "$alias" "base_url")"
  api_key="$(profile_field "$alias" "api_key")"

  if [[ -z "$base_url" || -z "$api_key" ]]; then
    echo "skip saved profile smoke for ${alias}: missing saved profile"
    return
  fi

  export HARNESS_TEST_SAVED_PROFILE_ALIAS="$alias"
  export HARNESS_TEST_SAVED_PROFILE_ROUTE="$route"
  export HARNESS_TEST_SAVED_PROFILE_MODEL="$model"
  export HARNESS_TEST_SAVED_PROFILE_BASE_URL="$base_url"
  export HARNESS_TEST_SAVED_PROFILE_API_KEY="$api_key"

  echo
  echo "saved profile smoke: ${alias} (${model})"
  if ! cargo test -p runtime live_saved_profile_prompt_when_enabled -- --nocapture; then
    FAILED_SAVED_PROFILES+=("${alias}:${model}")
  fi
}

FAILED_SAVED_PROFILES=()
run_saved_profile_smoke "zai" "openai-compat" "${HARNESS_TEST_ZAI_MODEL}"
run_saved_profile_smoke "minimax" "openai-compat" "${HARNESS_TEST_MINIMAX_MODEL}"
run_saved_profile_smoke "groq" "openai-compat" "${HARNESS_TEST_GROQ_MODEL}"
run_saved_profile_smoke "qwen-api" "openai-compat" "${HARNESS_TEST_QWEN_API_MODEL}"
run_saved_profile_smoke "deepinfra" "openai-compat" "${HARNESS_TEST_DEEPINFRA_MODEL}"

if (( ${#FAILED_SAVED_PROFILES[@]} > 0 )); then
  echo
  echo "saved profile smoke failures:"
  for entry in "${FAILED_SAVED_PROFILES[@]}"; do
    echo "- ${entry}"
  done
  exit 1
fi
