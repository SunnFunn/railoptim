#!/bin/bash


# Определяем абсолютный путь к директории, где лежит САМ скрипт
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
UNIT_PATH="$SCRIPT_DIR/systemd"
SYSTEMD_PATH="/etc/systemd/system"

echo "Установка симлинков из $UNIT_PATH..."

# -f перезапишет старые ссылки, если они были
sudo ln -sf "$UNIT_PATH/rzd-conventions.service" "$SYSTEMD_PATH/"
sudo ln -sf "$UNIT_PATH/rzd-conventions.timer" "$SYSTEMD_PATH/"

sudo systemctl daemon-reload

# Включаем и запускаем таймеры
sudo systemctl enable rzd-conventions.timer
sudo systemctl start rzd-conventions.timer

echo "--------------------------------------------------"
echo "Установка завершена успешно."
echo "Таймер активен:"
systemctl list-timers rzd-conventions*
