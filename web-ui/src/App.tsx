import { useCallback, useEffect, useMemo, useState } from "react";

import { fetchPlanMap, reloadPlan } from "./api/client";
import { RailwayFilter } from "./components/RailwayFilter";
import { MapTooltip } from "./components/MapTooltip";
import { filterArcs } from "./map/filterArcs";
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
    return filterArcs(data.arcs, selectedSupply, selectedDemand);
  }, [data, selectedSupply, selectedDemand]);

  useEffect(() => {
    setFitToken((t) => t + 1);
  }, [selectedSupply, selectedDemand]);

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
                  checked={showNodes}
                  onChange={(e) => setShowNodes(e.target.checked)}
                />
                Показывать узлы станций
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
                <span className="dot supply" /> Образование
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
