import { useCallback, useEffect, useMemo, useState } from "react";

import { fetchPlanMap, reloadPlan } from "./api/client";
import { RailwayFilter } from "./components/RailwayFilter";
import { MapTooltip } from "./components/MapTooltip";
import { filterArcs, isDislocationPeriod } from "./map/filterArcs";
import { MapView } from "./map/MapView";
import type { HoverInfo, PlanMapResponse } from "./types/map";

function fmtRub(value: number): string {
  return new Intl.NumberFormat("ru-RU", {
    style: "currency",
    currency: "RUB",
    maximumFractionDigits: 0,
  }).format(value);
}

export default function App() {
  const [data, setData] = useState<PlanMapResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [selectedSupply, setSelectedSupply] = useState<Set<string>>(new Set());
  const [selectedDemand, setSelectedDemand] = useState<Set<string>>(new Set());
  const [showNodes, setShowNodes] = useState(true);
  const [showRailwayZones, setShowRailwayZones] = useState(() => {
    try {
      const v = sessionStorage.getItem("railoptim.showRailwayZones");
      return v !== "0";
    } catch {
      return true;
    }
  });
  const [showDislocationArcs, setShowDislocationArcs] = useState(() => {
    try {
      return sessionStorage.getItem("railoptim.showDislocationArcs") === "1";
    } catch {
      return false;
    }
  });
  const [hover, setHover] = useState<HoverInfo | null>(null);
  const [fitToken, setFitToken] = useState(0);

  const loadMap = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const map = await fetchPlanMap();
      setData(map);
      setSelectedSupply(new Set());
      setSelectedDemand(new Set());
      setFitToken((t) => t + 1);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setData(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadMap();
  }, [loadMap]);

  const filtered = useMemo(() => {
    if (!data) {
      return { arcs: [], nodes: [], visibleArcCount: 0, visibleCars: 0 };
    }
    return filterArcs(
      data.arcs,
      selectedSupply,
      selectedDemand,
      showDislocationArcs,
    );
  }, [data, selectedSupply, selectedDemand, showDislocationArcs]);

  const arcCounts = useMemo(() => {
    if (!data) return { day1: 0, dislocation: 0, day1Cars: 0, dislocationCars: 0 };
    let day1 = 0;
    let dislocation = 0;
    let day1Cars = 0;
    let dislocationCars = 0;
    for (const arc of data.arcs) {
      if (arc.geo_status !== "ok") continue;
      if (isDislocationPeriod(arc.supply_period ?? 1)) {
        dislocation += 1;
        dislocationCars += arc.cars;
      } else {
        day1 += 1;
        day1Cars += arc.cars;
      }
    }
    return { day1, dislocation, day1Cars, dislocationCars };
  }, [data]);

  useEffect(() => {
    setFitToken((t) => t + 1);
  }, [selectedSupply, selectedDemand, showDislocationArcs]);

  const onReload = async () => {
    setLoading(true);
    try {
      await reloadPlan();
      await loadMap();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setLoading(false);
    }
  };

  const resetFilters = () => {
    setSelectedSupply(new Set());
    setSelectedDemand(new Set());
    setShowDislocationArcs(false);
    try {
      sessionStorage.setItem("railoptim.showDislocationArcs", "0");
    } catch {
      /* ignore */
    }
  };

  return (
    <div className="app">
      <aside className="sidebar">
        <header className="sidebar-header">
          <h1>railoptim</h1>
          <p className="subtitle">Карта назначений порожних вагонов</p>
        </header>

        {loading && <div className="status">Загрузка…</div>}
        {error && (
          <div className="error-box">
            <p>{error}</p>
            <p className="hint">
              Убедитесь, что railoptim-web запущен и в tmp/ есть result_*.json.
              После batch: <code>curl -X POST …/api/v1/plans/reload</code>
            </p>
            <button type="button" className="btn" onClick={() => void loadMap()}>
              Повторить
            </button>
          </div>
        )}

        {data && (
          <>
            <section className="panel">
              <h2>План</h2>
              <dl className="kv">
                <dt>ID</dt>
                <dd>{data.plan_id}</dd>
                <dt>Solver</dt>
                <dd>{data.summary.solver_status}</dd>
                <dt>Вагонов</dt>
                <dd>{data.summary.assigned_cars}</dd>
                <dt>Стоимость</dt>
                <dd>{fmtRub(data.summary.total_cost_rub)}</dd>
                <dt>Назначений</dt>
                <dd>{data.summary.assignment_count}</dd>
              </dl>
              <button
                type="button"
                className="btn"
                disabled={loading}
                onClick={() => void onReload()}
              >
                Обновить план
              </button>
            </section>

            <section className="panel">
              <h2>На карте</h2>
              <dl className="kv">
                <dt>Дуги</dt>
                <dd>
                  {filtered.visibleArcCount} / {data.stats.arcs_resolved} (geo ok)
                </dd>
                <dt>1-е сутки</dt>
                <dd>
                  {arcCounts.day1} дуг, {arcCounts.day1Cars} ваг.
                </dd>
                <dt>2–10 суток</dt>
                <dd>
                  {arcCounts.dislocation} дуг, {arcCounts.dislocationCars} ваг.
                </dd>
                <dt>Вагонов</dt>
                <dd>{filtered.visibleCars}</dd>
                <dt>Узлов</dt>
                <dd>{filtered.nodes.length}</dd>
                <dt>Без geo</dt>
                <dd>{data.stats.arcs_missing_geo}</dd>
              </dl>
              <label className="checkbox">
                <input
                  type="checkbox"
                  checked={showDislocationArcs}
                  onChange={(e) => {
                    const on = e.target.checked;
                    setShowDislocationArcs(on);
                    try {
                      sessionStorage.setItem(
                        "railoptim.showDislocationArcs",
                        on ? "1" : "0",
                      );
                    } catch {
                      /* ignore */
                    }
                  }}
                />
                Вагоны 2–10 суток (дислокация)
              </label>
              <label className="checkbox">
                <input
                  type="checkbox"
                  checked={showNodes}
                  onChange={(e) => setShowNodes(e.target.checked)}
                />
                Показывать узлы станций
              </label>
              <label className="checkbox">
                <input
                  type="checkbox"
                  checked={showRailwayZones}
                  onChange={(e) => {
                    const on = e.target.checked;
                    setShowRailwayZones(on);
                    try {
                      sessionStorage.setItem("railoptim.showRailwayZones", on ? "1" : "0");
                    } catch {
                      /* ignore */
                    }
                  }}
                />
                Зоны дорог (Supermap)
              </label>
            </section>

            <section className="panel">
              <h2>Фильтр дорог</h2>
              <RailwayFilter
                label="Дороги образования"
                options={data.filters.supply_railways}
                selected={selectedSupply}
                onChange={setSelectedSupply}
              />
              <RailwayFilter
                label="Дороги погрузки"
                options={data.filters.demand_railways}
                selected={selectedDemand}
                onChange={setSelectedDemand}
              />
              <button type="button" className="btn btn-secondary" onClick={resetFilters}>
                Сбросить все фильтры
              </button>
            </section>

            <section className="panel legend">
              <h2>Легенда</h2>
              <div className="legend-row">
                <span className="dot supply" /> 1-е сутки
              </div>
              <div className="legend-row">
                <span className="dot dislocation" /> 2–10 суток
              </div>
              <div className="legend-row">
                <span className="dot demand" /> Погрузка
              </div>
              <div className="legend-row">
                <span className="dot both" /> Обе роли
              </div>
            </section>
          </>
        )}
      </aside>

      <main className="map-wrap">
        {data && (
          <MapView
            arcs={filtered.arcs}
            nodes={filtered.nodes}
            showNodes={showNodes}
            showRailwayZones={showRailwayZones}
            onHover={setHover}
            fitToken={fitToken}
          />
        )}
        {!data && !loading && !error && (
          <div className="map-placeholder">Нет данных для карты</div>
        )}
        <MapTooltip info={hover} />
      </main>
    </div>
  );
}
