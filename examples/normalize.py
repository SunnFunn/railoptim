"""Нормализация имён станций, дорог и кодов ЕСР для lookup в NSI.Station."""

from __future__ import annotations

import re
from typing import Any

_WHITESPACE = re.compile(r"\s+")
_QUOTES = re.compile(r"[«»\"'`]+")

# Слово «эксп» / «экспорт» в любых регистрах, с точкой или без (не «эксплуатационный»)
_EXPORT_TOKEN = r"(?:эксп(?:орт)?\.?)"

# Опечатки / расхождения реестр ↔ NSI
_KNOWN_TYPOS: dict[str, str] = {
    "койбагор": "койбагар",
    "селинка": "силинка",
}

# Точное совпадение всей строки (реестр диспетчеров)
_STATION_EXACT_ALIASES: dict[str, str] = {
    "сптв": "Санкт-Петербург-Товарный-Московский",
}

# Суффиксы через дефис: сокращения в Excel → полное имя в NSI
_ABBREV_SUFFIX: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"(?i)-тов\.?$"), "-Товарный"),
    (re.compile(r"(?i)-гл\.?$"), "-Главный"),
    (re.compile(r"(?i)-сорт\.?$"), "-Сортировочная"),
    (re.compile(r"(?i)-пасс\.?$"), "-Пассажирский"),
)

# Хвост: римская цифра как отдельное слово (Краснодар I)
_ROMAN_END = (
    (re.compile(r"\s+III\s*$", re.I), " 3"),
    (re.compile(r"\s+II\s*$", re.I), " 2"),
    (re.compile(r"\s+I\s*$", re.I), " 1"),
)

# (эксп. АО …) / (ЭКСПОРТ на КЖД …) → (экспорт …)
_PAREN_EXPORT_PREFIX = re.compile(
    rf"\(\s*{_EXPORT_TOKEN}(\s+на\s+|\s+)",
    re.I,
)

# Только экспорт в скобках: (эксп), (ЭКСП.), ( экспорт )
_PAREN_EXPORT_ONLY = re.compile(
    rf"\(\s*{_EXPORT_TOKEN}\s*\)",
    re.I,
)

# Станция(эксп) без пробела перед скобкой
_ATTACHED_EXPORT_PAREN = re.compile(
    rf"(?i)([а-яёa-z0-9\-])\(\s*{_EXPORT_TOKEN}\s*\)",
)

# В конце строки: «… эксп» / «… экспорт.»
_TRAILING_EXPORT = re.compile(
    rf"(?i)\s+{_EXPORT_TOKEN}\s*$",
)


def normalize_name(raw: Any) -> str:
    """Trim и схлопывание пробелов (как в ячейке / NSI для отображения)."""
    if raw is None:
        return ""
    s = str(raw).replace("\u00a0", " ").strip()
    return _WHITESPACE.sub(" ", s)


def _normalize_export_notation(s: str) -> str:
    """
    Унификация пометок экспорта из реестра диспетчеров.

    Варианты в Excel: эксп / эксп. / Эксп / ЭКСП / экспорт / Экспорт,
    с точкой и без, в скобках и без, «экспорт на КЖД» vs «экспорт КЖД».
    Итог в скобках: префикс «экспорт» (регистр снимается позже casefold).
    """
    if not s:
        return s

    # Пробел перед «(эксп…»
    s = re.sub(r"(?i)([а-яёa-z0-9\-])(\()", r"\1 (", s)

    s = _PAREN_EXPORT_PREFIX.sub("(экспорт ", s)
    s = _PAREN_EXPORT_ONLY.sub("(экспорт)", s)
    s = _ATTACHED_EXPORT_PAREN.sub(r"\1 (экспорт)", s)
    s = _TRAILING_EXPORT.sub(" (экспорт)", s)

    # Точка сразу после «экспорт» внутри текста: «(Экспорт.)» → «(экспорт)»
    s = re.sub(r"(?i)\(экспорт\)\.", "(экспорт)", s)
    s = re.sub(r"(?i)\bэкспорт\.(?=\s|\)|$)", "экспорт", s)

    # «экспорт на КЖД» / «экспорт на ДСВН» ↔ «экспорт КЖД» (в скобках и вне)
    s = re.sub(r"(?i)\(экспорт\s+на\s+", "(экспорт ", s)
    s = re.sub(r"(?i)\bэкспорт\s+на\s+", "экспорт ", s)

    return s


def canonicalize_station_name(raw: Any) -> str:
    """
    Приведение к каноническому виду перед casefold (реестр Excel ↔ NSI).
    """
    s = normalize_name(raw)
    if not s:
        return ""

    s = _QUOTES.sub(" ", s)
    s = _WHITESPACE.sub(" ", s).strip()

    # Код ЕСР в скобках в конце наименования из реестра
    s = re.sub(r"\s*\(\d{6}\)\s*$", "", s)

    s = _normalize_export_notation(s)

    folded_full = s.casefold()
    if folded_full in _STATION_EXACT_ALIASES:
        return _STATION_EXACT_ALIASES[folded_full]

    for pat, repl in _ABBREV_SUFFIX:
        s = pat.sub(repl, s)

    # Г.З.Тагиева-сортировочная → Гаджизейналабдин Тагиев-Сортировка
    s = re.sub(
        r"(?i)г\.?\s*з\.?\s*тагиева",
        "Гаджизейналабдин Тагиев",
        s,
    )
    s = re.sub(
        r"(?i)гаджизейналабдин\s+тагиева",
        "Гаджизейналабдин Тагиев",
        s,
    )
    s = re.sub(r"(?i)\bсортировочная\b", "сортировка", s)

    for pat, repl in _ROMAN_END:
        s = pat.sub(repl, s)

    for old, new in _KNOWN_TYPOS.items():
        s = re.sub(rf"(?i)\b{re.escape(old)}\b", new, s)

    return _WHITESPACE.sub(" ", s).strip()


def station_lookup_keys(raw: Any) -> list[str]:
    """Все ключи поиска (casefold): канонический, исходный, без «на» в блоке экспорта."""
    display = normalize_name(raw)
    if not display:
        return []

    keys: list[str] = []

    def add(text: str) -> None:
        k = text.casefold()
        if k and k not in keys:
            keys.append(k)

    canon = canonicalize_station_name(display)
    add(canon)
    add(display)
    # NSI иногда с «на», реестр — без (уже в canon; дубль для старых ключей)
    add(re.sub(r"(?i)\s+на\s+(?=\[)", " ", canon))
    add(re.sub(r"(?i)\(экспорт\s+на\s+", "(экспорт ", canon))
    return keys


def name_lookup_key(raw: Any) -> str:
    """Основной ключ: canonicalize + casefold."""
    keys = station_lookup_keys(raw)
    return keys[0] if keys else ""


def normalize_railway_rw(raw: Any) -> str | None:
    if raw is None:
        return None
    s = str(raw).strip().upper()
    if not s:
        return None
    return s


def normalize_esr6(code6_raw: Any) -> str:
    if code6_raw is None:
        return ""
    if isinstance(code6_raw, bool):
        return ""
    if isinstance(code6_raw, int):
        digits = str(abs(code6_raw))
    elif isinstance(code6_raw, float):
        digits = str(int(code6_raw))
    else:
        s = str(code6_raw).strip()
        digits = "".join(c for c in s if c.isdigit())
    if not digits:
        return ""
    if len(digits) > 6:
        digits = digits[-6:]
    return digits.zfill(6)


def _assert_key_eq(a: str, b: str) -> None:
    assert name_lookup_key(a) == name_lookup_key(b), f"{a!r} != {b!r}"


if __name__ == "__main__":
    _assert_key_eq("НОВОСПАССКОЕ", "Новоспасское")
    _assert_key_eq("Краснодар I", "Краснодар 1")
    _assert_key_eq("Койбагор", "Койбагар")
    _assert_key_eq("Селинка", "Силинка")
    _assert_key_eq("Рыбинск-тов", "Рыбинск-Товарный")
    _assert_key_eq("Ярославль-Гл", "Ярославль-Главный")
    _assert_key_eq("СПТВ", "Санкт-Петербург-Товарный-Московский")
    _assert_key_eq("Новороссийск(эксп)", "Новороссийск (Экспорт)")
    _assert_key_eq("Новороссийск (ЭКСП)", "Новороссийск (экспорт.)")
    _assert_key_eq("Туапсе-Сортировочная (эксп.)", "Туапсе-Сортировочная (Экспорт)")
    _assert_key_eq("Туапсе-Сортировочная ( ЭКСП )", "Туапсе-Сортировочная (экспорт)")
    _assert_key_eq("Забайкальск (Экспорт КЖД [Китай])", "Забайкальск (Экспорт на КЖД [Китай])")
    _assert_key_eq("Г.З.Тагиева-сортировочная (546901)", "Гаджизейналабдин Тагиев-Сортировка")
    _assert_key_eq(
        "Наушки (эксп. АО «УБЖД»: участок Улаанбаатар (вкл.) и далее)",
        "Наушки (Экспорт АО «УБЖД»: участок Улаанбаатар (вкл.) и далее)",
    )
    _assert_key_eq("Астара (Экспорт)", "Астара (эксп)")
    _assert_key_eq("Зиемельблазма (ЭКСПОРТ)", "Зиемельблазма (эксп.)")
    print("normalize: ok")
