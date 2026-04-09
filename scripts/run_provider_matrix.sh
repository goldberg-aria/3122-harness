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
export HARNESS_TEST_CODEX_MODEL="${HARNESS_TEST_CODEX_MODEL:-o3}"
export HARNESS_TEST_OLLAMA_QWEN_MODEL="${HARNESS_TEST_OLLAMA_QWEN_MODEL:-$(pick_ollama_model "qwen")}"
export HARNESS_TEST_OLLAMA_GEMMA_MODEL="${HARNESS_TEST_OLLAMA_GEMMA_MODEL:-$(pick_ollama_model "gemma")}"
export HARNESS_TEST_OLLAMA_MODEL="${HARNESS_TEST_OLLAMA_MODEL:-${HARNESS_TEST_OLLAMA_QWEN_MODEL:-}}"

echo "syncing env-backed provider profiles"
cargo run -p cli -- providers sync-env

echo
echo "matrix configuration"
echo "- anthropic model: ${HARNESS_TEST_ANTHROPIC_MODEL:-"-"}"
echo "- openai model: ${HARNESS_TEST_OPENAI_MODEL:-"-"}"
echo "- ollama qwen model: ${HARNESS_TEST_OLLAMA_QWEN_MODEL:-"-"}"
echo "- ollama gemma model: ${HARNESS_TEST_OLLAMA_GEMMA_MODEL:-"-"}"
echo "- saved profile route: ${HARNESS_TEST_SAVED_PROFILE_ROUTE:-"-"}"
echo "- saved profile model: ${HARNESS_TEST_SAVED_PROFILE_MODEL:-"-"}"
echo "- auth adapter tests: ${HARNESS_RUN_AUTH_ADAPTER_TESTS:-0}"
echo "- anthropic key: $(mask "${ANTHROPIC_API_KEY:-}")"
echo "- openai key: $(mask "${OPENAI_API_KEY:-}")"
echo "- zai key: $(mask "${ZAI_API_KEY:-}")"
echo "- minimax key: $(mask "${MINIMAX_API_KEY:-}")"
echo "- groq key: $(mask "${GROQ_API_KEY:-}")"
echo "- qwen key: $(mask "${QWEN_API_KEY:-}")"

echo
echo "running cargo test --workspace"
cargo test --workspace
