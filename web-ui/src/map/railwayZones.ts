export interface RailwayZoneProperties {
  rw: string;
  label_lon: number;
  label_lat: number;
  name_supermap?: string;
  name_eng?: string;
  station_count?: number;
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

const ZONE_GEOJSON = "/map/railways_zones.geojson";

export async function loadRailwayZones(): Promise<RailwayZoneCollection | null> {
  const res = await fetch(ZONE_GEOJSON, { cache: "no-store" });
  if (!res.ok) {
    if (res.status === 404) {
      return null;
    }
    throw new Error(`${ZONE_GEOJSON}: HTTP ${res.status}`);
  }
  return (await res.json()) as RailwayZoneCollection;
}

export function labelsFromZones(
  collection: RailwayZoneCollection,
): RailwayZoneLabel[] {
  return collection.features.map((f) => ({
    position: [f.properties.label_lon, f.properties.label_lat],
    text: f.properties.rw.toUpperCase(),
  }));
}

/** Символы для font atlas deck.gl (кириллица в кодах rw). */
export function zoneLabelCharacterSet(labels: RailwayZoneLabel[]): string {
  const chars = new Set<string>();
  for (const { text } of labels) {
    for (const ch of text) {
      chars.add(ch);
    }
  }
  return [...chars].sort().join("");
}
