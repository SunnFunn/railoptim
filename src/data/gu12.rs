//! Правило 3 бизнес-правил: подсыл под погрузку только при наличии согласованной
//! РЖД заявки ГУ-12 (`business_rules.txt`, п. 3).
//!
//! Источник — MSSQL (БД SLP, `vClaimGU12` + график подач `vClaimGu12OtprGraphPod`)
//! через `src/data/gu12.py` (pymssql): по каждой согласованной заявке — грузоотправитель
//! (имя, ОКПО), станция погрузки и число вагонов по четырём периодам планирования.
//! Окна периодов передаются скрипту из [`super::demand::DEMAND_PERIODS`], чтобы
//! совпадать с периодами узлов спроса.
//!
//! [`apply_gu12_limits`] выполняется **сразу после** формирования узлов спроса
//! погрузки: для каждого российского узла запоминается потолок
//! [`crate::node::DemandNode::gu12_cap`] — сколько вагонов **с российских дорог**
//! можно подослать по согласованным заявкам ГУ-12. Спрос АПИ (`car_count`)
//! не уменьшается и узлы с нулевым потолком не удаляются: вагоны, образовавшиеся
//! на инотерритории, закрывают этот спрос без потолка (ГУ-12 — документ РЖД).
//!
//! Два режима ([`Gu12Mode`], флаг `run.sh`, по умолчанию ослабленный):
//! - [`Gu12Mode::Strong`] — потолок отдельно на каждый период спроса;
//! - [`Gu12Mode::Relaxed`] — заявка суммируется на горизонт 1–15 суток; если её
//!   не хватает на спрос горизонта, нехватка снимается с периода 11–15, затем
//!   9–10, 6–8 и только потом 1–5. К более близкому периоду переходим, только
//!   когда дальний занулён целиком.
//! Сопоставление — по коду станции погрузки и ОКПО грузоотправителя
//! (`DemandNode::sender_okpo` ↔ `LoaderFromOKPO`); без ОКПО — по имени
//! грузоотправителя; заявки, не привязанные ни к одному узлу станции,
//! распределяются пропорционально спросу по всем узлам этой станции.
//!
//! Проверка относится только к территории России: узлы на дорогах-инотерриториях
//! ([`crate::data::BusinessRules::foreign_railways`] — тот же список, что правило 1)
//! не получают потолок. Пустой список → вызывающий код не должен вызывать
//! [`apply_gu12_limits`] ([`crate::data::BusinessRules::gu12_ready`]). Дорога узла —
//! `DemandNode::railway_name` (RailWayShortFrom); классификация по коду станции ЕСР
//! намеренно не используется (ненадёжна).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::node::{DemandNode, DemandPurpose};

use super::demand::DEMAND_PERIODS;
use super::esr::normalize_esr6;

/// Согласованная заявка ГУ-12 (строка ответа `gu12.py json`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Gu12Claim {
    #[serde(rename = "ClaimNumber", default)]
    pub claim_number: String,
    /// Грузоотправитель на станции погрузки (`LoaderFromName`).
    #[serde(rename = "LoaderName", default)]
    pub loader_name: String,
    /// ОКПО грузоотправителя (`LoaderFromOKPO`); пусто — не указан.
    #[serde(rename = "LoaderOkpo", default)]
    pub loader_okpo: String,
    /// Код ЕСР-6 станции погрузки (`StationFromCode6`).
    #[serde(rename = "StationCode", default)]
    pub station_code: String,
    #[serde(rename = "StationName", default)]
    pub station_name: String,
    #[serde(rename = "EtsngCode", default)]
    pub etsng_code: String,
    #[serde(rename = "TotalCars", default)]
    pub total_cars: i32,
    /// Разрешённая погрузка по периодам 1..4, вагонов.
    #[serde(rename = "Cars", default)]
    pub cars: [i32; 4],
}

fn gu12_script_path() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("текущая директория")?
        .join("src/data/gu12.py"))
}

/// Окна периодов спроса в формате `gu12.py --periods` (`"0:4,5:7,8:9,10:14"`).
pub fn periods_arg() -> String {
    DEMAND_PERIODS
        .iter()
        .map(|(lo, hi)| format!("{lo}:{hi}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Запускает `gu12.py json --periods <DEMAND_PERIODS>`, читает JSON со stdout.
///
/// Коды станций нормализуются до ЕСР-6; строки без кода станции отбрасываются.
pub fn fetch_gu12_claims() -> Result<Vec<Gu12Claim>> {
    let script = gu12_script_path()?;
    let periods = periods_arg();
    let output = Command::new("python3")
        .arg(&script)
        .arg("json")
        .arg("--periods")
        .arg(&periods)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("python3 {}", script.display()))?;

    if !output.status.success() {
        bail!("gu12.py json завершился с кодом {:?}", output.status.code());
    }

    let mut claims: Vec<Gu12Claim> =
        serde_json::from_slice(&output.stdout).context("разбор JSON заявок ГУ-12")?;
    claims.retain_mut(|c| {
        c.station_code = normalize_esr6(&c.station_code);
        c.loader_okpo = normalize_okpo(&c.loader_okpo);
        for v in &mut c.cars {
            *v = (*v).max(0);
        }
        c.station_code.len() == 6
    });
    Ok(claims)
}

/// ОКПО для сравнения: только цифры, без ведущих нулей (`"00335717"` ≡ `"335717"`).
pub fn normalize_okpo(raw: &str) -> String {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.trim_start_matches('0').to_string()
}

/// Имя грузоотправителя для сравнения: верхний регистр, длинные формы ОПФ → короткие,
/// оставлены только буквы и цифры.
pub fn normalize_party_name(raw: &str) -> String {
    const FORMS: &[(&str, &str)] = &[
        ("ПУБЛИЧНОЕ АКЦИОНЕРНОЕ ОБЩЕСТВО", "ПАО"),
        ("ОТКРЫТОЕ АКЦИОНЕРНОЕ ОБЩЕСТВО", "ОАО"),
        ("ЗАКРЫТОЕ АКЦИОНЕРНОЕ ОБЩЕСТВО", "ЗАО"),
        ("АКЦИОНЕРНОЕ ОБЩЕСТВО", "АО"),
        ("ОБЩЕСТВО С ОГРАНИЧЕННОЙ ОТВЕТСТВЕННОСТЬЮ", "ООО"),
        ("ИНДИВИДУАЛЬНЫЙ ПРЕДПРИНИМАТЕЛЬ", "ИП"),
        ("СЕЛЬСКОХОЗЯЙСТВЕННЫЙ ПРОИЗВОДСТВЕННЫЙ КООПЕРАТИВ", "СПК"),
        ("КРЕСТЬЯНСКОЕ (ФЕРМЕРСКОЕ) ХОЗЯЙСТВО", "КФХ"),
    ];
    let mut s = raw.trim().to_uppercase().replace('Ё', "Е");
    for (long, short) in FORMS {
        s = s.replace(long, short);
    }
    s.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Как считать потолок ГУ-12. Выбирается при запуске (`run.sh`), не в JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gu12Mode {
    /// Потолок отдельно на каждый период спроса (1–5, 6–8, 9–10, 11–15 суток).
    Strong,
    /// Заявка на весь горизонт 1–15 суток; нехватка снимается с дальних периодов.
    Relaxed,
}

impl Gu12Mode {
    /// Короткая подпись для лога прогона.
    pub fn label(self) -> &'static str {
        match self {
            Self::Strong => "жёсткий, по периодам",
            Self::Relaxed => "ослабленный, горизонт 1–15 суток",
        }
    }
}

/// Как узел спроса был сопоставлен с заявками ГУ-12.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gu12Match {
    /// По ОКПО грузоотправителя.
    Okpo,
    /// По имени грузоотправителя (у заявки или узла нет ОКПО / ОКПО не совпал).
    Name,
    /// Только пропорциональная доля нераспределённых заявок станции.
    PoolOnly,
    /// На станции нет ни одной согласованной заявки ГУ-12.
    NoClaim,
}

/// Статистика корректировки спроса по ГУ-12 (для логов).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gu12Stats {
    pub claims_total: usize,
    pub claim_stations: usize,
    /// Заявки на станциях, где нет ни одного узла спроса погрузки (в подсыл не идут).
    pub claims_without_demand: usize,

    pub nodes_before: usize,
    pub cars_before: i32,
    pub nodes_after: usize,
    pub cars_after: i32,
    /// По периодам 1..4: вагонов спроса до и после корректировки.
    pub cars_before_by_period: [i32; 4],
    pub cars_after_by_period: [i32; 4],

    /// Узлы на инотерриториях: проверка не применяется.
    pub nodes_foreign: usize,
    pub cars_foreign: i32,

    pub nodes_matched_okpo: usize,
    pub nodes_matched_name: usize,
    pub nodes_pool_only: usize,
    /// Узлы без разрешённой погрузки: российский подсыл закрыт (`gu12_cap = 0`),
    /// узел в задаче остаётся для вагонов с инотерриторий.
    pub nodes_removed: usize,
    pub cars_removed: i32,
    /// Узлы, у которых потолок ГУ-12 для российских вагонов меньше спроса АПИ
    /// и больше нуля.
    pub nodes_capped: usize,
    pub cars_cut: i32,
}

/// Делит `total` на части, пропорциональные `weights` (метод наибольших остатков);
/// сумма частей равна `total`. При нулевых весах — поровну.
pub fn split_proportional(total: i32, weights: &[i32]) -> Vec<i32> {
    let n = weights.len();
    if n == 0 || total <= 0 {
        return vec![0; n];
    }
    let wsum: i64 = weights.iter().map(|&w| w.max(0) as i64).sum();
    let ws: Vec<i64> = if wsum > 0 {
        weights.iter().map(|&w| w.max(0) as i64).collect()
    } else {
        vec![1; n]
    };
    let wsum: i64 = ws.iter().sum();
    let total = total as i64;

    let mut parts: Vec<i32> = Vec::with_capacity(n);
    let mut rems: Vec<(i64, usize)> = Vec::with_capacity(n);
    let mut assigned: i64 = 0;
    for (i, &w) in ws.iter().enumerate() {
        let exact = total * w;
        let floor = exact / wsum;
        parts.push(floor as i32);
        assigned += floor;
        rems.push((exact % wsum, i));
    }
    let mut left = total - assigned;
    // Наибольшие остатки первыми; при равенстве — меньший индекс (детерминированно).
    rems.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (_, i) in rems {
        if left <= 0 {
            break;
        }
        parts[i] += 1;
        left -= 1;
    }
    parts
}

/// Ставит потолок ГУ-12 на подсыл с российских дорог (правило 3).
///
/// - `demand` — узлы спроса (обрабатываются только `purpose == Load`);
/// - `claims` — согласованные заявки ГУ-12 ([`fetch_gu12_claims`]);
/// - `foreign_railways` — короткие коды дорог-инотерриторий
///   ([`crate::data::BusinessRules::foreign_railways`]): узлы с такой
///   `railway_name` не получают потолок. Пустое множество трактует все узлы как
///   российские — вызывающий код должен передавать непустой список
///   ([`crate::data::BusinessRules::gu12_ready`]).
///
/// `car_count` (спрос АПИ) не меняется, узлы не удаляются. Для российского узла
/// погрузки [`DemandNode::gu12_cap`] — сколько вагонов с российских дорог можно
/// подослать (0 — российские дуги в узел не строятся). Вагоны с инотерриторий
/// этот потолок не расходуют.
///
/// [`Gu12Mode::Strong`] сравнивает заявку и спрос внутри каждого периода.
/// [`Gu12Mode::Relaxed`] суммирует заявку и спрос грузоотправителя на станции
/// за все четыре периода; излишек заявки в одном периоде закрывает нехватку
/// в другом, а нехватку горизонта снимает с периода 4 к периоду 1.
pub fn apply_gu12_limits(
    demand: &mut Vec<DemandNode>,
    claims: &[Gu12Claim],
    foreign_railways: &HashSet<String>,
    mode: Gu12Mode,
) -> Gu12Stats {
    let mut st = Gu12Stats {
        claims_total: claims.len(),
        nodes_before: demand.len(),
        cars_before: demand.iter().map(|d| d.car_count).sum(),
        ..Default::default()
    };
    for d in demand.iter() {
        if let Some(p) = period_slot(d.period) {
            st.cars_before_by_period[p] += d.car_count;
        }
    }

    // Заявки по станциям.
    let mut claims_by_station: HashMap<String, Vec<&Gu12Claim>> = HashMap::new();
    for c in claims {
        let code = normalize_esr6(&c.station_code);
        if code.len() != 6 {
            continue;
        }
        claims_by_station.entry(code).or_default().push(c);
    }
    st.claim_stations = claims_by_station.len();

    // Группы узлов — только погрузка на российских дорогах.
    // Жёсткий режим режет по (станция, период), ослабленный — по станции и грузоотправителю.
    let mut groups: HashMap<(String, usize), Vec<usize>> = HashMap::new();
    let mut nodes_by_station: HashMap<String, Vec<usize>> = HashMap::new();
    let mut demand_stations: HashSet<String> = HashSet::new();
    for (i, d) in demand.iter().enumerate() {
        if d.purpose != DemandPurpose::Load {
            continue;
        }
        if is_foreign_node(d, foreign_railways) {
            st.nodes_foreign += 1;
            st.cars_foreign += d.car_count;
            continue;
        }
        let Some(p) = period_slot(d.period) else { continue };
        let code = normalize_esr6(&d.station_code);
        demand_stations.insert(code.clone());
        groups.entry((code.clone(), p)).or_default().push(i);
        nodes_by_station.entry(code).or_default().push(i);
    }
    st.claims_without_demand = claims
        .iter()
        .filter(|c| !demand_stations.contains(&normalize_esr6(&c.station_code)))
        .count();

    let (allowance, matched) = match mode {
        Gu12Mode::Strong => strong_allowances(demand, &claims_by_station, &groups),
        Gu12Mode::Relaxed => relaxed_allowances(demand, &claims_by_station, &nodes_by_station),
    };

    // Потолок для вагонов с российских дорог. Спрос АПИ (`car_count`) не режется:
    // вагоны с инотерриторий закрывают его без ГУ-12, узлы с нулевым потолком остаются.
    for (i, d) in demand.iter_mut().enumerate() {
        let Some(&a) = allowance.get(&i) else { continue };
        match matched[&i] {
            Gu12Match::Okpo => st.nodes_matched_okpo += 1,
            Gu12Match::Name => st.nodes_matched_name += 1,
            Gu12Match::PoolOnly => st.nodes_pool_only += 1,
            Gu12Match::NoClaim => {}
        }
        let cap = a.max(0);
        if cap <= 0 {
            st.nodes_removed += 1;
            st.cars_removed += d.car_count;
        } else if cap < d.car_count {
            st.nodes_capped += 1;
            st.cars_cut += d.car_count - cap;
        }
        d.gu12_cap = Some(cap);
    }

    st.nodes_after = demand.len();
    st.cars_after = demand
        .iter()
        .map(|d| match d.gu12_cap {
            Some(cap) => d.car_count.min(cap.max(0)),
            None => d.car_count,
        })
        .sum();
    for d in demand.iter() {
        if let Some(p) = period_slot(d.period) {
            let covered = match d.gu12_cap {
                Some(cap) => d.car_count.min(cap.max(0)),
                None => d.car_count,
            };
            st.cars_after_by_period[p] += covered;
        }
    }
    st
}

/// Узел спроса вне территории России: дорога погрузки входит в список инотерриторий.
pub fn is_foreign_node(d: &DemandNode, foreign_railways: &HashSet<String>) -> bool {
    foreign_railways.contains(d.railway_name.trim())
}

/// Жёсткий режим: разрешённые вагоны заявки периода делятся между узлами этого периода.
fn strong_allowances(
    demand: &[DemandNode],
    claims_by_station: &HashMap<String, Vec<&Gu12Claim>>,
    groups: &HashMap<(String, usize), Vec<usize>>,
) -> (HashMap<usize, i32>, HashMap<usize, Gu12Match>) {
    let mut allowance: HashMap<usize, i32> = HashMap::new();
    let mut matched: HashMap<usize, Gu12Match> = HashMap::new();

    let mut keys: Vec<_> = groups.keys().cloned().collect();
    keys.sort();
    for key in keys {
        let idx = &groups[&key];
        let (code, p) = &key;
        let weights: Vec<i32> = idx.iter().map(|&i| demand[i].car_count).collect();

        let Some(st_claims) = claims_by_station.get(code) else {
            for &i in idx {
                allowance.insert(i, 0);
                matched.insert(i, Gu12Match::NoClaim);
            }
            continue;
        };

        let mut node_allow: Vec<i32> = vec![0; idx.len()];
        let mut node_match: Vec<Option<Gu12Match>> = vec![None; idx.len()];
        let mut pool: i32 = 0;

        for c in st_claims {
            let cars = c.cars[*p].max(0);
            let okpo = normalize_okpo(&c.loader_okpo);
            let name = normalize_party_name(&c.loader_name);

            // 1) ОКПО грузоотправителя.
            let mut hit: Vec<usize> = Vec::new();
            let mut kind = Gu12Match::Okpo;
            if !okpo.is_empty() {
                hit = idx
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| node_okpo(demand, **i) == okpo)
                    .map(|(k, _)| k)
                    .collect();
            }
            // 2) Имя грузоотправителя.
            if hit.is_empty() && !name.is_empty() {
                kind = Gu12Match::Name;
                hit = idx
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| node_name(demand, **i) == name)
                    .map(|(k, _)| k)
                    .collect();
            }
            if hit.is_empty() {
                // 3) Ни к одному узлу станции — в общий пул станции.
                pool += cars;
                continue;
            }
            let w: Vec<i32> = hit.iter().map(|&k| weights[k]).collect();
            for (k, share) in hit.iter().zip(split_proportional(cars, &w)) {
                node_allow[*k] += share;
                // ОКПО сильнее имени: не понижаем уже найденный тип сопоставления.
                node_match[*k] = Some(match node_match[*k] {
                    Some(Gu12Match::Okpo) => Gu12Match::Okpo,
                    _ => kind,
                });
            }
        }

        // Нераспределённые заявки станции — пропорционально по всем узлам станции/периода.
        if pool > 0 {
            for (k, share) in split_proportional(pool, &weights).into_iter().enumerate() {
                node_allow[k] += share;
            }
        }

        for (k, &i) in idx.iter().enumerate() {
            allowance.insert(i, node_allow[k]);
            matched.insert(i, node_match[k].unwrap_or(Gu12Match::PoolOnly));
        }
    }
    (allowance, matched)
}

/// Ослабленный режим: заявка грузоотправителя на станции суммируется за 1–15 суток.
///
/// Нехватка (`спрос − заявка`, если спрос больше) снимается с периода 4 (сутки 11–15),
/// затем 3, 2 и 1. Следующий, более близкий период трогаем только когда дальний
/// занулён целиком. Заявка одного ОКПО/имени не закрывает другого грузоотправителя;
/// заявка без совпадения идёт в пул станции и там тоже режется с дальнего периода.
fn relaxed_allowances(
    demand: &[DemandNode],
    claims_by_station: &HashMap<String, Vec<&Gu12Claim>>,
    nodes_by_station: &HashMap<String, Vec<usize>>,
) -> (HashMap<usize, i32>, HashMap<usize, Gu12Match>) {
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum Key {
        Okpo(String),
        Name(String),
        Pool,
    }

    let mut allowance: HashMap<usize, i32> = HashMap::new();
    let mut matched: HashMap<usize, Gu12Match> = HashMap::new();

    let mut stations: Vec<_> = nodes_by_station.keys().cloned().collect();
    stations.sort();
    for code in stations {
        let nodes = &nodes_by_station[&code];
        let st_claims = claims_by_station.get(&code).map(Vec::as_slice).unwrap_or(&[]);

        let mut direct: HashMap<Key, i32> = HashMap::new();
        let mut pool: i32 = 0;
        for c in st_claims {
            let cars: i32 = c.cars.iter().copied().map(|v| v.max(0)).sum();
            let okpo = normalize_okpo(&c.loader_okpo);
            let name = normalize_party_name(&c.loader_name);
            if !okpo.is_empty() && nodes.iter().any(|&i| node_okpo(demand, i) == okpo) {
                *direct.entry(Key::Okpo(okpo)).or_insert(0) += cars;
            } else if !name.is_empty() && nodes.iter().any(|&i| node_name(demand, i) == name) {
                *direct.entry(Key::Name(name)).or_insert(0) += cars;
            } else {
                pool += cars;
            }
        }

        let mut members: HashMap<Key, Vec<usize>> = HashMap::new();
        let mut node_key: HashMap<usize, Key> = HashMap::new();
        for &i in nodes {
            let okpo = node_okpo(demand, i);
            let name = node_name(demand, i);
            let key = if !okpo.is_empty() && direct.contains_key(&Key::Okpo(okpo.clone())) {
                Key::Okpo(okpo)
            } else if !name.is_empty() && direct.contains_key(&Key::Name(name.clone())) {
                Key::Name(name)
            } else {
                Key::Pool
            };
            node_key.insert(i, key.clone());
            members.entry(key).or_default().push(i);
        }

        if pool > 0 {
            let weights: Vec<i32> = nodes.iter().map(|&i| demand[i].car_count.max(0)).collect();
            for (&i, share) in nodes.iter().zip(split_proportional(pool, &weights)) {
                if let Some(key) = node_key.get(&i) {
                    *direct.entry(key.clone()).or_insert(0) += share;
                }
            }
        }

        let mut member_keys: Vec<_> = members.keys().cloned().collect();
        member_keys.sort_by_key(|k| match k {
            Key::Okpo(s) => (0, s.clone()),
            Key::Name(s) => (1, s.clone()),
            Key::Pool => (2, String::new()),
        });
        for key in member_keys {
            let idxs = &members[&key];
            let allow = direct.get(&key).copied().unwrap_or(0);
            let kind = match &key {
                Key::Okpo(_) => Gu12Match::Okpo,
                Key::Name(_) => Gu12Match::Name,
                Key::Pool if st_claims.is_empty() => Gu12Match::NoClaim,
                Key::Pool => Gu12Match::PoolOnly,
            };
            for (i, cap) in horizon_caps(demand, idxs, allow) {
                allowance.insert(i, cap);
                matched.insert(i, kind);
            }
        }
    }
    (allowance, matched)
}

/// Потолки узлов одной группы: `allow >= спрос` — спрос не трогаем, иначе нехватка
/// снимается с дальнего периода к ближнему.
fn horizon_caps(demand: &[DemandNode], nodes: &[usize], allow: i32) -> Vec<(usize, i32)> {
    let mut by_period: [Vec<usize>; 4] = Default::default();
    for &i in nodes {
        if let Some(p) = period_slot(demand[i].period) {
            by_period[p].push(i);
        }
    }
    let total_demand: i32 = nodes.iter().map(|&i| demand[i].car_count.max(0)).sum();
    let allow = allow.max(0);
    if allow >= total_demand {
        return nodes
            .iter()
            .map(|&i| (i, demand[i].car_count.max(0)))
            .collect();
    }

    let mut deficit = total_demand - allow;
    let mut caps: Vec<(usize, i32)> = Vec::with_capacity(nodes.len());
    for p in (0..4).rev() {
        let idxs = &by_period[p];
        if idxs.is_empty() {
            continue;
        }
        let period_demand: i32 = idxs.iter().map(|&i| demand[i].car_count.max(0)).sum();
        if deficit <= 0 {
            for &i in idxs {
                caps.push((i, demand[i].car_count.max(0)));
            }
            continue;
        }
        if deficit >= period_demand {
            for &i in idxs {
                caps.push((i, 0));
            }
            deficit -= period_demand;
        } else {
            let cover = period_demand - deficit;
            let weights: Vec<i32> = idxs.iter().map(|&i| demand[i].car_count.max(0)).collect();
            for (&i, share) in idxs.iter().zip(split_proportional(cover, &weights)) {
                caps.push((i, share.min(demand[i].car_count.max(0))));
            }
            deficit = 0;
        }
    }
    caps
}

fn node_okpo(demand: &[DemandNode], i: usize) -> String {
    demand[i]
        .sender_okpo
        .as_deref()
        .map(normalize_okpo)
        .unwrap_or_default()
}

fn node_name(demand: &[DemandNode], i: usize) -> String {
    demand[i]
        .sender
        .as_deref()
        .map(normalize_party_name)
        .unwrap_or_default()
}

fn period_slot(period: u8) -> Option<usize> {
    (1..=4).contains(&period).then(|| period as usize - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: usize, period: u8, station: &str, railway: &str, sender: Option<&str>, okpo: Option<&str>, cars: i32) -> DemandNode {
        DemandNode {
            d_id: id,
            purpose: DemandPurpose::Load,
            period,
            station_name: format!("ст.{station}"),
            station_code: station.to_string(),
            railway_name: railway.to_string(),
            railway_code: None,
            railway_part: None,
            station_to_name: None,
            station_to_code: None,
            railway_to_name: None,
            railway_to_code: None,
            railway_to_part: None,
            sender: sender.map(str::to_string),
            sender_okpo: okpo.map(str::to_string),
            sender_tgnl: None,
            client: None,
            customer_okpo: None,
            recipient: None,
            loader_to_okpo: None,
            gng_cargo: None,
            etsng: Some("011005".into()),
            request_numbers: None,
            request_dates: None,
            gu12_number: None,
            shipping_type: None,
            car_type: None,
            car_count: cars,
            gu12_cap: None,
            cars_on_station: 0,
        }
    }

    fn claim(station: &str, name: &str, okpo: &str, cars: [i32; 4]) -> Gu12Claim {
        Gu12Claim {
            claim_number: "1".into(),
            loader_name: name.into(),
            loader_okpo: okpo.into(),
            station_code: station.into(),
            cars,
            ..Default::default()
        }
    }

    fn foreign() -> HashSet<String> {
        ["КЗХ", "БЕЛ"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn periods_arg_matches_demand_periods() {
        let s = periods_arg();
        assert_eq!(s.split(',').count(), 4);
        assert!(s.starts_with(&format!("{}:{}", DEMAND_PERIODS[0].0, DEMAND_PERIODS[0].1)));
    }

    #[test]
    fn okpo_and_name_normalization() {
        assert_eq!(normalize_okpo(" 00335717 "), "335717");
        assert_eq!(normalize_okpo("abc"), "");
        assert_eq!(
            normalize_party_name("Общество с ограниченной ответственностью \"Раевский элеватор\""),
            normalize_party_name("ООО «Раевский Элеватор»")
        );
        assert_ne!(normalize_party_name("ООО Альфа"), normalize_party_name("ООО Бета"));
    }

    #[test]
    fn split_proportional_preserves_total() {
        assert_eq!(split_proportional(10, &[1, 1, 1]), vec![4, 3, 3]);
        assert_eq!(split_proportional(7, &[0, 0]), vec![4, 3]);
        assert_eq!(split_proportional(0, &[5, 5]), vec![0, 0]);
        assert_eq!(split_proportional(5, &[30, 10]).iter().sum::<i32>(), 5);
        assert_eq!(split_proportional(5, &[]), Vec::<i32>::new());
    }

    #[test]
    fn caps_by_okpo_and_removes_without_claim() {
        let mut demand = vec![
            node(1, 1, "583506", "ЮВС", Some("АО Кристалл"), Some("00335717"), 20),
            node(2, 1, "583506", "ЮВС", Some("ООО Другой"), Some("111"), 10),
            node(3, 2, "583506", "ЮВС", Some("АО Кристалл"), Some("335717"), 5),
            node(4, 1, "999999", "МСК", Some("Без заявки"), Some("222"), 8),
        ];
        let claims = vec![claim("583506", "Акционерное общество КРИСТАЛЛ", "00335717", [7, 9, 0, 0])];
        let st = apply_gu12_limits(&mut demand, &claims, &foreign(), Gu12Mode::Strong);

        // Узел 1: спрос АПИ 20, потолок ГУ-12 7. Узел 2: другой ОКПО, пула нет → потолок 0,
        // узел остаётся. Узел 3 (период 2): потолок 9 ≥ спроса 5. Узел 4: станция без заявок → 0.
        assert_eq!(demand.len(), 4);
        assert_eq!(demand[0].car_count, 20);
        assert_eq!(demand[0].gu12_cap, Some(7));
        assert_eq!(demand[1].car_count, 10);
        assert_eq!(demand[1].gu12_cap, Some(0));
        assert_eq!(demand[2].car_count, 5);
        assert_eq!(demand[2].gu12_cap, Some(9));
        assert_eq!(demand[3].gu12_cap, Some(0));
        assert_eq!(demand[0].d_id, 1);
        assert_eq!(demand[2].d_id, 3);
        assert_eq!(st.nodes_before, 4);
        assert_eq!(st.cars_before, 43);
        assert_eq!(st.nodes_after, 4);
        assert_eq!(st.cars_after, 12);
        assert_eq!(st.nodes_matched_okpo, 2);
        assert_eq!(st.nodes_removed, 2);
        assert_eq!(st.cars_removed, 18);
        assert_eq!(st.nodes_capped, 1);
        assert_eq!(st.cars_cut, 13);
        assert_eq!(st.cars_before_by_period, [38, 5, 0, 0]);
        assert_eq!(st.cars_after_by_period, [7, 5, 0, 0]);
    }

    #[test]
    fn falls_back_to_name_when_no_okpo() {
        let mut demand = vec![
            node(1, 1, "603409", "МСК", Some("АО \"Избердеевский элеватор\""), None, 30),
        ];
        let claims = vec![claim("603409", "Акционерное общество «Избердеевский элеватор»", "", [20, 0, 0, 0])];
        let st = apply_gu12_limits(&mut demand, &claims, &foreign(), Gu12Mode::Strong);
        assert_eq!(demand[0].car_count, 30);
        assert_eq!(demand[0].gu12_cap, Some(20));
        assert_eq!(st.nodes_matched_name, 1);
        assert_eq!(st.nodes_matched_okpo, 0);
    }

    #[test]
    fn unmatched_claims_spread_proportionally_over_station() {
        let mut demand = vec![
            node(1, 1, "811407", "ЮУР", Some("Элеватор А"), Some("1"), 30),
            node(2, 1, "811407", "ЮУР", Some("Элеватор Б"), Some("2"), 10),
        ];
        // Заявка от третьего лица без ОКПО и с чужим именем → пул 20 → 15 / 5.
        let claims = vec![claim("811407", "ООО Трейдер", "", [20, 0, 0, 0])];
        let st = apply_gu12_limits(&mut demand, &claims, &foreign(), Gu12Mode::Strong);
        assert_eq!(demand[0].car_count, 30);
        assert_eq!(demand[1].car_count, 10);
        assert_eq!(demand[0].gu12_cap, Some(15));
        assert_eq!(demand[1].gu12_cap, Some(5));
        assert_eq!(st.nodes_pool_only, 2);
        assert_eq!(st.cars_after, 20);
    }

    #[test]
    fn okpo_match_shared_between_nodes_of_same_sender() {
        // Один грузоотправитель, два узла (разные направления) — заявка делится по спросу.
        let mut demand = vec![
            node(1, 1, "657004", "КБШ", Some("Раевский"), Some("77697508"), 30),
            node(2, 1, "657004", "КБШ", Some("Раевский"), Some("77697508"), 10),
        ];
        let claims = vec![claim("657004", "ООО Раевский элеватор", "77697508", [8, 0, 0, 0])];
        apply_gu12_limits(&mut demand, &claims, &foreign(), Gu12Mode::Strong);
        assert_eq!(demand[0].car_count, 30);
        assert_eq!(demand[1].car_count, 10);
        assert_eq!(demand[0].gu12_cap, Some(6));
        assert_eq!(demand[1].gu12_cap, Some(2));

        // Заявка больше спроса — спрос АПИ не растёт, потолок может быть выше него.
        let mut demand = vec![node(1, 1, "657004", "КБШ", Some("Раевский"), Some("77697508"), 3)];
        apply_gu12_limits(&mut demand, &claims, &foreign(), Gu12Mode::Strong);
        assert_eq!(demand[0].car_count, 3);
        assert_eq!(demand[0].gu12_cap, Some(8));
    }

    /// Пустой список инотерриторий трактует все узлы как российские: казахстанский
    /// спрос без заявки получает потолок 0 (узел остаётся для вагонов с инотерриторий,
    /// но список пуст — таких исключений нет). Поэтому `main` не вызывает эту функцию,
    /// пока [`crate::data::BusinessRules::gu12_ready`] ложно.
    #[test]
    fn empty_foreign_list_treats_all_nodes_as_russia() {
        let mut demand = vec![node(1, 1, "687103", "КЗХ", Some("ТОО"), None, 40)];
        let st = apply_gu12_limits(&mut demand, &[], &HashSet::new(), Gu12Mode::Strong);
        assert_eq!(demand.len(), 1);
        assert_eq!(demand[0].car_count, 40);
        assert_eq!(demand[0].gu12_cap, Some(0));
        assert_eq!(st.nodes_foreign, 0);
        assert_eq!(st.nodes_removed, 1);
    }

    #[test]
    fn foreign_nodes_are_not_touched() {
        let mut demand = vec![
            node(1, 1, "687103", "КЗХ", Some("ТОО"), None, 40),
            node(2, 1, "583506", "ЮВС", Some("АО Кристалл"), Some("335717"), 20),
        ];
        let st = apply_gu12_limits(&mut demand, &[], &foreign(), Gu12Mode::Strong);
        assert_eq!(demand.len(), 2);
        assert_eq!(demand[0].railway_name, "КЗХ");
        assert_eq!(demand[0].car_count, 40);
        assert_eq!(demand[0].gu12_cap, None);
        assert_eq!(demand[1].car_count, 20);
        assert_eq!(demand[1].gu12_cap, Some(0));
        assert_eq!(st.nodes_foreign, 1);
        assert_eq!(st.cars_foreign, 40);
        assert_eq!(st.nodes_removed, 1);
    }

    /// Инотерритория определяется только по дороге узла из списка: код станции
    /// (казахстанский район 68) сам по себе узел не освобождает; пробелы в коде дороги — не помеха.
    #[test]
    fn foreign_only_by_railway_list_not_by_station_code() {
        let mut demand = vec![
            node(1, 1, "687103", "", Some("ТОО"), None, 40),          // дорога не указана
            node(2, 1, "687103", " КЗХ ", Some("ТОО"), None, 30),     // КЗХ с пробелами
        ];
        let st = apply_gu12_limits(&mut demand, &[], &foreign(), Gu12Mode::Strong);
        assert_eq!(demand.len(), 2);
        assert_eq!(demand[0].gu12_cap, Some(0));
        assert_eq!(demand[1].railway_name, " КЗХ ");
        assert_eq!(demand[1].car_count, 30);
        assert_eq!(demand[1].gu12_cap, None);
        assert_eq!(st.nodes_foreign, 1);
        assert_eq!(st.nodes_removed, 1);
    }

    #[test]
    fn wash_nodes_ignored() {
        let mut w = node(1, 1, "100000", "МСК", None, None, 50);
        w.purpose = DemandPurpose::Wash;
        let mut demand = vec![w];
        apply_gu12_limits(&mut demand, &[], &foreign(), Gu12Mode::Strong);
        assert_eq!(demand.len(), 1);
        assert_eq!(demand[0].car_count, 50);
        assert_eq!(demand[0].gu12_cap, None);
    }

    #[test]
    fn parses_script_json_shape() {
        let json = r#"[{"ClaimNumber":"0000549300","LoaderName":"АО \"КРИСТАЛЛ\"","LoaderOkpo":"00335717",
            "StationName":"КАЛАЧ","StationCode":"583506","StationToName":"X","StationToCode":"521001",
            "SendKind":"Групповая","Etsng":"ЖОМ","EtsngCode":"542034","FinishDate":"2026-09-30",
            "TotalCars":7,"Cars":[7,0,0,0]}]"#;
        let claims: Vec<Gu12Claim> = serde_json::from_str(json).unwrap();
        assert_eq!(claims[0].station_code, "583506");
        assert_eq!(claims[0].cars, [7, 0, 0, 0]);
        assert_eq!(claims[0].loader_okpo, "00335717");
    }

    /// Заявка только на дальние сутки покрывает ближний спрос: горизонт не режется.
    #[test]
    fn relaxed_keeps_demand_when_horizon_claim_covers_it() {
        let mut demand = vec![
            node(1, 1, "583506", "ЮВС", Some("АО Кристалл"), Some("111"), 10),
            node(2, 4, "583506", "ЮВС", Some("АО Кристалл"), Some("111"), 20),
        ];
        let claims = vec![claim("583506", "АО Кристалл", "111", [0, 0, 0, 30])];
        apply_gu12_limits(&mut demand, &claims, &foreign(), Gu12Mode::Relaxed);
        assert_eq!(demand[0].car_count, 10);
        assert_eq!(demand[1].car_count, 20);
        assert_eq!(demand[0].gu12_cap, Some(10));
        assert_eq!(demand[1].gu12_cap, Some(20));
    }

    /// Та же заявка в жёстком режиме не переносится на другой период.
    #[test]
    fn strong_does_not_move_claim_between_periods() {
        let mut demand = vec![
            node(1, 1, "583506", "ЮВС", Some("АО Кристалл"), Some("111"), 10),
            node(2, 4, "583506", "ЮВС", Some("АО Кристалл"), Some("111"), 20),
        ];
        let claims = vec![claim("583506", "АО Кристалл", "111", [0, 0, 0, 30])];
        apply_gu12_limits(&mut demand, &claims, &foreign(), Gu12Mode::Strong);
        assert_eq!(demand[0].gu12_cap, Some(0));
        assert_eq!(demand[1].gu12_cap, Some(30));
    }

    /// Нехватка 25 вагонов: период 11–15 и 9–10 зануляются, в 6–8 остаётся 5, 1–5 не трогаем.
    #[test]
    fn relaxed_deficit_is_taken_from_far_periods_first() {
        let mut demand = vec![
            node(1, 1, "583506", "ЮВС", Some("АО Кристалл"), Some("111"), 10),
            node(2, 2, "583506", "ЮВС", Some("АО Кристалл"), Some("111"), 10),
            node(3, 3, "583506", "ЮВС", Some("АО Кристалл"), Some("111"), 10),
            node(4, 4, "583506", "ЮВС", Some("АО Кристалл"), Some("111"), 10),
        ];
        let claims = vec![claim("583506", "АО Кристалл", "111", [15, 0, 0, 0])];
        let st = apply_gu12_limits(&mut demand, &claims, &foreign(), Gu12Mode::Relaxed);
        assert_eq!(demand[0].gu12_cap, Some(10));
        assert_eq!(demand[1].gu12_cap, Some(5));
        assert_eq!(demand[2].gu12_cap, Some(0));
        assert_eq!(demand[3].gu12_cap, Some(0));
        assert!(demand.iter().all(|d| d.car_count == 10));
        assert_eq!(st.cars_after, 15);
        assert_eq!(st.cars_after_by_period, [10, 5, 0, 0]);
    }

    /// Частичный срез дальнего периода не доходит до ближнего.
    #[test]
    fn relaxed_partial_far_period_stops_there() {
        let mut demand = vec![
            node(1, 1, "583506", "ЮВС", Some("АО Кристалл"), Some("111"), 10),
            node(2, 4, "583506", "ЮВС", Some("АО Кристалл"), Some("111"), 6),
            node(3, 4, "583506", "ЮВС", Some("АО Кристалл"), Some("111"), 4),
        ];
        // Спрос 20, заявка 15, нехватка 5 — оба узла периода 4, доли 3 и 2.
        let claims = vec![claim("583506", "АО Кристалл", "111", [15, 0, 0, 0])];
        apply_gu12_limits(&mut demand, &claims, &foreign(), Gu12Mode::Relaxed);
        assert_eq!(demand[0].gu12_cap, Some(10));
        assert_eq!(demand[1].gu12_cap, Some(3));
        assert_eq!(demand[2].gu12_cap, Some(2));
    }

    /// Заявка одного грузоотправителя не закрывает спрос другого.
    #[test]
    fn relaxed_senders_do_not_share_a_claim() {
        let mut demand = vec![
            node(1, 1, "583506", "ЮВС", Some("АО Кристалл"), Some("111"), 10),
            node(2, 4, "583506", "ЮВС", Some("ООО Другой"), Some("222"), 20),
        ];
        let claims = vec![claim("583506", "АО Кристалл", "111", [100, 0, 0, 0])];
        apply_gu12_limits(&mut demand, &claims, &foreign(), Gu12Mode::Relaxed);
        assert_eq!(demand[0].gu12_cap, Some(10));
        assert_eq!(demand[1].gu12_cap, Some(0));
    }
}
