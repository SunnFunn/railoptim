# Ограничение MIN_BATCH на уровне пары станций

## Суть изменения

**До:** `MIN_BATCH_FROM_MASS_STATION` проверялся на каждой дуге отдельно — количество вагонов,
назначаемых с конкретного `SupplyNode` (узла) на конкретный `DemandNode`, должно было быть
0 или ≥ MIN_BATCH.

**После:** ограничение применяется на уровне **пары станций**:
суммарный поток по всем дугам с одинаковой парой
`(supply_station_code → demand_station_code)` должен быть 0 или ≥ MIN_BATCH.

Одна станция массовой выгрузки может порождать несколько `SupplyNode` (разные типы вагонов,
ЭТСНГ-коды и т.д.). Назначения с этих узлов на одну станцию погрузки теперь суммируются
перед проверкой ограничения.

---

## Изменённые файлы

### `src/solver/model.rs`

Добавлена публичная функция:

```rust
pub fn collect_mass_pair_violations(
    flow: impl Iterator<Item = (usize, i32)>,  // (arc_id, quantity)
    arcs: &[TaskArc],
) -> Vec<(String, String)>
```

Функция принимает итератор `(arc_id, quantity)` (не зависит от конкретного типа назначения),
агрегирует поток по парам `(supply_station_code, demand_station_code)` для mass-unloading дуг
и возвращает пары, нарушающие ограничение: `0 < total < MIN_BATCH_FROM_MASS_STATION`.

### `src/solver/greedy.rs`

- **Удалена** per-arc проверка из основного цикла:
  ```rust
  // УДАЛЕНО:
  if arc.is_mass_unloading && qty < MIN_BATCH_FROM_MASS_STATION {
      continue;
  }
  ```
- **Добавлен** post-processing после основного цикла (перед подсчётом статистики):
  вызов `collect_mass_pair_violations`, затем удаление всех назначений для нарушающих
  пар через `swap_remove` с возвратом вагонов в `remaining_supply`/`remaining_demand`.

### `src/solver/alns.rs`

Три изменения:

1. **Новая внутренняя функция `remove_mass_pair_violations(state, arcs)`** —
   общий post-processing для обоих операторов ремонта. Проверяет **все** назначения
   в текущем состоянии (не только новые), т.к. операция `destroy` могла нарушить
   ранее корректные пары.

2. **`repair_greedy`** — убрана per-arc проверка в фильтре дуг, в конце функции
   добавлен вызов `remove_mass_pair_violations`.

3. **`repair_lp`** — старый per-arc цикл удаления (проверявший только новые
   назначения после `before`) заменён вызовом `remove_mass_pair_violations` по
   всему состоянию.

---

## Что не изменилось

- `TaskArc` — поля `supply_station_code`, `demand_station_code`, `is_mass_unloading`
  уже существовали и оказались достаточны.
- `lp.rs` — LP не умеет выражать бинарные ограничения вида «0 или ≥ N»;
  post-processing в ALNS заменяет это.
- `MIN_BATCH_FROM_MASS_STATION` остаётся константой в `model.rs`.

---

## Почему post-processing, а не проверка при назначении

Greedy и ALNS обрабатывают дуги последовательно, по одной. Если несколько `SupplyNode`
с одной станции могут в совокупности закрыть порог MIN_BATCH, но по отдельности каждый
дает < MIN_BATCH — per-arc проверка заблокировала бы все. Post-processing позволяет
назначить сначала, проверить суммарно потом и откатить только нарушающие пары.
