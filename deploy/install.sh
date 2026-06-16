#!/bin/bash
# Единый установщик railoptim.
#
# Использование:
#   ./deploy/install.sh web     — frontend + бинарник railoptim-web + сервис (restart)
#   ./deploy/install.sh optim   — бинарник railoptim + service/.timer (enable --now timer)
#   ./deploy/install.sh all     — web + optim
#
# Опции через env:
#   REBUILD_WEB_UI=1  — пересобрать web-ui/dist через npm (нужен Node.js)
#
# Бинарники собираются в release и кладутся в app/bin/. Unit-файлы ставятся
# симлинком на репозиторий (правки в IDE сразу на месте).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$REPO_ROOT/app/bin"
UNIT_PATH="$SCRIPT_DIR/systemd"
SYSTEMD_PATH="/etc/systemd/system"
DIST="$REPO_ROOT/web-ui/dist"
WEB_SERVICE="railoptim-web"

usage() {
  cat >&2 <<EOF
railoptim установщик — выберите режим:

  ./deploy/install.sh web     frontend + railoptim-web (long-running сервис)
  ./deploy/install.sh optim   batch railoptim + суточный timer (oneshot)
  ./deploy/install.sh all     всё сразу

env: REBUILD_WEB_UI=1 — пересборка web-ui/dist через npm
EOF
}

MODE="${1:-}"
do_web=false
do_optim=false
case "$MODE" in
  web)   do_web=true ;;
  optim) do_optim=true ;;
  all)   do_web=true; do_optim=true ;;
  *)     usage; exit 1 ;;
esac

echo "Корень репозитория: $REPO_ROOT"
echo "Режим установки:    $MODE"
echo ""

# --- Вспомогательные функции ---

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

ensure_frontend() {
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
    echo "На этой (оффлайн) машине: git pull и снова ./deploy/install.sh $MODE" >&2
    exit 1
  fi

  if [ ! -f "$DIST/index.html" ]; then
    echo "ERROR: после сборки нет $DIST/index.html" >&2
    exit 1
  fi
}

install_bin() {
  install -m 755 "$REPO_ROOT/target/release/$1" "$BIN_DIR/$1"
}

link_unit() {
  sudo ln -sf "$UNIT_PATH/$1" "$SYSTEMD_PATH/"
}

# --- 1. Frontend (только web) ---
if $do_web; then
  ensure_frontend
  echo ""
fi

# --- 2. Сборка бинарников (одним вызовом cargo) ---
BIN_FLAGS=()
$do_web   && BIN_FLAGS+=(--bin "$WEB_SERVICE")
$do_optim && BIN_FLAGS+=(--bin railoptim)

echo "==> Сборка release-бинарников: ${BIN_FLAGS[*]}"
(cd "$REPO_ROOT" && cargo build --release "${BIN_FLAGS[@]}")

echo ""
echo "==> Установка в $BIN_DIR/"
mkdir -p "$BIN_DIR"
$do_web   && install_bin "$WEB_SERVICE"
$do_optim && install_bin railoptim

# --- 3. systemd unit'ы ---
echo ""
echo "==> Установка симлинков unit'ов в $SYSTEMD_PATH/"
if $do_web; then
  chmod +x "$SCRIPT_DIR/start_web.sh"
  link_unit "$WEB_SERVICE.service"
fi
if $do_optim; then
  link_unit "railoptim.service"
  link_unit "railoptim.timer"
fi

echo ""
echo "==> systemctl daemon-reload"
sudo systemctl daemon-reload

# --- 4. Включение / запуск ---
if $do_web; then
  echo ""
  echo "==> enable + restart $WEB_SERVICE"
  sudo systemctl enable "$WEB_SERVICE"
  sudo systemctl restart "$WEB_SERVICE"
fi
if $do_optim; then
  echo ""
  echo "==> enable --now railoptim.timer (service запускается таймером)"
  sudo systemctl enable --now railoptim.timer
fi

# --- Итог ---
echo ""
echo "--------------------------------------------------"
echo "Установка завершена ($MODE)."
if $do_web; then
  echo "  app/bin/railoptim-web  — web API + SPA"
  echo "  Проверка:"
  echo "    curl -s http://127.0.0.1:8080/health"
  echo "    curl -s -o /dev/null -w 'SPA HTTP %{http_code}\n' http://127.0.0.1:8080/"
  systemctl status "$WEB_SERVICE" --no-pager || true
fi
if $do_optim; then
  echo "  app/bin/railoptim      — batch-оптимизация (run.sh prod), запуск раз в сутки"
  echo "  Проверка таймера:"
  echo "    systemctl list-timers railoptim*"
  echo "    systemctl status railoptim.timer --no-pager"
  echo "    journalctl -u railoptim.service -n 50 --no-pager"
  systemctl list-timers 'railoptim*' --no-pager || true
fi
