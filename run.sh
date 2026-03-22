#!/bin/bash
set -euo pipefail

# dir=$(pwd)
dir="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
cd "$dir" # Чтобы все относительные пути внутри Rust работали корректно

##### Проверка на праздники #############################################
TODAY=$(date +%Y-%m-%d)
if [ -f "$dir/holidays.txt" ]; then
    if grep -q "^$TODAY" "$dir/holidays.txt"; then
        echo "[$(date)] Сегодня праздник согласно holidays.txt. Пропускаю запуск railoptim."
        exit 0
    fi
fi
########################################################################

# Блокируем внешние запросы CLI (ускоряет таймауты до 0мс)
export http_proxy="http://127.0.0.1:1"
export https_proxy="http://127.0.0.1:1"
export all_proxy="http://127.0.0.1:1"
# Разрешаем только локальный трафик до сервера Infisical
export no_proxy="localhost,127.0.0.1,0.0.0.0,10.10.100.238,10.10.101.183"


# Ищем токен в User Keyring (@u), который доступен всем процессам пользователя atretyakov
TOKEN_REF=$(keyctl search @u user infisical_optim_token 2>/dev/null)

if [ -z "$TOKEN_REF" ]; then
    echo "Ошибка: Токен не найден в User Keyring (@u). Запустите auth-infisical.sh."
    exit 1
fi


# --- ПАРАМЕТРЫ ЗАПУСКА ---
ENV=${1:-"dev"} # По умолчанию dev (для ручного запуска)
PROJECT_ID="a28f09d6-1840-4ac3-ad90-f8c9464facef"

export APP_ENV="$ENV"

# --- ВЫБОР БИНАРНИКА ---
if [ "$ENV" == "prod" ]; then
    BINARY=("$dir/app/bin/railoptim")
else
    BINARY=("$dir/target/release/railoptim")
fi

# Настройки среды
export INFISICAL_TOKEN=$(keyctl pipe "$TOKEN_REF")
export INFISICAL_TELEMETRY_OFF=true
export INFISICAL_CHECK_UPDATE=false
export INFISICAL_DISABLE_UPDATE_CHECK=true
export INFISICAL_API_URL="http://127.0.0.1:9000"

echo "--- Начинаю загрузку секретов из Infisical (засекаем время) ---"
start_time=$(date +%s)

# Загружаем секреты проекта в переменные окружения текущего процесса
eval $(infisical secrets --env "$ENV" --path / --recursive --projectId "$PROJECT_ID" --output dotenv | sed "s/=\(.*\)/='\1'/;s/^/export /")

# Очистка токена Infisical (он больше не нужен)
unset INFISICAL_TOKEN

end_time=$(date +%s)
echo "Загрузка заняла: $((end_time - start_time)) сек."

LOG_DIR="$dir/logs"
mkdir -p "$LOG_DIR"
LOG_FILE="$LOG_DIR/railoptim_$(date +%F).log"

# --- ЗАПУСК СЕРВИСА (RUST) ---
echo "[$(date)] Запуск: ${BINARY[*]} (env=$ENV)" | tee -a "$LOG_FILE"

if "${BINARY[@]}" &>> "$LOG_FILE"; then
    echo "[SUCCESS] railoptim завершил работу успешно." | tee -a "$LOG_FILE"
else
    echo "[ERROR] railoptim завершился с ошибкой!" | tee -a "$LOG_FILE"
    exit 1
fi
