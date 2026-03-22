#!/bin/bash
set -euo pipefail

echo "--- Авторизация railoptim в Infisical ---"

# Вводим токен (скрыто, не попадает в history)
read -s -p "Введите Infisical Service Token: " service_token
echo ""

# Очищаем старый ключ из User Keyring, если он там был
OLD_KEY=$(keyctl search @u user infisical_optim_token 2>/dev/null || true)
if [ -n "$OLD_KEY" ]; then
    keyctl unlink "$OLD_KEY" @u
fi

# Записываем новый токен и выставляем права доступа
# printf '%s' безопаснее echo -n: не интерпретирует escape-последовательности и не печатает '-n' на старых системах
TOKEN_REF=$(printf '%s' "$service_token" | keyctl padd user infisical_optim_token @u)
keyctl setperm "$TOKEN_REF" 0x3f3f0000

# Затираем переменную с токеном из памяти процесса
unset service_token

echo "Токен сохранен в User Keyring (ID: $TOKEN_REF). systemd теперь его увидит."