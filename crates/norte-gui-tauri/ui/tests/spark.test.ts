// The throughput graph (ADR 0187): the window draws the series the host
// sends, and draws nothing without one.

import { describe, expect, it } from "vitest";

import { taskNode } from "../src/render/dom";
import { sparkSvg } from "../src/render/spark";
import type { TaskView } from "../src/types";

const task = (over: Partial<TaskView>): TaskView => ({
  task_id: 1,
  kind: "copy",
  state: "running",
  percent: 40,
  rate: "",
  eta: "",
  detail: "big.iso",
  detail_hostile: false,
  foreign: false,
  ...over,
});

describe("the throughput graph", () => {
  it("draws nothing for an empty series", () => {
    expect(sparkSvg([])).toBeNull();
  });

  it("draws one point per second, clamped to the box", () => {
    const series = Array.from({ length: 32 }, (_, i) => (i * 10) % 130);
    const svg = sparkSvg(series);
    expect(svg).not.toBeNull();
    const line = svg?.querySelector("polyline");
    const points = line?.getAttribute("points")?.split(" ") ?? [];
    expect(points).toHaveLength(32);
    for (const p of points) {
      const y = Number(p.split(",")[1]);
      expect(y).toBeGreaterThanOrEqual(0);
      expect(y).toBeLessThanOrEqual(8);
    }
    expect(svg?.getAttribute("aria-hidden")).toBe("true");
  });

  it("a task row carries its graph and its speed only when it has them", () => {
    const tr = (k: string): string => k;
    const moving = taskNode(task({ spark: [0, 50, 100], rate: "8 MiB/s" }), tr);
    expect(moving.querySelector("svg.spark")).not.toBeNull();
    expect(moving.querySelector(".task-rate")?.textContent).toBe("8 MiB/s");
    const quiet = taskNode(task({}), tr);
    expect(quiet.querySelector("svg.spark")).toBeNull();
    expect(quiet.querySelector(".task-rate")).toBeNull();
  });
});
