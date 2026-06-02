#!/usr/bin/env bash

package_metadata_workspace_version() {
  local root_cargo_file="$1"
  if command -v toml >/dev/null 2>&1; then
    toml get "${root_cargo_file}" workspace.package.version | sed 's/"//g'
    return
  fi

  awk '
    /^\[workspace\.package\]$/ { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && $1 == "version" {
      gsub(/"/, "", $3)
      print $3
      exit
    }
  ' "${root_cargo_file}"
}

package_metadata_cli_author() {
  local cli_cargo_file="$1"
  if command -v toml >/dev/null 2>&1; then
    toml get "${cli_cargo_file}" 'package.authors[0]' | sed 's/"//g'
    return
  fi

  sed -nE 's/^authors = \["([^"]+)".*/\1/p' "${cli_cargo_file}" | head -n 1
}

package_metadata_cli_description() {
  local cli_cargo_file="$1"
  if command -v toml >/dev/null 2>&1; then
    toml get "${cli_cargo_file}" package.description | sed 's/"//g'
    return
  fi

  sed -nE 's/^description = "([^"]+)".*/\1/p' "${cli_cargo_file}" | head -n 1
}
