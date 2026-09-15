//! Шаг 4 правила 5: индекс действующих конвенций для `classify_pair`.
//!
//! Не сканируем весь список на каждую пару supply×demand: кандидаты берутся по
//! ЕСР станции погрузки, ЕСР назначения груза и коротким кодам дорог. Фильтр
//! «все грузополучатели» vs ОКПО/имя применяется к уже найденным кандидатам.
//! Ограничение «на 50% от плана» в JSON нет — в v1 такая телеграмма = жёсткий запрет.

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
    pub supply_railway: &'a str,
    pub station_code: &'a str,
    pub station_name: &'a str,
    pub railway: &'a str,
    pub sender_okpo: Option<&'a str>,
    pub sender_name: Option<&'a str>,
    pub recipient_okpos: &'a [String],
    pub recipient_names: &'a [String],
}

/// Размеры индекса — в лог старта и дамп.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConventionIndexStats {
    pub rules: usize,
    pub empty_load_esr_keys: usize,
    pub grain_cargo_esr_keys: usize,
    pub dest_railway_keys: usize,
    pub dep_railway_keys: usize,
    pub road_pair_rules: usize,
    pub all_parties: usize,
    pub with_party_filter: usize,
    pub wash_rules: usize,
    pub repair_rules: usize,
    pub reserve_rules: usize,
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
    /// Empty/All, оба списка дорог: dest = дорога погрузки, затем dep = дорога supply.
    empty_pair_by_dest_rw: HashMap<String, Vec<usize>>,
    /// All/Grain, оба списка: dest = дорога назначения груза, затем dep = дорога погрузки.
    all_pair_by_dest_rw: HashMap<String, Vec<usize>>,
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

    pub fn summary_line(&self) -> String {
        let st = &self.stats;
        format!(
            "индекс: {} правил; ЕСР погрузки {}, ЕСР назн.груза {}, dest-дороги {}, dep-дороги {}, пары dep→dest {}; все получатели {}, с ОКПО/именем {}",
            st.rules,
            st.empty_load_esr_keys,
            st.grain_cargo_esr_keys,
            st.dest_railway_keys,
            st.dep_railway_keys,
            st.road_pair_rules,
            st.all_parties,
            st.with_party_filter,
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
        let (esr, extra) = preview(self.empty_by_load_esr.keys().cloned(), 15);
        if !esr.is_empty() {
            println!(
                "  ЕСР погрузки (Empty/All): {}{}",
                esr.join(", "),
                if extra > 0 { format!(" …ещё {extra}") } else { String::new() },
            );
        }
        let (gesr, extra) = preview(self.grain_by_cargo_esr.keys().cloned(), 15);
        if !gesr.is_empty() {
            println!(
                "  ЕСР назн.груза (Grain/All): {}{}",
                gesr.join(", "),
                if extra > 0 { format!(" …ещё {extra}") } else { String::new() },
            );
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
            println!(
                "  dest-дороги: {}{}",
                dest_rw.join(", "),
                if extra > 0 { format!(" …ещё {extra}") } else { String::new() },
            );
        }
        let (dep_rw, extra) = preview(self.all_by_dep_rw.keys().cloned(), 15);
        if !dep_rw.is_empty() {
            println!(
                "  dep-дороги (All/Grain без dest): {}{}",
                dep_rw.join(", "),
                if extra > 0 { format!(" …ещё {extra}") } else { String::new() },
            );
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
            idx.index_rule(i);
        }
        idx.stats.empty_load_esr_keys = idx.empty_by_load_esr.len();
        idx.stats.grain_cargo_esr_keys = idx.grain_by_cargo_esr.len();
        let mut dest_keys: std::collections::HashSet<&str> = std::collections::HashSet::new();
        dest_keys.extend(idx.empty_by_dest_rw.keys().map(String::as_str));
        dest_keys.extend(idx.grain_by_dest_rw.keys().map(String::as_str));
        dest_keys.extend(idx.empty_pair_by_dest_rw.keys().map(String::as_str));
        dest_keys.extend(idx.all_pair_by_dest_rw.keys().map(String::as_str));
        idx.stats.dest_railway_keys = dest_keys.len();
        idx.stats.dep_railway_keys = idx.all_by_dep_rw.len();
        idx
    }

    fn index_rule(&mut self, i: usize) {
        let (
            all_parties,
            convention_info,
            cargo_class,
            dest_esr,
            dest_names,
            dest_railways,
            dep_railways,
            dest_all_stations,
        ) = {
            let r = &self.rules[i];
            (
                r.all_parties,
                r.convention_info.clone(),
                r.cargo_class,
                r.dest_esr.clone(),
                r.dest_names.clone(),
                r.dest_railways.clone(),
                r.dep_railways.clone(),
                r.dest_all_stations,
            )
        };
        if all_parties {
            self.stats.all_parties += 1;
        } else {
            self.stats.with_party_filter += 1;
        }
        match convention_info {
            ConventionStatus::WashingStation => self.stats.wash_rules += 1,
            ConventionStatus::RepairStation => self.stats.repair_rules += 1,
            ConventionStatus::ReserveStation => self.stats.reserve_rules += 1,
            _ => {}
        }

        let empty_fx = matches!(cargo_class, ConventionCargoClass::Empty | ConventionCargoClass::All);
        let grain_fx = matches!(cargo_class, ConventionCargoClass::Grain | ConventionCargoClass::All);
        let station_level = !dest_esr.is_empty() || !dest_names.is_empty();

        if station_level {
            if empty_fx {
                for esr in dest_esr.iter().cloned() {
                    push_id(&mut self.empty_by_load_esr, esr, i);
                }
                for name in &dest_names {
                    let key = normalize_station_name(name);
                    if !key.is_empty() {
                        push_id(&mut self.empty_by_load_name, key, i);
                    }
                }
            }
            if grain_fx {
                for esr in dest_esr {
                    push_id(&mut self.grain_by_cargo_esr, esr, i);
                }
                for name in &dest_names {
                    let key = normalize_station_name(name);
                    if !key.is_empty() {
                        push_id(&mut self.grain_by_cargo_name, key, i);
                    }
                }
            }
            return;
        }

        if dest_all_stations && !dest_railways.is_empty() {
            if !dep_railways.is_empty() {
                self.stats.road_pair_rules += 1;
                // Empty + оба списка: подсыл с dep-дорог на dest-дороги.
                // All + оба списка (4702): только пара «погрузка с dep → груз на dest»,
                // без закрытия всей dest-дороги для порожняка.
                if cargo_class == ConventionCargoClass::Empty {
                    for rw in dest_railways.iter().cloned() {
                        push_id(&mut self.empty_pair_by_dest_rw, rw, i);
                    }
                }
                if grain_fx {
                    for rw in dest_railways.iter().cloned() {
                        push_id(&mut self.all_pair_by_dest_rw, rw, i);
                    }
                }
            } else {
                if empty_fx {
                    for rw in dest_railways.iter().cloned() {
                        push_id(&mut self.empty_by_dest_rw, rw, i);
                    }
                }
                if grain_fx {
                    for rw in dest_railways {
                        push_id(&mut self.grain_by_dest_rw, rw, i);
                    }
                }
            }
            return;
        }

        if grain_fx && !dep_railways.is_empty() && dest_esr.is_empty() {
            for rw in dep_railways {
                push_id(&mut self.all_by_dep_rw, rw, i);
            }
        }
    }

    /// Первый запрет для пары supply×demand (погрузка или промывка).
    pub fn ban_for(&self, s: &SupplyNode, d: &DemandNode) -> Option<&ParsedConvention> {
        if self.rules.is_empty() {
            return None;
        }
        let rec_okpo = d.loader_to_okpo.as_deref().unwrap_or(&[]);
        let rec_names = d.recipient.as_deref().unwrap_or(&[]);
        let cargo_code = d.station_to_code.as_deref().unwrap_or("");
        let cargo_name = d.station_to_name.as_deref().unwrap_or("");
        let cargo_rw = d.railway_to_name.as_deref().unwrap_or("");
        let scope = match d.purpose {
            DemandPurpose::Load => ConventionScope::Load,
            DemandPurpose::Wash => ConventionScope::Wash,
        };
        self.ban_query(&Query {
            supply_rw: &s.railway_to,
            load_code: &d.station_code,
            load_name: &d.station_name,
            load_rw: &d.railway_name,
            cargo_code,
            cargo_name,
            cargo_rw,
            sender_okpo: d.sender_okpo.as_deref(),
            sender_name: d.sender.as_deref(),
            rec_okpo,
            rec_names,
            scope,
            on: None,
        })
    }

    /// Как [`Self::ban_for`], но только если телеграмма покрывает дату прибытия.
    pub fn ban_for_on(&self, s: &SupplyNode, d: &DemandNode, on: NaiveDate) -> Option<&ParsedConvention> {
        if self.rules.is_empty() {
            return None;
        }
        let rec_okpo = d.loader_to_okpo.as_deref().unwrap_or(&[]);
        let rec_names = d.recipient.as_deref().unwrap_or(&[]);
        let cargo_code = d.station_to_code.as_deref().unwrap_or("");
        let cargo_name = d.station_to_name.as_deref().unwrap_or("");
        let cargo_rw = d.railway_to_name.as_deref().unwrap_or("");
        let scope = match d.purpose {
            DemandPurpose::Load => ConventionScope::Load,
            DemandPurpose::Wash => ConventionScope::Wash,
        };
        self.ban_query(&Query {
            supply_rw: &s.railway_to,
            load_code: &d.station_code,
            load_name: &d.station_name,
            load_rw: &d.railway_name,
            cargo_code,
            cargo_name,
            cargo_rw,
            sender_okpo: d.sender_okpo.as_deref(),
            sender_name: d.sender.as_deref(),
            rec_okpo,
            rec_names,
            scope,
            on: Some(on),
        })
    }

    /// Запрет на дату прибытия: `today` индекса + `arrival_day` суток от сегодня.
    pub fn ban_for_arrival(&self, s: &SupplyNode, d: &DemandNode, arrival_day: i32) -> Option<&ParsedConvention> {
        let on = self.today() + Duration::days(i64::from(arrival_day.max(0)));
        self.ban_for_on(s, d, on)
    }

    /// Empty-запрет на станцию ремонта или отстоя.
    pub fn ban_for_empty_dest(&self, dest: EmptyDestRef<'_>, scope: ConventionScope) -> Option<&ParsedConvention> {
        if self.rules.is_empty() {
            return None;
        }
        self.ban_query(&Query {
            supply_rw: dest.supply_railway,
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
            on: None,
        })
    }

    /// Empty-запрет на дату прибытия (`today` + `arrival_day`).
    pub fn ban_for_empty_dest_arrival(
        &self,
        dest: EmptyDestRef<'_>,
        scope: ConventionScope,
        arrival_day: i32,
    ) -> Option<&ParsedConvention> {
        if self.rules.is_empty() {
            return None;
        }
        let on = self.today() + Duration::days(i64::from(arrival_day.max(0)));
        self.ban_query(&Query {
            supply_rw: dest.supply_railway,
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
            on: Some(on),
        })
    }

    fn ban_query(&self, q: &Query<'_>) -> Option<&ParsedConvention> {
        let mut ids = Vec::new();
        let load_esr = normalize_esr6(q.load_code);
        let load_name = normalize_station_name(q.load_name);
        let cargo_esr = normalize_esr6(q.cargo_code);
        let cargo_name = normalize_station_name(q.cargo_name);
        let load_rw = rw_key(q.load_rw);
        let cargo_rw = rw_key(q.cargo_rw);
        let supply_rw = rw_key(q.supply_rw);

        if q.scope != ConventionScope::Load {
            extend_ids(&mut ids, &self.empty_by_load_esr, &load_esr);
            extend_ids(&mut ids, &self.empty_by_load_name, &load_name);
            extend_ids(&mut ids, &self.empty_by_dest_rw, &load_rw);
            extend_ids(&mut ids, &self.empty_pair_by_dest_rw, &load_rw);
        } else {
            extend_ids(&mut ids, &self.empty_by_load_esr, &load_esr);
            extend_ids(&mut ids, &self.empty_by_load_name, &load_name);
            extend_ids(&mut ids, &self.grain_by_cargo_esr, &cargo_esr);
            extend_ids(&mut ids, &self.grain_by_cargo_name, &cargo_name);
            extend_ids(&mut ids, &self.empty_by_dest_rw, &load_rw);
            extend_ids(&mut ids, &self.grain_by_dest_rw, &cargo_rw);
            extend_ids(&mut ids, &self.all_by_dep_rw, &load_rw);
            extend_ids(&mut ids, &self.empty_pair_by_dest_rw, &load_rw);
            extend_ids(&mut ids, &self.all_pair_by_dest_rw, &cargo_rw);
        }

        ids.sort_unstable();
        ids.dedup();
        for i in ids {
            let r = &self.rules[i];
            if self.rule_hits(r, q, &supply_rw, &load_esr, &load_name, &load_rw, &cargo_esr, &cargo_name, &cargo_rw)
                && q.on.is_none_or(|day| r.covers_date(day))
            {
                return Some(r);
            }
        }
        None
    }

    fn rule_hits(
        &self,
        r: &ParsedConvention,
        q: &Query<'_>,
        supply_rw: &str,
        load_esr: &str,
        load_name: &str,
        load_rw: &str,
        cargo_esr: &str,
        cargo_name: &str,
        cargo_rw: &str,
    ) -> bool {
        let empty_fx = matches!(r.cargo_class, ConventionCargoClass::Empty | ConventionCargoClass::All);
        let grain_fx = matches!(r.cargo_class, ConventionCargoClass::Grain | ConventionCargoClass::All);
        let station_level = !r.dest_esr.is_empty() || !r.dest_names.is_empty();

        if station_level {
            if empty_fx && empty_rule_applies(r, q.scope) {
                let esr_hit = !load_esr.is_empty() && r.dest_esr.iter().any(|e| e == load_esr);
                let name_hit = !load_name.is_empty()
                    && r.dest_names.iter().any(|n| normalize_station_name(n) == load_name);
                if (esr_hit || name_hit) && empty_party_ok(r, q) {
                    return true;
                }
            }
            if grain_fx && grain_rule_applies(r, q.scope) {
                let esr_hit = !cargo_esr.is_empty() && r.dest_esr.iter().any(|e| e == cargo_esr);
                let name_hit = !cargo_name.is_empty()
                    && r.dest_names.iter().any(|n| normalize_station_name(n) == cargo_name);
                if (esr_hit || name_hit) && grain_party_ok(r, q) {
                    return true;
                }
            }
            return false;
        }

        if r.dest_all_stations && !r.dest_railways.is_empty() && !r.dep_railways.is_empty() {
            if r.cargo_class == ConventionCargoClass::Empty
                && empty_rule_applies(r, q.scope)
                && rw_in(&r.dest_railways, load_rw)
                && rw_in(&r.dep_railways, supply_rw)
                && empty_party_ok(r, q)
            {
                return true;
            }
            if grain_fx && grain_rule_applies(r, q.scope) && rw_in(&r.dest_railways, cargo_rw) && rw_in(&r.dep_railways, load_rw)
            {
                let party = if r.cargo_class == ConventionCargoClass::Grain {
                    grain_party_ok(r, q)
                } else {
                    empty_party_ok(r, q)
                };
                if party {
                    return true;
                }
            }
            return false;
        }

        if r.dest_all_stations && !r.dest_railways.is_empty() {
            if empty_fx && empty_rule_applies(r, q.scope) && rw_in(&r.dest_railways, load_rw) && empty_party_ok(r, q)
            {
                return true;
            }
            if grain_fx
                && grain_rule_applies(r, q.scope)
                && rw_in(&r.dest_railways, cargo_rw)
                && grain_party_ok(r, q)
            {
                return true;
            }
            return false;
        }

        if grain_fx
            && grain_rule_applies(r, q.scope)
            && !r.dep_railways.is_empty()
            && rw_in(&r.dep_railways, load_rw)
        {
            let party = if r.cargo_class == ConventionCargoClass::Grain {
                grain_party_ok(r, q)
            } else {
                empty_party_ok(r, q)
            };
            return party;
        }

        false
    }
}

struct Query<'a> {
    supply_rw: &'a str,
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
    on: Option<NaiveDate>,
}

fn empty_rule_applies(r: &ParsedConvention, scope: ConventionScope) -> bool {
    if r.cargo_class == ConventionCargoClass::Grain {
        return false;
    }
    match r.convention_info {
        ConventionStatus::WashingStation => scope == ConventionScope::Wash,
        ConventionStatus::RepairStation => scope == ConventionScope::Repair,
        ConventionStatus::ReserveStation => scope == ConventionScope::Reserve,
        _ => true,
    }
}

fn grain_rule_applies(r: &ParsedConvention, scope: ConventionScope) -> bool {
    scope == ConventionScope::Load
        && r.cargo_class != ConventionCargoClass::Empty
        && !r.convention_info.is_service_station()
}

fn empty_party_ok(r: &ParsedConvention, q: &Query<'_>) -> bool {
    if r.all_parties {
        return true;
    }
    okpo_in(r, q.sender_okpo)
        || name_in(r, q.sender_name)
        || q.rec_okpo.iter().any(|o| okpo_in(r, Some(o)))
        || q.rec_names.iter().any(|n| name_in(r, Some(n)))
}

fn grain_party_ok(r: &ParsedConvention, q: &Query<'_>) -> bool {
    if r.all_parties {
        return true;
    }
    q.rec_okpo.iter().any(|o| okpo_in(r, Some(o))) || q.rec_names.iter().any(|n| name_in(r, Some(n)))
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

fn push_id(map: &mut HashMap<String, Vec<usize>>, key: String, i: usize) {
    if key.is_empty() {
        return;
    }
    let v = map.entry(key).or_default();
    if !v.contains(&i) {
        v.push(i);
    }
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
            cars_on_station: 0,
        }
    }

    fn banned(idx: &ConventionIndex, s: &SupplyNode, d: &DemandNode) -> bool {
        idx.ban_for(s, d).is_some()
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

    #[test]
    fn generic_empty_ban_applies_to_wash_and_reserve() {
        let mut r = base_rule();
        r.dest_esr = vec!["555001".into()];
        let idx = ConventionIndex::build(vec![r]);
        let s = supply("СКВ");
        let mut wash = demand_load("555001", "Промывка", "СКВ", None, None, None, None, None);
        wash.purpose = DemandPurpose::Wash;
        assert!(banned(&idx, &s, &wash));
        let dest = EmptyDestRef {
            supply_railway: "СКВ",
            station_code: "555001",
            station_name: "Отстой",
            railway: "СКВ",
            sender_okpo: None,
            sender_name: None,
            recipient_okpos: &[],
            recipient_names: &[],
        };
        assert!(idx.ban_for_empty_dest(dest, ConventionScope::Reserve).is_some());
        assert!(idx.ban_for_empty_dest(dest, ConventionScope::Repair).is_some());
    }

    #[test]
    fn disabled_index_never_bans() {
        let idx = ConventionIndex::disabled();
        let s = supply("ЗСБ");
        let d = demand_load("987303", "X", "ПРВ", None, None, None, None, None);
        assert!(!banned(&idx, &s, &d));
    }

    #[test]
    fn others_never_reach_index() {
        use std::collections::HashMap;
        use super::super::conventions::{parse_hash, RailwayCatalog, RAILWAY_MAP_PATH};
        use chrono::NaiveDate;
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
        let load = parse_hash(&raw, NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(), &cat);
        let idx = ConventionIndex::build(load.active);
        assert!(idx.is_empty());
    }
}
