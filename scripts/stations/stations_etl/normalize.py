"""Нормализация кодов ЕСР-6 и контрольная цифра (паритет с src/data/esr.rs)."""

from __future__ import annotations


def normalize_esr6(raw: str | int | None) -> str:
    """Цифровой код → 6 знаков с ведущими нулями; иначе trim исходной строки."""
    if raw is None:
        return ""
    if isinstance(raw, int):
        if raw < 0:
            return ""
        return f"{raw:06d}"
    t = str(raw).strip()
    if not t:
        return ""
    if t.isdigit():
        try:
            return f"{int(t):06d}"
        except ValueError:
            return t
    return t


def validate_esr6_checksum(code: str) -> bool:
    """Проверка 6-значного ЕСР по контрольной цифре (алгоритм ТР4 / RZD)."""
    c = normalize_esr6(code)
    if len(c) != 6 or not c.isdigit():
        return False
    digits = [int(x) for x in c]

    def check(offset: int) -> int | None:
        total = sum(d * (i + offset) for i, d in enumerate(digits[:5]))
        rem = total % 11
        if rem == 10:
            return None
        return rem

    rem = check(1)
    if rem is None:
        rem = check(3)
    if rem is None:
        return digits[5] == 0
    return rem == digits[5]
