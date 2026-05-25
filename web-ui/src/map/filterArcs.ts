import type {
  ArcDatum,
  FilteredMapData,
  MapArc,
  NodeDatum,
  NodeRole,
} from "../types/map";

function arcToDatum(arc: MapArc): ArcDatum | null {
  if (arc.geo_status !== "ok") return null;
  const { from, to } = arc;
  if (
    from.lat == null ||
    from.lon == null ||
    to.lat == null ||
    to.lon == null
  ) {
    return null;
  }
  return {
    id: arc.id,
    sourcePosition: [from.lon, from.lat],
    targetPosition: [to.lon, to.lat],
    cars: arc.cars,
    cost_rub: arc.cost_rub,
    distance_km: arc.distance_km,
    supply_kind: arc.supply_kind,
    supply_railway: arc.supply_railway,
    demand_railway: arc.demand_railway,
    supply_period: arc.supply_period ?? 1,
    from_name: from.name,
    to_name: to.name,
    from_esr6: from.esr6,
    to_esr6: to.esr6,
  };
}

function aggregateNodes(arcs: ArcDatum[]): NodeDatum[] {
  const byEsr = new Map<
    string,
    { name: string; lat: number; lon: number; supply: number; demand: number }
  >();

  for (const arc of arcs) {
    const [fromLon, fromLat] = arc.sourcePosition;
    const [toLon, toLat] = arc.targetPosition;

    const from = byEsr.get(arc.from_esr6) ?? {
      name: arc.from_name,
      lat: fromLat,
      lon: fromLon,
      supply: 0,
      demand: 0,
    };
    from.supply += arc.cars;
    byEsr.set(arc.from_esr6, from);

    const to = byEsr.get(arc.to_esr6) ?? {
      name: arc.to_name,
      lat: toLat,
      lon: toLon,
      supply: 0,
      demand: 0,
    };
    to.demand += arc.cars;
    byEsr.set(arc.to_esr6, to);
  }

  const nodes: NodeDatum[] = [];
  for (const [esr6, acc] of byEsr) {
    let role: NodeRole;
    let cars_total: number;
    if (acc.supply > 0 && acc.demand > 0) {
      role = "both";
      cars_total = acc.supply + acc.demand;
    } else if (acc.supply > 0) {
      role = "supply";
      cars_total = acc.supply;
    } else {
      role = "demand";
      cars_total = acc.demand;
    }
    nodes.push({
      esr6,
      name: acc.name,
      position: [acc.lon, acc.lat],
      role,
      cars_total,
    });
  }

  nodes.sort((a, b) => a.esr6.localeCompare(b.esr6));
  return nodes;
}

/** Предложение 2–10 суток (дислокация). */
export function isDislocationPeriod(supplyPeriod: number): boolean {
  return supplyPeriod === 10;
}

export function filterArcs(
  arcs: MapArc[],
  selectedSupply: ReadonlySet<string>,
  selectedDemand: ReadonlySet<string>,
  showDislocationArcs: boolean,
): FilteredMapData {
  const visible = arcs
    .filter((arc) => {
      if (isDislocationPeriod(arc.supply_period ?? 1) && !showDislocationArcs) {
        return false;
      }
      const okSupply =
        selectedSupply.size === 0 || selectedSupply.has(arc.supply_railway);
      const okDemand =
        selectedDemand.size === 0 || selectedDemand.has(arc.demand_railway);
      return okSupply && okDemand;
    })
    .map(arcToDatum)
    .filter((a): a is ArcDatum => a != null);

  const visibleCars = visible.reduce((sum, a) => sum + a.cars, 0);

  return {
    arcs: visible,
    nodes: aggregateNodes(visible),
    visibleArcCount: visible.length,
    visibleCars,
  };
}

export function boundsFromData(
  arcs: ArcDatum[],
  nodes: NodeDatum[],
): [[number, number], [number, number]] | null {
  let minLon = Infinity;
  let minLat = Infinity;
  let maxLon = -Infinity;
  let maxLat = -Infinity;

  const extend = (lon: number, lat: number) => {
    minLon = Math.min(minLon, lon);
    minLat = Math.min(minLat, lat);
    maxLon = Math.max(maxLon, lon);
    maxLat = Math.max(maxLat, lat);
  };

  for (const arc of arcs) {
    extend(...arc.sourcePosition);
    extend(...arc.targetPosition);
  }
  for (const node of nodes) {
    extend(...node.position);
  }

  if (!Number.isFinite(minLon)) return null;
  return [
    [minLon, minLat],
    [maxLon, maxLat],
  ];
}
