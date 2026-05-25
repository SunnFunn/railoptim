import type { HoverInfo } from "../types/map";

interface MapTooltipProps {
  info: HoverInfo | null;
}

function fmtRub(value: number): string {
  return new Intl.NumberFormat("ru-RU", {
    style: "currency",
    currency: "RUB",
    maximumFractionDigits: 0,
  }).format(value);
}

export function MapTooltip({ info }: MapTooltipProps) {
  if (!info) return null;

  if (info.kind === "arc" && info.arc) {
    const a = info.arc;
    return (
      <div className="map-tooltip" style={{ left: info.x + 12, top: info.y + 12 }}>
        <strong>
          {a.from_name} → {a.to_name}
        </strong>
        <div>
          {a.supply_railway} → {a.demand_railway}
        </div>
        <div>
          {a.from_esr6} → {a.to_esr6}
        </div>
        <div>Вагонов: {a.cars}</div>
        <div>Расстояние: {a.distance_km} км</div>
        <div>Стоимость: {fmtRub(a.cost_rub)}</div>
        <div>Тип: {a.supply_kind}</div>
      </div>
    );
  }

  if (info.kind === "node" && info.node) {
    const n = info.node;
    const roleLabel =
      n.role === "supply"
        ? "образование"
        : n.role === "demand"
          ? "погрузка"
          : "образование + погрузка";
    return (
      <div className="map-tooltip" style={{ left: info.x + 12, top: info.y + 12 }}>
        <strong>{n.name}</strong>
        <div>ЕСР: {n.esr6}</div>
        <div>Роль: {roleLabel}</div>
        <div>Вагонов: {n.cars_total}</div>
      </div>
    );
  }

  return null;
}
