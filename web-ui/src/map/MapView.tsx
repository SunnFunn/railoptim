import { useEffect, useMemo, useRef, useState } from "react";
import Map, { type MapRef, useControl } from "react-map-gl/maplibre";
import { MapboxOverlay } from "@deck.gl/mapbox";
import type { DeckProps } from "@deck.gl/core";
import type { StyleSpecification } from "maplibre-gl";
import maplibregl from "maplibre-gl";

import { buildArcLayer, buildNodeLayer } from "./layers";
import { boundsFromData } from "./filterArcs";
import { ensurePmtilesProtocol, loadOfflineMapStyle } from "./pmtilesProtocol";
import type { ArcDatum, HoverInfo, NodeDatum } from "../types/map";

const INITIAL_VIEW = {
  longitude: 50,
  latitude: 55,
  zoom: 3.5,
};

function DeckGLOverlay(props: DeckProps) {
  const overlay = useControl<MapboxOverlay>(() => new MapboxOverlay(props));
  overlay.setProps(props);
  return null;
}

interface MapViewProps {
  arcs: ArcDatum[];
  nodes: NodeDatum[];
  showNodes: boolean;
  onHover: (info: HoverInfo | null) => void;
  fitToken: number;
}

export function MapView({
  arcs,
  nodes,
  showNodes,
  onHover,
  fitToken,
}: MapViewProps) {
  const mapRef = useRef<MapRef>(null);
  const [mapStyle, setMapStyle] = useState<StyleSpecification | null>(null);
  const [styleError, setStyleError] = useState<string | null>(null);

  useEffect(() => {
    ensurePmtilesProtocol();
    loadOfflineMapStyle()
      .then(setMapStyle)
      .catch((e) =>
        setStyleError(e instanceof Error ? e.message : String(e)),
      );
  }, []);

  const layers = useMemo(() => {
    const result = [buildArcLayer(arcs, onHover)];
    if (showNodes) {
      result.push(buildNodeLayer(nodes, onHover));
    }
    return result;
  }, [arcs, nodes, showNodes, onHover]);

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
    <Map
      ref={mapRef}
      mapLib={maplibregl}
      initialViewState={INITIAL_VIEW}
      mapStyle={mapStyle}
      style={{ width: "100%", height: "100%" }}
      attributionControl={true}
    >
      <DeckGLOverlay layers={layers} interleaved />
    </Map>
  );
}
