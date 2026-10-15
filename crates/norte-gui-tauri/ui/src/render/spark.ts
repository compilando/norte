// The throughput graph (ADR 0187): the host sends the series, one value per
// second, 0–100 against its own maximum; the terminal paints it as braille
// and the window draws it here.

const NS = "http://www.w3.org/2000/svg";
const HEIGHT = 8;

/** The series as an SVG line over a faint area, or `null` with no series. */
export function sparkSvg(series: readonly number[]): SVGSVGElement | null {
  if (series.length === 0) {
    return null;
  }
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("class", "spark");
  svg.setAttribute("viewBox", `0 0 ${String(series.length)} ${String(HEIGHT)}`);
  svg.setAttribute("preserveAspectRatio", "none");
  svg.setAttribute("aria-hidden", "true");
  const points = series.map((v, i) => {
    const y = HEIGHT - (Math.max(0, Math.min(100, v)) / 100) * HEIGHT;
    return `${String(i + 0.5)},${y.toFixed(2)}`;
  });
  // Colour from `currentColor`, not a variable of its own: a variable is
  // one more thing every theme would have to feed.
  const area = document.createElementNS(NS, "polygon");
  area.setAttribute(
    "points",
    [
      `0.5,${String(HEIGHT)}`,
      ...points,
      `${String(series.length - 0.5)},${String(HEIGHT)}`,
    ].join(" "),
  );
  area.setAttribute("fill", "currentColor");
  area.setAttribute("fill-opacity", "0.2");
  const line = document.createElementNS(NS, "polyline");
  line.setAttribute("points", points.join(" "));
  line.setAttribute("fill", "none");
  line.setAttribute("stroke", "currentColor");
  line.setAttribute("stroke-width", "1");
  line.setAttribute("vector-effect", "non-scaling-stroke");
  svg.append(area, line);
  return svg;
}
