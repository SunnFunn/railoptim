export type GeoStatus = "ok" | "missing";
export type NodeRole = "supply" | "demand" | "both";

export interface MapGeoEndpoint {
  esr6: string;
  name: string;
  lat: number | null;
  lon: number | null;
}

export interface MapArc {
  id: number;
  from: MapGeoEndpoint;
  to: MapGeoEndpoint;
  cars: number;
  distance_km: number;
  cost_rub: number;
  supply_kind: string;
  supply_railway: string;
  demand_railway: string;
  demand_period: number;
  geo_status: GeoStatus;
}

export interface MapNode {
  esr6: string;
  name: string;
  lat: number;
  lon: number;
  role: NodeRole;
  cars_total: number;
}

export interface MapStats {
  arcs_total: number;
  arcs_resolved: number;
  arcs_missing_geo: number;
  nodes_total: number;
}

export interface MapFiltersMeta {
  supply_railways: string[];
  demand_railways: string[];
}

export interface PlanSummary {
  plan_id: string;
  path: string;
  loaded_at: string;
  report_timestamp: string;
  solver_status: string;
  total_cost_rub: number;
  assigned_cars: number;
  assignment_count: number;
}

export interface PlanMapResponse {
  plan_id: string;
  summary: PlanSummary;
  stats: MapStats;
  filters: MapFiltersMeta;
  arcs: MapArc[];
  nodes: MapNode[];
}

export interface MetaResponse {
  service: string;
  version: string;
  stations_geo_count: number;
  stations_geo_path: string;
  optim_result_dir: string;
  plan: PlanSummary | null;
}

export interface ApiErrorBody {
  error: string;
}

export interface ArcDatum {
  id: number;
  sourcePosition: [number, number];
  targetPosition: [number, number];
  cars: number;
  cost_rub: number;
  distance_km: number;
  supply_kind: string;
  supply_railway: string;
  demand_railway: string;
  from_name: string;
  to_name: string;
  from_esr6: string;
  to_esr6: string;
}

export interface NodeDatum {
  esr6: string;
  name: string;
  position: [number, number];
  role: NodeRole;
  cars_total: number;
}

export interface FilteredMapData {
  arcs: ArcDatum[];
  nodes: NodeDatum[];
  visibleArcCount: number;
  visibleCars: number;
}

export interface HoverInfo {
  kind: "arc" | "node";
  x: number;
  y: number;
  arc?: ArcDatum;
  node?: NodeDatum;
}
