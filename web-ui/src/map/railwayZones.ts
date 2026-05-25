export interface RailwayZoneProperties {
  rw: string;
  station_count: number;
  label_lon: number;
  label_lat: number;
}

export interface RailwayZoneFeature {
  type: "Feature";
  properties: RailwayZoneProperties;
  geometry: {
    type: string;
    coordinates: unknown;
  };
}

export interface RailwayZoneCollection {
  type: "FeatureCollection";
  features: RailwayZoneFeature[];
}

export interface RailwayZoneLabel {
  position: [number, number];
  text: string;
}

export async function loadRailwayZones(): Promise<RailwayZoneCollection | null> {
  const res = await fetch("/map/railways_voronoi.geojson", { cache: "no-store" });
  if (!res.ok) {
    if (res.status === 404) {
      return null;
    }
    throw new Error(`railways_voronoi.geojson: HTTP ${res.status}`);
  }
  return (await res.json()) as RailwayZoneCollection;
}

export function labelsFromZones(
  collection: RailwayZoneCollection,
): RailwayZoneLabel[] {
  return collection.features.map((f) => ({
    position: [f.properties.label_lon, f.properties.label_lat],
    text: f.properties.rw,
  }));
}
