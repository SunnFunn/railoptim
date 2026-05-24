#!/bin/bash
# Сборка prod-бинарников → app/bin/ + установка systemd unit симлинком.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$REPO_ROOT/app/bin"
UNIT_PATH="$SCRIPT_DIR/systemd"
SYSTEMD_PATH="/etc/systemd/system"
SERVICE_NAME="railoptim-web"

echo "Корень репозитория: $REPO_ROOT"
echo ""

echo "==> Сборка release-бинарников..."
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
echo "  app/bin/railoptim-web  — web API"
echo ""
systemctl status "$SERVICE_NAME" --no-pager || true
echo ""
echo "Проверка API:"
echo "  curl -s http://127.0.0.1:8080/health"
echo ""
echo "Unit: $UNIT_PATH/$SERVICE_NAME.service"
echo "После правок: sudo systemctl daemon-reload && sudo systemctl restart $SERVICE_NAME"
