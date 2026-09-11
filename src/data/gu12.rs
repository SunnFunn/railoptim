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
//! погрузки: для каждого узла (станция погрузки, период) верхняя граница подсыла
//! `car_count` ограничивается разрешённой в ГУ-12 погрузкой. Сопоставление —
//! по коду станции погрузки и ОКПО грузоотправителя (`DemandNode::sender_okpo` ↔
//! `LoaderFromOKPO`); без ОКПО — по имени грузоотправителя; заявки, не привязанные
//! ни к одному узлу станции, распределяются пропорционально спросу по всем узлам
//! этой станции. Узлы без разрешённой погрузки (0) исключаются.
//!
//! Проверка относится только к территории России: узлы на дорогах-инотерриториях
//! (список `ForeignRoads` из `data/references.json`, см.
//! [`super::references::load_foreign_roads`]) не корректируются. Дорога узла —
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
    /// Узлы без разрешённой погрузки (исключены).
    pub nodes_removed: usize,
    pub cars_removed: i32,
    /// Узлы, у которых спрос урезан до ГУ-12 (остались с `car_count > 0`).
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

/// Ограничивает спрос погрузки разрешённой в ГУ-12 погрузкой (правило 3).
///
/// - `demand` — узлы спроса (обрабатываются только `purpose == Load`);
/// - `claims` — согласованные заявки ГУ-12 ([`fetch_gu12_claims`]);
/// - `foreign_railways` — короткие коды дорог-инотерриторий (`ForeignRoads` из
///   `data/references.json`): узлы с такой `railway_name` не корректируются.
///
/// Узлы с итоговым `car_count == 0` удаляются, `d_id` перенумеровываются с 1
/// в исходном порядке (узлы промывки к этому моменту ещё не созданы).
pub fn apply_gu12_limits(
    demand: &mut Vec<DemandNode>,
    claims: &[Gu12Claim],
    foreign_railways: &HashSet<String>,
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

    // Группы узлов (станция, период) — только погрузка на российских дорогах.
    let mut groups: HashMap<(String, usize), Vec<usize>> = HashMap::new();
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
        groups.entry((code, p)).or_default().push(i);
    }
    st.claims_without_demand = claims
        .iter()
        .filter(|c| !demand_stations.contains(&normalize_esr6(&c.station_code)))
        .count();

    // Разрешённая погрузка по узлам.
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
                    .filter(|(_, i)| {
                        demand[**i]
                            .sender_okpo
                            .as_deref()
                            .is_some_and(|o| normalize_okpo(o) == okpo)
                    })
                    .map(|(k, _)| k)
                    .collect();
            }
            // 2) Имя грузоотправителя.
            if hit.is_empty() && !name.is_empty() {
                kind = Gu12Match::Name;
                hit = idx
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| {
                        demand[**i]
                            .sender
                            .as_deref()
                            .is_some_and(|n| normalize_party_name(n) == name)
                    })
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

    // Применяем: car_count = min(car_count, allowance).
    for (i, d) in demand.iter_mut().enumerate() {
        let Some(&a) = allowance.get(&i) else { continue };
        match matched[&i] {
            Gu12Match::Okpo => st.nodes_matched_okpo += 1,
            Gu12Match::Name => st.nodes_matched_name += 1,
            Gu12Match::PoolOnly => st.nodes_pool_only += 1,
            Gu12Match::NoClaim => {}
        }
        let new = d.car_count.min(a.max(0));
        if new <= 0 {
            st.nodes_removed += 1;
            st.cars_removed += d.car_count;
        } else if new < d.car_count {
            st.nodes_capped += 1;
            st.cars_cut += d.car_count - new;
        }
        d.car_count = new;
    }

    demand.retain(|d| d.car_count > 0);
    for (i, d) in demand.iter_mut().enumerate() {
        d.d_id = i + 1;
    }

    st.nodes_after = demand.len();
    st.cars_after = demand.iter().map(|d| d.car_count).sum();
    for d in demand.iter() {
        if let Some(p) = period_slot(d.period) {
            st.cars_after_by_period[p] += d.car_count;
        }
    }
    st
}

/// Узел спроса вне территории России: дорога погрузки входит в список инотерриторий.
pub fn is_foreign_node(d: &DemandNode, foreign_railways: &HashSet<String>) -> bool {
    foreign_railways.contains(d.railway_name.trim())
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
        let st = apply_gu12_limits(&mut demand, &claims, &foreign());

        // Узел 1: 20 → 7 (ОКПО). Узел 2: другой ОКПО, пула нет → 0, исключён.
        // Узел 3 (период 2): 5 ≤ 9 — без изменений. Узел 4: станция без заявок → исключён.
        assert_eq!(demand.len(), 2);
        assert_eq!(demand[0].car_count, 7);
        assert_eq!(demand[1].car_count, 5);
        assert_eq!(demand[0].d_id, 1);
        assert_eq!(demand[1].d_id, 2);
        assert_eq!(st.nodes_before, 4);
        assert_eq!(st.cars_before, 43);
        assert_eq!(st.nodes_after, 2);
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
        let st = apply_gu12_limits(&mut demand, &claims, &foreign());
        assert_eq!(demand[0].car_count, 20);
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
        let st = apply_gu12_limits(&mut demand, &claims, &foreign());
        assert_eq!(demand[0].car_count, 15);
        assert_eq!(demand[1].car_count, 5);
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
        apply_gu12_limits(&mut demand, &claims, &foreign());
        assert_eq!(demand[0].car_count, 6);
        assert_eq!(demand[1].car_count, 2);

        // Заявка больше спроса — спрос не растёт.
        let mut demand = vec![node(1, 1, "657004", "КБШ", Some("Раевский"), Some("77697508"), 3)];
        apply_gu12_limits(&mut demand, &claims, &foreign());
        assert_eq!(demand[0].car_count, 3);
    }

    #[test]
    fn foreign_nodes_are_not_touched() {
        let mut demand = vec![
            node(1, 1, "687103", "КЗХ", Some("ТОО"), None, 40),
            node(2, 1, "583506", "ЮВС", Some("АО Кристалл"), Some("335717"), 20),
        ];
        let st = apply_gu12_limits(&mut demand, &[], &foreign());
        assert_eq!(demand.len(), 1);
        assert_eq!(demand[0].railway_name, "КЗХ");
        assert_eq!(demand[0].car_count, 40);
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
        let st = apply_gu12_limits(&mut demand, &[], &foreign());
        assert_eq!(demand.len(), 1);
        assert_eq!(demand[0].railway_name, " КЗХ ");
        assert_eq!(demand[0].car_count, 30);
        assert_eq!(st.nodes_foreign, 1);
        assert_eq!(st.nodes_removed, 1);
    }

    #[test]
    fn wash_nodes_ignored() {
        let mut w = node(1, 1, "100000", "МСК", None, None, 50);
        w.purpose = DemandPurpose::Wash;
        let mut demand = vec![w];
        apply_gu12_limits(&mut demand, &[], &foreign());
        assert_eq!(demand.len(), 1);
        assert_eq!(demand[0].car_count, 50);
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
}
