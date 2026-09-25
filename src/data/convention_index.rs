//! Шаг 4 правила 5: индекс действующих конвенций для `classify_pair`.
//!
//! Не сканируем весь список на каждую пару supply×demand: кандидаты берутся по
//! ЕСР станции погрузки, ЕСР назначения груза и коротким кодам дорог. Фильтр
//! «все грузополучатели» vs ОКПО/имя применяется к уже найденным кандидатам.
//! Ограничение «на 50% от плана» в JSON нет — в v1 такая телеграмма = жёсткий запрет.
//!
//! Даты. Конвенция — запрет **приёма к перевозке** в период `date_beg…date_end`, поэтому
//! на дуге проверяется пересечение окна телеграммы с окном операции, а не одна точка:
//! - порожний (Empty-ветка, промывка/ремонт/отстой): `отправление…прибытие` — вагон,
//!   отправленный в период запрета или прибывающий в него, могут задержать;
//! - груз (Grain/All-ветка, назначение груза): окно погрузки `max(прибытие, L)…max(прибытие, U)`
//!   по периоду спроса — груз предъявляется к перевозке на станции погрузки.

use std::collections::{HashMap, HashSet};

use chrono::{Duration, Local, NaiveDate};

use crate::node::{DemandNode, DemandPurpose, SupplyNode};

use super::conventions::{ConventionCargoClass, ConventionStatus, ParsedConvention};
use super::esr::normalize_esr6;
use super::gu12::{normalize_okpo, normalize_party_name};

/// Куда относится запрет: погрузка или служебные назначения порожняка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConventionScope {
    Load,
    Wash,
    Repair,
    Reserve,
}

/// Станция, куда едет порожний (ремонт / отстой — не [`DemandNode`]).
#[derive(Debug, Clone, Copy)]
pub struct EmptyDestRef<'a> {
    /// Дорога и ЕСР станции, откуда отправляется порожний (для dep-списков телеграммы).
    pub supply_railway: &'a str,
    pub supply_station_code: &'a str,
    pub station_code: &'a str,
    pub station_name: &'a str,
    pub railway: &'a str,
    pub sender_okpo: Option<&'a str>,
    pub sender_name: Option<&'a str>,
    pub recipient_okpos: &'a [String],
    pub recipient_names: &'a [String],
}

/// Сроки движения по дуге в сутках от сегодня — для пересечения с датами телеграммы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArcTiming {
    /// Сутки отправления порожнего: `0` — АПИ, `5` — дислокация (`supply_period == 10`).
    pub dispatch_day: i32,
    /// Сутки прибытия на станцию назначения порожнего (`dispatch_day + срок доставки`).
    pub arrival_day: i32,
    /// Окно погрузки по периоду спроса `[L, U]` (только Load). `None` — погрузка «по прибытии».
    pub load_window: Option<(i32, i32)>,
}

impl ArcTiming {
    /// Порожний на служебную станцию: окна погрузки нет.
    pub fn empty_only(dispatch_day: i32, arrival_day: i32) -> Self {
        Self {
            dispatch_day,
            arrival_day,
            load_window: None,
        }
    }
}

/// Размеры индекса — в лог старта и дамп.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConventionIndexStats {
    pub rules: usize,
    pub empty_load_esr_keys: usize,
    pub grain_cargo_esr_keys: usize,
    pub dest_railway_keys: usize,
    pub dep_railway_keys: usize,
    pub dep_esr_keys: usize,
    pub road_pair_rules: usize,
    pub all_parties: usize,
    pub with_party_filter: usize,
    pub wash_rules: usize,
    pub repair_rules: usize,
    pub reserve_rules: usize,
    /// Действующие правила, которые не дали ни одного ключа индекса (не применяются).
    pub not_indexed: usize,
}

/// Индекс запретов по ЕСР и дорогам. Пустой ([`Self::disabled`]) — правило не действует.
#[derive(Debug, Clone, Default)]
pub struct ConventionIndex {
    rules: Vec<ParsedConvention>,
    /// Empty/All: ЕСР станции погрузки (назначение порожняка в телеграмме).
    empty_by_load_esr: HashMap<String, Vec<usize>>,
    empty_by_load_name: HashMap<String, Vec<usize>>,
    /// Grain/All: ЕСР станции назначения груза.
    grain_by_cargo_esr: HashMap<String, Vec<usize>>,
    grain_by_cargo_name: HashMap<String, Vec<usize>>,
    /// Empty/All, только dest-дороги: `DemandNode.railway_name`.
    empty_by_dest_rw: HashMap<String, Vec<usize>>,
    /// Grain/All, только dest-дороги: `DemandNode.railway_to_name`.
    grain_by_dest_rw: HashMap<String, Vec<usize>>,
    /// All/Grain, только dep-дороги: `DemandNode.railway_name`.
    all_by_dep_rw: HashMap<String, Vec<usize>>,
    /// All/Grain, только dep-ЕСР («запрет погрузки со станции X»): `DemandNode.station_code`.
    all_by_dep_esr: HashMap<String, Vec<usize>>,
    /// Empty/All, оба списка дорог: dest = дорога погрузки, затем dep = дорога supply.
    empty_pair_by_dest_rw: HashMap<String, Vec<usize>>,
    /// All/Grain, оба списка: dest = дорога назначения груза, затем dep = дорога погрузки.
    all_pair_by_dest_rw: HashMap<String, Vec<usize>>,
    /// Правила без ключей (для лога старта).
    not_indexed: Vec<usize>,
    /// День прогона (для пересечения с `date_beg…date_end` на дуге). `None` — даты не фильтруем.
    today: Option<NaiveDate>,
    pub stats: ConventionIndexStats,
}

impl ConventionIndex {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn today(&self) -> NaiveDate {
        self.today.unwrap_or_else(|| Local::now().date_naive())
    }

    /// Действующие правила, не давшие ни одного ключа (например, Empty только с дорогами отправления).
    pub fn not_indexed_rules(&self) -> impl Iterator<Item = &ParsedConvention> {
        self.not_indexed.iter().map(|&i| &self.rules[i])
    }

    pub fn summary_line(&self) -> String {
        let st = &self.stats;
        format!(
            "индекс: {} правил; ЕСР погрузки {}, ЕСР назн.груза {}, dest-дороги {}, dep-дороги {}, dep-ЕСР {}, пары dep→dest {}; все получатели {}, с ОКПО/именем {}; без ключей {}",
            st.rules,
            st.empty_load_esr_keys,
            st.grain_cargo_esr_keys,
            st.dest_railway_keys,
            st.dep_railway_keys,
            st.dep_esr_keys,
            st.road_pair_rules,
            st.all_parties,
            st.with_party_filter,
            st.not_indexed,
        )
    }

    /// ЕСР, дороги и пары dep→dest для лога старта.
    pub fn log_geography(&self) {
        fn preview(keys: impl Iterator<Item = String>, n: usize) -> (Vec<String>, usize) {
            let mut v: Vec<String> = keys.collect();
            v.sort();
            let extra = v.len().saturating_sub(n);
            v.truncate(n);
            (v, extra)
        }
        fn tail(extra: usize) -> String {
            if extra > 0 {
                format!(" …ещё {extra}")
            } else {
                String::new()
            }
        }
        let (esr, extra) = preview(self.empty_by_load_esr.keys().cloned(), 15);
        if !esr.is_empty() {
            println!("  ЕСР погрузки (Empty/All): {}{}", esr.join(", "), tail(extra));
        }
        let (gesr, extra) = preview(self.grain_by_cargo_esr.keys().cloned(), 15);
        if !gesr.is_empty() {
            println!("  ЕСР назн.груза (Grain/All): {}{}", gesr.join(", "), tail(extra));
        }
        let st = &self.stats;
        if st.wash_rules + st.repair_rules + st.reserve_rules > 0 {
            println!(
                "  служебные станции: промывка {}, ремонт {}, отстой {}",
                st.wash_rules, st.repair_rules, st.reserve_rules,
            );
        }
        let (dest_rw, extra) = preview(
            self.empty_by_dest_rw
                .keys()
                .chain(self.grain_by_dest_rw.keys())
                .chain(self.empty_pair_by_dest_rw.keys())
                .chain(self.all_pair_by_dest_rw.keys())
                .cloned()
                .collect::<HashSet<_>>()
                .into_iter(),
            15,
        );
        if !dest_rw.is_empty() {
            println!("  dest-дороги: {}{}", dest_rw.join(", "), tail(extra));
        }
        let (dep_rw, extra) = preview(self.all_by_dep_rw.keys().cloned(), 15);
        if !dep_rw.is_empty() {
            println!("  dep-дороги (All/Grain без dest): {}{}", dep_rw.join(", "), tail(extra));
        }
        let (dep_esr, extra) = preview(self.all_by_dep_esr.keys().cloned(), 15);
        if !dep_esr.is_empty() {
            println!("  dep-ЕСР (All/Grain, запрет погрузки со станций): {}{}", dep_esr.join(", "), tail(extra));
        }
        let pairs: Vec<&ParsedConvention> = self
            .rules
            .iter()
            .filter(|r| r.dest_all_stations && !r.dest_railways.is_empty() && !r.dep_railways.is_empty())
            .collect();
        if !pairs.is_empty() {
            println!("  пары дорог dep→dest (как 4702):");
            for r in pairs.iter().take(15) {
                println!(
                    "    · №{} {} → {}",
                    r.rzd_number,
                    r.dep_railways.join(","),
                    r.dest_railways.join(","),
                );
            }
            if pairs.len() > 15 {
                println!("    · ...ещё {} пар", pairs.len() - 15);
            }
        }
        for r in self.not_indexed_rules() {
            eprintln!(
                "  [!] №{} ({:?}) не даёт ключей индекса — не применяется (Empty только с дорогами/ЕСР отправления?)",
                r.rzd_number, r.cargo_class,
            );
        }
    }

    pub fn build(active: Vec<ParsedConvention>) -> Self {
        Self::build_at(active, Local::now().date_naive())
    }

    pub fn build_at(active: Vec<ParsedConvention>, today: NaiveDate) -> Self {
        let mut idx = Self {
            stats: ConventionIndexStats {
                rules: active.len(),
                ..Default::default()
            },
            rules: active,
            today: Some(today),
            ..Default::default()
        };
        for i in 0..idx.rules.len() {
            if !idx.index_rule(i) {
                idx.not_indexed.push(i);
            }
        }
        idx.stats.not_indexed = idx.not_indexed.len();
        idx.stats.empty_load_esr_keys = idx.empty_by_load_esr.len();
        idx.stats.grain_cargo_esr_keys = idx.grain_by_cargo_esr.len();
        let mut dest_keys: HashSet<&str> = HashSet::new();
        dest_keys.extend(idx.empty_by_dest_rw.keys().map(String::as_str));
        dest_keys.extend(idx.grain_by_dest_rw.keys().map(String::as_str));
        dest_keys.extend(idx.empty_pair_by_dest_rw.keys().map(String::as_str));
        dest_keys.extend(idx.all_pair_by_dest_rw.keys().map(String::as_str));
        idx.stats.dest_railway_keys = dest_keys.len();
        idx.stats.dep_railway_keys = idx.all_by_dep_rw.len();
        idx.stats.dep_esr_keys = idx.all_by_dep_esr.len();
        idx
    }

    /// Кладёт правило в карты кандидатов. `false` — ни одного ключа (правило не применяется).
    fn index_rule(&mut self, i: usize) -> bool {
        let r = self.rules[i].clone();
        if r.all_parties {
            self.stats.all_parties += 1;
        } else {
            self.stats.with_party_filter += 1;
        }
        match r.convention_info {
            ConventionStatus::WashingStation => self.stats.wash_rules += 1,
            ConventionStatus::RepairStation => self.stats.repair_rules += 1,
            ConventionStatus::ReserveStation => self.stats.reserve_rules += 1,
            _ => {}
        }

        let empty_fx = r.cargo_class.has_empty_effect();
        let grain_fx = r.cargo_class.has_cargo_effect();
        let mut pushed = false;

        if is_station_level(&r) {
            if empty_fx {
                for esr in &r.dest_esr {
                    pushed |= push_id(&mut self.empty_by_load_esr, esr.clone(), i);
                }
                for name in &r.dest_names {
                    pushed |= push_id(&mut self.empty_by_load_name, normalize_station_name(name), i);
                }
            }
            if grain_fx {
                for esr in &r.dest_esr {
                    pushed |= push_id(&mut self.grain_by_cargo_esr, esr.clone(), i);
                }
                for name in &r.dest_names {
                    pushed |= push_id(&mut self.grain_by_cargo_name, normalize_station_name(name), i);
                }
            }
            return pushed;
        }

        if r.dest_all_stations && !r.dest_railways.is_empty() {
            if !r.dep_railways.is_empty() {
                self.stats.road_pair_rules += 1;
                // Empty + оба списка: подсыл с dep-дорог на dest-дороги.
                // All + оба списка (4702): только пара «погрузка с dep → груз на dest»,
                // без закрытия всей dest-дороги для порожняка.
                if r.cargo_class == ConventionCargoClass::Empty {
                    for rw in &r.dest_railways {
                        pushed |= push_id(&mut self.empty_pair_by_dest_rw, rw.clone(), i);
                    }
                }
                if grain_fx {
                    for rw in &r.dest_railways {
                        pushed |= push_id(&mut self.all_pair_by_dest_rw, rw.clone(), i);
                    }
                }
            } else {
                if empty_fx {
                    for rw in &r.dest_railways {
                        pushed |= push_id(&mut self.empty_by_dest_rw, rw.clone(), i);
                    }
                }
                if grain_fx {
                    for rw in &r.dest_railways {
                        pushed |= push_id(&mut self.grain_by_dest_rw, rw.clone(), i);
                    }
                }
            }
            return pushed;
        }

        // Только отправление: запрет погрузки с дорог / станций dep (Grain/All).
        // Empty только с отправлением — не про подсыл «на», в v1 не применяем.
        if grain_fx {
            for rw in &r.dep_railways {
                pushed |= push_id(&mut self.all_by_dep_rw, rw.clone(), i);
            }
            for esr in &r.dep_esr {
                pushed |= push_id(&mut self.all_by_dep_esr, esr.clone(), i);
            }
        }
        pushed
    }

    /// Первый запрет для пары supply×demand без учёта дат (тесты / диагностика).
    pub fn ban_for(&self, s: &SupplyNode, d: &DemandNode) -> Option<&ParsedConvention> {
        self.ban_for_pair(s, d, None)
    }

    /// Первый запрет для дуги supply×demand (погрузка или промывка) с учётом сроков движения.
    pub fn ban_for_arc(&self, s: &SupplyNode, d: &DemandNode, timing: ArcTiming) -> Option<&ParsedConvention> {
        self.ban_for_pair(s, d, Some(timing))
    }

    fn ban_for_pair(&self, s: &SupplyNode, d: &DemandNode, timing: Option<ArcTiming>) -> Option<&ParsedConvention> {
        if self.rules.is_empty() {
            return None;
        }
        let scope = match d.purpose {
            DemandPurpose::Load => ConventionScope::Load,
            DemandPurpose::Wash => ConventionScope::Wash,
        };
        let (empty_window, cargo_window) = self.windows(timing);
        self.ban_query(&Query {
            supply_rw: &s.railway_to,
            supply_code: &s.station_to_code,
            load_code: &d.station_code,
            load_name: &d.station_name,
            load_rw: &d.railway_name,
            cargo_code: d.station_to_code.as_deref().unwrap_or(""),
            cargo_name: d.station_to_name.as_deref().unwrap_or(""),
            cargo_rw: d.railway_to_name.as_deref().unwrap_or(""),
            sender_okpo: d.sender_okpo.as_deref(),
            sender_name: d.sender.as_deref(),
            rec_okpo: d.loader_to_okpo.as_deref().unwrap_or(&[]),
            rec_names: d.recipient.as_deref().unwrap_or(&[]),
            scope,
            empty_window,
            cargo_window,
        })
    }

    /// Empty-запрет на станцию ремонта или отстоя без учёта дат.
    pub fn ban_for_empty_dest(&self, dest: EmptyDestRef<'_>, scope: ConventionScope) -> Option<&ParsedConvention> {
        self.ban_for_empty_dest_impl(dest, scope, None)
    }

    /// Empty-запрет на станцию ремонта или отстоя с учётом сроков `отправление…прибытие`.
    pub fn ban_for_empty_dest_timed(
        &self,
        dest: EmptyDestRef<'_>,
        scope: ConventionScope,
        dispatch_day: i32,
        arrival_day: i32,
    ) -> Option<&ParsedConvention> {
        self.ban_for_empty_dest_impl(dest, scope, Some(ArcTiming::empty_only(dispatch_day, arrival_day)))
    }

    fn ban_for_empty_dest_impl(
        &self,
        dest: EmptyDestRef<'_>,
        scope: ConventionScope,
        timing: Option<ArcTiming>,
    ) -> Option<&ParsedConvention> {
        if self.rules.is_empty() {
            return None;
        }
        let (empty_window, cargo_window) = self.windows(timing);
        self.ban_query(&Query {
            supply_rw: dest.supply_railway,
            supply_code: dest.supply_station_code,
            load_code: dest.station_code,
            load_name: dest.station_name,
            load_rw: dest.railway,
            cargo_code: "",
            cargo_name: "",
            cargo_rw: "",
            sender_okpo: dest.sender_okpo,
            sender_name: dest.sender_name,
            rec_okpo: dest.recipient_okpos,
            rec_names: dest.recipient_names,
            scope,
            empty_window,
            cargo_window,
        })
    }

    /// Окна дат по веткам: порожний `отправление…прибытие`, груз — окно погрузки.
    #[allow(clippy::type_complexity)]
    fn windows(&self, timing: Option<ArcTiming>) -> (Option<(NaiveDate, NaiveDate)>, Option<(NaiveDate, NaiveDate)>) {
        let Some(t) = timing else {
            return (None, None);
        };
        let today = self.today();
        let day = |d: i32| today + Duration::days(i64::from(d.max(0)));
        let arrival = t.arrival_day.max(t.dispatch_day);
        let empty = (day(t.dispatch_day), day(arrival));
        let cargo = match t.load_window {
            Some((l, u)) => (day(arrival.max(l)), day(arrival.max(u.max(l)))),
            None => (day(arrival), day(arrival)),
        };
        (Some(empty), Some(cargo))
    }

    fn ban_query(&self, q: &Query<'_>) -> Option<&ParsedConvention> {
        let mut ids = Vec::new();
        let keys = QueryKeys {
            supply_rw: rw_key(q.supply_rw),
            supply_esr: normalize_esr6(q.supply_code),
            load_esr: normalize_esr6(q.load_code),
            load_name: normalize_station_name(q.load_name),
            load_rw: rw_key(q.load_rw),
            cargo_esr: normalize_esr6(q.cargo_code),
            cargo_name: normalize_station_name(q.cargo_name),
            cargo_rw: rw_key(q.cargo_rw),
        };

        extend_ids(&mut ids, &self.empty_by_load_esr, &keys.load_esr);
        extend_ids(&mut ids, &self.empty_by_load_name, &keys.load_name);
        extend_ids(&mut ids, &self.empty_by_dest_rw, &keys.load_rw);
        extend_ids(&mut ids, &self.empty_pair_by_dest_rw, &keys.load_rw);
        if q.scope == ConventionScope::Load {
            extend_ids(&mut ids, &self.grain_by_cargo_esr, &keys.cargo_esr);
            extend_ids(&mut ids, &self.grain_by_cargo_name, &keys.cargo_name);
            extend_ids(&mut ids, &self.grain_by_dest_rw, &keys.cargo_rw);
            extend_ids(&mut ids, &self.all_by_dep_rw, &keys.load_rw);
            extend_ids(&mut ids, &self.all_by_dep_esr, &keys.load_esr);
            extend_ids(&mut ids, &self.all_pair_by_dest_rw, &keys.cargo_rw);
        }

        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .map(|i| &self.rules[i])
            .find(|r| rule_hits(r, q, &keys))
    }
}

/// Нормализованные ключи запроса (считаются один раз на пару).
struct QueryKeys {
    supply_rw: String,
    supply_esr: String,
    load_esr: String,
    load_name: String,
    load_rw: String,
    cargo_esr: String,
    cargo_name: String,
    cargo_rw: String,
}

struct Query<'a> {
    supply_rw: &'a str,
    supply_code: &'a str,
    load_code: &'a str,
    load_name: &'a str,
    load_rw: &'a str,
    cargo_code: &'a str,
    cargo_name: &'a str,
    cargo_rw: &'a str,
    sender_okpo: Option<&'a str>,
    sender_name: Option<&'a str>,
    rec_okpo: &'a [String],
    rec_names: &'a [String],
    scope: ConventionScope,
    /// Окно движения порожнего (`отправление…прибытие`). `None` — даты не проверяем.
    empty_window: Option<(NaiveDate, NaiveDate)>,
    /// Окно погрузки груза. `None` — даты не проверяем.
    cargo_window: Option<(NaiveDate, NaiveDate)>,
}

impl ConventionCargoClass {
    /// Правило закрывает подсыл порожняка (Empty / All).
    fn has_empty_effect(self) -> bool {
        matches!(self, Self::Empty | Self::All)
    }

    /// Правило закрывает погрузку назначением (Grain / All).
    fn has_cargo_effect(self) -> bool {
        matches!(self, Self::Grain | Self::All)
    }
}

/// Станционное правило: конкретные ЕСР или имена станций назначения.
/// Имена без ЕСР при «все станции …» — не станции, а дороги (страховка от разбора).
fn is_station_level(r: &ParsedConvention) -> bool {
    !r.dest_esr.is_empty() || (!r.dest_names.is_empty() && !r.dest_all_stations)
}

fn rule_hits(r: &ParsedConvention, q: &Query<'_>, k: &QueryKeys) -> bool {
    let empty_fx = r.cargo_class.has_empty_effect() && empty_rule_applies(r, q.scope);
    let cargo_fx = r.cargo_class.has_cargo_effect() && cargo_rule_applies(r, q.scope);

    // Даты по веткам: порожний едет `отправление…прибытие`, груз предъявляется в окно погрузки.
    let empty_fx = empty_fx && q.empty_window.is_none_or(|(a, b)| r.covers_range(a, b));
    let cargo_fx = cargo_fx && q.cargo_window.is_none_or(|(a, b)| r.covers_range(a, b));

    // Списки отправления телеграммы: для порожнего — станция/дорога supply,
    // для груза — станция/дорога погрузки. Пустые списки = без ограничения.
    let dep_ok_empty = dep_matches(r, &k.supply_rw, &k.supply_esr);
    let dep_ok_cargo = dep_matches(r, &k.load_rw, &k.load_esr);

    if is_station_level(r) {
        if empty_fx && dep_ok_empty {
            let esr_hit = !k.load_esr.is_empty() && r.dest_esr.contains(&k.load_esr);
            let name_hit = !k.load_name.is_empty()
                && r.dest_names.iter().any(|n| normalize_station_name(n) == k.load_name);
            if (esr_hit || name_hit) && empty_party_ok(r, q) {
                return true;
            }
        }
        if cargo_fx && dep_ok_cargo {
            let esr_hit = !k.cargo_esr.is_empty() && r.dest_esr.contains(&k.cargo_esr);
            let name_hit = !k.cargo_name.is_empty()
                && r.dest_names.iter().any(|n| normalize_station_name(n) == k.cargo_name);
            if (esr_hit || name_hit) && cargo_party_ok(r, q) {
                return true;
            }
        }
        return false;
    }

    if r.dest_all_stations && !r.dest_railways.is_empty() {
        let is_pair = !r.dep_railways.is_empty();
        // All + оба списка (4702) — только пара «погрузка с dep → груз на dest»;
        // dest-дорогу для порожняка закрывает лишь Empty-пара или All без dep.
        let empty_side = if is_pair { r.cargo_class == ConventionCargoClass::Empty } else { true };
        if empty_fx && empty_side && rw_in(&r.dest_railways, &k.load_rw) && dep_ok_empty && empty_party_ok(r, q) {
            return true;
        }
        if cargo_fx && rw_in(&r.dest_railways, &k.cargo_rw) && dep_ok_cargo && cargo_party_ok(r, q) {
            return true;
        }
        return false;
    }

    // Только отправление: запрет погрузки с дорог/станций dep на любые назначения.
    if cargo_fx && (!r.dep_railways.is_empty() || !r.dep_esr.is_empty()) && dep_ok_cargo {
        return cargo_party_ok(r, q);
    }

    false
}

/// Списки отправления телеграммы (дороги и/или ЕСР) допускают `rw` / `esr`. Пустые — без ограничения.
fn dep_matches(r: &ParsedConvention, rw: &str, esr: &str) -> bool {
    (r.dep_railways.is_empty() || rw_in(&r.dep_railways, rw))
        && (r.dep_esr.is_empty() || (!esr.is_empty() && r.dep_esr.iter().any(|e| e == esr)))
}

fn empty_rule_applies(r: &ParsedConvention, scope: ConventionScope) -> bool {
    match r.convention_info {
        ConventionStatus::WashingStation => scope == ConventionScope::Wash,
        ConventionStatus::RepairStation => scope == ConventionScope::Repair,
        ConventionStatus::ReserveStation => scope == ConventionScope::Reserve,
        _ => true,
    }
}

fn cargo_rule_applies(r: &ParsedConvention, scope: ConventionScope) -> bool {
    scope == ConventionScope::Load && !r.convention_info.is_service_station()
}

/// Empty-ветка: контрагент порожнего — грузоотправитель на станции погрузки (или получатель узла).
///
/// Промывка / ремонт / отстой: если у назначения нет ни ОКПО, ни имени (узлы промывки
/// строятся без оператора ППС, у отстоя владелец может отсутствовать), сравнить не с чем —
/// закрываем станцию целиком (fail-closed). Для погрузки фильтр строгий: грузоотправитель есть.
fn empty_party_ok(r: &ParsedConvention, q: &Query<'_>) -> bool {
    if r.all_parties {
        return true;
    }
    if q.scope != ConventionScope::Load && !query_has_party(q) {
        return true;
    }
    okpo_in(r, q.sender_okpo)
        || name_in(r, q.sender_name)
        || q.rec_okpo.iter().any(|o| okpo_in(r, Some(o)))
        || q.rec_names.iter().any(|n| name_in(r, Some(n)))
}

/// Grain-ветка: контрагент груза — грузополучатель узла. Для `All` — любой из сторон узла.
fn cargo_party_ok(r: &ParsedConvention, q: &Query<'_>) -> bool {
    if r.all_parties {
        return true;
    }
    if r.cargo_class == ConventionCargoClass::All {
        return empty_party_ok(r, q);
    }
    q.rec_okpo.iter().any(|o| okpo_in(r, Some(o))) || q.rec_names.iter().any(|n| name_in(r, Some(n)))
}

fn query_has_party(q: &Query<'_>) -> bool {
    let non_blank = |s: &str| !s.trim().is_empty();
    q.sender_okpo.is_some_and(non_blank)
        || q.sender_name.is_some_and(non_blank)
        || q.rec_okpo.iter().any(|s| non_blank(s))
        || q.rec_names.iter().any(|s| non_blank(s))
}

fn okpo_in(r: &ParsedConvention, raw: Option<&str>) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    let n = normalize_okpo(raw);
    !n.is_empty() && r.recipient_okpo.iter().any(|o| o == &n)
}

fn name_in(r: &ParsedConvention, raw: Option<&str>) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    let n = normalize_party_name(raw);
    !n.is_empty() && r.recipient_names.iter().any(|o| o == &n)
}

fn rw_key(s: &str) -> String {
    s.trim().to_uppercase()
}

fn rw_in(list: &[String], key: &str) -> bool {
    !key.is_empty() && list.iter().any(|c| c == key)
}

/// `true` — ключ непустой и правило добавлено (или уже было) под этим ключом.
fn push_id(map: &mut HashMap<String, Vec<usize>>, key: String, i: usize) -> bool {
    if key.is_empty() {
        return false;
    }
    let v = map.entry(key).or_default();
    if !v.contains(&i) {
        v.push(i);
    }
    true
}

fn extend_ids(out: &mut Vec<usize>, map: &HashMap<String, Vec<usize>>, key: &str) {
    if key.is_empty() {
        return;
    }
    if let Some(ids) = map.get(key) {
        out.extend_from_slice(ids);
    }
}

fn normalize_station_name(raw: &str) -> String {
    let t: String = raw
        .trim()
        .to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{CarKind, RepairStatus};

    fn base_rule() -> ParsedConvention {
        ParsedConvention {
            rzd_number: "1".into(),
            cargo_class: ConventionCargoClass::Empty,
            cargo_name: String::new(),
            date_beg: "2026-01-01".into(),
            date_end: "3000-01-01".into(),
            dest_esr: vec![],
            dest_names: vec![],
            dest_railways: vec![],
            dest_all_stations: false,
            dep_esr: vec![],
            dep_names: vec![],
            dep_railways: vec![],
            dep_all_stations: false,
            junction: None,
            all_parties: true,
            recipient_okpo: vec![],
            recipient_names: vec![],
            unknown_road_fragments: vec![],
            convention_info: ConventionStatus::Other,
            kzh_stripped_dest: false,
            kzh_stripped_dep: false,
        }
    }

    fn supply(rw: &str) -> SupplyNode {
        SupplyNode {
            s_id: 0,
            kind: CarKind::Free,
            car_count: 10,
            station_to: String::new(),
            station_to_code: "100001".into(),
            railway_to: rw.into(),
            railway_to_code: None,
            railway_part_to: None,
            car_type: None,
            etsng: None,
            etsng_name: None,
            repair_status: RepairStatus::Ok,
            status: None,
            supply_period: 1,
            car_numbers: vec![],
            stations_from: vec![],
            stations_from_code: vec![],
            railways_from: vec![],
            railways_from_code: vec![],
            railways_part_from: vec![],
            is_mass_unloading: false,
            prev_etsngs: vec![],
            prev_etsng_names: vec![],
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn demand_load(
        load_code: &str,
        load_name: &str,
        load_rw: &str,
        cargo_code: Option<&str>,
        cargo_name: Option<&str>,
        cargo_rw: Option<&str>,
        sender_okpo: Option<&str>,
        sender_name: Option<&str>,
    ) -> DemandNode {
        DemandNode {
            d_id: 0,
            purpose: DemandPurpose::Load,
            period: 1,
            station_name: load_name.into(),
            station_code: load_code.into(),
            railway_name: load_rw.into(),
            railway_code: None,
            railway_part: None,
            station_to_name: cargo_name.map(str::to_string),
            station_to_code: cargo_code.map(str::to_string),
            railway_to_name: cargo_rw.map(str::to_string),
            railway_to_code: None,
            railway_to_part: None,
            sender: sender_name.map(str::to_string),
            sender_okpo: sender_okpo.map(str::to_string),
            sender_tgnl: None,
            client: None,
            customer_okpo: None,
            recipient: None,
            loader_to_okpo: None,
            gng_cargo: None,
            etsng: None,
            request_numbers: None,
            request_dates: None,
            gu12_number: None,
            shipping_type: None,
            car_type: None,
            car_count: 10,
            gu12_cap: None,
            cars_on_station: 0,
        }
    }

    fn banned(idx: &ConventionIndex, s: &SupplyNode, d: &DemandNode) -> bool {
        idx.ban_for(s, d).is_some()
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn empty_esr_closes_only_matching_okpo() {
        let mut r = base_rule();
        r.dest_esr = vec!["987303".into()];
        r.all_parties = false;
        r.recipient_okpo = vec![normalize_okpo("00111")];
        let idx = ConventionIndex::build(vec![r]);
        let s = supply("ПРВ");
        let hit = demand_load("987303", "Руденск", "ПРВ", None, None, None, Some("111"), Some("ООО А"));
        let miss = demand_load("987303", "Руденск", "ПРВ", None, None, None, Some("222"), Some("ООО Б"));
        let other_st = demand_load("100001", "Другая", "ПРВ", None, None, None, Some("111"), Some("ООО А"));
        assert!(banned(&idx, &s, &hit));
        assert!(!banned(&idx, &s, &miss));
        assert!(!banned(&idx, &s, &other_st));
        assert_eq!(idx.stats.empty_load_esr_keys, 1);
        assert_eq!(idx.stats.with_party_filter, 1);
    }

    #[test]
    fn empty_esr_all_parties_closes_whole_station() {
        let mut r = base_rule();
        r.dest_esr = vec!["987303".into()];
        r.all_parties = true;
        let idx = ConventionIndex::build(vec![r]);
        let s = supply("ПРВ");
        let a = demand_load("987303", "Руденск", "ПРВ", None, None, None, Some("111"), Some("ООО А"));
        let b = demand_load("987303", "Руденск", "ПРВ", None, None, None, Some("222"), Some("ООО Б"));
        assert!(banned(&idx, &s, &a));
        assert!(banned(&idx, &s, &b));
        assert_eq!(idx.stats.all_parties, 1);
    }

    #[test]
    fn grain_novorossiysk_matches_cargo_dest_not_load_station() {
        let mut r = base_rule();
        r.cargo_class = ConventionCargoClass::Grain;
        r.dest_esr = vec!["514003".into()];
        r.dest_names = vec!["Новороссийск (эксп.)".into()];
        let idx = ConventionIndex::build(vec![r]);
        let s = supply("СКВ");
        let closed = demand_load(
            "100001",
            "Погрузка",
            "СКВ",
            Some("514003"),
            Some("Новороссийск эксп."),
            Some("СКВ"),
            None,
            None,
        );
        let open = demand_load(
            "514003",
            "Новороссийск (эксп.)",
            "СКВ",
            Some("200002"),
            Some("Другая"),
            Some("ОКТ"),
            None,
            None,
        );
        assert!(banned(&idx, &s, &closed));
        assert!(!banned(&idx, &s, &open));
        assert_eq!(idx.stats.grain_cargo_esr_keys, 1);
    }

    #[test]
    fn all_road_pair_4702_style() {
        let mut r = base_rule();
        r.rzd_number = "4702".into();
        r.cargo_class = ConventionCargoClass::All;
        r.dep_railways = vec!["КРС".into(), "ЗСБ".into(), "ЮУР".into(), "СВР".into()];
        r.dest_railways = vec!["ГОР".into(), "МСК".into(), "ОКТ".into(), "СЕВ".into()];
        r.dest_all_stations = true;
        r.dep_all_stations = true;
        let idx = ConventionIndex::build(vec![r]);
        let s = supply("ЗСБ");
        let zsb_okt = demand_load("1", "A", "ЗСБ", Some("2"), Some("B"), Some("ОКТ"), None, None);
        let zsb_skv = demand_load("1", "A", "ЗСБ", Some("2"), Some("B"), Some("СКВ"), None, None);
        let msk_okt = demand_load("1", "A", "МСК", Some("2"), Some("B"), Some("ОКТ"), None, None);
        assert_eq!(idx.ban_for(&s, &zsb_okt).unwrap().rzd_number, "4702");
        assert!(!banned(&idx, &s, &zsb_skv));
        assert!(!banned(&idx, &s, &msk_okt));
        assert_eq!(idx.stats.road_pair_rules, 1);
        assert_eq!(idx.stats.not_indexed, 0);
    }

    #[test]
    fn road_rule_with_leaked_station_names_still_indexes_as_pair() {
        // Страховка: если разбор всё же оставил «Северной» в dest_names при «все станции» —
        // правило остаётся дорожным, а не станционным.
        let mut r = base_rule();
        r.cargo_class = ConventionCargoClass::All;
        r.dep_railways = vec!["ЗСБ".into()];
        r.dest_railways = vec!["ОКТ".into(), "СЕВ".into()];
        r.dest_names = vec!["Северной".into()];
        r.dest_all_stations = true;
        r.dep_all_stations = true;
        let idx = ConventionIndex::build(vec![r]);
        assert_eq!(idx.stats.road_pair_rules, 1);
        assert_eq!(idx.stats.empty_load_esr_keys, 0);
        let s = supply("ЗСБ");
        let zsb_okt = demand_load("1", "A", "ЗСБ", Some("2"), Some("B"), Some("ОКТ"), None, None);
        assert!(banned(&idx, &s, &zsb_okt));
    }

    #[test]
    fn empty_road_pair_uses_supply_railway() {
        let mut r = base_rule();
        r.dep_railways = vec!["ЗСБ".into()];
        r.dest_railways = vec!["ОКТ".into()];
        r.dest_all_stations = true;
        r.dep_all_stations = true;
        let idx = ConventionIndex::build(vec![r]);
        let from_zsb = supply("ЗСБ");
        let from_skv = supply("СКВ");
        let d = demand_load("1", "A", "ОКТ", None, None, None, None, None);
        assert!(banned(&idx, &from_zsb, &d));
        assert!(!banned(&idx, &from_skv, &d));
    }

    #[test]
    fn dest_only_empty_closes_load_railway() {
        let mut r = base_rule();
        r.dest_railways = vec!["СКВ".into()];
        r.dest_all_stations = true;
        let idx = ConventionIndex::build(vec![r]);
        let s = supply("ОКТ");
        let on_skv = demand_load("1", "A", "СКВ", Some("2"), Some("B"), Some("ОКТ"), None, None);
        let on_okt = demand_load("1", "A", "ОКТ", Some("2"), Some("B"), Some("СКВ"), None, None);
        assert!(banned(&idx, &s, &on_skv));
        assert!(!banned(&idx, &s, &on_okt));
    }

    #[test]
    fn dest_only_all_closes_cargo_dest_railway() {
        let mut r = base_rule();
        r.cargo_class = ConventionCargoClass::All;
        r.dest_railways = vec!["ОКТ".into()];
        r.dest_all_stations = true;
        let idx = ConventionIndex::build(vec![r]);
        let s = supply("ЗСБ");
        let grain_side = demand_load("1", "A", "ЗСБ", Some("2"), Some("B"), Some("ОКТ"), None, None);
        let empty_side = demand_load("1", "A", "ОКТ", Some("2"), Some("B"), Some("СКВ"), None, None);
        let open = demand_load("1", "A", "ЗСБ", Some("2"), Some("B"), Some("СКВ"), None, None);
        assert!(banned(&idx, &s, &grain_side));
        assert!(banned(&idx, &s, &empty_side));
        assert!(!banned(&idx, &s, &open));
    }

    #[test]
    fn dep_only_all_closes_loading_from_road() {
        let mut r = base_rule();
        r.cargo_class = ConventionCargoClass::All;
        r.dep_railways = vec!["ЗСБ".into()];
        r.dep_all_stations = true;
        r.dest_all_stations = true;
        let idx = ConventionIndex::build(vec![r]);
        let s = supply("ОКТ");
        let from_zsb = demand_load("1", "A", "ЗСБ", Some("2"), Some("B"), Some("СКВ"), None, None);
        let from_okt = demand_load("1", "A", "ОКТ", Some("2"), Some("B"), Some("СКВ"), None, None);
        assert!(banned(&idx, &s, &from_zsb));
        assert!(!banned(&idx, &s, &from_okt));
    }

    #[test]
    fn dep_esr_only_all_closes_loading_from_station() {
        // «Запрет погрузки всех грузов со станции 300001 на все станции».
        let mut r = base_rule();
        r.cargo_class = ConventionCargoClass::All;
        r.dep_esr = vec!["300001".into()];
        r.dest_all_stations = true;
        let idx = ConventionIndex::build(vec![r]);
        assert_eq!(idx.stats.dep_esr_keys, 1);
        let s = supply("ОКТ");
        let from_st = demand_load("300001", "A", "ЗСБ", Some("2"), Some("B"), Some("СКВ"), None, None);
        let other = demand_load("300002", "A", "ЗСБ", Some("2"), Some("B"), Some("СКВ"), None, None);
        assert!(banned(&idx, &s, &from_st));
        assert!(!banned(&idx, &s, &other));
    }

    #[test]
    fn empty_dep_only_is_not_indexed_and_reported() {
        // Empty только с дорогами отправления — не про подсыл «на»; правило без ключей.
        let mut r = base_rule();
        r.rzd_number = "77".into();
        r.dep_railways = vec!["ЗСБ".into()];
        r.dep_all_stations = true;
        r.dest_all_stations = true;
        let idx = ConventionIndex::build(vec![r]);
        assert_eq!(idx.stats.not_indexed, 1);
        assert_eq!(idx.not_indexed_rules().next().unwrap().rzd_number, "77");
        let s = supply("ЗСБ");
        let d = demand_load("1", "A", "ОКТ", None, None, None, None, None);
        assert!(!banned(&idx, &s, &d));
    }

    #[test]
    fn station_rule_with_dep_railways_limits_origin() {
        // Grain: «запрет погрузки зерна со станций ЗСБ назначением на 514003».
        let mut r = base_rule();
        r.cargo_class = ConventionCargoClass::Grain;
        r.dest_esr = vec!["514003".into()];
        r.dep_railways = vec!["ЗСБ".into()];
        let idx = ConventionIndex::build(vec![r]);
        let s = supply("ОКТ");
        let from_zsb = demand_load("1", "A", "ЗСБ", Some("514003"), Some("Н"), Some("СКВ"), None, None);
        let from_prv = demand_load("1", "A", "ПРВ", Some("514003"), Some("Н"), Some("СКВ"), None, None);
        assert!(banned(&idx, &s, &from_zsb));
        assert!(!banned(&idx, &s, &from_prv), "погрузка не с dep-дороги — открыта");

        // Empty: «запрет отправления порожних со станций ЗСБ на станцию 987303» — по дороге supply.
        let mut r = base_rule();
        r.dest_esr = vec!["987303".into()];
        r.dep_railways = vec!["ЗСБ".into()];
        let idx = ConventionIndex::build(vec![r]);
        let d = demand_load("987303", "A", "ПРВ", None, None, None, None, None);
        assert!(banned(&idx, &supply("ЗСБ"), &d));
        assert!(!banned(&idx, &supply("ОКТ"), &d));
    }

    #[test]
    fn wash_telegram_does_not_close_load() {
        let mut r = base_rule();
        r.dest_esr = vec!["555001".into()];
        r.convention_info = ConventionStatus::WashingStation;
        let idx = ConventionIndex::build(vec![r]);
        let s = supply("СКВ");
        let mut load = demand_load("555001", "Промывка", "СКВ", None, None, None, None, None);
        assert!(!banned(&idx, &s, &load));
        load.purpose = DemandPurpose::Wash;
        assert!(banned(&idx, &s, &load));
        assert_eq!(idx.stats.wash_rules, 1);
    }

    fn dest_ref<'a>(code: &'a str, rw: &'a str) -> EmptyDestRef<'a> {
        EmptyDestRef {
            supply_railway: "СКВ",
            supply_station_code: "100001",
            station_code: code,
            station_name: "Служебная",
            railway: rw,
            sender_okpo: None,
            sender_name: None,
            recipient_okpos: &[],
            recipient_names: &[],
        }
    }

    #[test]
    fn generic_empty_ban_applies_to_wash_and_reserve() {
        let mut r = base_rule();
        r.dest_esr = vec!["555001".into()];
        let idx = ConventionIndex::build(vec![r]);
        let s = supply("СКВ");
        let mut wash = demand_load("555001", "Промывка", "СКВ", None, None, None, None, None);
        wash.purpose = DemandPurpose::Wash;
        assert!(banned(&idx, &s, &wash));
        let dest = dest_ref("555001", "СКВ");
        assert!(idx.ban_for_empty_dest(dest, ConventionScope::Reserve).is_some());
        assert!(idx.ban_for_empty_dest(dest, ConventionScope::Repair).is_some());
    }

    #[test]
    fn service_dest_without_party_is_closed_by_party_specific_rule() {
        // Телеграмма промывки в адрес ООО ППС (ОКПО 111). Узел промывки без контрагента —
        // сравнить не с чем, станция закрыта. Ремстанция с другим ОКПО — открыта, с тем же — закрыта.
        let mut r = base_rule();
        r.dest_esr = vec!["555001".into()];
        r.all_parties = false;
        r.recipient_okpo = vec![normalize_okpo("00111")];
        r.recipient_names = vec![normalize_party_name("ООО ППС")];
        let idx = ConventionIndex::build(vec![r]);

        let s = supply("СКВ");
        let mut wash = demand_load("555001", "Промывка", "СКВ", None, None, None, None, None);
        wash.purpose = DemandPurpose::Wash;
        assert!(banned(&idx, &s, &wash), "узел промывки без оператора — fail-closed");
        // Тот же ЕСР под погрузку с другим отправителем — открыт (фильтр Load строгий).
        let load = demand_load("555001", "Промывка", "СКВ", None, None, None, Some("222"), Some("ООО Иное"));
        assert!(!banned(&idx, &s, &load));

        let other: Vec<String> = vec!["999".into()];
        let same: Vec<String> = vec!["111".into()];
        let mut dest = dest_ref("555001", "СКВ");
        dest.recipient_okpos = &other;
        assert!(idx.ban_for_empty_dest(dest, ConventionScope::Repair).is_none());
        dest.recipient_okpos = &same;
        assert!(idx.ban_for_empty_dest(dest, ConventionScope::Repair).is_some());
        let blank: Vec<String> = vec![String::new()];
        dest.recipient_okpos = &blank;
        assert!(idx.ban_for_empty_dest(dest, ConventionScope::Reserve).is_some(), "пустая строка = нет контрагента");
    }

    #[test]
    fn grain_rule_never_touches_empty_destinations() {
        let mut r = base_rule();
        r.cargo_class = ConventionCargoClass::Grain;
        r.dest_esr = vec!["555001".into()];
        let idx = ConventionIndex::build(vec![r]);
        let dest = dest_ref("555001", "СКВ");
        assert!(idx.ban_for_empty_dest(dest, ConventionScope::Reserve).is_none());
        let s = supply("СКВ");
        let mut wash = demand_load("555001", "Промывка", "СКВ", None, None, None, None, None);
        wash.purpose = DemandPurpose::Wash;
        assert!(!banned(&idx, &s, &wash));
    }

    #[test]
    fn disabled_index_never_bans() {
        let idx = ConventionIndex::disabled();
        let s = supply("ЗСБ");
        let d = demand_load("987303", "X", "ПРВ", None, None, None, None, None);
        assert!(!banned(&idx, &s, &d));
    }

    // --- Даты -----------------------------------------------------------------

    fn timing(dispatch: i32, arrival: i32, window: Option<(i32, i32)>) -> ArcTiming {
        ArcTiming {
            dispatch_day: dispatch,
            arrival_day: arrival,
            load_window: window,
        }
    }

    #[test]
    fn empty_ban_covers_dispatch_even_if_arrival_after_end() {
        // Телеграмма 15…16 сентября; отправление сегодня (15-е), прибытие 18-го.
        let mut r = base_rule();
        r.dest_esr = vec!["987303".into()];
        r.date_beg = "2026-09-15".into();
        r.date_end = "2026-09-16".into();
        let idx = ConventionIndex::build_at(vec![r], d(2026, 9, 15));
        let s = supply("ПРВ");
        let dm = demand_load("987303", "A", "ПРВ", None, None, None, None, None);
        assert!(idx.ban_for_arc(&s, &dm, timing(0, 3, Some((0, 4)))).is_some());
        // Дислокация: отправление через 5 суток, прибытие на 8-е — телеграмма уже истекла.
        assert!(idx.ban_for_arc(&s, &dm, timing(5, 8, Some((5, 7)))).is_none());
    }

    #[test]
    fn empty_ban_covers_arrival_even_if_starts_after_dispatch() {
        let mut r = base_rule();
        r.dest_esr = vec!["987303".into()];
        r.date_beg = "2026-09-18".into();
        r.date_end = "2026-09-20".into();
        let idx = ConventionIndex::build_at(vec![r], d(2026, 9, 15));
        let s = supply("ПРВ");
        let dm = demand_load("987303", "A", "ПРВ", None, None, None, None, None);
        assert!(idx.ban_for_arc(&s, &dm, timing(0, 3, Some((0, 4)))).is_some(), "прибытие 18-го");
        assert!(idx.ban_for_arc(&s, &dm, timing(0, 2, Some((0, 4)))).is_none(), "прибытие 17-го");
    }

    #[test]
    fn grain_ban_uses_loading_window_not_dispatch() {
        // Grain 15…16 сентября. Узел периода 4 (сут. 10–14): погрузка 25-го и позже — открыто,
        // хотя порожний отправляется 15-го.
        let mut r = base_rule();
        r.cargo_class = ConventionCargoClass::Grain;
        r.dest_esr = vec!["514003".into()];
        r.date_beg = "2026-09-15".into();
        r.date_end = "2026-09-16".into();
        let idx = ConventionIndex::build_at(vec![r], d(2026, 9, 15));
        let s = supply("ПРВ");
        let dm = demand_load("1", "A", "ПРВ", Some("514003"), Some("Н"), Some("СКВ"), None, None);
        assert!(idx.ban_for_arc(&s, &dm, timing(0, 3, Some((10, 14)))).is_none());
        // Период 1 (сут. 0–4), прибытие на 1-е сутки: погрузка 16…19-го — пересекает.
        assert!(idx.ban_for_arc(&s, &dm, timing(0, 1, Some((0, 4)))).is_some());
        // Прибытие позже окна (на 3-и сутки при периоде 1): погрузка по прибытии 18-го — открыто.
        assert!(idx.ban_for_arc(&s, &dm, timing(0, 3, Some((0, 1)))).is_none());
    }

    #[test]
    fn grain_ban_starting_inside_loading_window_closes_arc() {
        let mut r = base_rule();
        r.cargo_class = ConventionCargoClass::Grain;
        r.dest_esr = vec!["514003".into()];
        r.date_beg = "2026-09-27".into();
        r.date_end = "3000-01-01".into();
        let idx = ConventionIndex::build_at(vec![r], d(2026, 9, 15));
        let s = supply("ПРВ");
        let dm = demand_load("1", "A", "ПРВ", Some("514003"), Some("Н"), Some("СКВ"), None, None);
        assert!(idx.ban_for_arc(&s, &dm, timing(0, 3, Some((10, 14)))).is_some(), "окно 25…29 задевает 27-е");
        assert!(idx.ban_for_arc(&s, &dm, timing(0, 3, Some((5, 7)))).is_none(), "окно 20…22 — до начала");
    }

    #[test]
    fn all_rule_checks_both_windows_separately() {
        // All на ЕСР 987303 (порожняк на станцию) 18…20 сентября. Порожний прибывает 17-го,
        // груз едет не туда → ни одна ветка не пересекается.
        let mut r = base_rule();
        r.cargo_class = ConventionCargoClass::All;
        r.dest_esr = vec!["987303".into()];
        r.date_beg = "2026-09-18".into();
        r.date_end = "2026-09-20".into();
        let idx = ConventionIndex::build_at(vec![r], d(2026, 9, 15));
        let s = supply("ПРВ");
        let dm = demand_load("987303", "A", "ПРВ", Some("2"), Some("B"), Some("СКВ"), None, None);
        assert!(idx.ban_for_arc(&s, &dm, timing(0, 2, Some((0, 4)))).is_none());
        assert!(idx.ban_for_arc(&s, &dm, timing(0, 3, Some((0, 4)))).is_some());
    }

    #[test]
    fn empty_dest_timed_uses_dispatch_arrival_window() {
        let mut r = base_rule();
        r.dest_esr = vec!["555001".into()];
        r.date_beg = "2026-09-15".into();
        r.date_end = "2026-09-15".into();
        let idx = ConventionIndex::build_at(vec![r], d(2026, 9, 15));
        let dest = dest_ref("555001", "СКВ");
        assert!(idx.ban_for_empty_dest_timed(dest, ConventionScope::Reserve, 0, 4).is_some());
        assert!(idx.ban_for_empty_dest_timed(dest, ConventionScope::Reserve, 5, 9).is_none());
    }

    #[test]
    fn others_never_reach_index() {
        use super::super::conventions::{parse_hash, RailwayCatalog, RAILWAY_MAP_PATH};
        use std::path::Path;

        let json = r#"{
            "id": 1, "rzd_number": "9", "date_create": "2026-09-01",
            "date_beg": "2026-01-01", "date_end": "3000-01-01", "text": "t",
            "cargo_class": "Others", "cargo_name": "крытые",
            "departure_st": "Все станции", "departure_st_code": "Все станции",
            "destination_st": "Все станции СКВ", "destination_st_code": "Все станции",
            "railroad_junction": "None", "recipient_name": "все грузополучатели",
            "recipient_okpo": "All", "convention_info": {"type": "Other"}
        }"#;
        let mut raw = HashMap::new();
        raw.insert("9".into(), json.into());
        let cat = RailwayCatalog::load_csv(&Path::new(env!("CARGO_MANIFEST_DIR")).join(RAILWAY_MAP_PATH)).unwrap();
        let load = parse_hash(&raw, d(2026, 9, 15), &cat);
        let idx = ConventionIndex::build(load.active);
        assert!(idx.is_empty());
    }
}
