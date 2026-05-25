import { ArcLayer, GeoJsonLayer, ScatterplotLayer, TextLayer } from "@deck.gl/layers";
import type { Layer } from "@deck.gl/core";
import type { FeatureCollection } from "geojson";
import type { ArcDatum, HoverInfo, NodeDatum } from "../types/map";
import type {
  RailwayZoneCollection,
  RailwayZoneLabel,
  RailwayZoneProperties,
} from "./railwayZones";

const SUPPLY_COLOR: [number, number, number, number] = [46, 125, 50, 220];
const DEMAND_COLOR: [number, number, number, number] = [211, 47, 47, 220];
const BOTH_COLOR: [number, number, number, number] = [123, 31, 162, 220];

function nodeColor(role: NodeDatum["role"]): [number, number, number, number] {
  switch (role) {
    case "supply":
      return SUPPLY_COLOR;
    case "demand":
      return DEMAND_COLOR;
    case "both":
      return BOTH_COLOR;
  }
}

function arcWidth(cars: number): number {
  return Math.min(8, Math.max(1, Math.sqrt(cars) * 2));
}

function nodeRadius(cars: number): number {
  return Math.min(12000, Math.max(3000, Math.sqrt(cars) * 4000));
}

export function buildArcLayer(
  data: ArcDatum[],
  onHover: (info: HoverInfo | null) => void,
): Layer {
  return new ArcLayer<ArcDatum>({
    id: "assignment-arcs",
    data,
    pickable: true,
    autoHighlight: true,
    getSourcePosition: (d) => d.sourcePosition,
    getTargetPosition: (d) => d.targetPosition,
    getSourceColor: SUPPLY_COLOR,
    getTargetColor: DEMAND_COLOR,
    getWidth: (d) => arcWidth(d.cars),
    getTilt: 0,
    greatCircle: true,
    onHover: (info) => {
      if (info.object && info.x != null && info.y != null) {
        onHover({
          kind: "arc",
          x: info.x,
          y: info.y,
          arc: info.object,
        });
      } else {
        onHover(null);
      }
    },
  });
}

export function buildNodeLayer(
  data: NodeDatum[],
  onHover: (info: HoverInfo | null) => void,
): Layer {
  return new ScatterplotLayer<NodeDatum>({
    id: "stations",
    data,
    pickable: true,
    autoHighlight: true,
    stroked: true,
    filled: true,
    radiusUnits: "meters",
    lineWidthUnits: "pixels",
    getPosition: (d) => d.position,
    getFillColor: (d) => nodeColor(d.role),
    getLineColor: [255, 255, 255, 200],
    getRadius: (d) => nodeRadius(d.cars_total),
    getLineWidth: 1,
    onHover: (info) => {
      if (info.object && info.x != null && info.y != null) {
        onHover({
          kind: "node",
          x: info.x,
          y: info.y,
          node: info.object,
        });
      } else {
        onHover(null);
      }
    },
  });
}

function rwLineColor(rw: string): [number, number, number, number] {
  let h = 0;
  for (let i = 0; i < rw.length; i++) {
    h = (Math.imul(31, h) + rw.charCodeAt(i)) >>> 0;
  }
  return [(h & 0x9f) + 80, ((h >> 8) & 0x9f) + 80, ((h >> 16) & 0x9f) + 80, 200];
}

export function buildRailwayZoneLayers(
  collection: RailwayZoneCollection,
  labels: RailwayZoneLabel[],
): Layer[] {
  const outline = new GeoJsonLayer({
    id: "railway-zones-outline",
    data: collection as FeatureCollection,
    pickable: false,
    filled: false,
    stroked: true,
    lineWidthUnits: "pixels",
    getLineWidth: 2,
    getLineColor: (f: { properties?: RailwayZoneProperties }) =>
      rwLineColor(f.properties?.rw ?? ""),
  });

  const text = new TextLayer<RailwayZoneLabel>({
    id: "railway-zones-labels",
    data: labels,
    pickable: false,
    getPosition: (d) => d.position,
    getText: (d) => d.text,
    getSize: 14,
    getColor: [40, 40, 40, 230],
    getTextAnchor: "start",
    getAlignmentBaseline: "top",
    fontFamily: "Noto Sans Regular, sans-serif",
    outlineWidth: 2,
    outlineColor: [255, 255, 255, 200],
  });

  return [outline, text];
}

export { SUPPLY_COLOR, DEMAND_COLOR, BOTH_COLOR };
