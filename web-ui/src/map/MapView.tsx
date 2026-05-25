import { useEffect, useMemo, useRef, useState } from "react";
import Map, { type MapRef, useControl } from "react-map-gl/maplibre";
import { MapboxOverlay } from "@deck.gl/mapbox";
import type { DeckProps, Layer } from "@deck.gl/core";
import type { StyleSpecification } from "maplibre-gl";
import maplibregl from "maplibre-gl";

import { buildArcLayer, buildNodeLayer, buildRailwayZoneLayers } from "./layers";
import {
  labelsFromZones,
  loadRailwayZones,
  type RailwayZoneCollection,
} from "./railwayZones";
import { boundsFromData } from "./filterArcs";
import { ensurePmtilesProtocol, loadOfflineMapStyle } from "./pmtilesProtocol";
import type { ArcDatum, HoverInfo, NodeDatum } from "../types/map";

const INITIAL_VIEW = {
  longitude: 50,
  latitude: 55,
  zoom: 3.5,
};

type DeckOverlayProps = DeckProps & { interleaved?: boolean };

function DeckGLOverlay({ interleaved = false, ...deckProps }: DeckOverlayProps) {
  const overlay = useControl<MapboxOverlay>(
    () => new MapboxOverlay({ ...deckProps, interleaved }),
  );
  overlay.setProps({ ...deckProps, interleaved });
  return null;
}

interface MapViewProps {
  arcs: ArcDatum[];
  nodes: NodeDatum[];
  showNodes: boolean;
  showRailwayZones: boolean;
  onHover: (info: HoverInfo | null) => void;
  fitToken: number;
}

export function MapView({
  arcs,
  nodes,
  showNodes,
  showRailwayZones,
  onHover,
  fitToken,
}: MapViewProps) {
  const mapRef = useRef<MapRef>(null);
  const [mapStyle, setMapStyle] = useState<StyleSpecification | null>(null);
  const [styleError, setStyleError] = useState<string | null>(null);
  const [railwayZones, setRailwayZones] = useState<RailwayZoneCollection | null>(
    null,
  );
  const [zonesError, setZonesError] = useState<string | null>(null);
  const [zonesLoaded, setZonesLoaded] = useState(false);

  useEffect(() => {
    ensurePmtilesProtocol();
    loadOfflineMapStyle()
      .then(setMapStyle)
      .catch((e) =>
        setStyleError(e instanceof Error ? e.message : String(e)),
      );
  }, []);

  // GeoJSON грузим сразу (не только при включённом чекбоксе), чтобы не зависеть от кэша toggle.
  useEffect(() => {
    let cancelled = false;
    loadRailwayZones()
      .then((data) => {
        if (!cancelled) {
          setRailwayZones(data);
          setZonesLoaded(true);
          if (!data || data.features.length === 0) {
            setZonesError("Файл зон пуст или не найден");
          } else {
            setZonesError(null);
          }
        }
      })
      .catch((e) => {
        if (!cancelled) {
          setRailwayZones(null);
          setZonesLoaded(true);
          setZonesError(e instanceof Error ? e.message : String(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const layers = useMemo(() => {
    const result: Layer[] = [];
    result.push(buildArcLayer(arcs, onHover));
    if (showNodes) {
      result.push(buildNodeLayer(nodes, onHover));
    }
    // Контуры поверх дуг (overlay, не interleaved — иначе линии под тайлами).
    if (showRailwayZones && railwayZones && railwayZones.features.length > 0) {
      result.push(
        ...buildRailwayZoneLayers(railwayZones, labelsFromZones(railwayZones)),
      );
    }
    return result;
  }, [arcs, nodes, showNodes, showRailwayZones, railwayZones, onHover]);

  useEffect(() => {
    const bounds = boundsFromData(arcs, nodes);
    const map = mapRef.current?.getMap();
    if (!bounds || !map) return;
    map.fitBounds(bounds, { padding: 72, duration: 700, maxZoom: 8 });
  }, [fitToken, arcs, nodes]);

  if (styleError) {
    return (
      <div className="map-style-error">
        Подложка недоступна: {styleError}. Проверьте `data/map/` и
        `ru_cis.pmtiles` на сервере.
      </div>
    );
  }

  if (!mapStyle) {
    return <div className="map-style-loading">Загрузка карты…</div>;
  }

  return (
    <>
      {showRailwayZones && zonesLoaded && zonesError && (
        <div className="map-zones-warn" role="status">
          Зоны дорог: {zonesError}
        </div>
      )}
      {showRailwayZones && zonesLoaded && !zonesError && railwayZones && (
        <div className="map-zones-ok" role="status">
          Зоны дорог: {railwayZones.features.length} сетей
        </div>
      )}
      <Map
        ref={mapRef}
        mapLib={maplibregl}
        initialViewState={INITIAL_VIEW}
        mapStyle={mapStyle}
        style={{ width: "100%", height: "100%" }}
        attributionControl={{}}
      >
        <DeckGLOverlay layers={layers} interleaved={false} />
      </Map>
    </>
  );
}
