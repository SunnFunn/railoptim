import { useEffect, useMemo, useRef } from "react";
import Map, { type MapRef, useControl } from "react-map-gl/maplibre";
import { MapboxOverlay } from "@deck.gl/mapbox";
import type { DeckProps } from "@deck.gl/core";
import maplibregl from "maplibre-gl";

import { buildArcLayer, buildNodeLayer } from "./layers";
import { boundsFromData } from "./filterArcs";
import type { ArcDatum, HoverInfo, NodeDatum } from "../types/map";

const DEFAULT_STYLE =
  import.meta.env.VITE_MAP_STYLE ??
  "https://tiles.openfreemap.org/styles/liberty";

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

  return (
    <Map
      ref={mapRef}
      mapLib={maplibregl}
      initialViewState={INITIAL_VIEW}
      mapStyle={DEFAULT_STYLE}
      style={{ width: "100%", height: "100%" }}
      attributionControl={true}
    >
      <DeckGLOverlay layers={layers} interleaved />
    </Map>
  );
}
