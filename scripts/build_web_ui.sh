#!/bin/bash
# Сборка web-ui/dist на машине с Node.js/npm для коммита в GitHub.
# Оффлайн prod подтягивает готовый dist без npm.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UI="$ROOT/web-ui"

if ! command -v npm >/dev/null 2>&1; then
  echo "npm не найден. Установите Node.js (nvm/homebrew) на этой машине." >&2
  exit 1
fi

echo "==> map assets (style, css, glyphs)"
"$ROOT/scripts/map/copy_map_assets.sh"

echo "==> web-ui: npm ci + build"
cd "$UI"
if [ -f package-lock.json ]; then
  npm ci
else
  npm install
fi
npm run build

if [ ! -f dist/index.html ]; then
  echo "Ошибка: dist/index.html не создан" >&2
  exit 1
fi

GIT_REV=""
if command -v git >/dev/null 2>&1 && git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  GIT_REV="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || true)"
fi

cat > dist/build-info.json <<EOF
{
  "built_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "node": "$(node -v)",
  "npm": "$(npm -v)",
  "git_rev": "${GIT_REV}"
}
EOF

echo ""
echo "Сборка завершена: $UI/dist"
echo ""
echo "Дальше закоммитьте и отправьте в GitHub:"
echo "  git add web-ui/dist web-ui/package-lock.json data/map/"
echo "  git commit -m \"build(web-ui): обновить dist для оффлайн prod\""
echo "  git push"
echo ""
echo "На оффлайн prod: git pull && ./deploy/install_web_service.sh"
