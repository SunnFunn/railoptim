#!/bin/bash
# Сборка prod-бинарников → app/bin/ + установка systemd unit симлинком.
# Frontend: готовый web-ui/dist из git (оффлайн prod без npm).
# Опционально: REBUILD_WEB_UI=1 и npm на машине — пересобрать dist локально.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$REPO_ROOT/app/bin"
UNIT_PATH="$SCRIPT_DIR/systemd"
SYSTEMD_PATH="/etc/systemd/system"
SERVICE_NAME="railoptim-web"
DIST="$REPO_ROOT/web-ui/dist"

echo "Корень репозитория: $REPO_ROOT"
echo ""

build_frontend() {
  (
    cd "$REPO_ROOT/web-ui"
    if [ -f package-lock.json ]; then
      npm ci
    else
      npm install
    fi
    npm run build
  )
}

echo "==> Frontend (web-ui/dist)..."
if [ "${REBUILD_WEB_UI:-0}" = "1" ]; then
  if ! command -v npm >/dev/null 2>&1; then
    echo "ERROR: REBUILD_WEB_UI=1, но npm не установлен" >&2
    exit 1
  fi
  echo "    пересборка (REBUILD_WEB_UI=1)..."
  build_frontend
elif [ -f "$DIST/index.html" ]; then
  if [ -f "$DIST/build-info.json" ]; then
    echo "    готовый dist из репозитория ($(cat "$DIST/build-info.json" | tr -d '\n'))"
  else
    echo "    готовый dist из репозитория ($DIST)"
  fi
elif command -v npm >/dev/null 2>&1; then
  echo "    dist не найден — сборка через npm..."
  build_frontend
else
  echo "ERROR: нет web-ui/dist/index.html и npm не установлен." >&2
  echo "" >&2
  echo "На машине с Node.js выполните:" >&2
  echo "  ./scripts/build_web_ui.sh" >&2
  echo "  git add web-ui/dist && git commit && git push" >&2
  echo "" >&2
  echo "На этой (оффлайн) машине: git pull и снова ./deploy/install_web_service.sh" >&2
  exit 1
fi

if [ ! -f "$DIST/index.html" ]; then
  echo "ERROR: после сборки нет $DIST/index.html" >&2
  exit 1
fi

echo ""
echo "==> Сборка release-бинарников (cargo)..."
(cd "$REPO_ROOT" && cargo build --release --bin railoptim --bin "$SERVICE_NAME")

echo ""
echo "==> Установка в $BIN_DIR/"
mkdir -p "$BIN_DIR"
install -m 755 "$REPO_ROOT/target/release/railoptim" "$BIN_DIR/railoptim"
install -m 755 "$REPO_ROOT/target/release/$SERVICE_NAME" "$BIN_DIR/$SERVICE_NAME"

echo ""
echo "==> Права на deploy/start_web.sh..."
chmod +x "$SCRIPT_DIR/start_web.sh"

echo ""
echo "==> Установка симлинка $UNIT_PATH/$SERVICE_NAME.service → $SYSTEMD_PATH/"
sudo ln -sf "$UNIT_PATH/$SERVICE_NAME.service" "$SYSTEMD_PATH/"

echo ""
echo "==> systemctl daemon-reload"
sudo systemctl daemon-reload

echo ""
echo "==> enable + restart $SERVICE_NAME"
sudo systemctl enable "$SERVICE_NAME"
sudo systemctl restart "$SERVICE_NAME"

echo ""
echo "--------------------------------------------------"
echo "Установка завершена."
echo "  app/bin/railoptim      — batch-оптимизация (run.sh prod)"
echo "  app/bin/railoptim-web  — web API + SPA (web-ui/dist)"
echo ""
systemctl status "$SERVICE_NAME" --no-pager || true
echo ""
echo "Проверка:"
echo "  curl -s http://127.0.0.1:8080/health"
echo "  curl -s -o /dev/null -w 'SPA HTTP %{http_code}\n' http://127.0.0.1:8080/"
echo ""
echo "Unit: $UNIT_PATH/$SERVICE_NAME.service"
echo "После правок: sudo systemctl daemon-reload && sudo systemctl restart $SERVICE_NAME"
