use highs::{ColProblem, Sense};
use serde::Serialize;

use crate::node::{DemandNode, DemandPurpose, SupplyNode};
use super::model::TaskArc;

// ---------------------------------------------------------------------------
// Результат оптимизации
// ---------------------------------------------------------------------------

/// Сводная статистика после решения LP-задачи.
#[derive(Debug, Clone, Serialize)]
pub struct OptimResult {
    /// Суммарная стоимость по реальным дугам.
    pub total_cost: f64,
    /// Вагоны, успешно назначенные на реальные узлы спроса.
    pub assigned_cars: f64,
    /// Вагоны, назначенные на dummy-узел предложения (неудовлетворённый спрос).
    pub penalty_cars: f64,
    /// Вагоны, назначенные на dummy-узел спроса (избыток предложения, не нашедший погрузки).
    pub excess_supply: f64,
    /// Статус решателя (строка из HiGHS).
    pub status: String,
}

// ---------------------------------------------------------------------------
// LP-решатель
// ---------------------------------------------------------------------------

/// Штраф за 1 вагон **неудовлетворённого спроса на погрузку** (dummy-предложение → узел спроса).
///
/// Должен быть заведомо выше стоимости любого реального плеча, чтобы MIP всегда
/// предпочёл реальный вагон закрытию спроса через dummy.
/// Минимальный реальный тариф в данных ≈ 15 000 руб., максимальный (тариф + порожний
/// ход + промывка) не превышает ~700 000 руб. Значение 1 000 000 покрывает весь диапазон.
pub const PENALTY_UNMET: f64 = 1_000_000.0;

/// Штраф за 1 вагон **избытка предложения** (узел предложения → dummy-спрос, т.е. отстой).
///
/// Намеренно ниже минимального реального тарифа (~15 000 руб.), чтобы MIP не тянул
/// дорогие вагоны (period=10, тариф ~600–700k) под погрузку только ради «использования
/// предложения». Вагон попадёт в real-дугу лишь если это объективно дешевле.
/// Ненулевое значение устраняет вырожденность задачи (несколько равноценных оптимумов).
pub const PENALTY_EXCESS: f64 = 5_000.0;

/// Штраф за 1 вагон избытка для **грязного** узла предложения в режиме **дефицита**.
///
/// Грязный вагон (есть Wash-дуги, под погрузку без промывки идти не может) в остатке —
/// мёртвый актив: он уедет в отстой и до промывки ни одну заявку не закроет. При
/// дефиците спроса каждый промытый вагон завтра закрывает заявку, которую сегодня
/// нечем закрыть (её штраф — [`PENALTY_UNMET`]). Но узлы промывки — только верхняя
/// ёмкость без штрафа за незаполнение, и с базовым [`PENALTY_EXCESS`] (5 тыс.) MIP
/// корректно оставлял грязный вагон в остатке вместо промывки за 50–60 тыс.
/// (прогон 10.09.2026: 9 вагонов с одними Wash-дугами ушли в отстой при 6 тыс.
/// незакрытого спроса).
///
/// Значение — фактически **потолок стоимости промывочного плеча**: вагон едет в
/// промывку, пока `arc.cost < PENALTY_EXCESS_DIRTY`, т.е. тариф до промывки не дороже
/// `150 000 − WASH_PATH_SURCHARGE_RUB (50 000) = 100 000` руб. Типовое плечо с учётом
/// промывки и последующего подсыла чистым — 70–100 тыс.; более дальняя промывка
/// (через всю страну) проигрывает остатку, и вагон идёт в отстой.
///
/// На выбор «аналогичный груз vs промывка» константа **не влияет**: погрузка грязного
/// вагона под тот же ЕТСНГ снимает штраф [`PENALTY_UNMET`] (1 млн) и выигрывает при
/// любом значении здесь; дальние подсылы под аналогичный груз держат потолок
/// `MaxEmptyRunDistanceKm` и cap «дальняя погрузка дороже промывки» в `classify_pair`.
///
/// В режиме профицита (предложение ≥ спрос на погрузку) не применяется: закрывать
/// нечего, промывка за реальные деньги ради простоя не нужна — вагон идёт в отстой.
pub const PENALTY_EXCESS_DIRTY: f64 = 150_000.0;

/// Дефицит: суммарный спрос на **погрузку** больше суммарного предложения.
pub fn is_deficit(supply: &[SupplyNode], demand: &[DemandNode]) -> bool {
    let total_supply: i64 = supply.iter().map(|s| s.car_count as i64).sum();
    let total_load: i64 = demand
        .iter()
        .filter(|d| d.purpose == DemandPurpose::Load)
        .map(|d| d.car_count as i64)
        .sum();
    total_load > total_supply
}

/// Штрафы за остаток предложения по узлам (индекс = `s_idx`).
///
/// Единая точка согласования целевой функции LP / MIP / ALNS / greedy: все они
/// оценивают остаток вагонов узла `s` по `per_node[s]`. Строится один раз для
/// полной задачи ([`ExcessPenalties::build`]); для подзадач ALNS берётся срез
/// по локальной индексации ([`ExcessPenalties::subset`]).
#[derive(Debug, Clone)]
pub struct ExcessPenalties {
    /// Штраф за 1 вагон остатка узла предложения, руб.
    pub per_node: Vec<f64>,
    /// Режим дефицита, при котором грязные узлы получают [`PENALTY_EXCESS_DIRTY`].
    pub deficit: bool,
    /// Узлов с повышенным штрафом (грязные, есть Wash-дуги).
    pub dirty_nodes: usize,
    /// Вагонов в этих узлах.
    pub dirty_cars: i32,
}

impl ExcessPenalties {
    /// Базовый штраф [`PENALTY_EXCESS`] для всех `n` узлов (без учёта промывки).
    pub fn uniform(n: usize) -> Self {
        Self { per_node: vec![PENALTY_EXCESS; n], deficit: false, dirty_nodes: 0, dirty_cars: 0 }
    }

    /// Строит штрафы по задаче: узлы с хотя бы одной Wash-дугой (грязные вагоны)
    /// при дефиците получают [`PENALTY_EXCESS_DIRTY`], остальные — [`PENALTY_EXCESS`].
    pub fn build(arcs: &[TaskArc], supply: &[SupplyNode], demand: &[DemandNode]) -> Self {
        let deficit = is_deficit(supply, demand);
        let mut per_node = vec![PENALTY_EXCESS; supply.len()];
        let mut dirty_nodes = 0usize;
        let mut dirty_cars = 0i32;
        if deficit {
            let mut has_wash = vec![false; supply.len()];
            for a in arcs {
                if demand[a.d_idx].purpose == DemandPurpose::Wash {
                    has_wash[a.s_idx] = true;
                }
            }
            for (s_idx, dirty) in has_wash.into_iter().enumerate() {
                if dirty {
                    per_node[s_idx] = PENALTY_EXCESS_DIRTY;
                    dirty_nodes += 1;
                    dirty_cars += supply[s_idx].car_count;
                }
            }
        }
        Self { per_node, deficit, dirty_nodes, dirty_cars }
    }

    /// Штраф узла `s_idx`; вне диапазона — базовый [`PENALTY_EXCESS`].
    #[inline]
    pub fn get(&self, s_idx: usize) -> f64 {
        self.per_node.get(s_idx).copied().unwrap_or(PENALTY_EXCESS)
    }

    /// Срез для подзадачи: `s_map[local] = original s_idx`.
    pub fn subset(&self, s_map: &[usize]) -> Self {
        Self {
            per_node: s_map.iter().map(|&s| self.get(s)).collect(),
            deficit: self.deficit,
            dirty_nodes: 0,
            dirty_cars: 0,
        }
    }

    /// Штрафная стоимость остатка предложения `remaining[s]` (отрицательные остатки игнорируются).
    pub fn cost_of_remaining(&self, remaining: &[i32]) -> f64 {
        remaining
            .iter()
            .enumerate()
            .filter(|(_, r)| **r > 0)
            .map(|(s, &r)| self.get(s) * r as f64)
            .sum()
    }
}

/// Решает сбалансированную транспортную задачу методом LP (HiGHS / IPM).
///
/// # Балансировка через явные dummy-узлы
///
/// Задача всегда сбалансирована: суммарное предложение = суммарный спрос.
/// Достигается добавлением двух dummy-узлов с дугами ко/от **всех** реальных узлов:
///
/// | Dummy-узел        | Ёмкость         | Стоимость дуги | Назначение                       |
/// |-------------------|-----------------|----------------|----------------------------------|
/// | Dummy **спрос**   | `total_supply`  | `excess.per_node[s]` (5k; грязные при дефиците 300k) | Поглощает незадействованное предложение (отстой) |
/// | Dummy **предложение** | `total_load_demand` | `PENALTY_UNMET` (1M) | Покрывает незакрытый спрос на погрузку |
///
/// Ёмкость dummy-узлов согласована с суммарным предложением / спросом на **погрузку**.
/// Узлы промывки — только верхняя граница входящего потока (без штрафа за незаполнение);
/// штрафные дуги dummy-предложения ведут только к узлам погрузки. Чтобы грязный вагон
/// при дефиците всё же ехал в промывку, его остаток штрафуется дороже промывочного
/// плеча — см. [`ExcessPenalties`] / [`PENALTY_EXCESS_DIRTY`].
///
/// Строки спроса на погрузку — равенства; на промывку — неравенство «не больше ёмкости».
///
/// # Возврат
/// `(OptimResult, Vec<f64>)` — второй элемент содержит значения только
/// **реальных дуговых** переменных (в порядке `arcs`), без dummy.
pub fn solve(
    arcs: &[TaskArc],
    supply: &[SupplyNode],
    demand: &[DemandNode],
    excess: &ExcessPenalties,
) -> (OptimResult, Vec<f64>) {
    let total_supply: f64 = supply.iter().map(|s| s.car_count as f64).sum();
    let total_load_demand: f64 = demand
        .iter()
        .filter(|d| d.purpose == DemandPurpose::Load)
        .map(|d| d.car_count as f64)
        .sum();

    let mut model = ColProblem::default();

    // --- Строки предложения: Σ x[из s] = car_count[s] ---
    let supply_rows: Vec<_> = supply
        .iter()
        .map(|s| { let c = s.car_count as f64; model.add_row(c..=c) })
        // .map(|s| { let c = s.car_count as f64; model.add_row(0.0..c) })
        .collect();

    // --- Строки спроса: погрузка — равенство; промывка — только верхняя ёмкость (без штрафа за незаполнение).
    let demand_rows: Vec<_> = demand
        .iter()
        .map(|d| {
            let c = d.car_count as f64;
            if d.purpose == DemandPurpose::Wash {
                model.add_row(0.0..=c)
            } else {
                model.add_row(c..=c)
            }
        })
        .collect();

    // --- Реальные дуговые переменные ---
    for arc in arcs {
        model.add_column(
            arc.cost,
            0.0..,
            [(supply_rows[arc.s_idx], 1.0), (demand_rows[arc.d_idx], 1.0)],
        );
    }

    // --- Dummy-узел СПРОСА (поглощает незадействованное предложение / отстой) ---
    // Базовый штраф PENALTY_EXCESS намеренно ниже минимального реального тарифа: MIP не
    // будет «тянуть» дорогие вагоны period=10 под погрузку ради снижения excess_supply.
    // Грязные узлы при дефиците — PENALTY_EXCESS_DIRTY (промывка выгоднее остатка).
    let dummy_demand_row = model.add_row(..total_supply);
    for (s_idx, s_row) in supply_rows.iter().enumerate() {
        model.add_column(excess.get(s_idx), 0.0.., [(*s_row, 1.0), (dummy_demand_row, 1.0)]);
    }

    // --- Dummy-узел ПРЕДЛОЖЕНИЯ (покрывает незакрытый спрос **погрузки**) ---
    // Штраф PENALTY_UNMET >> max(arc.cost): MIP всегда предпочтёт реальный вагон.
    // Штрафные дуги только к узлам спроса на погрузку; промывка — опциональный приёмник.
    let dummy_supply_row = model.add_row(..total_load_demand);
    for (d_row, d) in demand_rows.iter().zip(demand.iter()) {
        if d.purpose == DemandPurpose::Load {
            model.add_column(PENALTY_UNMET, 0.0.., [(dummy_supply_row, 1.0), (*d_row, 1.0)]);
        }
    }

    // --- Решатель ---
    // IPM значительно быстрее simplex для задач с >50K переменных.
    let mut optimizer = model.optimise(Sense::Minimise);
    optimizer.set_option("solver",   "simplex");
    optimizer.set_option("presolve", "on");
    optimizer.set_option("parallel", "on");
    optimizer.set_option("threads",  8_i32);

    let solved   = optimizer.solve();
    let solution = solved.get_solution();
    let col_vals = solution.columns();

    // Столбцы по порядку добавления:
    // [реальные дуги (n_arcs)] [dummy-demand дуги (n_supply)] [dummy-supply только Load (n_load)]
    let n_arcs    = arcs.len();
    let n_supply  = supply.len();
    let n_load_demand = demand.iter().filter(|d| d.purpose == DemandPurpose::Load).count();

    let arc_vals          = &col_vals[..n_arcs];
    let dummy_demand_vals = &col_vals[n_arcs..n_arcs + n_supply];
    let dummy_supply_vals = &col_vals[n_arcs + n_supply..n_arcs + n_supply + n_load_demand];

    // --- Статистика ---
    let total_cost: f64 = arcs.iter().zip(arc_vals)
        .filter(|(_, q)| **q > 1e-4)
        .map(|(a, &q)| q * a.cost)
        .sum();

    let assigned_cars: f64 = arc_vals.iter().filter(|&&q| q > 1e-4).sum();
    let excess_supply: f64 = dummy_demand_vals.iter().filter(|&&q| q > 1e-4).sum();
    let penalty_cars:  f64 = dummy_supply_vals.iter().filter(|&&q| q > 1e-4).sum();

    let result = OptimResult {
        total_cost,
        assigned_cars,
        penalty_cars,
        excess_supply,
        status: format!("{:?}", solved.status()),
    };

    (result, arc_vals.to_vec())
}

// ---------------------------------------------------------------------------
// Анализ баланса (вывод до solve)
// ---------------------------------------------------------------------------

/// Выводит в консоль соотношение суммарного предложения и спроса.
///
/// В «спросе» учитывается только **погрузка**; ёмкость промывки выводится отдельно.
pub fn print_balance(supply: &[SupplyNode], demand: &[DemandNode]) {
    let total_supply: i32 = supply.iter().map(|s| s.car_count).sum();
    let total_load: i32 = demand
        .iter()
        .filter(|d| d.purpose == DemandPurpose::Load)
        .map(|d| d.car_count)
        .sum();
    let wash_cap: i32 = demand
        .iter()
        .filter(|d| d.purpose == DemandPurpose::Wash)
        .map(|d| d.car_count)
        .sum();
    let diff = total_supply - total_load;

    println!("--- АНАЛИЗ РЕСУРСОВ ---");
    println!("Предложение: {} ваг.", total_supply);
    println!("Спрос (погрузка): {} ваг.", total_load);
    if wash_cap > 0 {
        println!("Ёмкость промывки (верх): {} ваг.", wash_cap);
    }
    if diff >= 0 {
        println!("Статус: ПРОФИЦИТ к погрузке (+{} ваг.)", diff);
    } else {
        println!("Статус: ДЕФИЦИТ  ({} ваг. — штрафные дуги)", diff.abs());
    }
    println!("-----------------------");
}
