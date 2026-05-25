import maplibregl from "maplibre-gl";
import { Protocol } from "pmtiles";

let registered = false;

/** Регистрирует pmtiles:// один раз на приложение. */
export function ensurePmtilesProtocol(): void {
  if (registered) return;
  const protocol = new Protocol();
  maplibregl.addProtocol("pmtiles", protocol.tile);
  registered = true;
}

/** Подставляет same-origin URL тайлов в style с сервера. */
export async function loadOfflineMapStyle(): Promise<maplibregl.StyleSpecification> {
  const res = await fetch("/map/style.json");
  if (!res.ok) {
    throw new Error(`не удалось загрузить /map/style.json: ${res.status}`);
  }
  const style = (await res.json()) as maplibregl.StyleSpecification & {
    sources?: Record<string, { url?: string }>;
  };
  const pmtilesUrl = `pmtiles://${window.location.origin}/map/ru_cis.pmtiles`;
  if (style.sources?.protomaps) {
    style.sources.protomaps.url = pmtilesUrl;
  }
  return style;
}
