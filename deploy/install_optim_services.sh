#!/bin/bash
# Установка systemd timer + service для суточного запуска batch-оптимизации railoptim.
# Бинарник app/bin/railoptim собирается отдельно (см. deploy/install_web_service.sh).
set -euo pipefail

# Определяем абсолютный путь к директории, где лежит САМ скрипт
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
UNIT_PATH="$SCRIPT_DIR/systemd"
SYSTEMD_PATH="/etc/systemd/system"

echo "Установка симлинков из $UNIT_PATH..."

# -f перезапишет старые ссылки, если они были
sudo ln -sf "$UNIT_PATH/railoptim.service" "$SYSTEMD_PATH/"
sudo ln -sf "$UNIT_PATH/railoptim.timer" "$SYSTEMD_PATH/"

sudo systemctl daemon-reload

# Включаем и запускаем таймер (service запускается таймером, не enable'им отдельно)
sudo systemctl enable --now railoptim.timer

echo "--------------------------------------------------"
echo "Установка завершена успешно."
echo "Таймер активен:"
systemctl list-timers railoptim*
