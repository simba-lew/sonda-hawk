#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd -- "$project_dir"

if [[ ! -x "$project_dir/.venv/bin/python" ]]; then
    echo 'Najpierw wykonaj w katalogu projektu:' >&2
    echo '  python3 -m venv .venv' >&2
    echo '  .venv/bin/python -m pip install -r scripts/requirements-panel.txt' >&2
    exit 1
fi

exec "$project_dir/.venv/bin/python" "$project_dir/scripts/panel.py" "$@"
