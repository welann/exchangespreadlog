#!/bin/sh
set -eu

CONFIG_PATH="${GENERATED_CONFIG_PATH:-/app/config.generated.toml}"

uv run python scripts/generate_config_from_lighter.py --output "$CONFIG_PATH"

exec exchangespreadlog --config "$CONFIG_PATH" "$@"
