#!/usr/bin/env node
import { writeFileSync, mkdirSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { layers, LIGHT } from "../../web-ui/node_modules/@protomaps/basemaps/dist/esm/index.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "../..");
const mapDir = join(root, "data/map");

mkdirSync(mapDir, { recursive: true });

const style = {
  version: 8,
  name: "railoptim-offline-light",
  glyphs: "/map/glyphs/{fontstack}/{range}.pbf",
  sprite: "/map/sprites/v4/light",
  sources: {
    protomaps: {
      type: "vector",
      url: "pmtiles:///map/ru_cis.pmtiles",
      attribution:
        '<a href="https://protomaps.com">Protomaps</a> © <a href="https://openstreetmap.org">OpenStreetMap</a>',
    },
  },
  layers: layers("protomaps", LIGHT, { lang: "ru" }),
};

writeFileSync(join(mapDir, "style.json"), JSON.stringify(style));
console.log("wrote data/map/style.json");
