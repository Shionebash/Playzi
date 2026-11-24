#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)

CARGO_TOML="$REPO_ROOT/Cargo.toml"
TAURI_CONF="$REPO_ROOT/crates/playzi-desktop/tauri.conf.json"
CARGO_LOCK="$REPO_ROOT/Cargo.lock"

ARG="${1:-}"

CURRENT_VERSION=$(awk '
  /^\[workspace\.package\]/ { in_ws = 1; next }
  /^\[/ { in_ws = 0 }
  in_ws && /^version[[:space:]]*=/ {
    gsub(/.*"|".*/, "", $0)
    print $0
    exit
  }
' "$CARGO_TOML")

if [ -z "$CURRENT_VERSION" ]; then
  echo "Error: Could not find [workspace.package] version in $CARGO_TOML" >&2
  exit 1
fi

OLD_IFS=$IFS
IFS=.
set -- $CURRENT_VERSION
CUR_MAJOR=$1
CUR_MINOR=$2
CUR_PATCH=$3
IFS=$OLD_IFS

case "$ARG" in
  ""|"--patch")
    NEW_VERSION="$CUR_MAJOR.$CUR_MINOR.$((CUR_PATCH + 1))"
    ;;
  "--minor")
    NEW_VERSION="$CUR_MAJOR.$((CUR_MINOR + 1)).0"
    ;;
  "--major")
    NEW_VERSION="$((CUR_MAJOR + 1)).0.0"
    ;;
  *)
    CLEANED=$(printf '%s' "$ARG" | sed 's/^[vV]//')
    if ! printf '%s' "$CLEANED" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
      echo "Error: Invalid version '$ARG'. Expected X.Y.Z (e.g. 1.2.0)." >&2
      exit 1
    fi
    NEW_VERSION="$CLEANED"
    ;;
esac

# Update Cargo.toml
awk -v new_ver="$NEW_VERSION" '
  /^\[workspace\.package\]/ { in_ws = 1 }
  /^\[/ && !/^\[workspace\.package\]/ { in_ws = 0 }
  in_ws && !done && /^version[[:space:]]*=/ {
    sub(/"[0-9]+\.[0-9]+\.[0-9]+"/, "\"" new_ver "\"")
    done = 1
  }
  { print }
' "$CARGO_TOML" > "$CARGO_TOML.tmp" && mv "$CARGO_TOML.tmp" "$CARGO_TOML"

# Update crates/playzi-desktop/tauri.conf.json
if [ -f "$TAURI_CONF" ]; then
  awk -v new_ver="$NEW_VERSION" '
    !done && /"version"[[:space:]]*:/ {
      sub(/"[0-9]+\.[0-9]+\.[0-9]+"/, "\"" new_ver "\"")
      done = 1
    }
    { print }
  ' "$TAURI_CONF" > "$TAURI_CONF.tmp" && mv "$TAURI_CONF.tmp" "$TAURI_CONF"
fi

# Update Cargo.lock for playzi-core and playzi-desktop
if [ -f "$CARGO_LOCK" ]; then
  awk -v new_ver="$NEW_VERSION" '
    /^name = "playzi-core"$/ || /^name = "playzi-desktop"$/ { target = 1; print; next }
    target && /^version = "/ {
      sub(/"[0-9]+\.[0-9]+\.[0-9]+"/, "\"" new_ver "\"")
      target = 0
    }
    { print }
  ' "$CARGO_LOCK" > "$CARGO_LOCK.tmp" && mv "$CARGO_LOCK.tmp" "$CARGO_LOCK"
fi

echo "Playzi version updated: $CURRENT_VERSION -> $NEW_VERSION"
