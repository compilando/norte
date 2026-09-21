// Los iconos de la barra de actividad (spec 2026-09-21): uno por panel de
// serie, dibujados aquí con trazos simples sobre una rejilla de 24.
//
// Son PROPIOS y no de un juego de iconos ajeno: seis trazos no valen una
// dependencia ni una licencia más que auditar. Pintan con `currentColor`, así
// que el estado del botón —cerrado, abierto, con el teclado— y el tema los
// colorean sin que esta tabla sepa nada de ellos.
//
// Un kind que no está aquí —el de un plugin, o uno nuevo que se olvidó— no se
// queda sin botón: el renderer pinta su LETRA, que es lo que ya se sabe de él.

const SVG = "http://www.w3.org/2000/svg";

/** Las piezas de un icono: trazos `d` y círculos `[cx, cy, r]`. */
interface Figura {
  paths: string[];
  circles?: [number, number, number][];
}

const FIGURAS: Record<string, Figura> = {
  // Sitios: una estrella, lo marcado como favorito.
  places: {
    paths: ["M12 3.5l2.6 5.3 5.9.9-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8-4.3-4.1 5.9-.9z"],
  },
  // Árbol: una carpeta arriba y dos ramas que cuelgan de ella.
  tree: {
    paths: ["M4 4h6v5H4z", "M14 11h6v4h-6z", "M14 17h6v4h-6z", "M7 9v10h7", "M7 13h7"],
  },
  // Visor: un ojo.
  viewer: {
    paths: ["M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"],
    circles: [[12, 12, 3]],
  },
  // Procesos: el pulso de algo que está trabajando.
  processes: { paths: ["M3 12h4l2.5-6 5 12 2.5-6h4"] },
  // Detalles: la «i» de información.
  metadata: { paths: ["M12 11v6", "M12 7.5v.5"], circles: [[12, 12, 9]] },
  // Registro: líneas de texto que se acumulan.
  log: { paths: ["M5 6h14", "M5 10h14", "M5 14h10", "M5 18h7"] },
  // Mapa de disco: un queso con una porción fuera.
  "disk-map": { paths: ["M11 4a8 8 0 1 0 9 9h-9z", "M14 3.5a7 7 0 0 1 6.5 6.5H14z"] },
  // Línea de tiempo: un reloj.
  timeline: { paths: ["M12 7v5l3.5 2"], circles: [[12, 12, 9]] },
  // Historia de git: una rama que se separa y vuelve.
  gitlog: {
    paths: ["M6 8v8", "M18 8.5c0 5-6 4-11 7"],
    circles: [
      [6, 5.5, 2.5],
      [6, 18.5, 2.5],
      [18, 6, 2.5],
    ],
  },
};

/**
 * El icono de un kind, o `null` si no tiene uno propio.
 *
 * Decorativo para la accesibilidad (`aria-hidden`): el nombre del panel va en
 * el botón, y un lector que leyera también el dibujo diría dos veces lo mismo.
 */
export function iconoDePanel(doc: Document, kind: string): SVGSVGElement | null {
  const figura = FIGURAS[kind];
  if (figura === undefined) {
    return null;
  }
  const svg = doc.createElementNS(SVG, "svg");
  svg.setAttribute("class", "panelbar-icon");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  for (const d of figura.paths) {
    const p = doc.createElementNS(SVG, "path");
    p.setAttribute("d", d);
    svg.append(p);
  }
  for (const [cx, cy, r] of figura.circles ?? []) {
    const c = doc.createElementNS(SVG, "circle");
    c.setAttribute("cx", String(cx));
    c.setAttribute("cy", String(cy));
    c.setAttribute("r", String(r));
    svg.append(c);
  }
  return svg;
}

/** La cifra de una insignia: a partir de cien, `99+`. */
export function cifraDeInsignia(n: number): string {
  return n > 99 ? "99+" : String(n);
}
