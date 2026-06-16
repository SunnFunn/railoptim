#!/bin/bash
# Тонкая обёртка для обратной совместимости: см. deploy/install.sh.
# Эквивалент: ./deploy/install.sh web
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/install.sh" web
