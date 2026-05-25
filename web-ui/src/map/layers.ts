import { ArcLayer, ScatterplotLayer } from "@deck.gl/layers";
import type { Layer } from "@deck.gl/core";
import type { ArcDatum, HoverInfo, NodeDatum } from "../types/map";

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

export { SUPPLY_COLOR, DEMAND_COLOR, BOTH_COLOR };
